"""Capture invocation policy without a local Rust/GPU toolchain."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "mobile_capture", Path(__file__).resolve().parents[1] / "capture-mobile-ui.py")
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)


class MobileCaptureTests(unittest.TestCase):
    def test_existing_output_is_refused_without_touching_evidence_or_running_cargo(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "previous"
            output.mkdir()
            evidence = output / "failed.txt"
            evidence.write_text("preserve")
            with patch.object(capture.subprocess, "run") as run:
                with self.assertRaises(SystemExit):
                    capture.main(["--output", str(output)])
                run.assert_not_called()
            self.assertEqual(evidence.read_text(), "preserve")

    def test_failure_retains_attempt_and_runs_only_identified_ignored_capture_with_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory).resolve() / "new"

            def fail(arguments, **kwargs):
                output.mkdir()
                (output / "partial.png").write_bytes(b"synthetic partial evidence")
                raise subprocess.CalledProcessError(1, arguments)

            with patch.object(capture.subprocess, "run", side_effect=fail) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    capture.main(["--output", str(output)])
            command = run.call_args.args[0]
            self.assertEqual(command[-4:], ["ui_gallery::capture_mobile_ui_gallery", "--", "--ignored", "--exact"])
            self.assertIn("--locked", command)
            self.assertEqual(command[command.index("--features") + 1], "live-ssh")
            self.assertEqual(run.call_args.kwargs["timeout"], 600)
            self.assertTrue(run.call_args.kwargs["check"])
            self.assertEqual(run.call_args.kwargs["env"]["FESTERM_MOBILE_UI_OUT"], str(output))
            self.assertEqual((output / "partial.png").read_bytes(), b"synthetic partial evidence")


if __name__ == "__main__":
    unittest.main()
