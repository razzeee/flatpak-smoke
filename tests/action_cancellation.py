"""Host-side cancellation of the real action and Flatpak workload (CI fixture image)."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def run(user):
    output = ROOT / f"target/action-cancellation-{user}"
    record_path = output.with_suffix(".execution.json")
    env = dict(os.environ, GITHUB_WORKSPACE=str(ROOT),
               GITHUB_OUTPUT=str(output.with_suffix(".outputs")),
               FLATPAK_SMOKE_INPUTS=json.dumps({
                   "bundle": "target/org.example.FlatpakSmokeFixture.flatpak",
                   "image": "flatpak-smoke", "user": user,
                   "output": str(output.relative_to(ROOT)),
                   "allow-network-remotes": "true", "execution-timeout": "420",
                   "cleanup-timeout": "30",
                   "args": "--overall-timeout 5m"}))
    with output.with_suffix(".log").open("w") as log:
        child = subprocess.Popen([sys.executable, str(ROOT / "action/run.py")], env=env,
                                 stdout=log, stderr=log, start_new_session=True)
        try:
            deadline = time.monotonic() + 390
            while time.monotonic() < deadline:
                runner = output / "logs/runner.log"
                if runner.exists() and "first visible sample" in runner.read_text():
                    break
                assert child.poll() is None, output.with_suffix(".log").read_text()
                time.sleep(.1)
            else:
                raise AssertionError("real Flatpak never reached observation")
            initial = json.loads(record_path.read_text())
            assert initial["status"] == "running", initial
            assert subprocess.check_output(["docker", "inspect", "--format", "{{.State.Running}}",
                                            initial["container"]], text=True).strip() == "true"
            # Match consumers that signal the entire process group.
            os.killpg(child.pid, signal.SIGTERM)
            assert child.wait(timeout=35) == 130
            record = json.loads(record_path.read_text())
            assert record["status"] == "cancelled" and record["signal"] == signal.SIGTERM, record
            assert record["cleanup"]["stop"]["status"] in ("ok", "absent"), record
            assert record["cleanup"]["remove"]["status"] in ("ok", "absent"), record
            containers = subprocess.check_output(["docker", "ps", "-a", "--format", "{{.Names}}"], text=True)
            assert record["container"] not in containers.splitlines(), containers
            result = json.loads((output / "result.json").read_text())
            assert result["status"] == "failed", result
            assert (output / "logs/runner.log").stat().st_size > 0
            # Cancellation occurs during observation, before screenshot publication.
            assert (output / "screenshots").is_dir()
            for path in [output, record_path, *output.rglob("*")]:
                assert (path.lstat().st_uid, path.lstat().st_gid) == (os.getuid(), os.getgid()), path
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=35)
            if record_path.exists():
                name = json.loads(record_path.read_text())["container"]
                subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=10)


if __name__ == "__main__":
    for user in ("root", "nobody"):
        run(user)
