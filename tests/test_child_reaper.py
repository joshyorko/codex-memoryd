"""Exercise the Linux isolated-test launcher against real owned processes."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

LAUNCHER = Path(__file__).resolve().parents[1] / "scripts" / "run-with-child-reaper.py"


@unittest.skipUnless(sys.platform == "linux", "Linux child-subreaper contract")
class ChildReaperTests(unittest.TestCase):
    def run_fixture(self, code, directory, **kwargs):
        return subprocess.Popen(
            [sys.executable, str(LAUNCHER), sys.executable, "-c", code,
             "child-reaper-fixture", str(directory)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kwargs,
        )

    def cleanup_fixture(self, process, directory):
        if process.poll() is None:
            process.kill()
        for path in directory.glob("*.pid"):
            pid = int(path.read_text())
            try:
                command = Path(f"/proc/{pid}/cmdline").read_bytes()
                if b"child-reaper-fixture" in command:
                    os.kill(pid, signal.SIGKILL)
            except (FileNotFoundError, ProcessLookupError):
                pass
        process.communicate(timeout=5)

    def test_success_preserves_output_and_exit(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            process = self.run_fixture("print('completed isolated command')", directory)
            try:
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0, stderr)
                self.assertEqual(stdout, "completed isolated command\n")
            finally:
                self.cleanup_fixture(process, directory)

    def test_main_exit_seven_is_preserved_while_orphans_are_cleaned(self):
        code = """
import os, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[2])
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)', 'child-reaper-fixture'])
(root / 'orphan.pid').write_text(str(child.pid))
sys.exit(7)
"""
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            process = self.run_fixture(code, directory)
            try:
                self.assertEqual(process.wait(timeout=5), 7)
                pid = int((directory / "orphan.pid").read_text())
                self.assertFalse(Path(f"/proc/{pid}").exists(), "owned orphan must be reaped before handoff")
            finally:
                self.cleanup_fixture(process, directory)

    def test_real_double_fork_orphan_is_reaped_while_command_runs(self):
        code = """
import os, pathlib, sys, time
root = pathlib.Path(sys.argv[2])
intermediate = os.fork()
if intermediate == 0:
    orphan = os.fork()
    if orphan == 0:
        os._exit(0)
    (root / 'orphan.pid').write_text(str(orphan))
    os._exit(0)
os.waitpid(intermediate, 0)
pid = int((root / 'orphan.pid').read_text())
deadline = time.monotonic() + 2
while pathlib.Path(f'/proc/{pid}').exists() and time.monotonic() < deadline:
    time.sleep(0.01)
if pathlib.Path(f'/proc/{pid}').exists():
    sys.exit(9)
print('orphan reaped while main command remains live')
"""
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            process = self.run_fixture(code, directory)
            try:
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0, stderr)
                self.assertIn("orphan reaped", stdout)
            finally:
                self.cleanup_fixture(process, directory)

    def test_interruption_reaches_main_and_cleans_owned_descendants(self):
        code = """
import os, pathlib, signal, subprocess, sys, time
root = pathlib.Path(sys.argv[2])
(root / 'main.pid').write_text(str(os.getpid()))
signal.signal(signal.SIGTERM, lambda *_: sys.exit(17))
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)', 'child-reaper-fixture'])
(root / 'orphan.pid').write_text(str(child.pid))
(root / 'ready').write_text('ready')
time.sleep(60)
"""
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            process = self.run_fixture(code, directory)
            try:
                deadline = time.monotonic() + 3
                while not (directory / "ready").exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue((directory / "ready").exists(), "fixture must be live before interruption")
                started = time.monotonic()
                process.send_signal(signal.SIGTERM)
                self.assertEqual(process.wait(timeout=4), 17)
                self.assertLess(time.monotonic() - started, 4)
                for path in directory.glob("*.pid"):
                    self.assertFalse(Path(f"/proc/{int(path.read_text())}").exists(), "owned fixture must not survive interruption")
            finally:
                self.cleanup_fixture(process, directory)

    def test_ignored_interrupt_is_escalated_within_cleanup_deadline(self):
        code = """
import os, pathlib, signal, subprocess, sys, time
root = pathlib.Path(sys.argv[2])
(root / 'main.pid').write_text(str(os.getpid()))
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)', 'child-reaper-fixture'])
(root / 'orphan.pid').write_text(str(child.pid))
(root / 'ready').write_text('ready')
time.sleep(60)
"""
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            process = self.run_fixture(code, directory)
            try:
                deadline = time.monotonic() + 3
                while not (directory / "ready").exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertTrue((directory / "ready").exists())
                started = time.monotonic()
                process.send_signal(signal.SIGTERM)
                self.assertEqual(process.wait(timeout=4), 137)
                self.assertLess(time.monotonic() - started, 4)
                for path in directory.glob("*.pid"):
                    self.assertFalse(Path(f"/proc/{int(path.read_text())}").exists())
            finally:
                self.cleanup_fixture(process, directory)


if __name__ == "__main__":
    unittest.main()
