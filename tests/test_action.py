"""Action wrapper contract tested at its Docker process boundary."""
import json
import contextlib
import io
import os
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


class ActionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.workspace = Path(self.temp.name)
        self.bin = self.workspace / "bin"
        self.bin.mkdir()
        docker = self.bin / "docker"
        docker.write_text("#!/usr/bin/env python3\nimport json, os, sys\n"
                          "if sys.argv[1] == 'run':\n"
                          "    open(os.environ['DOCKER_RECORD'], 'w').write(json.dumps(sys.argv[1:]))\n"
                          "    sys.exit(int(os.environ.get('DOCKER_EXIT', '0')))\n")
        docker.chmod(0o755)
        (self.workspace / "app bundle.flatpak").touch()

    def run_action(self, inputs, exit_code=0):
        env = {**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}",
               "GITHUB_WORKSPACE": str(self.workspace),
               "GITHUB_OUTPUT": str(self.workspace / "outputs"),
               "FLATPAK_SMOKE_INPUTS": json.dumps(inputs),
               "DOCKER_RECORD": str(self.workspace / "docker.json"),
               "DOCKER_EXIT": str(exit_code)}
        return subprocess.run(["python3", str(ROOT / "action/run.py")], env=env,
                              text=True, capture_output=True)

    def test_verify_bundle_preserves_quoted_args_failure_and_output(self):
        result = self.run_action({"bundle": "app bundle.flatpak", "image": "local-smoke",
                                  "allow-network-remotes": "true",
                                  "args": '--screenshot-after-click "Log In" --overall-timeout 5m'}, 7)
        self.assertEqual(result.returncode, 7, result.stderr)
        command = json.loads((self.workspace / "docker.json").read_text())
        self.assertIn("--privileged", command)
        self.assertEqual(command[command.index("flatpak-smoke") + 1:], [
            "verify-bundle", "app bundle.flatpak", "--output", "artifacts/flatpak-smoke",
            "--allow-network-remotes", "--screenshot-after-click", "Log In", "--overall-timeout", "5m"])
        self.assertEqual((self.workspace / "outputs").read_text(), "output=artifacts/flatpak-smoke\n")
        self.assertTrue((self.workspace / "artifacts/flatpak-smoke").is_dir())
        self.assertEqual((self.workspace / "artifacts").stat().st_uid, os.getuid())

    def test_screenshot_repo_nonroot_and_literal_arguments(self):
        (self.workspace / "repo").mkdir()
        (self.workspace / "recipe.yml").write_text("version: 1")
        result = self.run_action({"mode": "screenshots", "repo": "repo",
                                  "app-ref": "app/org.example.App/x86_64/stable",
                                  "recipe": "recipe.yml", "user": "nobody",
                                  "image": "example/image@sha256:1234",
                                  "args": '--screenshot-timeout 20s "$(touch sentinel)"'})
        self.assertEqual(result.returncode, 0, result.stderr)
        command = json.loads((self.workspace / "docker.json").read_text())
        self.assertIn("/dev/dri", command)
        self.assertIn("nobody", command)
        self.assertIn("example/image@sha256:1234", command)
        self.assertEqual(command[command.index("flatpak-smoke") + 1:], [
            "screenshot-repo", "repo", "app/org.example.App/x86_64/stable",
            "--output", "artifacts/flatpak-smoke", "--recipe", "recipe.yml",
            "--screenshot-timeout", "20s", "$(touch sentinel)"])
        self.assertFalse((self.workspace / "sentinel").exists())

    def test_both_modes_and_artifact_types_select_versioned_images(self):
        (self.workspace / "repo").mkdir()
        (self.workspace / "recipe.yml").touch()
        for mode in ("verify", "screenshots"):
            for kind in ("bundle", "repo"):
                with self.subTest(mode=mode, kind=kind):
                    inputs = {"mode": mode, kind: "app bundle.flatpak" if kind == "bundle" else "repo"}
                    if kind == "repo":
                        inputs["app-ref"] = "app/org.example.App/x86_64/stable"
                    if mode == "screenshots":
                        inputs["recipe"] = "recipe.yml"
                    result = self.run_action(inputs)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    command = json.loads((self.workspace / "docker.json").read_text())
                    suffix = "-screenshots" if mode == "screenshots" else ""
                    self.assertIn(f"ghcr.io/razzeee/flatpak-smoke{suffix}:v0.1.2", command)

    def test_invalid_inputs_fail_before_docker(self):
        for changes in [
            {"mode": "bogus"}, {"repo": "repo"}, {"app-ref": "app/a/b/c"},
            {"recipe": "recipe.yml"}, {"mode": "screenshots"},
            {"output": "../outside"}, {"output": "."}, {"output": "new\nline"},
            {"output": "/absolute"}, {"user": "arbitrary"}, {"image": "--privileged"},
            {"allow-network-remotes": "yes"}, {"bundle": "missing.flatpak"},
            {"args": "--output=elsewhere"}, {"args": "--recipe other.yml"},
            {"args": "--allow-network-remotes"}, {"args": '"unclosed'},
        ]:
            with self.subTest(changes=changes):
                result = self.run_action({"bundle": "app bundle.flatpak", **changes})
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.workspace / "docker.json").exists(), result.stdout)

    def test_symlink_output_cannot_escape_workspace(self):
        (self.workspace / "escape").symlink_to(self.workspace.parent, target_is_directory=True)
        result = self.run_action({"bundle": "app bundle.flatpak", "output": "escape/out"})
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.workspace / "docker.json").exists())

    def test_cleanup_exceptions_preserve_run_status_and_original_error(self):
        action = runpy.run_path(str(ROOT / "action/run.py"))
        env = {"GITHUB_WORKSPACE": str(self.workspace),
               "GITHUB_OUTPUT": str(self.workspace / "outputs"),
               "FLATPAK_SMOKE_INPUTS": json.dumps({"bundle": "app bundle.flatpak"})}
        launch_error = OSError("original launch failure")
        for cleanup_error in (OSError("cleanup unavailable"), subprocess.TimeoutExpired("docker rm", 30)):
            for outcome in (0, 7, launch_error):
                with self.subTest(cleanup=type(cleanup_error).__name__, outcome=outcome):
                    run_result = outcome if isinstance(outcome, Exception) else subprocess.CompletedProcess([], outcome)
                    stderr = io.StringIO()
                    with mock.patch.dict(os.environ, env), contextlib.redirect_stderr(stderr), \
                            mock.patch("subprocess.run", side_effect=[run_result, cleanup_error]), \
                            mock.patch("signal.signal"):
                        if isinstance(outcome, Exception):
                            with self.assertRaises(OSError) as raised:
                                action["main"]()
                            self.assertIs(raised.exception, launch_error)
                        else:
                            self.assertEqual(action["main"](), outcome)
                    self.assertIn("container cleanup failed", stderr.getvalue())


class ContainerTests(unittest.TestCase):
    def test_ownership_failure_fails_success_but_preserves_cli_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            bin_path = Path(directory)
            for name in ("mkdir", "dbus-daemon", "chown"):
                command = bin_path / name
                command.write_text("#!/bin/sh\nexit " + ("${CHOWN_EXIT}" if name == "chown" else "0") + "\n")
                command.chmod(0o755)
            for cli_status, chown_status, expected in [(0, 0, 0), (0, 1, 1), (7, 0, 7), (7, 1, 7)]:
                with self.subTest(cli=cli_status, chown=chown_status):
                    result = subprocess.run([
                        "sh", str(ROOT / "action/container.sh"), "root", "1000", "1000", "output",
                        "sh", "-c", f"exit {cli_status}",
                    ], env={**os.environ, "PATH": f"{bin_path}:{os.environ['PATH']}",
                            "CHOWN_EXIT": str(chown_status)}, text=True, capture_output=True)
                    self.assertEqual(result.returncode, expected, result.stderr)
                    if chown_status:
                        self.assertIn("failed to restore output ownership", result.stderr)


if __name__ == "__main__":
    unittest.main()
