"""Exercise seeded profiles and launch arguments inside the screenshot container."""
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    for attempt in range(2):
        output = ROOT / f"target/screenshot-setup-{attempt}"
        result = subprocess.run([
            "flatpak-smoke", "screenshot-bundle", "target/org.example.ScreenshotGtk.flatpak",
            "--recipe", "fixtures/screenshot/seeded.yml", "--output", str(output),
            "--force", "--allow-network-remotes",
        ], cwd=ROOT, timeout=330)
        assert result.returncode == 0, (output / "result.json").read_text()
        manifest = json.loads((output / "screenshots.json").read_text())
        assert [capture["name"] for capture in manifest["captures"]] == ["seeded"], manifest
        assert (output / manifest["captures"][0]["path"]).stat().st_size > 0
        log = (output / "logs/app.stdout.log").read_text()
        assert "seeded paths match sandbox XDG directories" in log, log
    assert (ROOT / "fixtures/screenshot/sample-data/document.txt").read_text() == "Sample document\n"
    assert (ROOT / "fixtures/screenshot/sample-settings.ini").read_text() == "welcome=false\n"
    recipe = ROOT / "target/screenshot-setup-missing.json"
    recipe.write_text(json.dumps({"version": 1, "setup": {"files": [
        {"source": "does-not-exist", "destination": "data/document.txt"}]},
        "steps": [{"capture": {"name": "missing", "caption": "Missing document"}}]}))
    output = ROOT / "target/screenshot-setup-missing"
    result = subprocess.run([
        "flatpak-smoke", "screenshot-bundle", "target/org.example.ScreenshotGtk.flatpak",
        "--recipe", str(recipe), "--output", str(output), "--force", "--allow-network-remotes",
    ], cwd=ROOT, timeout=330)
    assert result.returncode != 0
    failure = json.loads((output / "result.json").read_text())
    assert "does-not-exist" in failure["failure"]["message"], failure
    assert json.loads((output / "screenshots.json").read_text())["captures"] == []
    assert not (output / "logs/app.stdout.log").exists(), "app must not launch after setup failure"
    print("Seeded screenshot profiles and launch arguments passed", flush=True)


if __name__ == "__main__":
    main()
