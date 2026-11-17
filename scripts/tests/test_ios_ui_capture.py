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
    def setUp(self):
        self.temporary_parent = Path(__file__).resolve().parents[2] / "target/mobile-capture-policy-tests"
        self.temporary_parent.mkdir(parents=True, exist_ok=True)
        provenance = patch.object(capture, "source_snapshot", return_value={"synthetic.rs": "input-hash"})
        provenance.start()
        self.addCleanup(provenance.stop)

    def test_existing_output_is_refused_without_touching_evidence_or_running_cargo(self):
        with tempfile.TemporaryDirectory(dir=self.temporary_parent) as directory:
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
        with tempfile.TemporaryDirectory(dir=self.temporary_parent) as directory:
            output = Path(directory).resolve() / "new"

            def fail(arguments, **kwargs):
                output.mkdir()
                (output / "partial.png").write_bytes(b"synthetic partial evidence")
                raise subprocess.CalledProcessError(1, arguments)

            with patch.object(capture.subprocess, "run", side_effect=fail) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    capture.main(["--output", str(output)])
            calls = [call for call in run.call_args_list if call.args[0][:2] == ["cargo", "test"]]
            self.assertEqual(len(calls), 1)
            invocation = calls[0]
            command = invocation.args[0]
            self.assertEqual(command[-4:], ["ui_gallery::capture_mobile_ui_gallery", "--", "--ignored", "--exact"])
            self.assertIn("--locked", command)
            self.assertEqual(command[command.index("--features") + 1], "host-clients")
            self.assertEqual(invocation.kwargs["timeout"], 600)
            self.assertTrue(invocation.kwargs["check"])
            self.assertEqual(invocation.kwargs["env"]["FESTERM_MOBILE_UI_OUT"], str(output))
            self.assertEqual((output / "partial.png").read_bytes(), b"synthetic partial evidence")

    def test_document_capture_is_separate_attributed_evidence_and_failure_is_not_success(self):
        with tempfile.TemporaryDirectory(dir=self.temporary_parent) as directory:
            output = Path(directory).resolve() / "new"

            def capture_then_fail(arguments, **kwargs):
                if arguments[-4] == "ui_gallery::capture_mobile_ui_gallery":
                    output.mkdir()
                    (output / "manifest.json").write_text("shell evidence")
                else:
                    documents = Path(kwargs["env"]["FESTERM_DOCUMENT_UI_OUT"])
                    documents.mkdir()
                    (documents / "partial.png").write_bytes(b"partial documents")
                    raise subprocess.CalledProcessError(1, arguments)

            with patch.object(capture.subprocess, "run", side_effect=capture_then_fail) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    capture.main(["--output", str(output)])
            calls = [call for call in run.call_args_list if call.args[0][:2] == ["cargo", "test"]]
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[-1].args[0][-4:],
                             ["documents::tests::capture_document_ui", "--", "--ignored", "--exact"])
            self.assertEqual(calls[-1].kwargs["timeout"], 600)
            self.assertTrue(calls[-1].kwargs["check"])
            self.assertEqual((output / "manifest.json").read_text(), "shell evidence")
            self.assertEqual((output / "documents/partial.png").read_bytes(), b"partial documents")

    def test_missing_host_generator_manifest_refuses_incomplete_capture(self):
        with tempfile.TemporaryDirectory(dir=self.temporary_parent) as directory:
            output = Path(directory).resolve() / "new"

            def capture_without_host(arguments, **kwargs):
                test = arguments[-4]
                if test == "ui_gallery::capture_host_client_ui_gallery":
                    return
                variable = ("FESTERM_DOCUMENT_UI_OUT" if test == "documents::tests::capture_document_ui"
                            else "FESTERM_MOBILE_UI_OUT")
                destination = Path(kwargs["env"][variable])
                destination.mkdir(parents=True, exist_ok=True)
                (destination / "manifest.json").write_text("{}")

            with patch.object(capture.subprocess, "run", side_effect=capture_without_host) as run:
                with self.assertRaisesRegex(RuntimeError, "capture_host_client_ui_gallery produced no manifest"):
                    capture.main(["--output", str(output)])
            self.assertEqual(run.call_count, 5)
            self.assertTrue((output / "documents/manifest.json").exists())
            self.assertFalse((output / "hosts/manifest.json").exists())


if __name__ == "__main__":
    unittest.main()
