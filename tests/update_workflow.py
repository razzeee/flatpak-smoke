"""Real forward updates, migration failures, graceful shutdown and caller isolation.

Run inside the screenshot image with --dependency-repo pointing at a frozen
org.gnome.Platform runtime. This Cairo fixture does not need GL extensions.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
APP = "org.example.UpdateFixture"


def checked(*args):
    subprocess.run(args, check=True, timeout=120)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="flatpak-smoke")
    parser.add_argument("--dependency-repo", required=True)
    args = parser.parse_args()
    root = ROOT / "target/update-workflow"
    root.mkdir(parents=True, exist_ok=True)
    build = root / "build"
    (build / "files/bin").mkdir(parents=True, exist_ok=True)
    (build / "export").mkdir(exist_ok=True)
    app = build / "files/bin/update-fixture"
    app.write_bytes((ROOT / "fixtures/update/app.py").read_bytes())
    app.chmod(0o755)
    # Broad grants must be reset by the runner, including an absolute caller path.
    caller = root / "caller"
    sentinel = caller / ".var/app" / APP / "data/sentinel"
    sentinel.parent.mkdir(parents=True, exist_ok=True)
    sentinel.write_text("caller data")
    (build / "metadata").write_text(
        f"[Application]\nname={APP}\nruntime=org.gnome.Platform/x86_64/49\n"
        f"sdk=org.gnome.Sdk/x86_64/49\ncommand=update-fixture\n"
        f"[Context]\nsockets=wayland;\nfilesystems=host;home;{caller};\n")
    for version in ["old", "new", "broken", "missing-runtime"]:
        if version == "missing-runtime":
            metadata = build / "metadata"
            metadata.write_text(metadata.read_text().replace("org.gnome.Platform/x86_64/49", "org.example.MissingPlatform/x86_64/1"))
        (build / "files/version").write_text(version)
        repo = root / (version + "-repo")
        checked("flatpak", "build-export", str(repo), str(build))
        bundle = root / (version + ".flatpak")
        bundle.unlink(missing_ok=True)
        checked("flatpak", "build-bundle", str(repo), str(bundle), APP)

    prepare = {"version": 1, "launch": {"args": ["--probe-path", str(sentinel)]},
               "quit": {"key": "Ctrl+q", "timeout": "5s"}, "steps": [
                   {"type_text": "Persistent document"}, {"key": "Ctrl+s"},
                   {"wait_text": "Saved changes"},
                   {"capture": {"name": "saved", "caption": "Saved old data"}}]}
    verify = {"version": 1, "launch": prepare["launch"], "steps": [
        {"wait_text": "Persistent document"}, {"wait_text": "Theme ocean"},
        {"wait_text": "Record Persistent document", "timeout": "3s"},
        {"wait_text": "Version new"}, {"key": "Ctrl+a"},
        {"type_text": "Edited after update"}, {"key": "Ctrl+s"},
        {"wait_text": "Saved changes"},
        {"capture": {"name": "usable", "caption": "Updated data remains editable"}}]}
    prepare_path, verify_path = root / "prepare.json", root / "verify.json"
    prepare_path.write_text(json.dumps(prepare))
    verify_path.write_text(json.dumps(verify))
    env = dict(os.environ, HOME=str(caller), XDG_DATA_HOME=str(caller / "data"),
               XDG_CONFIG_HOME=str(caller / "config"), XDG_CACHE_HOME=str(caller / "cache"))

    def command(name, candidate="new"):
        return [args.binary, "verify-update-bundle", str(root / (candidate + ".flatpak")),
                "--from", str(root / "old.flatpak"), "--prepare-recipe", str(prepare_path),
                "--verify-recipe", str(verify_path), "--dependency-repo", args.dependency_repo,
                "--output", str(root / name), "--force", "--overall-timeout", "5m"]

    def evidence(name, success, phase=None):
        output = root / name
        result = json.loads((output / "result.json").read_text())
        assert result["status"] == ("passed" if success else "failed"), result
        assert result["update"]["failed_phase"] == phase, result
        assert sentinel.read_text() == "caller data"
        log = (output / "logs/runner.log").read_text()
        workspace = re.search(r"update workspace: (.+)", log)
        if workspace:
            assert not Path(workspace[1]).exists(), "workspace survived cleanup"
            markers = {f"HOME={workspace[1]}".encode(),
                       f"XDG_RUNTIME_DIR={workspace[1]}/runtime".encode()}
            for process in Path("/proc").glob("[0-9]*"):
                try:
                    environment = set((process / "environ").read_bytes().split(b"\0"))
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    continue
                assert not markers.intersection(environment), f"owned process survived: {process.name}"
        return result

    for name, candidate, success, phase in [
        ("success", "new", True, None),
        ("broken", "broken", False, "verify"),
        ("same-commit", "old", False, "validate"),
        ("install-failure", "missing-runtime", False, "update"),
    ]:
        process = subprocess.run(command(name, candidate), env=env, timeout=330)
        assert (process.returncode == 0) == success
        result = evidence(name, success, phase)
        if name == "broken":
            assert "Record Persistent document" in result["failure"]["message"], result
        if success:
            assert result["update"]["before"]["app"]["commit"] != result["update"]["after"]["app"]["commit"]
            assert len(result["screenshots"]) == 2, result

    prepare["launch"]["args"].append("--refuse-quit")
    prepare_path.write_text(json.dumps(prepare))
    process = subprocess.run(command("shutdown-timeout"), env=env, timeout=330)
    assert process.returncode != 0
    result = evidence("shutdown-timeout", False, "stop_old")
    assert result["update"]["after"]["app"] is None

    # Cancel while an old app is running, before any candidate installation.
    prepare["steps"].insert(0, {"wait_text": "Never appears", "timeout": "120s"})
    prepare_path.write_text(json.dumps(prepare))
    # Never accept a previous run's readiness marker or failure result. In
    # particular, --force only clears these after the new process starts.
    cancelled_output = root / "cancelled"
    if cancelled_output.exists():
        shutil.rmtree(cancelled_output)
    process = subprocess.Popen(command("cancelled"), env=env)
    try:
        log = root / "cancelled/phases/prepare/logs/runner.log"
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            if log.exists() and "recipe step 0" in log.read_text():
                break
            assert process.poll() is None, "runner exited before cancellation"
            time.sleep(0.1)
        else:
            raise AssertionError("preparation did not start")
        process.send_signal(signal.SIGTERM)
        assert process.wait(timeout=20) != 0
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    result = evidence("cancelled", False, "prepare")
    assert "run cancelled" in result["failure"]["message"], result
    assert result["update"]["before"]["app"] is not None, result
    assert result["update"]["after"]["app"] is None, result
    print("Forward update lifecycle and isolation passed", flush=True)


if __name__ == "__main__":
    main()
