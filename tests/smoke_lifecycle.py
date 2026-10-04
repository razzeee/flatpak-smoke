"""Real Flatpak profile isolation, launcher handoff and cancellation in the test image."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import time

from screenshot_failures import process_snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/usr/local/bin/flatpak-smoke")
    parser.add_argument("--bundle", default="target/org.example.FlatpakSmokeFixture.flatpak")
    parser.add_argument("--output", type=Path, default=Path("target/smoke-lifecycle"))
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    caller = root / "caller-home"
    sentinel = caller / ".var/app/org.example.FlatpakSmokeFixture/config/smoke-fixture-marker"
    sentinel.parent.mkdir(parents=True)
    sentinel.write_text("caller data must survive")
    tools = root / "tools"
    tools.mkdir()
    wrapper = tools / "flatpak"
    wrapper.write_text(
        "#!/usr/bin/python3\nimport os, sys, subprocess, time\nfrom pathlib import Path\n"
        "if sys.argv[1:2] == ['run']:\n"
        "    child = subprocess.Popen(['/usr/bin/flatpak', *sys.argv[1:]], start_new_session=True)\n"
        "    while child.poll() is None:\n"
        "        apps = subprocess.check_output(['/usr/bin/flatpak', 'ps', '--columns=application'], text=True).splitlines()\n"
        "        if 'org.example.FlatpakSmokeFixture' in apps:\n"
        "            Path(os.environ['HANDOFF_MARKER']).write_text(str(child.pid))\n"
        "            sys.exit(0)\n"
        "        time.sleep(.01)\n"
        "    sys.exit(child.returncode)\n"
        "if sys.argv[1:2] == ['kill']:\n"
        "    Path(os.environ['CLEANUP_MARKER']).write_text(os.environ['XDG_RUNTIME_DIR'])\n"
        "os.execv('/usr/bin/flatpak', ['flatpak', *sys.argv[1:]])\n"
    )
    wrapper.chmod(0o755)

    def run(name, detached=False, cancel=False):
        output = root / name
        handoff = root / f"{name}-handoff"
        cleanup = root / f"{name}-cleanup"
        env = dict(os.environ, HOME=str(caller), FLATPAK_SMOKE_FIXTURE_CHECK_PROFILE="1")
        if detached:
            env.update(PATH=f"{tools}:{os.environ['PATH']}", HANDOFF_MARKER=str(handoff),
                       CLEANUP_MARKER=str(cleanup))
        command = [args.binary, "verify-bundle", args.bundle, "--output", str(output),
                   "--allow-network-remotes", "--overall-timeout", "5m"]
        observed = {}
        with (root / f"{name}.log").open("w") as log:
            child = subprocess.Popen(command, env=env, stdout=log, stderr=log)
            try:
                if cancel:
                    deadline = time.monotonic() + 290
                    while time.monotonic() < deadline:
                        runner = output / "logs/runner.log"
                        text = runner.read_text() if runner.exists() else ""
                        if "first visible sample" in text:
                            home = Path(next(line.split(": ", 1)[1] for line in text.splitlines()
                                             if line.startswith("workspace: ")))
                            instances = subprocess.check_output(
                                ["/usr/bin/flatpak", "ps", "--columns=pid,child-pid"],
                                env=dict(env, XDG_RUNTIME_DIR=str(home / "runtime")),
                                text=True, timeout=5,
                            )
                            snapshot = process_snapshot()
                            owned = {int(pid) for pid in instances.split() if pid.isdecimal() and int(pid) > 0}
                            while True:
                                descendants = {pid for pid, state in snapshot.items() if state["parent"] in owned}
                                if descendants <= owned:
                                    break
                                owned.update(descendants)
                            observed = {pid: snapshot[pid] for pid in owned if pid in snapshot}
                            assert observed, "no sandbox process observed before cancellation"
                            launcher = int(next(line.split(": ", 1)[1] for line in text.splitlines()
                                                if line.startswith("app launcher pid: ")))
                            assert any(state["group"] != launcher for state in observed.values()), "handoff did not detach"
                            child.send_signal(signal.SIGTERM)
                            break
                        assert child.poll() is None, (root / f"{name}.log").read_text()
                        time.sleep(.02)
                    else:
                        raise AssertionError("app never reached observation")
                code = child.wait(timeout=10 if cancel else 310)
            finally:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
        result = json.loads((output / "result.json").read_text())
        assert code == (1 if cancel else 0), result
        assert result["status"] == ("failed" if cancel else "passed"), result
        assert "fixture: fresh application profile" in (output / "logs/app.stdout.log").read_text()
        assert sentinel.read_text() == "caller data must survive"
        text = (output / "logs/runner.log").read_text()
        home = Path(next(line.split(": ", 1)[1] for line in text.splitlines() if line.startswith("workspace: ")))
        if detached:
            assert handoff.is_file(), "test did not exercise a successful launcher handoff"
            assert cleanup.read_text() == str(home / "runtime"), "cleanup escaped the private runtime directory"
        deadline = time.monotonic() + 5
        while True:
            remaining = []
            for pid in process_snapshot():
                try:
                    environment = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
                except (FileNotFoundError, ProcessLookupError):
                    continue
                if f"HOME={home}".encode() in environment:
                    remaining.append(pid)
            if not remaining or time.monotonic() >= deadline:
                break
            time.sleep(.05)
        (root / f"{name}-remaining.json").write_text(json.dumps(remaining))
        assert not remaining, f"processes from the private home survived cleanup: {remaining}"
        if cancel:
            deadline = time.monotonic() + 5
            while True:
                current = process_snapshot()
                survivors = [pid for pid, state in observed.items()
                             if pid in current and current[pid]["start"] == state["start"]]
                if not survivors or time.monotonic() >= deadline:
                    break
                time.sleep(.05)
            (root / "cancel-processes.json").write_text(json.dumps({"observed": observed, "survivors": survivors}, indent=2))
            assert not survivors, survivors
        print(f"{name}: passed", flush=True)

    run("fresh-first")
    run("fresh-second")
    run("detached", detached=True)
    run("cancel", detached=True, cancel=True)


if __name__ == "__main__":
    main()
