"""Non-root GNOME/Wayland capture with authenticated Xwayland compatibility.

Run inside the screenshot e2e image, with both minimal fixture bundles built.
The image must not pre-create /tmp/.X11-unix: this exercises runner initialization.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from screenshot_failures import process_snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/usr/local/bin/flatpak-smoke")
    parser.add_argument("--wayland-bundle", default="target/org.example.FlatpakSmokeFixture.flatpak")
    parser.add_argument("--x11-bundle", default="target/org.example.FlatpakSmokeX11Fixture.flatpak")
    parser.add_argument("--output", type=Path, default=Path("target/xwayland-capture"))
    args = parser.parse_args()
    assert os.geteuid() != 0, "exercise the hosted non-root capture path"
    assert not Path("/tmp/.X11-unix").exists(), "run in a fresh container to test missing socket-directory setup"
    args.output.mkdir(parents=True, exist_ok=False)
    recipe = args.output / "recipe.json"
    recipe.write_text(json.dumps({"version": 1, "window": {"width": 600, "height": 400},
                                  "steps": [{"capture": {"name": "before", "caption": "Before clicking"}},
                                            {"key": "space"},
                                            {"capture": {"name": "after", "caption": "After clicking"}}]}))

    def run(name, x11):
        output = args.output / name
        env = dict(os.environ, DISPLAY=":999", XAUTHORITY="/nonexistent/caller-authority")
        with (args.output / f"{name}.log").open("w") as log:
            completed = subprocess.run([args.binary, "screenshot-bundle",
                                        args.x11_bundle if x11 else args.wayland_bundle,
                                        "--recipe", str(recipe), "--output", str(output),
                                        "--overall-timeout", "60s"], env=env, umask=0o022,
                                       stdout=log, stderr=log, timeout=70)
        result = json.loads((output / "result.json").read_text())
        assert completed.returncode == 0 and result["status"] == "passed", result
        manifest = json.loads((output / "screenshots.json").read_text())
        assert [capture["name"] for capture in manifest["captures"]] == ["before", "after"], manifest
        for capture in manifest["captures"]:
            assert (output / capture["path"]).stat().st_size > 0, capture
        text = subprocess.check_output(["tesseract", str(output / manifest["captures"][1]["path"]),
                                        "stdout", "--psm", "6"], text=True, stderr=subprocess.DEVNULL)
        assert "clicked" in text.lower(), text
        runner = (output / "logs/runner.log").read_text()
        assert f"selected screenshot display backend: {'Xwayland' if x11 else 'Wayland'}" in runner
        assert runner.count("app launcher pid:") == 1, "app was retried"
        workspace = Path(next(line.split(": ", 1)[1] for line in runner.splitlines()
                              if line.startswith("capture workspace: ")))
        assert not workspace.exists(), "capture workspace leaked"
        launcher = int(next(line.split(": ", 1)[1] for line in runner.splitlines()
                            if line.startswith("desktop launcher pid: ")))
        assert not any(state["group"] == launcher for state in process_snapshot().values()), "desktop group leaked"
        if x11:
            desktop = (output / "logs/desktop.stderr.log").read_text()
            display = re.search(r"Using public X11 display :(\d+)", desktop)[1]
            assert not Path(f"/tmp/.X11-unix/X{display}").exists(), "Xwayland socket leaked"
            assert not Path(f"/tmp/.X{display}-lock").exists(), "Xwayland lock leaked"
        print(f"PASS: {name}", flush=True)

    run("wayland", False)
    assert not Path("/tmp/.X11-unix").exists(), "Wayland-only capture must not initialize Xwayland"
    run("x11", True)
    assert Path("/tmp/.X11-unix").stat().st_mode & 0o7777 == 0o1777
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run, name, True) for name in ("concurrent-a", "concurrent-b")]
        for future in futures:
            future.result()
    # Reuse the real detached-instance cancellation contract for this backend.
    subprocess.run([sys.executable, "tests/screenshot_failures.py", "--binary", args.binary,
                    "--bundle", args.x11_bundle, "--case", "cancel-instances",
                    "--output", str(args.output / "cancellation")], check=True, timeout=90)
    assert not list(Path("/tmp/.X11-unix").iterdir()), "session socket files survived cancellation"
    print("PASS: Xwayland detached-instance cancellation and socket cleanup", flush=True)


if __name__ == "__main__":
    main()
