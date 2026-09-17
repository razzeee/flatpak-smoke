"""Failure and isolation contracts for the screenshot CLI, inside its test image."""
import argparse
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
OVERVIEW = {"capture": {"name": "overview", "caption": "Browse the initial view"}}


def process_snapshot():
    processes = {}
    for path in Path("/proc").iterdir():
        if not path.name.isdecimal():
            continue
        try:
            fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
        except (FileNotFoundError, ProcessLookupError):
            continue
        if fields[0] not in ("Z", "X"):
            processes[int(path.name)] = {
                "parent": int(fields[1]), "group": int(fields[2]), "start": fields[19],
            }
    return processes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/workspace/target/debug/flatpak-smoke")
    parser.add_argument("--bundle", default="target/org.example.ScreenshotGtk.flatpak")
    parser.add_argument("--output", default="target/screenshot-failures")
    parser.add_argument("--case")
    parser.add_argument("--expect-detached", action="store_true",
                        help="Require cancellation to exercise processes outside the launcher group")
    args = parser.parse_args()
    root = ROOT / args.output
    root.mkdir(parents=True, exist_ok=True)

    def verify(name, recipe, reason, step_index, extra=(), fault=None, environment=None):
        if args.case and args.case != name:
            return
        print(f"Checking {name}", flush=True)
        recipe_path = root / f"{name}.json"
        recipe_path.write_text(json.dumps({"version": 1, **recipe}))
        output = root / name
        if output.exists():
            shutil.rmtree(output)
        with (root / f"{name}.log").open("w") as log:
            child = subprocess.Popen([args.binary, "screenshot-bundle", args.bundle,
                                      "--recipe", str(recipe_path), "--output", str(output),
                                      "--force", "--overall-timeout", "60s", *extra],
                                     cwd=ROOT, env=environment, stdout=log, stderr=log)
            try:
                if fault:
                    fault(child, output)
                code = child.wait(timeout=65)
            finally:
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=5)
        result = json.loads((output / "result.json").read_text())
        manifest = json.loads((output / "screenshots.json").read_text())
        assert code == (1 if reason else 0), result
        assert result["status"] == ("failed" if reason else "passed"), result
        assert (result["failure"]["reason"] if reason else result["failure"]) == reason, result
        assert manifest["failed_step_index"] == step_index, manifest
        if step_index is not None and step_index > 0:
            assert manifest["captures"][0]["name"] == "overview", manifest
            assert "screenshots/000-overview.png" in result["screenshots"], result
        for path in result["screenshots"]:
            assert (output / path).stat().st_size > 0, path
        return result, manifest

    verify("initial-title", {"window": {"title": "This window never exists"}, "steps": [OVERVIEW]},
           "window_timeout", None, extra=["--window-timeout", "1s"])

    verify("missing-text", {"steps": [OVERVIEW, {"wait_text": "This text never exists", "timeout": "2s"}]},
           "screenshot_failed", 1)
    verify("missing-click", {"steps": [OVERVIEW, {"click_text": "This button never exists"}]},
           "screenshot_failed", 1)
    verify("ambiguous-click", {"steps": [OVERVIEW, {"type_text": "duplicates"}, {"key": "Return"},
                                         {"wait_text": "Results for duplicates"}, {"click_text": "Duplicate"}]},
           "screenshot_failed", 4)
    verify("app-exit", {"steps": [OVERVIEW, {"key": "Ctrl+q"}]}, "early_exit", 1)
    verify("ambiguous-window", {"steps": [OVERVIEW, {"key": "Ctrl+comma"}, {"key": "Ctrl+comma"},
                                          {"select_window": {"title": "Preferences"}}]}, "screenshot_failed", 3)
    verify("closed-window", {"steps": [OVERVIEW, {"key": "Ctrl+comma"},
                                       {"select_window": {"title": "Preferences"}}, {"key": "Alt+F4"},
                                       {"capture": {"name": "closed", "caption": "Closed window"}}]},
           "screenshot_failed", 4)
    verify("delayed-results", {"steps": [{"type_text": "delay"}, {"key": "Return"},
                                         {"wait_text": "Results for delay"}, OVERVIEW]}, None, None)
    verify("unicode-input", {"steps": [{"type_text": "café example"}, {"key": "Return"},
                                       {"wait_text": "Results for café example"}, OVERVIEW]}, None, None)
    verify("unsupported-text", {"steps": [OVERVIEW, {"key": "Tab"}, {"type_text": "This is a button"}]},
           "screenshot_failed", 2)
    verify("replace-selection", {"steps": [{"type_text": "original"}, {"key": "Ctrl+a"},
                                           {"type_text": "replacement"}, {"key": "Return"},
                                           {"wait_text": "Results for replacement"}, OVERVIEW]}, None, None)
    verify("blank-client", {"steps": [OVERVIEW, {"type_text": "blank"}, {"key": "Return"},
                                      {"capture": {"name": "blank", "caption": "Blank view"}, "timeout": "3s"}]},
           "screenshot_failed", 3)

    cancelled_processes = {}

    def interrupt(kind):
        def fault(child, output):
            deadline = time.monotonic() + 40
            log = ""
            while time.monotonic() < deadline:
                path = output / "logs/runner.log"
                log = path.read_text() if path.exists() else ""
                if "recipe step 1" in log:
                    break
                assert child.poll() is None, log
                time.sleep(0.05)
            else:
                raise AssertionError("Recipe did not reach injection point")
            if kind == "cancel-instances":
                home = Path(next(line.split(": ", 1)[1] for line in log.splitlines() if line.startswith("capture workspace:")))
                launcher = int(next(line.split(": ", 1)[1] for line in log.splitlines() if line.startswith("app launcher pid:")))
                instances = subprocess.check_output(
                    ["/usr/bin/flatpak", "ps", "--columns=instance,pid,child-pid,application"],
                    env=dict(os.environ, HOME=str(home), XDG_RUNTIME_DIR=str(home / "runtime")),
                    text=True, timeout=5,
                ).splitlines()
                assert instances, "no Flatpak instance observed before cancellation"
                snapshot = process_snapshot()
                owned = set()
                for row in instances:
                    fields = row.split("\t")
                    assert len(fields) == 4, row
                    owned.update(int(pid) for pid in fields[1:3] if pid.isdecimal() and int(pid) > 0)
                # Include descendants while their parent relationships still exist.
                while True:
                    descendants = {pid for pid, state in snapshot.items() if state["parent"] in owned}
                    if descendants <= owned:
                        break
                    owned.update(descendants)
                cancelled_processes.update({pid: snapshot[pid] for pid in owned if pid in snapshot})
                assert cancelled_processes, "no live sandbox processes observed"
                if args.expect_detached:
                    assert any(state["group"] != launcher for state in cancelled_processes.values()), "no detached process observed"
                (root / "cancel-instances-processes.json").write_text(json.dumps({
                    "instances": instances, "launcher": launcher, "processes": cancelled_processes,
                }, indent=2) + "\n")
                child.send_signal(signal.SIGTERM)
            elif kind == "cancel":
                child.send_signal(signal.SIGTERM)
            elif kind == "desktop":
                pid = int(next(line.split(": ", 1)[1] for line in log.splitlines() if line.startswith("desktop launcher pid:")))
                os.killpg(pid, signal.SIGKILL)
            elif kind in ("helper", "helper-final"):
                if kind == "helper-final":
                    marker = root / "final-inspection"
                    while not marker.exists():
                        assert time.monotonic() < deadline, "Final inspection did not start"
                        assert child.poll() is None, log
                        time.sleep(0.01)
                home = Path(next(line.split(": ", 1)[1] for line in log.splitlines() if line.startswith("capture workspace:")))
                env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=(home / "session-bus").read_text())
                subprocess.run(["gnome-extensions", "disable", "capture@flatpak-smoke"], env=env, check=True, timeout=5)
        return fault

    waiting = {"steps": [OVERVIEW, {"wait_text": "This text never exists", "timeout": "30s"}]}
    verify("desktop-exit", waiting, "display_exited", 1, fault=interrupt("desktop"))
    verify("helper-exit", waiting, "screenshot_failed", 1, fault=interrupt("helper"))
    verify("cancel", waiting, "screenshot_failed", 1, fault=interrupt("cancel"))

    # Model a launcher that exits successfully while its Flatpak keeps running.
    # A slow liveness probe must be killed at the recipe deadline, not allowed
    # to finish using a fresh independent timeout.
    deadline_tools = root / "deadline-tools"
    deadline_tools.mkdir(exist_ok=True)
    probe_started = root / "probe-started"
    probe_finished = root / "probe-finished-late"
    for marker in (probe_started, probe_finished):
        marker.unlink(missing_ok=True)
    deadline_wrapper = deadline_tools / "flatpak"
    deadline_wrapper.write_text(
        "#!/usr/bin/python3\nimport os, sys, subprocess, time\nfrom pathlib import Path\n"
        f"root = Path({str(root)!r})\n"
        "if sys.argv[1:2] == ['run']:\n"
        "    child = subprocess.Popen(['/usr/bin/flatpak', *sys.argv[1:]], start_new_session=True)\n"
        "    while child.poll() is None:\n"
        "        apps = subprocess.check_output(['/usr/bin/flatpak', 'ps', '--columns=application'], text=True).splitlines()\n"
        "        if sys.argv[-1] in apps:\n"
        "            sys.exit(0)\n"
        "        time.sleep(.01)\n"
        "    sys.exit(child.returncode)\n"
        "if sys.argv[1:2] == ['ps']:\n"
        "    log = root / 'liveness-deadline/logs/runner.log'\n"
        "    if log.exists() and 'recipe step 1' in log.read_text():\n"
        "        (root / 'probe-started').touch()\n"
        "        time.sleep(.6)\n"
        "        (root / 'probe-finished-late').touch()\n"
        "os.execv('/usr/bin/flatpak', ['flatpak', *sys.argv[1:]])\n")
    deadline_wrapper.chmod(0o755)
    timed_out = verify("liveness-deadline", {
        "steps": [OVERVIEW, {"wait_text": "This text never exists", "timeout": "250ms"}]},
        "screenshot_failed", 1,
        environment=dict(os.environ, PATH=str(deadline_tools) + ":" + os.environ["PATH"]))
    if timed_out:
        assert probe_started.exists(), "the test did not exercise detached-app liveness"
        assert not probe_finished.exists(), "liveness probe exceeded the action deadline"

    # Cancellation must still invoke instance cleanup, including detached apps.
    cleanup_tools = root / "cleanup-tools"
    cleanup_tools.mkdir(exist_ok=True)
    cleanup_marker = root / "instance-cleanup"
    cleanup_marker.unlink(missing_ok=True)
    cleanup_wrapper = cleanup_tools / "flatpak"
    cleanup_wrapper.write_text(
        "#!/usr/bin/python3\nimport os, sys\nfrom pathlib import Path\n"
        "if sys.argv[1:2] == ['kill']:\n"
        f"    Path({str(cleanup_marker)!r}).write_text(os.environ['XDG_RUNTIME_DIR'])\n"
        "os.execv('/usr/bin/flatpak', ['flatpak', *sys.argv[1:]])\n")
    cleanup_wrapper.chmod(0o755)
    cancelled = verify("cancel-instances", waiting, "screenshot_failed", 1,
                       fault=interrupt("cancel-instances"),
                       environment=dict(os.environ, PATH=str(cleanup_tools) + ":" + os.environ["PATH"]))
    if cancelled:
        log = (root / "cancel-instances/logs/runner.log").read_text()
        home = next(line.split(": ", 1)[1] for line in log.splitlines() if line.startswith("capture workspace:"))
        assert cleanup_marker.read_text() == str(Path(home) / "runtime"), "cleanup escaped the private runtime directory"
        deadline = time.monotonic() + 5
        while True:
            snapshot = process_snapshot()
            survivors = [pid for pid, state in cancelled_processes.items()
                         if pid in snapshot and snapshot[pid]["start"] == state["start"]]
            if not survivors or time.monotonic() >= deadline:
                break
            time.sleep(.05)
        (root / "cancel-instances-survivors.json").write_text(json.dumps(survivors) + "\n")
        assert not survivors, f"sandbox processes survived cancellation: {survivors}"

    # Pause the third image inspection of the final capture, after native capture
    # returned. Losing the helper here must not become a successful final result.
    tools = root / "tools"
    tools.mkdir(exist_ok=True)
    for name in ["final-inspection", "inspection-count"]:
        (root / name).unlink(missing_ok=True)
    wrapper = tools / "identify"
    wrapper.write_text("#!/usr/bin/python3\nimport os, sys, time\nfrom pathlib import Path\n"
                       f"root = Path({str(root)!r})\n"
                       "log = root / 'helper-final-capture/logs/runner.log'\n"
                       "if log.exists() and 'recipe step 1' in log.read_text():\n"
                       "    counter = root / 'inspection-count'\n"
                       "    count = int(counter.read_text()) + 1 if counter.exists() else 1\n"
                       "    counter.write_text(str(count))\n"
                       "    if count == 3:\n"
                       "        (root / 'final-inspection').touch()\n"
                       "        time.sleep(1)\n"
                       "os.execv('/usr/bin/identify', ['identify', *sys.argv[1:]])\n")
    wrapper.chmod(0o755)
    verify("helper-final-capture", {"steps": [OVERVIEW, {"capture": {"name": "final", "caption": "Final view"}}]},
           "screenshot_failed", 1, fault=interrupt("helper-final"),
           environment=dict(os.environ, PATH=str(tools) + ":" + os.environ["PATH"]))

    caller_home = root / "caller-home"
    caller_config = caller_home / ".var/app/org.example.ScreenshotGtk/config"
    caller_config.mkdir(parents=True, exist_ok=True)
    sentinel = caller_config / "screenshot-fixture-marker"
    sentinel.write_text("caller data must survive")
    env = dict(os.environ, HOME=str(caller_home), GTK_THEME="Adwaita:dark", LANGUAGE="de")
    fresh = {"steps": [{"wait_text": "Ready to capture"}, OVERVIEW]}
    verify("fresh-profile-first", fresh, None, None, environment=env)
    verify("fresh-profile-second", fresh, None, None, environment=env)
    assert sentinel.read_text() == "caller data must survive"


if __name__ == "__main__":
    main()
