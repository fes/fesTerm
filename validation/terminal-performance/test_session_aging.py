import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from PIL import Image

import check_session_aging as aging


class SessionAgingTests(unittest.TestCase):
    def phase(self, mode="active", frames=2):
        events = {"active": 1, "background": 5, "frozen": 0, "idle": 0}[mode]
        summary = {
            "schema": 1,
            "phase": f"fresh-{mode}", "mode": mode, "completed_frames": frames,
            "event_driven": mode == "idle", "wall_seconds": 1.0, "process_cpu_ms": 40,
            "pending_events": [0] * 6, "requested_cadence_ms": None if mode == "idle" else 100,
            "supplied_events": frames * events,
            "completed_hz": float(frames),
            "cpu_ms_per_completed_frame": 40 / frames if frames else None,
            "immediate_repaint_callbacks": 2, "delayed_repaint_callbacks": 0,
        }
        samples = [
            {
                "index": index, "supplied_events": events, "elapsed_ms": 100 * (index + 1),
                "work_ms": 10, "cpu_ms": 20, "dirty_rows": 2,
                "rendering": {
                    "native_calls": 1, "host_copy": True, "updated_pixels": 20,
                    "surface_pixels": 200, "retained_texture_bytes": 400,
                    "retained_signature_bytes": 80, "font_atlas_bytes": 100,
                    "font_atlas_cloned_bytes": 0, "uploaded_textures": 0,
                    "retained_reused": True, "retained_rebuilt": False,
                },
            }
            for index in range(frames)
        ]
        return summary, samples

    def test_all_modes_and_empty_event_driven_idle_are_valid(self):
        for mode in aging.MODES:
            summary, samples = self.phase(mode, 0 if mode == "idle" else 2)
            result = aging.check_phase(summary, samples, 2, 1)
            self.assertEqual(result["retained_reused_frames"], len(samples))
            self.assertEqual(result["summary"]["mode"], mode)

    def test_missing_duplicate_fallback_and_invalid_damage_are_rejected(self):
        mutations = [
            lambda summary, samples: samples.pop(),
            lambda summary, samples: samples[1].update(index=0),
            lambda summary, samples: samples[0].update(supplied_events=5),
            lambda summary, samples: samples[0]["rendering"].update(native_calls=0),
            lambda summary, samples: samples[0]["rendering"].update(host_copy=False),
            lambda summary, samples: samples[0]["rendering"].update(updated_pixels=201),
            lambda summary, samples: samples[0]["rendering"].update(surface_pixels=0),
            lambda summary, samples: samples[0].update(cpu_ms=float("nan")),
            lambda summary, samples: summary.update(pending_events=[1] + [0] * 5),
            lambda summary, samples: summary.update(requested_cadence_ms=200),
            lambda summary, samples: summary.update(wall_seconds=0),
            lambda summary, samples: summary.update(completed_hz=100),
            lambda summary, samples: summary.update(cpu_ms_per_completed_frame=100),
            lambda summary, samples: summary.update(delayed_repaint_callbacks=float("inf")),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                summary, samples = self.phase()
                mutate(summary, samples)
                with self.assertRaises(ValueError):
                    aging.check_phase(summary, samples, 2, 1)

    def test_idle_cannot_be_truncated_or_forcibly_paced(self):
        summary, samples = self.phase("idle", 0)
        for change in ({"wall_seconds": 0.5}, {"requested_cadence_ms": 100}, {"event_driven": False}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                aging.check_phase({**summary, **change}, samples, 2, 1)

    def test_impossible_reversed_and_unpaced_completion_times_are_rejected(self):
        mutations = [
            lambda summary, samples: summary.update(wall_seconds=0.001, completed_hz=2000),
            lambda summary, samples: samples[1].update(elapsed_ms=0),
            lambda summary, samples: samples[1].update(elapsed_ms=1001),
            lambda summary, samples: (
                summary.update(wall_seconds=0.15, completed_hz=2 / 0.15),
                samples[1].update(elapsed_ms=120),
            ),
            lambda summary, samples: (
                samples[0].update(elapsed_ms=50),
                samples[1].update(elapsed_ms=99),
            ),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                summary, samples = self.phase()
                mutate(summary, samples)
                with self.assertRaises(ValueError):
                    aging.check_phase(summary, samples, 2, 1)
        summary, samples = self.phase()
        summary.update(wall_seconds=0.1999995, completed_hz=2 / 0.1999995)
        aging.check_phase(summary, samples, 2, 1)

    def test_retention_outcomes_are_boolean_and_mutually_exclusive(self):
        for outcomes in [(1000, False), (True, 1), (True, True), (None, False)]:
            with self.subTest(outcomes=outcomes):
                summary, samples = self.phase()
                samples[0]["rendering"].update(
                    retained_reused=outcomes[0], retained_rebuilt=outcomes[1],
                )
                with self.assertRaises(ValueError):
                    aging.check_phase(summary, samples, 2, 1)
        summary, samples = self.phase()
        samples[0]["rendering"].update(retained_reused=False, retained_rebuilt=False)
        result = aging.check_phase(summary, samples, 2, 1)
        self.assertEqual(result["retained_reused_frames"], 1)
        self.assertEqual(result["retained_rebuilt_frames"], 0)

    def test_complete_source_bound_matrix_keeps_adverse_results(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            probe = root / "probe"
            probe.mkdir()
            binary = b"owned executable fixture"
            (root / "festerm-aging-probe.exe").write_bytes(binary)
            binding = {
                "schema": 1, "source_head": "a" * 40, "source_tree": "b" * 40,
                "profile": "debug", "exit_code": 0, "process_id": 7,
                "executable_sha256": hashlib.sha256(binary).hexdigest().upper(),
                "cycles": 1, "frames": 2, "idle_seconds": 1,
            }
            (root / "source.json").write_text(json.dumps(binding))
            manifest = {
                "schema": 1,
                "states": list(aging.STATES), "modes": list(aging.MODES), "session_count": 6,
                "normalized_pixels_equal": True, "installed_sessions_accessed": False,
                "production_cadence_changed": False, "churn_cycles": 1, "churn_submitted_frames": 6,
                "frames_per_paced_phase": 2, "idle_seconds": 1, "physical_size": [2058, 1658],
                "measurement_scale": 2.0, "churn_scales": [1.25, 2.0],
            }
            (probe / "manifest.json").write_text(json.dumps(manifest))
            for state in aging.STATES:
                Image.new("RGBA", (2058, 1658), (0, 0, 0, 255)).save(probe / f"{state}-normalized.png")
                for mode in aging.MODES:
                    summary, samples = self.phase(mode, 0 if mode == "idle" else 2)
                    summary["phase"] = f"{state}-{mode}"
                    if state == "churned":
                        summary["process_cpu_ms"] = 500
                        summary["cpu_ms_per_completed_frame"] = 500 / len(samples) if samples else None
                    (probe / f"{state}-{mode}.json").write_text(json.dumps(summary))
                    (probe / f"{state}-{mode}.jsonl").write_text(
                        "".join(json.dumps(sample) + "\n" for sample in samples)
                    )
            resource = {
                "pid": 7, "phase": "fresh-active", "unix_ms": 1000, "elapsed_seconds": 1,
                "process_cpu_ms": 40, "working_set_bytes": 100, "private_bytes": 200,
                "peak_working_set_bytes": 300, "handles": 40, "thread_count": 5,
            }
            (root / "resources.jsonl").write_text(json.dumps(resource) + "\n")
            result = aging.validate(root)
            self.assertEqual(len(result["phases"]), 12)
            self.assertEqual(result["phases"]["churned-active"]["summary"]["process_cpu_ms"], 500)
            self.assertEqual(result["resources"]["rebuilt-idle"]["sample_count"], 0)
            for change in ({"exit_code": 101}, {"source_head": "not-a-head"}, {"process_id": 8}, {"profile": "unknown"}):
                with self.subTest(change=change):
                    (root / "source.json").write_text(json.dumps({**binding, **change}))
                    with self.assertRaises(ValueError):
                        aging.validate(root)
            (root / "source.json").write_text(json.dumps(binding))
            (root / "festerm-aging-probe.exe").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "hash"):
                aging.validate(root)
            (root / "festerm-aging-probe.exe").write_bytes(binary)
            (probe / "rebuilt-active.json").unlink()
            with self.assertRaisesRegex(ValueError, "phase"):
                aging.validate(root)


if __name__ == "__main__":
    unittest.main()
