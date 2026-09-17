"""Exercise cleanup with a real descendant whose launcher has already exited."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import unittest

from native_capture import stop_process_group


class NativeCleanup(unittest.TestCase):
    def test_cleanup_stops_descendants_after_the_launcher_exits(self):
        worker = "import os, signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print(os.getpid(), flush=True); time.sleep(60)"
        launcher = subprocess.Popen(
            [sys.executable, "-c", f"import subprocess, sys; subprocess.Popen([sys.executable, '-c', {worker!r}])"],
            start_new_session=True, stdout=subprocess.PIPE, text=True,
        )
        try:
            worker_pid = int(launcher.stdout.readline())
            launcher.wait(timeout=3)
            stop_process_group(launcher)
            stat = Path(f"/proc/{worker_pid}/stat")
            try:
                state = stat.read_text().rsplit(")", 1)[1].split()[0]
            except (FileNotFoundError, ProcessLookupError):
                state = "X"
            self.assertIn(state, ("Z", "X"), "descendant survived process-group cleanup")
        finally:
            try:
                os.killpg(launcher.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            launcher.wait(timeout=3)
            launcher.stdout.close()


if __name__ == "__main__":
    unittest.main()
