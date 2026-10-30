"""Capture invocation policy without a local Rust/GPU toolchain."""
import importlib.util
import hashlib
import json
import io
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
    def fixture(self, output):
        images = []
        for family in ("phone", "tablet"):
            for state in ("terminal", "connect"):
                name = f"{family}-{state}.png"
                payload = b"synthetic image bytes"
                (output / name).write_bytes(payload)
                images.append({"image": name, "sha256": hashlib.sha256(payload).hexdigest()})
        manifest = {"images": images, "source_commit": "synthetic-source"}
        (output / "manifest.json").write_text(json.dumps(manifest))
        return manifest

    def test_review_transport_exports_only_hash_checked_controlled_images(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            self.fixture(output)
            with patch("sys.stdout", new_callable=io.StringIO) as stream:
                capture.emit_review_data(output)
            records = stream.getvalue().splitlines()
            self.assertEqual(len(records), 5)
            self.assertTrue(records[0].startswith('MOBILE_UI_MANIFEST '))
            self.assertTrue(all(line.startswith('MOBILE_UI_PNG ') for line in records[1:]))

    def test_review_transport_refuses_foreign_names_corruption_and_oversize_before_export(self):
        for refusal in ("foreign", "hash", "oversize"):
            with self.subTest(refusal=refusal), tempfile.TemporaryDirectory() as directory:
                output = Path(directory)
                manifest = self.fixture(output)
                if refusal == "foreign":
                    manifest["images"][0]["image"] = "../private.txt"
                    (output / "manifest.json").write_text(json.dumps(manifest))
                else:
                    (output / "phone-terminal.png").write_bytes(
                        b"changed" if refusal == "hash" else b"x" * (512 * 1024 + 1))
                with patch("sys.stdout", new_callable=io.StringIO) as stream:
                    with self.assertRaises(ValueError):
                        capture.emit_review_data(output)
                self.assertEqual(stream.getvalue(), "")

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
            output = Path(directory) / "new"

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
