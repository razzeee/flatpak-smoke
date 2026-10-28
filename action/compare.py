"""Run a published bundle and candidate against frozen screenshot inputs.

Requires a local Docker screenshot image and Python 3.9+. See docs/paired-runs.md.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys

import run as runner


COMMIT = re.compile(r"[0-9a-f]{64}")
DIGEST = re.compile(r"v1:sha256:[0-9a-f]{64}")
APP_REF = re.compile(r"app/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
ORDER = ("baseline", "candidate", "candidate", "baseline")


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def object_value(value):
    return value if isinstance(value, dict) else {}


def read_record(path):
    try:
        value = json.loads(path.read_text())
        return value if isinstance(value, dict) else None
    except (OSError, ValueError):
        return None


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def tree_hash(root):
    """Fingerprint the private snapshot, without following OSTree payload symlinks."""
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            content = ["symlink", os.readlink(path)]
        elif stat.S_ISREG(mode):
            content = ["file", file_hash(path)]
        elif stat.S_ISDIR(mode):
            content = ["directory"]
        else:
            raise ValueError(f"unsupported input file: {path}")
        row = [path.relative_to(root).as_posix(), stat.S_IMODE(mode), content]
        digest.update(json.dumps(row, ensure_ascii=True).encode() + b"\n")
    return "sha256:" + digest.hexdigest()


def artifacts(root, result, manifest):
    """Retain screenshot byte identities for review, never for verdict thresholds."""
    errors, inventory = [], {}
    paths = object_value(result).get("screenshots")
    captures = object_value(manifest).get("captures")
    if not isinstance(paths, list) or not isinstance(captures, list):
        return {}, ["missing_screenshot_inventory"]
    for name in [*paths, *(object_value(capture).get("path") for capture in captures)]:
        try:
            path = runner.workspace_path(root, name, "screenshot", "file")
            file = root / path
            if file.stat().st_size == 0:
                raise ValueError("empty screenshot")
            inventory[path] = {"sha256": file_hash(file), "bytes": file.stat().st_size}
        except (OSError, ValueError, TypeError):
            errors.append("missing_or_invalid_screenshot")
    if object_value(result).get("status") == "passed" and not captures:
        errors.append("passed_without_captures")
    return inventory, errors


def classify(attempts, baseline, snapshot_unchanged=True):
    """Conservative evidence classification; never infer a category from a message."""
    reasons = []

    def problem(code, attempt=None):
        entry = {"code": code}
        if attempt is not None:
            entry["attempt"] = attempt
        if entry not in reasons:
            reasons.append(entry)

    if not snapshot_unchanged:
        problem("inputs_changed")
    if [a.get("role") for a in attempts] != list(ORDER):
        problem("incomplete_pair")
    shared = []
    identities = {role: [] for role in set(ORDER)}
    observations = {role: [] for role in set(ORDER)}
    for index, attempt in enumerate(attempts):
        role = attempt.get("role")
        if role not in identities:
            problem("invalid_role", index)
            continue
        result = object_value(attempt.get("result"))
        manifest = object_value(attempt.get("manifest"))
        execution = object_value(attempt.get("execution"))
        environment = object_value(attempt.get("environment"))
        if attempt.get("artifact_errors") != []:
            problem("missing_artifact_evidence", index)
        if any(type(record.get("schema_version")) is not int or record["schema_version"] != 1
               for record in (result, manifest, execution)):
            problem("missing_or_unsupported_record", index)
        status = result.get("status")
        code = execution.get("exit_code")
        cleanup = object_value(execution.get("cleanup"))
        if (execution.get("status") != "completed" or execution.get("signal") is not None
                or type(code) is not int or code not in (0, 1)
                or attempt.get("exit_code") != code
                or not cleanup or any(object_value(v).get("status") not in ("ok", "absent")
                                      for v in cleanup.values())
                or status not in ("passed", "failed") or (status == "passed") != (code == 0)):
            problem("execution_incomplete_or_inconsistent", index)
        metadata = object_value(result.get("phase_metadata"))
        app = object_value(metadata.get("app"))
        app_ref, commit = app.get("ref"), app.get("commit")
        if (not matches(APP_REF, app_ref) or not matches(COMMIT, commit)
                or result.get("app_ref") != app_ref or manifest.get("app_ref") != app_ref):
            problem("missing_or_inconsistent_app_identity", index)
        else:
            identities[role].append((app_ref, commit))
            if role == "baseline" and (app_ref != baseline.get("ref") or commit != baseline.get("commit")):
                problem("published_identity_mismatch", index)
        runtime_set = []
        runtimes = metadata.get("runtimes")
        if not isinstance(runtimes, list) or not runtimes:
            problem("missing_runtime_provenance", index)
        else:
            for runtime in runtimes:
                runtime = object_value(runtime)
                ref = runtime.get("ref")
                if isinstance(ref, str):
                    ref = ref.removeprefix("runtime/")
                if (not isinstance(ref, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", ref)
                        or not matches(COMMIT, runtime.get("commit"))):
                    problem("missing_runtime_provenance", index)
                else:
                    runtime_set.append((ref, runtime["commit"]))
            if len({ref for ref, _ in runtime_set}) != len(runtime_set):
                problem("duplicate_runtime_ref", index)
        recipe = object_value(metadata.get("recipe"))
        digest = recipe.get("input_digest")
        if not matches(DIGEST, digest):
            problem("missing_or_unsupported_input_digest", index)
        desktop_keys = ("desktop", "desktop_version", "backend", "runner_version", "language", "scale")
        desktop = {key: manifest.get(key) for key in desktop_keys}
        if (any(not isinstance(desktop[key], str) or not desktop[key] for key in desktop_keys[:-1])
                or desktop["backend"] not in ("wayland", "xwayland")
                or type(desktop["scale"]) is not int or desktop["scale"] <= 0):
            problem("missing_desktop_identity", index)
        if (not matches(re.compile(r"sha256:[0-9a-f]{64}"), environment.get("image_id"))
                or any(not environment.get(key) for key in
                       ("architecture", "os", "host", "user", "network", "settings"))):
            problem("missing_environment_identity", index)
        shared.append({"runtimes": sorted(runtime_set), "input_digest": digest,
                       "content_hash": recipe.get("content_hash"),
                       "desktop": desktop, "environment": environment})
        failure = object_value(result.get("failure"))
        step = manifest.get("failed_step_index")
        action = manifest.get("failed_step_action")
        if status == "passed":
            if result.get("failure") is not None or step is not None or action is not None:
                problem("inconsistent_pass", index)
            observations[role].append(("passed",))
        elif (matches(DIGEST, digest) and failure.get("category") == "expectation" and type(step) is int and step >= 0
              and action in ("capture", "click_text", "type_text", "key", "wait_text", "select_window")
              and failure.get("reason") in ("screenshot_failed", "window_timeout")):
            # Digest + index identifies the exact recipe action and its target/arguments.
            observations[role].append(("expectation", digest, step, action, failure["reason"]))
        else:
            problem("unclassified_or_infrastructure_failure", index)
    if shared:
        for key in shared[0]:
            if any(item[key] != shared[0][key] for item in shared[1:]):
                problem("mismatched_" + key)
    for role in identities:
        if len(identities[role]) != 2 or len(set(identities[role])) != 1:
            problem(role + "_identity_not_repeatable")
        if len(observations[role]) != 2 or len(set(observations[role])) != 1:
            problem(role + "_observations_not_repeatable")
    if all(identities.values()):
        if identities["baseline"][0][0].split("/")[1:3] != identities["candidate"][0][0].split("/")[1:3]:
            problem("different_app_or_architecture")
    verdict = "inconclusive"
    if not reasons:
        before, after = observations["baseline"][0], observations["candidate"][0]
        if identities["baseline"][0][1] == identities["candidate"][0][1] and before != after:
            problem("same_app_commit_different_outcomes")
        elif before == after:
            verdict = "unchanged_pass" if before[0] == "passed" else "existing_failure"
        elif before[0] == "passed":
            verdict = "new_regression_signal"
        elif after[0] == "passed":
            verdict = "improvement"
        else:
            problem("different_expectation_failures")
    return {"verdict": verdict, "reasons": reasons}


def write_report(output, report):
    runner.write_record(output / "comparison.json", report)
    lines = ["# Paired screenshot run", "", f"Verdict: **{report['verdict']}**", "",
             "This is observed recipe evidence, not proof of app causality.",
             "Pixel differences and timings do not determine the verdict. Later steps after a failure are unobserved.", ""]
    lines += [f"- `{reason['code']}`" + (f" (attempt {reason['attempt'] + 1})" if "attempt" in reason else "")
              for reason in report["reasons"]]
    lines += ["", "## Attempts", ""]
    for attempt in report["attempts"]:
        path = attempt["path"]
        result = object_value(attempt.get("result"))
        lines += [f"- {path}: {result.get('status', 'no result')}; "
                  f"[result]({path}/result.json), [captures]({path}/screenshots.json), "
                  f"[logs]({path}/logs/), [execution]({path}.execution.json)",
                  "  Timings (ms): `" + json.dumps(result.get("timings_ms"), sort_keys=True) + "`"]
    (output / "comparison.md").write_text("\n".join(lines) + "\n")


def paired(args):
    source = args.inputs.resolve(strict=True)
    output = args.output.resolve()
    if not source.is_dir() or output.is_relative_to(source) or source.is_relative_to(output):
        raise ValueError("input and output directories must not overlap")
    if not matches(APP_REF, args.baseline_ref) or not matches(COMMIT, args.baseline_commit):
        raise ValueError("baseline provenance requires app/ID/ARCH/BRANCH and a full commit checksum")
    paths = {key: runner.workspace_path(source, getattr(args, key), key,
                                       "directory" if key == "dependency_repo" else "file")
             for key in ("baseline", "candidate", "recipe", "dependency_repo")}
    if not (source / paths["dependency_repo"] / "summary").is_file():
        raise ValueError("dependency repository must contain a summary")
    # Resolve once. Every container uses the same local content-addressed image ID.
    image = json.loads(subprocess.check_output(
        ["docker", "image", "inspect", args.image], text=True, timeout=30))[0]
    if not matches(re.compile(r"sha256:[0-9a-f]{64}"), image.get("Id")):
        raise ValueError("Docker did not return an immutable image identity")
    output.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": 1, "verdict": "inconclusive", "reasons": [{"code": "incomplete_pair"}],
              "published_baseline": {"source": args.baseline_source, "ref": args.baseline_ref,
                                     "commit": args.baseline_commit, "verification": "caller_supplied"},
              "requested_image": args.image, "attempts": []}
    write_report(output, report)
    try:
        snapshot = output / "inputs"
        # copy2 preserves modes/xattrs; never hard-link mutable caller-owned files.
        shutil.copytree(source, snapshot, symlinks=True)
        frozen = tree_hash(snapshot)
        report["input_snapshot_digest"] = frozen
        report["bundles"] = {role: {"path": paths[role], "sha256": file_hash(snapshot / paths[role])}
                             for role in ("baseline", "candidate")}
        settings = {"execution-timeout": str(args.execution_timeout),
                    "cleanup-timeout": str(args.cleanup_timeout)}
        environment = {"image_id": image["Id"], "architecture": image.get("Architecture"),
                       "os": image.get("Os"),
                       "user": args.user, "network": "none", "settings": settings}
        counts = {"baseline": 0, "candidate": 0}
        for role in ORDER:
            daemon = json.loads(subprocess.check_output(
                ["docker", "info", "--format", "{{json .}}"], text=True, timeout=30))
            host = {key: daemon.get(key) for key in ("ID", "KernelVersion", "OSType", "Architecture", "ServerVersion")}
            if not all(host.values()):
                raise ValueError("Docker daemon identity is incomplete")
            attempt_environment = {**environment, "host": host}
            counts[role] += 1
            path = f"{role}/{counts[role]}"
            inputs = {"mode": "screenshots", "bundle": "inputs/" + paths[role],
                      "recipe": "inputs/" + paths["recipe"], "output": path,
                      "dependency-repo": "inputs/" + paths["dependency_repo"],
                      "image": image["Id"], "user": args.user, **settings}
            code = runner.execute(inputs, output, isolated=True)
            result = read_record(output / path / "result.json")
            manifest = read_record(output / path / "screenshots.json")
            inventory, artifact_errors = artifacts(output / path, result, manifest)
            report["attempts"].append({"role": role, "path": path, "exit_code": code,
                                       "environment": attempt_environment,
                                       "result": result, "manifest": manifest,
                                       "artifacts": inventory, "artifact_errors": artifact_errors,
                                       "execution": read_record(output / (path + ".execution.json"))})
            write_report(output, report)
            if code == 130:
                return 130
        report["inputs_unchanged"] = tree_hash(snapshot) == frozen
        report.update(classify(report["attempts"], report["published_baseline"], report["inputs_unchanged"]))
        write_report(output, report)
    except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        report.update(verdict="inconclusive", reasons=[{"code": "orchestration_failed", "detail": str(error)}])
        write_report(output, report)
        raise
    print(f"{report['verdict']}: {output / 'comparison.md'}")
    return int(args.fail_on_regression and report["verdict"] == "new_regression_signal")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", required=True, type=Path, help="Dedicated input tree; copied into the output snapshot")
    for key in ("baseline", "candidate", "recipe", "dependency-repo"):
        parser.add_argument("--" + key, required=True, help="Path relative to --inputs")
    parser.add_argument("--baseline-source", required=True, help="Published origin URL/name (caller supplied)")
    parser.add_argument("--baseline-ref", required=True)
    parser.add_argument("--baseline-commit", required=True)
    parser.add_argument("--image", required=True, help="Already-pulled screenshot image")
    parser.add_argument("--output", required=True, type=Path, help="New output directory")
    parser.add_argument("--user", choices=("root", "nobody"), default="root")
    parser.add_argument("--execution-timeout", type=int, default=900)
    parser.add_argument("--cleanup-timeout", type=int, default=30)
    parser.add_argument("--fail-on-regression", action="store_true")
    args = parser.parse_args()
    for value in (args.execution_timeout, args.cleanup_timeout):
        if not 1 <= value <= 86400:
            parser.error("timeouts must be between 1 and 86400 seconds")
    try:
        return paired(args)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"paired run: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
