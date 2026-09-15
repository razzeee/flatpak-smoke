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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/workspace/target/debug/flatpak-smoke")
    parser.add_argument("--bundle", default="target/org.example.ScreenshotGtk.flatpak")
    parser.add_argument("--output", default="target/screenshot-failures")
    parser.add_argument("--case")
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
            if kind == "cancel":
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
