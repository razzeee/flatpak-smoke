"""Translate action inputs into a privileged Docker invocation, without a shell."""
import json
import os
from pathlib import Path
import platform
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import uuid

HERE = Path(__file__).resolve().parent


def workspace_path(workspace, value, name, kind=None):
    path = Path(value)
    if not value or path.is_absolute() or any(c in value for c in "\n\r\0"):
        raise ValueError(f"{name} must be a nonempty workspace-relative path")
    resolved = (workspace / path).resolve()
    if not resolved.is_relative_to(workspace) or resolved == workspace:
        raise ValueError(f"{name} must stay beneath the workspace")
    if kind == "file" and not resolved.is_file():
        raise ValueError(f"{name} file does not exist: {value}")
    if kind == "directory" and not resolved.is_dir():
        raise ValueError(f"{name} directory does not exist: {value}")
    return resolved.relative_to(workspace).as_posix()


def command(inputs, workspace):
    mode = inputs.get("mode") or "verify"
    if mode not in ("verify", "screenshots"):
        raise ValueError("mode must be verify or screenshots")
    bundle, repo, app_ref = (inputs.get(key, "") for key in ("bundle", "repo", "app-ref"))
    if bool(bundle) == bool(repo) or bool(repo) != bool(app_ref):
        raise ValueError("provide either bundle, or repo and app-ref")
    source = [workspace_path(workspace, bundle or repo, "bundle" if bundle else "repo",
                             "file" if bundle else "directory")]
    if repo:
        if not re.fullmatch(r"app/[A-Za-z0-9_][A-Za-z0-9_.-]*/[A-Za-z0-9_][A-Za-z0-9_.-]*/[A-Za-z0-9_][A-Za-z0-9_.-]*", app_ref):
            raise ValueError("app-ref must be app/ID/ARCH/BRANCH")
        source.append(app_ref)
    output = workspace_path(workspace, inputs.get("output") or "artifacts/flatpak-smoke", "output")
    subcommand = ("screenshot" if mode == "screenshots" else "verify") + ("-bundle" if bundle else "-repo")
    cli = ["flatpak-smoke", subcommand, *source, "--output", output]
    dependency_repo = inputs.get("dependency-repo", "")
    if dependency_repo:
        dependency_repo = workspace_path(workspace, dependency_repo, "dependency-repo", "directory")
        if Path(output).is_relative_to(dependency_repo) or Path(dependency_repo).is_relative_to(output):
            raise ValueError("output and dependency-repo must not overlap")
        cli += ["--dependency-repo", dependency_repo]
    recipe = inputs.get("recipe", "")
    if mode == "screenshots":
        cli += ["--recipe", workspace_path(workspace, recipe, "recipe", "file")]
    elif recipe:
        raise ValueError("recipe requires mode: screenshots")
    network = inputs.get("allow-network-remotes") or "false"
    if network not in ("true", "false"):
        raise ValueError("allow-network-remotes must be true or false")
    if network == "true":
        cli.append("--allow-network-remotes")
    extra = shlex.split(inputs.get("args", ""))
    for arg in extra:
        if "\0" in arg or arg.split("=", 1)[0] in ("--output", "--recipe", "--allow-network-remotes", "--dependency-repo"):
            raise ValueError("args must not contain NUL or override output, recipe, allow-network-remotes, or dependency-repo inputs")
    cli += extra
    user = inputs.get("user") or "root"
    if user not in ("root", "nobody"):
        raise ValueError("user must be root or nobody")
    image = inputs.get("image") or (
        "ghcr.io/razzeee/flatpak-smoke" + ("-screenshots" if mode == "screenshots" else "")
        + ":" + (HERE / "image-version").read_text().strip())
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/:@-]*", image):
        raise ValueError("image must be a container image reference")
    name = "flatpak-smoke-" + uuid.uuid4().hex
    docker = ["docker", "run", "--rm", "--init", "--name", name, "--privileged", "--tmpfs", "/run"]
    if mode == "screenshots":
        docker += ["--tmpfs", "/dev/dri"]
    docker += ["-v", f"{workspace}:/workspace", "-w", "/workspace"]
    if dependency_repo:
        docker += ["-v", f"{workspace / dependency_repo}:/workspace/{dependency_repo}:ro"]
    docker += [
               "-v", f"{HERE / 'container.sh'}:/flatpak-smoke-action.sh:ro",
               "--entrypoint", "sh", image, "/flatpak-smoke-action.sh",
               user, str(os.getuid()), str(os.getgid()), output, *cli]
    return docker, output, name


def seconds(inputs, key, default):
    value = inputs.get(key) or str(default)
    if not re.fullmatch(r"[0-9]+", value) or not 1 <= int(value) <= 86400:
        raise ValueError(f"{key} must be an integer number of seconds from 1 to 86400")
    return int(value)


def write_record(path, record):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(json.dumps(record, indent=2) + "\n")
            stream.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def cleanup(name, budget, interrupted):
    deadline = time.monotonic() + budget
    results = {}
    commands = []
    if interrupted:
        # Reserve half the shared budget for forced removal if graceful stop hangs.
        commands.append(("stop", ["docker", "stop", "--time", str(max(1, budget // 2 - 1)), name], budget / 2))
    commands.append(("remove", ["docker", "rm", "-f", name], budget))
    for stage, command, limit in commands:
        stage_deadline = min(deadline, time.monotonic() + limit)
        try:
            while True:
                remaining = stage_deadline - time.monotonic()
                if remaining <= 0:
                    results[stage] = {"status": "timeout"}
                    break
                result = subprocess.run(command, capture_output=True, text=True, check=False,
                                        start_new_session=True, timeout=remaining)
                # docker run --rm may still be removing the stopped container.
                # Retry until removal succeeds or Docker confirms it is absent.
                if (stage == "remove" and result.returncode != 0
                        and "removal of container" in result.stderr
                        and "is already in progress" in result.stderr):
                    time.sleep(max(0, min(.1, stage_deadline - time.monotonic())))
                    continue
                absent = "No such container" in result.stderr
                results[stage] = {"status": "absent" if absent else "ok" if result.returncode == 0 else "failed",
                                  "exit_code": result.returncode, "stderr": result.stderr[-4096:]}
                break
        except subprocess.TimeoutExpired:
            results[stage] = {"status": "timeout"}
        except OSError as error:
            results[stage] = {"status": "failed", "error": str(error)}
    return results


def main():
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "amd64"):
        raise ValueError("the action requires a Linux amd64 runner with Docker")
    inputs = json.loads(os.environ["FLATPAK_SMOKE_INPUTS"])
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
    docker, output, name = command(inputs, workspace)
    execution_timeout = seconds(inputs, "execution-timeout", 900)
    cleanup_timeout = seconds(inputs, "cleanup-timeout", 30)
    # Create parent directories as the host user so checkout cleanup can remove them.
    (workspace / output).mkdir(parents=True, exist_ok=True)
    # A sibling stays host-writable even while container.sh gives output to nobody.
    record_path = workspace / (output + ".execution.json")
    record = {"schema_version": 1, "container": name, "status": "running",
              "execution_timeout_seconds": execution_timeout,
              "cleanup_timeout_seconds": cleanup_timeout, "exit_code": None,
              "signal": None, "cleanup": None}
    write_record(record_path, record)
    with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
        stream.write(f"output={output}\nexecution={output}.execution.json\n")
    def cancel(signum, _frame):
        record["signal"] = signum
        raise KeyboardInterrupt
    previous = {sig: signal.signal(sig, cancel) for sig in (signal.SIGTERM, signal.SIGINT)}
    started = time.monotonic()
    try:
        try:
            code = subprocess.run(docker, check=False, timeout=execution_timeout,
                                  start_new_session=True).returncode
            record["status"] = "docker_start_failed" if code in (125, 126, 127) else "completed"
        except subprocess.TimeoutExpired:
            record["status"] = "timeout"
            code = 124
        except KeyboardInterrupt:
            record["status"] = "cancelled"
            code = 130
        except OSError as error:
            record.update(status="docker_start_failed", error=str(error))
            code = 1
        record["exit_code"] = code
    finally:
        # Repeated TERM/INT must not interrupt the bounded finally cleanup.
        for sig in previous:
            signal.signal(sig, signal.SIG_IGN)
        try:
            record["cleanup"] = cleanup(name, cleanup_timeout, record["status"] in ("timeout", "cancelled"))
            for stage, result in record["cleanup"].items():
                if result["status"] in ("failed", "timeout"):
                    print(f"flatpak-smoke action: container cleanup {stage}: {json.dumps(result)}",
                          file=sys.stderr)
            record["elapsed_seconds"] = round(time.monotonic() - started, 3)
            try:
                write_record(record_path, record)
            except OSError as error:
                print(f"flatpak-smoke action: execution record failed: {error}", file=sys.stderr)
                if code == 0:
                    code = 1
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)
    return code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError) as error:
        print(f"flatpak-smoke action: {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
