import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import Mock, patch


spec = importlib.util.spec_from_file_location(
    "running_sessions", Path(__file__).parents[1] / "check_running_sessions.py")
running_sessions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(running_sessions)


class RunningSessionRunnerTests(unittest.TestCase):
    def test_cleanup_selects_exact_live_provider_identifiers_only(self):
        self.assertEqual(running_sessions.owned_targets("tmux", "$3\nmain\n$4\n"), ["$3", "$4"])
        self.assertEqual(running_sessions.owned_targets("screen",
            "There are screens on:\n 42.main (Detached)\n 43.main (Attached)\n 44.dead (Dead ???)\n"),
            ["42.main", "43.main"])

    def test_failure_status_is_not_masked(self):
        process = Mock()
        process.wait.return_value = 7
        process.poll.return_value = 7
        with patch.object(running_sessions.subprocess, "Popen", return_value=process):
            with self.assertRaises(subprocess.CalledProcessError) as error:
                running_sessions.run_checked(["cargo", "test"], cwd=".", env={}, timeout=1)
        self.assertEqual(error.exception.returncode, 7)

    @unittest.skipIf(running_sessions.os.name == "nt", "Unix process-group ownership")
    def test_timeout_terminates_only_the_captured_owned_process_group(self):
        process = Mock(pid=12345)
        process.wait.side_effect = [subprocess.TimeoutExpired("cargo", 1), 0, 0]
        process.poll.return_value = None
        with patch.object(running_sessions.subprocess, "Popen", return_value=process) as spawn:
            with patch.object(running_sessions.os, "killpg") as terminate:
                with self.assertRaises(subprocess.TimeoutExpired):
                    running_sessions.run_checked(["cargo", "test"], cwd=".", env={}, timeout=1)
        self.assertTrue(spawn.call_args.kwargs["start_new_session"])
        terminate.assert_called_once_with(12345, running_sessions.signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
