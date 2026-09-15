"""Screenshot CLI contract, run inside the GNOME screenshot container."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/workspace/target/debug/flatpak-smoke")
    parser.add_argument("--bundle", default="target/org.example.ScreenshotGtk.flatpak")
    parser.add_argument("--repo")
    parser.add_argument("--app-ref", default="app/org.example.ScreenshotGtk/x86_64/master")
    parser.add_argument("--output", default="target/screenshot-workflow")
    args = parser.parse_args()
    output = ROOT / args.output
    source = ["screenshot-repo", args.repo, args.app_ref] if args.repo else ["screenshot-bundle", args.bundle]
    result = subprocess.run([args.binary, *source,
                             "--recipe", "fixtures/screenshot/recipe.yml",
                             "--output", str(output), "--force", "--allow-network-remotes"],
                            cwd=ROOT, timeout=330)
    assert result.returncode == 0, (output / "result.json").read_text() if (output / "result.json").exists() else result
    manifest = json.loads((output / "screenshots.json").read_text())
    assert [capture["name"] for capture in manifest["captures"]] == ["overview", "search", "preferences"], manifest
    assert [capture["caption"] for capture in manifest["captures"]] == ["Browse the initial view", "Find items by name", "Choose how the app behaves"], manifest
    assert manifest["failed_step_index"] is None, manifest
    assert manifest["desktop"] == "gnome" and manifest["desktop_version"], manifest
    for capture in manifest["captures"]:
        image = output / capture["path"]
        assert image.is_file() and image.stat().st_size > 0, capture
        assert capture["language"] == "en", capture
        assert 0 < capture["window_width"] <= 1000 and 0 < capture["window_height"] <= 700, capture
        assert capture["width"] > capture["window_width"], capture
        alpha = subprocess.check_output(["convert", str(image), "-alpha", "extract", "-format", "%[fx:minima] %[fx:maxima] %k", "info:"], text=True).split()
        assert float(alpha[0]) == 0 and float(alpha[1]) == 1 and int(alpha[2]) > 2, alpha
    print("Screenshot workflow passed", flush=True)


if __name__ == "__main__":
    main()
