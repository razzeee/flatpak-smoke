"""Real X11-only/Wayland Flatpaks on Weston, inside the smoke e2e image.

No host display is used. Run after building both minimal fixture bundles.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time

from screenshot_failures import process_snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/usr/local/bin/flatpak-smoke")
    parser.add_argument("--wayland-bundle", default="target/org.example.FlatpakSmokeFixture.flatpak")
    parser.add_argument("--x11-bundle", default="target/org.example.FlatpakSmokeX11Fixture.flatpak")
    parser.add_argument("--output", type=Path, default=Path("target/xwayland"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)

    def run(name, x11=True, mode="normal", cancel=False, readiness_failure=False):
        output = args.output / name
        command = [args.binary, "verify-bundle", args.x11_bundle if x11 else args.wayland_bundle,
                   "--output", str(output), "--overall-timeout", "90s"]
        if not cancel:
            command += ["--screenshot-after-click", "Click Me"]
        env = dict(os.environ, DISPLAY=":987", WAYLAND_DISPLAY="caller-wayland",
                    XAUTHORITY="/nonexistent/caller-authority", FLATPAK_SMOKE_FIXTURE_MODE=mode)
        # These GTK fixtures use Cairo, including GSK_RENDERER=cairo supplied by
        # the verifier. They need no GL extensions. Debian's Flatpak 1.16.6 can
        # misreport transient openat2(RESOLVE_BENEATH) EAGAIN errors while merging
        # those extensions as "invalid merge-dirs" during concurrent launches.
        # Keep this test about real display/capture isolation, without retries
        # or changing the production launch environment.
        env["FLATPAK_GL_DRIVERS"] = "none"
        if readiness_failure:
            tools = args.output / f"{name}-tools"
            tools.mkdir()
            probe = tools / "xdpyinfo"
            probe.write_text("#!/bin/sh\necho 'injected X11 readiness failure' >&2\nexit 1\n")
            probe.chmod(0o755)
            env["PATH"] = f"{tools.resolve()}:{os.environ['PATH']}"
        observed = {}
        with (args.output / f"{name}.log").open("w") as log:
            child = subprocess.Popen(command, env=env, stdout=log, stderr=log)
            try:
                if cancel:
                    deadline = time.monotonic() + 85
                    while time.monotonic() < deadline:
                        runner = output / "logs/runner.log"
                        text = runner.read_text() if runner.exists() else ""
                        if "first visible sample" in text:
                            snapshot = process_snapshot()
                            owned = {child.pid}
                            while True:
                                descendants = {pid for pid, state in snapshot.items()
                                               if state["parent"] in owned}
                                if descendants <= owned:
                                    break
                                owned.update(descendants)
                            observed = {pid: state for pid, state in snapshot.items()
                                        if pid in owned}
                            xwayland_args = [Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
                                             for pid in observed
                                             if "Xwayland" in Path(f"/proc/{pid}/comm").read_text()]
                            assert xwayland_args, "no Xwayland process observed"
                            for argv in xwayland_args:
                                assert b"-rootless" in argv, argv
                                display_number = int(argv[1].removeprefix(b":"))
                                # Xwayland can disable TCP by default rather than
                                # receiving an explicit -nolisten argument.
                                for table in (Path("/proc/net/tcp"), Path("/proc/net/tcp6")):
                                    if table.exists():
                                        listeners = [line.split() for line in table.read_text().splitlines()[1:]]
                                        assert not any(row[3] == "0A" and
                                                       int(row[1].split(":")[1], 16) == 6000 + display_number
                                                       for row in listeners), "Xwayland exposed a TCP listener"
                            child.send_signal(signal.SIGTERM)
                            break
                        assert child.poll() is None, (args.output / f"{name}.log").read_text()
                        time.sleep(.02)
                    else:
                        raise AssertionError("app did not reach observation before cancellation")
                code = child.wait(timeout=10 if cancel else 100)
            finally:
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=10)
        result = json.loads((output / "result.json").read_text())
        failed = cancel or mode == "exit-after-click" or readiness_failure
        assert code == (1 if failed else 0), result
        assert result["status"] == ("failed" if failed else "passed"), result
        if mode == "exit-after-click":
            assert result["failure"]["reason"] == "early_exit", result
        if readiness_failure:
            assert result["failure"]["reason"] == "display_start_failed", result
        runner = (output / "logs/runner.log").read_text()
        assert f"selected app display backend: {'Xwayland' if x11 else 'Wayland'}" in runner
        assert runner.count("app launcher pid:") == (0 if readiness_failure else 1), "unexpected app launch count"
        workspace = Path(next(line.split(": ", 1)[1] for line in runner.splitlines()
                              if line.startswith("workspace: ")))
        if workspace.exists():
            # Capture evidence before the surrounding container is removed.
            # Bound the listing in case a whole installation remains behind.
            try:
                listing = subprocess.run(
                    ["find", str(workspace), "-maxdepth", "4", "-ls"],
                    capture_output=True, text=True, timeout=10, check=False,
                )
            except (subprocess.TimeoutExpired, OSError) as error:
                raise AssertionError(
                    f"private workspace leaked: {workspace}\n"
                    f"could not list remaining files: {error}"
                ) from error
            raise AssertionError(
                f"private workspace leaked: {workspace}\n"
                f"{listing.stdout[:16384]}\n{listing.stderr[:4096]}"
            )
        weston = (output / "logs/weston.stderr.log").read_text()
        assert ("--xwayland" in weston) == x11, weston
        if not failed:
            assert "screenshots/001-after-click-click-me.png" in result["screenshots"], result
        if x11:
            display = re.search(r"xserver listening on display :(\d+)", weston)[1]
            # Weston owns socket allocation and teardown. Never unlink other runs' files.
            assert not Path(f"/tmp/.X11-unix/X{display}").exists(), "X11 socket leaked"
            assert not Path(f"/tmp/.X{display}-lock").exists(), "X11 lock leaked"
        if observed:
            deadline = time.monotonic() + 3
            while True:
                current = process_snapshot()
                survivors = [pid for pid, state in observed.items()
                             if current.get(pid, {}).get("start") == state["start"]]
                if not survivors:
                    break
                assert time.monotonic() < deadline, f"owned processes survived cancellation: {survivors}"
                time.sleep(.02)
        print(f"PASS: {name}", flush=True)

    # Sequential checks prove both native Wayland and X11 captures/clicks work.
    run("wayland", x11=False)
    run("x11")
    # Keep two independent X11 apps/compositors in the same socket namespace.
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run, "concurrent-a"), pool.submit(run, "concurrent-b")]
        for future in futures:
            future.result()
    run("exit-after-click", mode="exit-after-click")
    run("cancel", cancel=True)
    run("readiness-failure", readiness_failure=True)


if __name__ == "__main__":
    main()
