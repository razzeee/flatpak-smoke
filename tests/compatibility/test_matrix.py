"""Tests for the matrix's public capture-artifact validation boundary."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

from matrix import invoke, validate_run, workspace_status


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

    def test_missing_workspace_provenance_cannot_be_passed(self):
        runs = [{"status": "passed", "workspace": "/tmp/one"}, {"status": "passed"}]
        provenance, unique, workspaces = workspace_status(runs)
        self.assertFalse(provenance)
        self.assertTrue(unique)
        self.assertEqual(workspaces, ["/tmp/one"])

    def test_forced_runner_termination_has_a_bounded_wait(self):
        process = mock.Mock()
        process.wait.side_effect = [subprocess.TimeoutExpired("runner", 330),
                                    subprocess.TimeoutExpired("runner", 5),
                                    subprocess.TimeoutExpired("runner", 5)]
        with mock.patch("matrix.subprocess.Popen", return_value=process), \
             mock.patch("matrix.os.killpg"):
            with self.assertRaisesRegex(TimeoutError, "after SIGKILL"):
                invoke(["runner"], mock.Mock())


if __name__ == "__main__":
    unittest.main()
