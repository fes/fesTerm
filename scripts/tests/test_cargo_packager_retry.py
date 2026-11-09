import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "run-cargo-packager.py"
SPEC = importlib.util.spec_from_file_location("cargo_packager_retry", SCRIPT)
packager = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(packager)


class CargoPackagerRetryTests(unittest.TestCase):
    def test_reads_colored_packager_error_from_combined_output(self):
        error = "ERROR https://github.com/tauri-apps/tool: status code 504"
        process = unittest.mock.Mock()
        process.stdout = io.StringIO(f"building\n\x1b[31m{error}\x1b[0m\n")
        process.wait.return_value = 1
        with patch.object(packager.subprocess, "Popen", return_value=process):
            self.assertEqual(packager.execute(["--formats", "nsis"]), (1, error))

    def test_retries_known_transient_download_then_succeeds(self):
        failure = "ERROR https://github.com/tauri-apps/binary-releases/releases/download/test: status code 504"
        with patch.object(packager, "execute", side_effect=[(1, failure), (0, "")]) as execute, \
                patch.object(packager.time, "sleep"):
            self.assertEqual(packager.run(["--formats", "appimage"]), 0)
            self.assertEqual(execute.call_count, 2)

    def test_other_errors_are_not_retried(self):
        for failure in (
            "ERROR signing failed",
            "ERROR https://github.com/tauri-apps/binary-releases/releases/download/test: status code 404",
            "ERROR https://example.invalid/tool: status code 504",
        ):
            with self.subTest(failure=failure), patch.object(
                packager, "execute", return_value=(1, failure),
            ) as execute, patch.object(packager.time, "sleep") as sleep:
                self.assertEqual(packager.run(["--formats", "nsis"]), 1)
                self.assertEqual(execute.call_count, 1)
                sleep.assert_not_called()

    def test_persistent_download_failure_is_reported(self):
        failure = "ERROR https://github.com/linuxdeploy/tool: status code 503"
        with patch.object(packager, "execute", return_value=(7, failure)) as execute, \
                patch.object(packager.time, "sleep"):
            self.assertEqual(packager.run([]), 7)
            self.assertEqual(execute.call_count, len(packager.DELAYS) + 1)

    def test_each_download_has_its_own_bounded_retry_budget(self):
        first = "ERROR https://github.com/tauri-apps/AppRun: status code 504"
        second = "ERROR https://github.com/tauri-apps/linuxdeploy: status code 504"
        failures = [(1, first)] * len(packager.DELAYS) + [(1, second)] * 2 + [(0, "")]
        with patch.object(packager, "execute", side_effect=failures), \
                patch.object(packager.time, "sleep") as sleep:
            self.assertEqual(packager.run([]), 0)
            self.assertEqual([call.args[0] for call in sleep.call_args_list], [5, 15, 30, 60, 120, 5, 15])

    def test_total_retries_are_bounded_across_different_downloads(self):
        failures = [
            (1, f"ERROR https://github.com/tauri-apps/tool-{index}: status code 504")
            for index in range(4)
        ]
        with patch.object(packager, "MAX_TOTAL_RETRIES", 3), \
                patch.object(packager, "execute", side_effect=failures) as execute, \
                patch.object(packager.time, "sleep"):
            self.assertEqual(packager.run([]), 1)
            self.assertEqual(execute.call_count, 4)


if __name__ == "__main__":
    unittest.main()
