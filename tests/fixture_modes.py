"""Exercise readiness and exit handling with the real Wayland fixture in Docker."""

import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="flatpak-smoke")
    parser.add_argument("--binary", default="/usr/local/bin/flatpak-smoke")
    parser.add_argument("--bundle", default="target/org.example.FlatpakSmokeFixture.flatpak")
    parser.add_argument("--output", default="target/fixture-modes")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    cases = [
        ("delayed", None, False),
        ("never-draw", "window_timeout", False),
        ("crash-after-frame", "early_exit", False),
        ("animated", None, True),
        ("exit-after-click", "early_exit", True),
    ]
    failures = []
    for mode, reason, click in cases:
        output = Path(args.output) / mode
        command = [
            "docker", "run", "--rm", "--privileged",
            "-v", f"{root}:/workspace", "-w", "/workspace",
            "-e", f"FLATPAK_SMOKE_FIXTURE_MODE={mode}",
            "--entrypoint", args.binary, args.image,
            "verify-bundle", args.bundle,
            "--output", str(output), "--force", "--allow-network-remotes",
            "--window-timeout", "3s" if mode == "never-draw" else "8s",
            "--overall-timeout", "5m",
        ]
        if click:
            command.extend(["--screenshot-after-click", "Click Me"])
        print(f"Checking fixture mode {mode}", flush=True)
        completed = subprocess.run(command, cwd=root, timeout=330)
        try:
            result = json.loads((root / output / "result.json").read_text())
            assert completed.returncode == (1 if reason else 0), result
            assert result["status"] == ("failed" if reason else "passed"), result
            assert (result["failure"]["reason"] if result["failure"] else None) == reason, result
            assert result["screenshots"], result
            for screenshot in result["screenshots"]:
                assert (root / output / screenshot).stat().st_size > 0, screenshot
            stdout = (root / output / "logs/app.stdout.log").read_text()
            assert f"fixture mode: {mode}" in stdout, stdout
            weston_log = (root / output / "logs/weston.stderr.log").read_text()
            assert weston_log.count("New VNC client connected") == 1, (
                "readiness and capture must share one VNC connection", weston_log)
            if reason:
                assert "logs/last-candidate.png" in result["screenshots"], result
            else:
                assert "screenshots/000-window-visible.png" in result["screenshots"], result
            if mode == "crash-after-frame":
                assert "fixture: first frame painted" in stdout, stdout
            if mode == "exit-after-click":
                assert "fixture: exiting after click" in stdout, stdout
                assert "screenshots/000-window-visible.png" in result["screenshots"], result
            if mode == "animated":
                assert "screenshots/001-after-click-click-me.png" in result["screenshots"], result
        except (AssertionError, OSError, ValueError) as error:
            failures.append(f"{mode}: {error}")
    if failures:
        raise SystemExit("\n".join(failures))
    print("All fixture modes passed")


if __name__ == "__main__":
    main()
