"""Action wrapper contract tested at its Docker process boundary."""
import json
import contextlib
import io
import os
from pathlib import Path
import runpy
import signal
import subprocess
import tempfile
import time
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
        docker.write_text("#!/usr/bin/env python3\nimport json, os, sys, time\n"
                          "if sys.argv[1] == 'run':\n"
                          "    open(os.environ['DOCKER_RECORD'], 'w').write(json.dumps(sys.argv[1:]))\n"
                          "    time.sleep(float(os.environ.get('DOCKER_SLEEP', '0')))\n"
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
        self.assertEqual((self.workspace / "outputs").read_text(),
                         "output=artifacts/flatpak-smoke\nexecution=artifacts/flatpak-smoke.execution.json\n")
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
                    self.assertIn(f"ghcr.io/razzeee/flatpak-smoke{suffix}:v0.6.0", command)

    def test_invalid_inputs_fail_before_docker(self):
        for changes in [
            {"mode": "bogus"}, {"repo": "repo"}, {"app-ref": "app/a/b/c"},
            {"recipe": "recipe.yml"}, {"mode": "screenshots"},
            {"output": "../outside"}, {"output": "."}, {"output": "new\nline"},
            {"output": "/absolute"}, {"user": "arbitrary"}, {"image": "--privileged"},
            {"allow-network-remotes": "yes"}, {"bundle": "missing.flatpak"},
            {"args": "--output=elsewhere"}, {"args": "--recipe other.yml"},
            {"args": "--allow-network-remotes"}, {"args": '"unclosed'},
            {"execution-timeout": "0"}, {"execution-timeout": "5m"},
            {"cleanup-timeout": "-1"}, {"cleanup-timeout": "86401"},
            {"dependency-repo": "../outside"}, {"dependency-repo": "missing"},
            {"args": "--dependency-repo=other"},
        ]:
            with self.subTest(changes=changes):
                result = self.run_action({"bundle": "app bundle.flatpak", **changes})
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.workspace / "docker.json").exists(), result.stdout)

    def test_dependency_repo_is_read_only_in_both_modes(self):
        (self.workspace / "dependencies").mkdir()
        (self.workspace / "recipe.yml").touch()
        for mode in ("verify", "screenshots"):
            inputs = {"bundle": "app bundle.flatpak", "dependency-repo": "dependencies", "mode": mode}
            if mode == "screenshots":
                inputs["recipe"] = "recipe.yml"
            result = self.run_action(inputs)
            self.assertEqual(result.returncode, 0, result.stderr)
            command = json.loads((self.workspace / "docker.json").read_text())
            self.assertIn(f"{self.workspace}/dependencies:/workspace/dependencies:ro", command)
            self.assertEqual(command[command.index("--dependency-repo") + 1], "dependencies")
        result = self.run_action({"bundle": "app bundle.flatpak", "dependency-repo": "dependencies",
                                  "output": "dependencies/results"})
        self.assertNotEqual(result.returncode, 0)

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
                        self.assertEqual(action["main"](), 1 if isinstance(outcome, Exception) else outcome)
                    record = json.loads((self.workspace / "artifacts/flatpak-smoke.execution.json").read_text())
                    self.assertIn(record["cleanup"]["remove"]["status"], ("failed", "timeout"))
                    if isinstance(outcome, Exception):
                        self.assertEqual(record["error"], "original launch failure")

    def test_timeout_and_cancellation_preserve_cli_result_and_bound_cleanup(self):
        action = runpy.run_path(str(ROOT / "action/run.py"))
        output = self.workspace / "artifacts/flatpak-smoke"
        output.mkdir(parents=True)
        result = output / "result.json"
        result.write_text('{"reason":"specific_cli_failure"}')
        env = {"GITHUB_WORKSPACE": str(self.workspace),
               "GITHUB_OUTPUT": str(self.workspace / "outputs"),
               "FLATPAK_SMOKE_INPUTS": json.dumps({"bundle": "app bundle.flatpak",
                                                   "execution-timeout": "420", "cleanup-timeout": "30"})}
        for error, status, code in [(subprocess.TimeoutExpired("docker", 420), "timeout", 124),
                                    (KeyboardInterrupt(), "cancelled", 130)]:
            with self.subTest(status=status), mock.patch.dict(os.environ, env), \
                    mock.patch("signal.signal"), mock.patch("subprocess.run", side_effect=[
                        error, subprocess.TimeoutExpired("stop", 15),
                        subprocess.CompletedProcess([], 0, "", "")]) as run:
                self.assertEqual(action["main"](), code)
                self.assertEqual(run.call_args_list[0].kwargs["timeout"], 420)
                self.assertLessEqual(run.call_args_list[1].kwargs["timeout"], 15)
                self.assertLessEqual(run.call_args_list[2].kwargs["timeout"], 30)
            record = json.loads(output.with_suffix(".execution.json").read_text())
            self.assertEqual(record["status"], status)
            self.assertEqual(record["cleanup"]["stop"]["status"], "timeout")
            self.assertEqual(record["cleanup"]["remove"]["status"], "ok")
            self.assertEqual(result.read_text(), '{"reason":"specific_cli_failure"}')

    def test_docker_start_failure_is_distinct_from_cli_failure(self):
        for code, status in [(125, "docker_start_failed"), (126, "docker_start_failed"),
                             (127, "docker_start_failed"), (1, "completed")]:
            self.assertEqual(self.run_action({"bundle": "app bundle.flatpak"}, code).returncode, code)
            record = json.loads((self.workspace / "artifacts/flatpak-smoke.execution.json").read_text())
            self.assertEqual(record["status"], status)

    def test_cleanup_waits_for_removal_already_in_progress(self):
        action = runpy.run_path(str(ROOT / "action/run.py"))
        conflict = "Error response from daemon: removal of container fixture is already in progress"
        with mock.patch("subprocess.run", side_effect=[
            subprocess.CompletedProcess([], 0, "", ""),
            subprocess.CompletedProcess([], 1, "", conflict),
            subprocess.CompletedProcess([], 1, "", "Error response from daemon: No such container: fixture"),
        ]) as run, mock.patch("time.sleep"):
            result = action["cleanup"]("fixture", 30, True)
        self.assertEqual(result["stop"]["status"], "ok")
        self.assertEqual(result["remove"]["status"], "absent")
        self.assertEqual(run.call_count, 3)
        self.assertEqual(run.call_args_list[2].args[0], ["docker", "rm", "-f", "fixture"])

    def test_cleanup_removal_retries_share_the_stop_deadline(self):
        action = runpy.run_path(str(ROOT / "action/run.py"))
        now = 0.0

        def run(command, **kwargs):
            nonlocal now
            if command[1] == "stop":
                self.assertLessEqual(kwargs["timeout"], .5)
                now += .4
                return subprocess.CompletedProcess(command, 0, "", "")
            self.assertGreater(kwargs["timeout"], 0)
            self.assertLessEqual(kwargs["timeout"], 1 - now)
            self.assertLessEqual(kwargs["timeout"], .6)
            return subprocess.CompletedProcess(command, 1, "",
                "Error response from daemon: removal of container fixture is already in progress")

        def sleep(seconds):
            nonlocal now
            now += seconds

        with mock.patch("time.monotonic", side_effect=lambda: now), \
                mock.patch("time.sleep", side_effect=sleep), \
                mock.patch("subprocess.run", side_effect=run):
            result = action["cleanup"]("fixture", 1, True)
        self.assertEqual(result["remove"]["status"], "timeout")
        self.assertEqual(now, 1)

    def test_cleanup_does_not_retry_other_removal_errors(self):
        action = runpy.run_path(str(ROOT / "action/run.py"))
        with mock.patch("subprocess.run", return_value=subprocess.CompletedProcess(
                [], 1, "", "Cannot connect to the Docker daemon")) as run:
            result = action["cleanup"]("fixture", 30, False)
        self.assertEqual(result["remove"]["status"], "failed")
        self.assertEqual(run.call_count, 1)

    def test_execution_deadline_terminates_real_docker_client_process(self):
        started = time.monotonic()
        with mock.patch.dict(os.environ, {"DOCKER_SLEEP": "30"}):
            result = self.run_action({"bundle": "app bundle.flatpak", "execution-timeout": "1"})
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assertLess(time.monotonic() - started, 5)
        record = json.loads((self.workspace / "artifacts/flatpak-smoke.execution.json").read_text())
        self.assertEqual(record["status"], "timeout")
        self.assertEqual(record["cleanup"]["stop"]["status"], "ok")
        self.assertEqual(record["cleanup"]["remove"]["status"], "ok")


class ContainerTests(unittest.TestCase):
    def test_term_forwards_to_cli_then_restores_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, body in [("mkdir", ":"), ("dbus-daemon", ":"),
                               ("chown", f"test -f '{root}/result'; echo restored > '{root}/owner'")]:
                tool = root / name
                tool.write_text(f"#!/bin/sh\n{body}\n")
                tool.chmod(0o755)
            cli = root / "cli.py"
            cli.write_text("import signal, time\nfrom pathlib import Path\n"
                           f"root = Path({str(root)!r})\n"
                           "def cancel(*_):\n"
                           "    (root / 'result').write_text('cancelled')\n"
                           "    raise SystemExit(1)\n"
                           "signal.signal(signal.SIGTERM, cancel)\n"
                           "(root / 'ready').touch()\n"
                           "time.sleep(30)\n")
            child = subprocess.Popen(["sh", str(ROOT / "action/container.sh"), "root", "1000", "1000",
                                      "output", "python3", str(cli)],
                                     env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}"})
            try:
                deadline = time.monotonic() + 5
                while not (root / "ready").exists():
                    self.assertIsNone(child.poll())
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                child.send_signal(signal.SIGTERM)
                self.assertNotEqual(child.wait(timeout=5), 0)
                self.assertEqual((root / "result").read_text(), "cancelled")
                self.assertEqual((root / "owner").read_text(), "restored\n")
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()

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
