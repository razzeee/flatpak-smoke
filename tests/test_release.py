"""Release safeguards without building or publishing containers."""
import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / ".github/scripts"


class ReleaseTests(unittest.TestCase):
    def test_version_guard_accepts_prepared_tag_and_rejects_historical_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory)
            (workspace / "action").mkdir()
            # The v0.1.1 tag was created with this v0.1.0 image default.
            (workspace / "action/image-version").write_text("v0.1.0\n")
            for tag, expected in (("v0.1.0", 0), ("v0.1.1", 1)):
                with self.subTest(tag=tag):
                    result = subprocess.run(
                        ["python3", str(SCRIPTS / "check_release.py")], cwd=workspace,
                        env={**os.environ, "RELEASE_TAG": tag}, capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode, expected, result.stderr)
                    if expected:
                        self.assertIn("do not move existing tags", result.stderr)

    def verify_image(self, failure=None, digest=None, failure_at=1):
        script = runpy.run_path(str(SCRIPTS / "verify_release_image.py"))
        expected = "sha256:" + "a" * 64
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            env = {"RELEASE_IMAGE": "ghcr.io/razzeee/flatpak-smoke",
                   "RELEASE_TAG": "v0.4.1", "RELEASE_REVISION": "b" * 40,
                   "RELEASE_DIGEST": digest if digest is not None else expected,
                   "GITHUB_STEP_SUMMARY": str(summary), "DOCKER_CONFIG": "/publishing-credentials"}
            calls = []

            def inspect(command, **kwargs):
                config = Path(kwargs["env"]["DOCKER_CONFIG"])
                self.assertNotEqual(str(config), env["DOCKER_CONFIG"])
                self.assertEqual(json.loads((config / "config.json").read_text()), {
                    "cliPluginsExtraDirs": ["/publishing-credentials/cli-plugins"],
                })
                calls.append(command[4])
                outcome = failure if len(calls) == failure_at else None
                if isinstance(outcome, Exception):
                    raise outcome
                return subprocess.CompletedProcess(command, 0, stdout=outcome or expected)

            with mock.patch.dict(os.environ, env), mock.patch("subprocess.run", side_effect=inspect), \
                    contextlib.redirect_stdout(io.StringIO()):
                if failure is not None or digest is not None:
                    with self.assertRaises((SystemExit, subprocess.CalledProcessError)):
                        script["main"]()
                    self.assertFalse(summary.exists())
                else:
                    script["main"]()
                    self.assertEqual(calls, [f'{env["RELEASE_IMAGE"]}:v0.4.1',
                                            f'{env["RELEASE_IMAGE"]}:sha-{"b" * 40}',
                                            f'{env["RELEASE_IMAGE"]}@{expected}'])
                    self.assertIn(f'{env["RELEASE_IMAGE"]}@{expected}', summary.read_text())

    def test_public_references_match_push_digest_without_credentials(self):
        self.verify_image()

    def test_wrong_digest_is_not_reported_as_published(self):
        for position in (1, 2, 3):
            with self.subTest(position=position):
                self.verify_image(failure="sha256:" + "c" * 64, failure_at=position)

    def test_missing_or_private_image_is_not_reported_as_published(self):
        for position in (1, 2, 3):
            with self.subTest(position=position):
                self.verify_image(failure=subprocess.CalledProcessError(1, "docker"), failure_at=position)

    def test_missing_push_digest_is_rejected(self):
        self.verify_image(digest="")


if __name__ == "__main__":
    unittest.main()
