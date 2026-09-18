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
        if "\0" in arg or arg.split("=", 1)[0] in ("--output", "--recipe", "--allow-network-remotes"):
            raise ValueError("args must not contain NUL or override output, recipe, or allow-network-remotes inputs")
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
    docker += ["-v", f"{workspace}:/workspace", "-w", "/workspace",
               "-v", f"{HERE / 'container.sh'}:/flatpak-smoke-action.sh:ro",
               "--entrypoint", "sh", image, "/flatpak-smoke-action.sh",
               user, str(os.getuid()), str(os.getgid()), output, *cli]
    return docker, output, name


def main():
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "amd64"):
        raise ValueError("the action requires a Linux amd64 runner with Docker")
    inputs = json.loads(os.environ["FLATPAK_SMOKE_INPUTS"])
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
    docker, output, name = command(inputs, workspace)
    # Create parent directories as the host user so checkout cleanup can remove them.
    (workspace / output).mkdir(parents=True, exist_ok=True)
    with open(os.environ["GITHUB_OUTPUT"], "a") as stream:
        stream.write(f"output={output}\n")
    def cancel(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, cancel)
    try:
        return subprocess.run(docker, check=False).returncode
    finally:
        try:
            subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, check=False, timeout=30)
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"flatpak-smoke action: container cleanup failed: {error}", file=sys.stderr)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError) as error:
        print(f"flatpak-smoke action: {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
