"""Real paired-run contract using the existing GTK fixture and screenshot image.

Run on the host after screenshot CI builds its bundles and frozen dependencies.
"""
import json
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def main():
    work = ROOT / "target/paired-fixture"
    work.mkdir(parents=True, exist_ok=False)
    inputs = work / "inputs"
    inputs.mkdir()
    shutil.copy2(ROOT / "target/org.example.ScreenshotGtk.flatpak", inputs / "baseline.flatpak")
    shutil.copy2(inputs / "baseline.flatpak", inputs / "candidate.flatpak")
    shutil.copytree(ROOT / "target/dependencies", inputs / "dependencies", symlinks=True)
    (inputs / "recipe.yml").write_text("""version: 1
steps:
  - wait_text: Ready to capture
  - click_text: Preferences
  - capture:
      name: preferences
      caption: Open preferences
""")
    commit = subprocess.check_output([
        "ostree", f"--repo={ROOT / 'target/screenshot-Gtk-repo'}", "rev-parse",
        "app/org.example.ScreenshotGtk/x86_64/master"], text=True).strip()
    common = [sys.executable, str(ROOT / "action/compare.py"), "--inputs", str(inputs),
              "--baseline", "baseline.flatpak", "--candidate", "candidate.flatpak",
              "--recipe", "recipe.yml", "--dependency-repo", "dependencies",
              "--baseline-source", "local-ci-fixture", "--baseline-ref", "app/org.example.ScreenshotGtk/x86_64/master",
              "--baseline-commit", commit, "--image", "screenshot-production", "--user", "nobody",
              "--execution-timeout", "120"]
    for name, expected in (("unchanged", "unchanged_pass"), ("regression", "new_regression_signal")):
        if name == "regression":
            # Export a second app commit with a missing recipe target, retaining
            # the same app identity, runtime, permissions and fixtures.
            build = work / "candidate-build"
            shutil.copytree(ROOT / "target/screenshot-Gtk-build", build, symlinks=True)
            script = build / "files/bin/screenshot-fixture"
            contents = script.read_text()
            original = 'Gtk.Button(label="Preferences",'
            assert contents.count(original) == 1
            script.write_text(contents.replace(original, 'Gtk.Button(label="Options",'))
            repo = work / "candidate-repo"
            subprocess.run(["flatpak", "build-export", str(repo), str(build)], check=True)
            (inputs / "candidate.flatpak").unlink()
            subprocess.run(["flatpak", "build-bundle", str(repo), str(inputs / "candidate.flatpak"),
                            "org.example.ScreenshotGtk"], check=True)
        output = ROOT / ("target/paired-" + name)
        subprocess.run([*common, "--output", str(output)], check=True)
        report = json.loads((output / "comparison.json").read_text())
        assert report["verdict"] == expected, report["reasons"]
        assert len(report["attempts"]) == 4
        assert report["inputs_unchanged"]
        # Keep the diagnostic evidence, not a second copy of the runtime repo.
        shutil.rmtree(output / "inputs")
    print("Paired GTK fixture passed")


if __name__ == "__main__":
    main()
