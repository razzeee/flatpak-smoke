"""Real VLC first-run focus regression; run in the compatibility image as non-root.

python3 tests/compatibility/vlc_focus.py --bundle /opt/compatibility/artifacts/org.videolan.VLC.flatpak \
    --output target/vlc-focus
"""
import argparse
import json
from pathlib import Path

from matrix import invoke, validate_run


def run():
    """Verify fresh-profile modal rejection and configured VLC capture in separate runs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", default="flatpak-smoke")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    recipe = Path(__file__).with_name("vlc.yml")
    blocked_recipe = args.output / "privacy-prompt.yml"
    blocked_recipe.write_text(recipe.read_text().replace("    - --no-qt-privacy-ask\n", ""))
    for name, path in [("blocked", blocked_recipe), ("configured", recipe)]:
        output = args.output / name
        with (args.output / f"{name}.log").open("w") as log:
            code = invoke([args.binary, "screenshot-bundle", str(args.bundle.resolve()),
                           "--recipe", str(path.resolve()), "--output", str(output.resolve())], log)
        if name == "blocked":
            result = json.loads((output / "result.json").read_text())
            assert code != 0 and result["status"] == "failed", result
            message = result["failure"]["message"]
            prefix = "desktop helper: An application modal dialog blocks the selected window: "
            assert message.startswith(prefix), message
            state = json.loads(message[len(prefix):])
            assert state["selected"]["title"] == "VLC media player", state
            assert any(window["title"] == "Privacy and Network Access Policy" and window["type"] == 4
                       for window in state["windows"]), state
            assert not result["screenshots"], result
            assert "recipe step" not in (output / "logs/runner.log").read_text()
            print("VLC privacy dialog correctly blocks initialization", flush=True)
        else:
            manifest = json.loads((output / "screenshots.json").read_text())
            assert manifest["app_ref"].startswith("app/org.videolan.VLC/"), manifest
            validate_run(output, {"ref": manifest["app_ref"],
                                  "captures": ["overview", "playlist"]}, code)
            print("Configured VLC initializes, accepts input, and captures both views", flush=True)


if __name__ == "__main__":
    run()
