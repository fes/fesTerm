from datetime import datetime, timedelta, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from PIL import Image

import compare_retained as comparison


class RetainedComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.evidence = self.root / "evidence"
        self.evidence.mkdir()
        self.sha = "a" * 64
        comparison.write_json(self.evidence / "manifest.json", {
            "schema_version": 1, "executable": "declared-probe.exe",
            "executable_sha256": self.sha, "source_label": "synthetic fixture",
        })
        self.image = io.BytesIO()
        Image.new("RGBA", (120, 40), (12, 34, 56, 255)).save(self.image, format="PNG")
        for index, (name, mode) in enumerate(comparison.RUNS):
            folder = self.evidence / name
            folder.mkdir()
            (folder / "original.png").write_bytes(self.image.getvalue())
            started = datetime(2026, 1, 1, tzinfo=timezone.utc) + timedelta(minutes=index)
            comparison.write_json(self.evidence / f"{name}.metadata.json", {
                "mode": mode, "executable_sha256": self.sha, "source_label": "synthetic fixture",
                "reference_run": comparison.RUNS[0][0] if index else None,
                "exit_code": 0, "verified": True,
                "started_utc": started.isoformat(),
                "finished_utc": (started + timedelta(seconds=45)).isoformat(),
            })
            (self.evidence / f"{name}.stdout.log").write_text("test result: ok. 1 passed; 0 failed;\n", encoding="utf-8")
            report = {
                "adapter": "device_type: Cpu, backend: Dx12",
                "scene": "application", "grid": [120, 40], "physical_size": [120, 40],
                "pixels_per_point": 2, "target_format": "Bgra8Unorm",
                "logical_processors": 16, "interval_ms": 100,
                "primitives": [{"kind": "callback", "native_image": True}],
                "removed_fill_triangles": 1, "host_copy_probe": True,
                "direct_copy_probe": False, "retained_composition_probe": mode == "on",
                "exact_sampled_pixels": True, "exact_interpolated_pixels": True,
                "measurements": [],
            }
            for case in comparison.CASES:
                composed = mode == "on" and case != "localized-without-composition"
                reused = (93 if case == "localized-all" else 100) if composed else 0
                cpu = 20 if composed else 40
                report["measurements"].append({
                    "case": case, "frames": 100, "cpu_ms": cpu * 100,
                    "cpu_ms_per_frame": cpu, "cpu_percent": cpu / 16,
                    "wall_ms": 10000, "frames_per_second": 10,
                    "completed_draw_ms_per_frame": cpu,
                    "retained_prefix": {
                        "decline_reason": None, "reused_frames": reused,
                        "rebuilt_frames": 100 - reused if composed else 0,
                        "texture_bytes": 120 * 40 * 4 if composed else 0,
                        "signature_bytes": 100 if composed else 0,
                    },
                })
            comparison.write_json(folder / "profile.json", report)

    def change_report(self, action, name="abba-02-on"):
        path = self.evidence / name / "profile.json"
        value = comparison.read_json(path)
        action(value)
        path.write_text(json.dumps(value), encoding="utf-8")

    def test_digest_covers_more_than_one_chunk(self):
        content = bytes(range(256)) * 4097
        path = self.root / "digest-input"
        path.write_bytes(content)
        self.assertEqual(comparison.digest(path), hashlib.sha256(content).hexdigest())

    def test_complete_series_preserves_samples_and_adverse_controls(self):
        for name, mode in comparison.RUNS:
            if mode == "on":
                def adverse(report):
                    item = report["measurements"][2]
                    item["cpu_ms"] = 5000
                    item["cpu_ms_per_frame"] = 50
                    item["cpu_percent"] = 50 / 16
                self.change_report(adverse, name)
        summary = comparison.summarize(self.evidence)
        self.assertEqual(summary["cases"]["localized-all"]["change_percent"], -50)
        self.assertEqual(summary["cases"]["localized-without-composition"]["change_percent"], 25)
        self.assertEqual(summary["cases"]["localized-all"]["on"]["cpu_ms_per_frame"], [20] * 4)
        self.assertEqual(len(summary["runs"]), 8)

    def test_missing_run_cannot_be_aggregated(self):
        (self.evidence / "baab-04-on.metadata.json").unlink()
        with self.assertRaises(FileNotFoundError):
            comparison.summarize(self.evidence)

    def test_failed_process_and_mixed_provenance_are_rejected(self):
        path = self.evidence / "abba-02-on.metadata.json"
        original = comparison.read_json(path)
        for key, value in (("exit_code", 101), ("verified", False), ("executable_sha256", "b" * 64), ("source_label", "other"), ("reference_run", None)):
            with self.subTest(key=key):
                path.write_text(json.dumps({**original, key: value}), encoding="utf-8")
                with self.assertRaises(ValueError):
                    comparison.summarize(self.evidence)

    def test_guarded_measurement_invariants(self):
        path = self.evidence / "abba-02-on" / "profile.json"
        original = path.read_text(encoding="utf-8")
        mutations = (
            lambda r: r.update(host_copy_probe=False),
            lambda r: r.update(retained_composition_probe=False),
            lambda r: r.update(exact_sampled_pixels=False),
            lambda r: r.update(physical_size=[121, 40]),
            lambda r: r["primitives"].append({"kind": "mesh"}),
            lambda r: r["measurements"].pop(),
            lambda r: r["measurements"][1].update(frames=99),
            lambda r: r["measurements"][1].update(cpu_ms_per_frame=float("nan")),
            lambda r: r["measurements"][1].update(cpu_percent=100),
            lambda r: r["measurements"][1].update(frames_per_second=9),
            lambda r: r["measurements"][1].update(wall_ms=0),
            lambda r: r["measurements"][1]["retained_prefix"].update(reused_frames=0),
            lambda r: r["measurements"][1]["retained_prefix"].update(texture_bytes=comparison.MAX_TEXTURE_BYTES + 1),
            lambda r: r["measurements"][1]["retained_prefix"].update(signature_bytes=comparison.MAX_SIGNATURE_BYTES + 1),
            lambda r: r["measurements"][2]["retained_prefix"].update(reused_frames=1),
        )
        for index, mutation in enumerate(mutations):
            with self.subTest(index=index):
                path.write_text(original, encoding="utf-8")
                self.change_report(mutation)
                with self.assertRaises(ValueError):
                    comparison.summarize(self.evidence)

    def test_changed_png_is_rejected(self):
        path = self.evidence / "abba-02-on" / "original.png"
        Image.new("RGBA", (120, 40), (12, 34, 57, 255)).save(path)
        with self.assertRaisesRegex(ValueError, "PNG bytes"):
            comparison.summarize(self.evidence)

    def test_overlapping_processes_are_rejected(self):
        path = self.evidence / "abba-02-on.metadata.json"
        metadata = comparison.read_json(path)
        metadata["started_utc"] = "2026-01-01T00:00:01+00:00"
        path.write_text(json.dumps(metadata), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "overlapping"):
            comparison.summarize(self.evidence)

    def test_failure_stops_without_retry_and_preserves_logs(self):
        probe = self.root / "probe.exe"
        probe.write_bytes(b"not executed by this test")
        destination = self.root / "failed-run"
        with patch.dict(os.environ, {"FESTERM_RUN_OPTIONAL_VALIDATION": "1"}), \
                patch.object(comparison.platform, "system", return_value="Windows"), \
                patch.object(comparison.platform, "machine", return_value="AMD64"), \
                patch.object(comparison.subprocess, "run", return_value=subprocess.CompletedProcess([], 101)) as execute:
            with self.assertRaisesRegex(ValueError, "without retry"):
                comparison.run_comparison(probe, destination, "fixture")
        self.assertEqual(execute.call_count, 1)
        self.assertEqual(comparison.read_json(destination / "abba-01-off.metadata.json")["exit_code"], 101)
        self.assertTrue((destination / "abba-01-off.stdout.log").exists())
        self.assertFalse((destination / "abba-02-on.metadata.json").exists())
        self.assertFalse((destination / "summary.json").exists())

    def test_runner_uses_both_orders_and_one_reference_with_no_override_leakage(self):
        probe = self.root / "probe.exe"
        probe.write_bytes(b"not executed by this test")
        destination = self.root / "successful-run"
        observed = []

        def execute(arguments, **kwargs):
            environment = dict(kwargs["env"])
            output = Path(environment["FESTERM_TUI_PROFILE_OUT"])
            name = output.name
            output.mkdir()
            (output / "original.png").write_bytes(self.image.getvalue())
            comparison.write_json(
                output / "profile.json",
                comparison.read_json(self.evidence / name / "profile.json"),
            )
            kwargs["stdout"].write(b"test result: ok. 1 passed; 0 failed;\n")
            observed.append((arguments, environment))
            return subprocess.CompletedProcess(arguments, 0)

        with patch.dict(os.environ, {
            "FESTERM_RUN_OPTIONAL_VALIDATION": "1",
            "FESTERM_TUI_PROFILE_COPY": "1",
            "FESTERM_TUI_PROFILE_SAMPLER": "nearest",
            "FESTERM_TUI_PROFILE_REFERENCE": "unrelated",
        }), patch.object(comparison.platform, "system", return_value="Windows"), \
                patch.object(comparison.platform, "machine", return_value="AMD64"), \
                patch.object(comparison.subprocess, "run", side_effect=execute):
            summary = comparison.run_comparison(probe, destination, "fixture")
        self.assertEqual(len(summary["runs"]), 8)
        self.assertTrue((destination / "summary.json").exists())
        for index, ((arguments, environment), (name, mode)) in enumerate(zip(observed, comparison.RUNS)):
            self.assertEqual(arguments[1:], [comparison.TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            self.assertEqual(Path(environment["FESTERM_TUI_PROFILE_OUT"]).name, name)
            self.assertEqual(environment["FESTERM_EXPERIMENTAL_HOST_COPY"], "1")
            self.assertEqual(environment["FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION"], "1" if mode == "on" else "0")
            self.assertEqual(environment["FESTERM_TUI_PROFILE_SCENE"], "application")
            self.assertEqual(environment["FESTERM_TUI_PROFILE_CASES"], ",".join(comparison.CASES))
            self.assertNotIn("FESTERM_TUI_PROFILE_COPY", environment)
            self.assertNotIn("FESTERM_TUI_PROFILE_SAMPLER", environment)
            reference = str((destination / comparison.RUNS[0][0] / "original.png").resolve()) if index else None
            self.assertEqual(environment.get("FESTERM_TUI_PROFILE_REFERENCE"), reference)

    def test_signature_budget_is_inclusive(self):
        self.change_report(lambda r: r["measurements"][1]["retained_prefix"].update(
            signature_bytes=comparison.MAX_SIGNATURE_BYTES,
        ))
        self.assertEqual(len(comparison.summarize(self.evidence)["runs"]), 8)

    def test_changed_executable_cannot_be_verified_despite_successful_exit(self):
        probe = self.root / "probe.exe"
        probe.write_bytes(b"original")
        destination = self.root / "changed-probe"

        def replace_probe(arguments, **kwargs):
            probe.write_bytes(b"changed")
            return subprocess.CompletedProcess(arguments, 0)

        with patch.dict(os.environ, {"FESTERM_RUN_OPTIONAL_VALIDATION": "1"}), \
                patch.object(comparison.platform, "system", return_value="Windows"), \
                patch.object(comparison.platform, "machine", return_value="AMD64"), \
                patch.object(comparison.subprocess, "run", side_effect=replace_probe) as execute:
            with self.assertRaisesRegex(ValueError, "executable changed"):
                comparison.run_comparison(probe, destination, "fixture")
        metadata = comparison.read_json(destination / "abba-01-off.metadata.json")
        self.assertEqual(metadata["exit_code"], 0)
        self.assertFalse(metadata["verified"])
        self.assertIn("executable changed", metadata["error"])
        self.assertEqual(execute.call_count, 1)

    def test_timeout_preserves_failure_without_retry(self):
        probe = self.root / "probe.exe"
        probe.write_bytes(b"not executed by this test")
        destination = self.root / "timeout"
        with patch.dict(os.environ, {"FESTERM_RUN_OPTIONAL_VALIDATION": "1"}), \
                patch.object(comparison.platform, "system", return_value="Windows"), \
                patch.object(comparison.platform, "machine", return_value="AMD64"), \
                patch.object(comparison.subprocess, "run", side_effect=subprocess.TimeoutExpired("probe", 180)) as execute:
            with self.assertRaises(subprocess.TimeoutExpired):
                comparison.run_comparison(probe, destination, "fixture")
        metadata = comparison.read_json(destination / "abba-01-off.metadata.json")
        self.assertIsNone(metadata["exit_code"])
        self.assertFalse(metadata["verified"])
        self.assertIn("timed out", metadata["error"])
        self.assertEqual(execute.call_count, 1)

    def test_existing_evidence_and_missing_opt_in_do_not_start_processes(self):
        probe = self.root / "probe.exe"
        probe.write_bytes(b"not executed by this test")
        with patch.object(comparison.platform, "system", return_value="Windows"), \
                patch.object(comparison.platform, "machine", return_value="AMD64"), \
                patch.object(comparison.subprocess, "run") as execute:
            with patch.dict(os.environ, {}, clear=True):
                with self.assertRaises(ValueError):
                    comparison.run_comparison(probe, self.root / "new", "fixture")
            with patch.dict(os.environ, {"FESTERM_RUN_OPTIONAL_VALIDATION": "1"}):
                with self.assertRaises(ValueError):
                    comparison.run_comparison(probe, self.evidence, "fixture")
        execute.assert_not_called()


if __name__ == "__main__":
    unittest.main()
