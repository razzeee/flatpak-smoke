"""Author and replay recipes inside the existing GNOME screenshot test image."""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="/usr/local/bin/flatpak-smoke")
    parser.add_argument("--output", default="target/screenshot-author")
    args = parser.parse_args()
    root = ROOT / args.output
    root.mkdir(parents=True, exist_ok=True)
    recipe = root / "recipe.yml"
    shutil.copytree(ROOT / "fixtures/screenshot/sample-data", root / "sample-data", dirs_exist_ok=True)
    shutil.copyfile(ROOT / "fixtures/screenshot/sample-settings.ini", root / "sample-settings.ini")
    recipe.write_text("""version: 1
setup:
  files:
    - source: sample-data
      destination: data/example project
    - source: sample-settings.ini
      destination: config/settings.ini
launch:
  args: ['--open', '${APP_DATA}/example project/document.txt']
steps: []
""")
    output = root / "author"
    result = subprocess.run([
        args.binary, "author-bundle", "target/org.example.ScreenshotGtk.flatpak",
        "--recipe", str(recipe), "--output", str(output), "--force",
        "--overall-timeout", "10m",
    ], input="\n".join([
        "observe",
        "wait_text: Sample document",
        "wait_text: Ready to capture",
        "capture: {name: overview, caption: Browse the initial view}",
        "type_text: example",
        "key: Return",
        "wait_text: Results for example",
        "capture: {name: search, caption: Find items by name}",
        # Reject duplicate names before executing, without dirtying the session.
        "capture: {name: search, caption: Duplicate name}",
        "click_text: Preferences",
        "select_window: {title: Preferences}",
        "wait_text: Choose how the app behaves",
        "capture: {name: preferences, caption: Choose how the app behaves}",
        # A failed expectation is excluded; subsequent input must be blocked.
        "{wait_text: This text is absent, timeout: 1s}",
        "type_text: must not be saved",
        "finish",
        "restart",
        "finish",
        "",
    ]), text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, cwd=ROOT, timeout=660)
    (root / "author.log").write_text(result.stdout)
    assert result.returncode == 0, result.stdout
    assert "duplicate capture name" in result.stdout, result.stdout
    assert "Step not saved" in result.stdout, result.stdout
    assert "restart from the saved draft" in result.stdout, result.stdout
    assert "OCR pass 1:" in result.stdout, result.stdout
    assert (output / "session-001/logs/observation.png").stat().st_size > 0
    assert json.loads((output / "author.json").read_text())["status"] == "validated"
    assert json.loads((output / "session-001/result.json").read_text())["status"] == "failed"
    exported = recipe.read_text()
    assert "This text is absent" not in exported, exported
    assert "must not be saved" not in exported, exported
    assert "key: Return" in exported, exported
    assert "${APP_DATA}" in exported, exported
    # The automatic validation and this ordinary replay must both use fresh state.
    second = root / "second-replay"
    subprocess.run([
        args.binary, "screenshot-bundle", "target/org.example.ScreenshotGtk.flatpak",
        "--recipe", str(recipe), "--output", str(second), "--force",
    ], cwd=ROOT, check=True, timeout=330)
    for run in [output / "validation", second]:
        assert json.loads((run / "result.json").read_text())["status"] == "passed"
        manifest = json.loads((run / "screenshots.json").read_text())
        assert [capture["name"] for capture in manifest["captures"]] == ["overview", "search", "preferences"]
        assert "seeded paths match sandbox XDG directories" in (run / "logs/app.stdout.log").read_text()
    assert (root / "sample-data/document.txt").read_text() == "Sample document\n"
    assert (root / "sample-settings.ini").read_text() == "welcome=false\n"

    ambiguous = root / "ambiguous.yml"
    ambiguous.write_text("version: 1\nsteps: []\n")
    ambiguous_output = root / "ambiguous"
    result = subprocess.run([
        args.binary, "author-bundle", "target/org.example.ScreenshotGtk.flatpak",
        "--recipe", str(ambiguous), "--output", str(ambiguous_output), "--force",
    ], input="\n".join([
        "type_text: duplicates", "key: Return", "wait_text: Duplicate",
        "click_text: Duplicate", "key: Tab", "quit", "",
    ]), text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, cwd=ROOT, timeout=330)
    (root / "ambiguous.log").write_text(result.stdout)
    assert result.returncode == 0, result.stdout
    assert "matched 2 locations" in result.stdout, result.stdout
    assert "click_text" not in ambiguous.read_text()
    assert "key: Tab" not in ambiguous.read_text()
    assert json.loads((ambiguous_output / "author.json").read_text())["status"] == "draft"

    # Keep stdin open without sending a command: cancellation must interrupt the
    # prompt's wait and release the desktop/profile, rather than block on readline.
    cancelled = root / "cancelled.yml"
    original = "version: 1\nsteps: []\n"
    cancelled.write_text(original)
    cancelled_output = root / "cancelled"
    child = subprocess.Popen([
        args.binary, "author-bundle", "target/org.example.ScreenshotGtk.flatpak",
        "--recipe", str(cancelled), "--output", str(cancelled_output), "--force",
    ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, cwd=ROOT)
    transcript = b""
    try:
        deadline = time.monotonic() + 90
        while b"author> " not in transcript:
            budget = deadline - time.monotonic()
            assert budget > 0, transcript.decode()
            ready, _, _ = select.select([child.stdout], [], [], budget)
            assert ready, transcript.decode()
            chunk = os.read(child.stdout.fileno(), 4096)
            assert chunk, transcript.decode()
            transcript += chunk
        child.send_signal(signal.SIGTERM)
        tail, _ = child.communicate(timeout=15)
        transcript += tail
        assert child.returncode != 0, transcript.decode()
    finally:
        if child.poll() is None:
            child.kill()
            child.communicate(timeout=5)
    (root / "cancelled.log").write_bytes(transcript)
    assert cancelled.read_text() == original
    assert json.loads((cancelled_output / "author.json").read_text())["status"] == "draft"
    session = cancelled_output / "session-001"
    assert json.loads((session / "result.json").read_text())["status"] == "failed"
    runner = (session / "logs/runner.log").read_text()
    workspace = Path(next(line.split(": ", 1)[1] for line in runner.splitlines()
                          if line.startswith("capture workspace:")))
    assert not workspace.exists(), workspace
    print("Recipe authoring, restart and clean replays passed", flush=True)


if __name__ == "__main__":
    main()
