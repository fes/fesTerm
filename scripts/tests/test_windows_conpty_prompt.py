import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import Mock


spec = importlib.util.spec_from_file_location(
    "windows_conpty_prompt",
    Path(__file__).parents[1] / "check_windows_conpty_prompt.py",
)
windows_conpty_prompt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(windows_conpty_prompt)


class WindowsConptyPromptRunnerTests(unittest.TestCase):
    def test_repeats_the_exact_regression_with_a_per_attempt_deadline(self):
        runner = Mock(return_value=subprocess.CompletedProcess([], 0))

        status = windows_conpty_prompt.run_cycles(3, repo=Path("checkout"), runner=runner)

        self.assertEqual(status, 0)
        self.assertEqual(runner.call_count, 3)
        for call in runner.call_args_list:
            self.assertEqual(call.args[0][0:5], ["cargo", "test", "--quiet", "-p", "festerm"])
            self.assertIn(windows_conpty_prompt.TEST_NAME, call.args[0])
            self.assertEqual(call.args[0][-2:], ["--", "--exact"])
            self.assertEqual(call.kwargs["timeout"], 45)

    def test_stops_at_the_first_failed_attempt(self):
        runner = Mock(
            side_effect=[
                subprocess.CompletedProcess([], 0),
                subprocess.CompletedProcess([], 7),
                subprocess.CompletedProcess([], 0),
            ]
        )

        status = windows_conpty_prompt.run_cycles(20, repo=Path("checkout"), runner=runner)

        self.assertEqual(status, 7)
        self.assertEqual(runner.call_count, 2)

    def test_reports_a_timed_out_attempt_without_retrying(self):
        runner = Mock(side_effect=subprocess.TimeoutExpired("cargo", 45))

        status = windows_conpty_prompt.run_cycles(20, repo=Path("checkout"), runner=runner)

        self.assertEqual(status, 124)
        runner.assert_called_once()


if __name__ == "__main__":
    unittest.main()
