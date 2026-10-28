"""Paired evidence policy and orchestration, with Docker mocked at its boundary."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "action"))
spec = importlib.util.spec_from_file_location("paired_compare", ROOT / "action/compare.py")
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)

BASELINE = {"ref": "app/org.example.App/x86_64/stable", "commit": "a" * 64}


def attempt(role, failed=False):
    app = {"ref": BASELINE["ref"], "commit": ("a" if role == "baseline" else "b") * 64}
    return {
        "role": role, "exit_code": int(failed), "artifact_errors": [],
        "environment": {"image_id": "sha256:" + "c" * 64, "architecture": "amd64", "os": "linux",
                        "host": {"ID": "test"}, "user": "root", "network": "none", "settings": {"timeout": 900}},
        "execution": {"schema_version": 1, "status": "completed", "exit_code": int(failed),
                      "signal": None, "cleanup": {"remove": {"status": "ok"}}},
        "result": {"schema_version": 1, "status": "failed" if failed else "passed", "app_ref": app["ref"],
                   "phase_metadata": {"app": app, "runtimes": [
                       {"ref": "org.example.Platform/x86_64/1", "commit": "d" * 64},
                       {"ref": "runtime/org.example.Extension/x86_64/1", "commit": "e" * 64}],
                       "recipe": {"content_hash": "md5:legacy", "input_digest": "v1:sha256:" + "f" * 64}},
                   "failure": {"category": "expectation", "reason": "screenshot_failed", "message": "missing text"}
                   if failed else None, "timings_ms": {"total": 123}, "screenshots": ["screenshots/main.png"]},
        "manifest": {"schema_version": 1, "app_ref": app["ref"], "desktop": "gnome", "desktop_version": "48.2",
                     "backend": "wayland", "runner_version": "0.5.1", "language": "en", "scale": 1,
                     "failed_step_index": 1 if failed else None,
                     "failed_step_action": "click_text" if failed else None,
                     "captures": [{"name": "main", "path": "screenshots/main.png"}]},
    }


def attempts(before=False, after=False):
    return [attempt(role, before if role == "baseline" else after) for role in compare.ORDER]


class ComparisonTests(unittest.TestCase):
    def classify(self, values, **kwargs):
        return compare.classify(values, BASELINE, **kwargs)

    def test_outcome_matrix(self):
        for before, after, expected in [(False, False, "unchanged_pass"), (False, True, "new_regression_signal"),
                                        (True, False, "improvement"), (True, True, "existing_failure")]:
            with self.subTest(expected=expected):
                self.assertEqual(self.classify(attempts(before, after)), {"verdict": expected, "reasons": []})

    def test_unknown_provenance_is_not_equality(self):
        for key in ("app", "runtimes", "recipe"):
            values = attempts()
            for value in values:
                value["result"]["phase_metadata"][key] = None
            with self.subTest(key=key):
                self.assertEqual(self.classify(values)["verdict"], "inconclusive")
        for field in ("input_digest", "content_hash"):
            values = attempts()
            for value in values:
                del value["result"]["phase_metadata"]["recipe"][field]
            # content_hash is legacy diagnostic metadata, not required input identity.
            expected = "inconclusive" if field == "input_digest" else "unchanged_pass"
            self.assertEqual(self.classify(values)["verdict"], expected)

    def test_runtime_order_and_prefix_are_not_differences(self):
        values = attempts()
        values[1]["result"]["phase_metadata"]["runtimes"].reverse()
        values[2]["result"]["phase_metadata"]["runtimes"][0]["ref"] = "runtime/org.example.Platform/x86_64/1"
        self.assertEqual(self.classify(values)["verdict"], "unchanged_pass")

    def test_runtime_fixture_environment_and_desktop_changes(self):
        for change in ("runtime", "fixture", "image", "desktop", "backend", "app", "commit"):
            values = attempts(False, True)
            value = values[1]
            if change == "runtime":
                value["result"]["phase_metadata"]["runtimes"][0]["commit"] = "0" * 64
            elif change == "fixture":
                value["result"]["phase_metadata"]["recipe"]["input_digest"] = "v1:sha256:" + "0" * 64
            elif change == "image":
                value["environment"]["image_id"] = "sha256:" + "0" * 64
            elif change in ("desktop", "backend"):
                value["manifest"][change] = "other"
            elif change == "app":
                value["result"]["phase_metadata"]["app"]["ref"] = "app/org.other.App/x86_64/stable"
            else:
                value["result"]["phase_metadata"]["app"]["commit"] = None
            with self.subTest(change=change):
                self.assertEqual(self.classify(values)["verdict"], "inconclusive")
        self.assertEqual(self.classify(attempts(), snapshot_unchanged=False)["verdict"], "inconclusive")

    def test_flaky_observations_different_failures_and_uncategorized_errors(self):
        values = attempts(False, True)
        values[2] = attempt("candidate")
        self.assertEqual(self.classify(values)["verdict"], "inconclusive")
        values = attempts(True, True)
        for index in (1, 2):
            values[index]["manifest"]["failed_step_index"] = 2
        self.assertEqual(self.classify(values)["reasons"], [{"code": "different_expectation_failures"}])
        for category in (None, "infrastructure"):
            values = attempts(False, True)
            for index in (1, 2):
                values[index]["result"]["failure"]["category"] = category
            self.assertEqual(self.classify(values)["verdict"], "inconclusive")

    def test_missing_record_timeout_cancellation_cleanup_and_exit_mismatch(self):
        for change in ("missing", "timeout", "cancel", "cleanup", "exit", "schema"):
            values = attempts()
            value = values[1]
            if change == "missing":
                value["result"] = None
            elif change == "timeout":
                value["execution"]["status"] = "timeout"
            elif change == "cancel":
                value["execution"]["signal"] = 15
            elif change == "cleanup":
                value["execution"]["cleanup"]["remove"]["status"] = "failed"
            elif change == "schema":
                value["result"]["schema_version"] = 2
            else:
                value["execution"]["exit_code"] = 1
            with self.subTest(change=change):
                self.assertEqual(self.classify(values)["verdict"], "inconclusive")

    def test_pixels_timings_and_messages_do_not_determine_verdict(self):
        values = attempts(True, True)
        values[1]["result"]["failure"]["message"] = "different incidental details"
        values[1]["result"]["timings_ms"]["total"] = 999999
        values[1]["manifest"]["captures"] = [{"name": "different", "width": 300}]
        self.assertEqual(self.classify(values)["verdict"], "existing_failure")

    def test_legacy_manifest_and_malformed_digest_are_inconclusive(self):
        values = attempts(False, True)
        values[1]["result"]["phase_metadata"]["recipe"]["input_digest"] = []
        values[2]["manifest"].pop("backend")
        self.assertEqual(self.classify(values)["verdict"], "inconclusive")

    def test_identical_commit_cannot_be_a_new_app_regression(self):
        values = attempts(False, True)
        for index in (1, 2):
            values[index]["result"]["phase_metadata"]["app"]["commit"] = BASELINE["commit"]
        self.assertEqual(self.classify(values)["reasons"], [{"code": "same_app_commit_different_outcomes"}])


class OrchestrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        source = self.root / "source"
        source.mkdir()
        for name in ("baseline.flatpak", "candidate.flatpak", "recipe.yml"):
            (source / name).write_text(name)
        (source / "fixtures").mkdir()
        (source / "fixtures/document").write_text("initial fixture")
        (source / "dependencies").mkdir()
        (source / "dependencies/summary").touch()
        self.args = argparse.Namespace(inputs=source, output=self.root / "output", baseline="baseline.flatpak",
                                       candidate="candidate.flatpak", recipe="recipe.yml", dependency_repo="dependencies",
                                       baseline_ref=BASELINE["ref"], baseline_commit=BASELINE["commit"],
                                       baseline_source="https://example.org/published", image="test:latest", user="root",
                                       execution_timeout=900, cleanup_timeout=30, fail_on_regression=False)

    def docker_inspect(self, command, **kwargs):
        if command[1] == "image":
            return json.dumps([{"Id": "sha256:" + "c" * 64, "Architecture": "amd64", "Os": "linux"}])
        return json.dumps({"ID": "daemon", "KernelVersion": "test", "OSType": "linux",
                           "Architecture": "x86_64", "ServerVersion": "test"})

    def run_pair(self, fail_candidate=False, cancel=False, mutate=False):
        calls = []

        def execute(inputs, workspace, **kwargs):
            self.assertTrue(kwargs["isolated"])
            self.assertEqual(inputs["image"], "sha256:" + "c" * 64)
            role = inputs["output"].split("/")[0]
            calls.append(role)
            output = workspace / inputs["output"]
            output.mkdir(parents=True)
            record = attempt(role, role == "candidate" and fail_candidate)
            (output / "screenshots").mkdir()
            (output / "screenshots/main.png").write_bytes(b"fake screenshot at the Docker boundary")
            (output / "result.json").write_text(json.dumps(record["result"]))
            (output / "screenshots.json").write_text(json.dumps(record["manifest"]))
            execution = record["execution"]
            if cancel:
                execution.update(status="cancelled", signal=15, exit_code=130)
            (workspace / (inputs["output"] + ".execution.json")).write_text(json.dumps(execution))
            if mutate:
                (workspace / "inputs/fixtures/document").write_text("modified")
            # Original inputs cannot affect the private snapshot after preparation.
            (self.args.inputs / "fixtures/document").write_text("caller changed")
            return execution["exit_code"]

        with mock.patch.object(compare.subprocess, "check_output", side_effect=self.docker_inspect), \
                mock.patch.object(compare.runner, "execute", side_effect=execute):
            code = compare.paired(self.args)
        return code, calls, json.loads((self.args.output / "comparison.json").read_text())

    def test_pair_order_snapshot_and_report_only_regression(self):
        code, calls, report = self.run_pair(fail_candidate=True)
        self.assertEqual(code, 0)
        self.assertEqual(calls, list(compare.ORDER))
        self.assertEqual(report["verdict"], "new_regression_signal")
        self.assertEqual((self.args.output / "inputs/fixtures/document").read_text(), "initial fixture")
        self.assertTrue((self.args.output / "comparison.md").is_file())

    def test_pass_pass_and_opt_in_gate(self):
        self.assertEqual(self.run_pair()[2]["verdict"], "unchanged_pass")
        self.args.output = self.root / "second"
        self.args.fail_on_regression = True
        self.assertEqual(self.run_pair(fail_candidate=True)[0], 1)

    def test_cancellation_stops_pair_and_preserves_report(self):
        code, calls, report = self.run_pair(cancel=True)
        self.assertEqual(code, 130)
        self.assertEqual(calls, ["baseline"])
        self.assertEqual(report["verdict"], "inconclusive")
        self.assertEqual(report["attempts"][0]["execution"]["status"], "cancelled")

    def test_snapshot_change_is_inconclusive(self):
        self.assertEqual(self.run_pair(mutate=True)[2]["verdict"], "inconclusive")

    def test_isolated_command_has_readonly_inputs_writable_output_and_no_network(self):
        inputs = {"mode": "screenshots", "bundle": "baseline.flatpak", "recipe": "recipe.yml",
                  "dependency-repo": "dependencies", "output": "results/1", "image": "test"}
        command, _, _ = compare.runner.command(inputs, self.args.inputs, isolated=True)
        self.assertIn(f"{self.args.inputs}:/workspace:ro", command)
        self.assertIn(f"{self.args.inputs}/results/1:/workspace/results/1:rw", command)
        self.assertEqual(command[command.index("--network") + 1], "none")
        self.assertNotIn("--allow-network-remotes", command)

    def test_missing_and_escaping_artifacts_are_not_accepted(self):
        record = attempt("baseline")
        inventory, errors = compare.artifacts(self.root, record["result"], record["manifest"])
        self.assertFalse(inventory)
        self.assertTrue(errors)
        record["result"]["screenshots"] = ["../outside.png", None]
        self.assertTrue(compare.artifacts(self.root, record["result"], record["manifest"])[1])


if __name__ == "__main__":
    unittest.main()
