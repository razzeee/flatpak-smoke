"""Tests for the matrix's public capture-artifact validation boundary."""
import json
from pathlib import Path
import tempfile
import unittest

from matrix import validate_run


class CaptureArtifacts(unittest.TestCase):
    def test_failed_process_cannot_be_reported_as_passing(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "result.json").write_text(json.dumps({"status": "passed"}))
            with self.assertRaisesRegex(ValueError, "exit"):
                validate_run(output, {"ref": "app/org.example.App/x86_64/stable", "captures": []}, 1)

    def test_missing_capture_is_not_a_successful_repeat(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            ref = "app/org.example.App/x86_64/stable"
            (output / "result.json").write_text(json.dumps({"status": "passed", "failure": None, "app_ref": ref, "screenshots": []}))
            (output / "screenshots.json").write_text(json.dumps({
                "app_ref": ref, "failed_step_index": None, "captures": [],
                "desktop": "gnome", "desktop_version": "48.7", "language": "en", "scale": 1,
            }))
            with self.assertRaisesRegex(ValueError, "capture order"):
                validate_run(output, {"ref": ref, "captures": ["overview"]}, 0)


if __name__ == "__main__":
    unittest.main()
