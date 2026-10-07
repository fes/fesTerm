import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from PIL import Image

import check_session_aging as aging


class SessionAgingTests(unittest.TestCase):
    def owned_directory(self):
        scratch = Path(__file__).resolve().parents[2] / "target" / "session-aging-checker-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        return tempfile.TemporaryDirectory(dir=scratch)

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

    def test_declared_submission_observations_cannot_be_omitted_or_fabricated(self):
        summary, samples = self.phase()
        for sample in samples:
            sample["rendering"]["wgpu_submission_completed"] = True
        aging.check_phase(summary, samples, 2, 1, 1)
        with self.assertRaisesRegex(ValueError, "undeclared"):
            aging.check_phase(summary, samples, 2, 1)
        for value in (None, False, 1, "true"):
            with self.subTest(value=value):
                samples[0]["rendering"]["wgpu_submission_completed"] = value
                with self.assertRaisesRegex(ValueError, "submission"):
                    aging.check_phase(summary, samples, 2, 1, 1)
        samples[0]["rendering"].pop("wgpu_submission_completed")
        with self.assertRaisesRegex(ValueError, "submission"):
            aging.check_phase(summary, samples, 2, 1, 1)

    def registry_records(self, names):
        records = [
            {
                "name": name,
                "registries": {
                    registry: {
                        "num_allocated": 2, "num_kept_from_user": 2,
                        "num_released_from_user": 100, "element_size": 8,
                    }
                    for registry in aging.REGISTRIES
                },
            }
            for name in names
        ]
        for record in records:
            if record["name"] in aging.TEARDOWN_POINTS:
                record["window"] = {
                    "started_unix_ms": 1000, "completed_unix_ms": 2000, "wall_seconds": 1,
                }
        return records

    def test_registry_checkpoints_reject_missing_reversed_and_invalid_counters(self):
        records = self.registry_records([
            "fresh", "churn-20", "churn-25", "churned", *aging.TEARDOWN_POINTS[:2],
            "rebuilt", *aging.TEARDOWN_POINTS[2:],
        ])
        # Vacant IDs and retained public IDs are observations, not a memory-budget verdict.
        self.assertEqual(aging.check_registries(records, 25, 1), records)
        mutations = [
            lambda data: data.pop(),
            lambda data: data.reverse(),
            lambda data: data[0]["registries"].pop("textures"),
            lambda data: data[0]["registries"]["textures"].update(num_allocated=True),
            lambda data: data[0]["registries"]["textures"].update(num_kept_from_user=-1),
            lambda data: data[0]["registries"]["textures"].update(element_size=0),
            lambda data: data[4]["window"].update(wall_seconds=0.5),
            lambda data: data[4]["window"].update(completed_unix_ms=999),
            lambda data: data[4]["window"].update(completed_unix_ms=3000),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                malformed = copy.deepcopy(records)
                mutate(malformed)
                with self.assertRaises(ValueError):
                    aging.check_registries(malformed, 25, 1)

    def lifecycle_records(self, repeats=2):
        registries = self.registry_records(["live"])[0]["registries"]
        return [
            {
                "index": index, "churn_cycles": 2, "churn_submitted_frames": 12,
                "reporting_instance_dropped": True, "repaint_owner_released": True,
                "wgpu_submission_completed": True,
                "temporary_oracle_rgba_bytes": 2 * 2058 * 1658 * 4,
                "live_registries": copy.deepcopy(registries),
                "drained_registries": copy.deepcopy(registries),
                "window": {
                    "started_unix_ms": (index + 1) * 1000,
                    "completed_unix_ms": (index + 2) * 1000, "wall_seconds": 1,
                },
            }
            for index in range(repeats)
        ]

    def test_lifecycle_windows_require_all_owners_work_and_oracles_but_preserve_adverse_ids(self):
        records = self.lifecycle_records()
        self.assertEqual(aging.check_lifecycles(records, 2, 1), records)
        mutations = [
            lambda data: data.pop(),
            lambda data: data.reverse(),
            lambda data: data[0].update(index=True),
            lambda data: data[0].update(churn_submitted_frames=6),
            lambda data: data[0].update(reporting_instance_dropped=False),
            lambda data: data[0].update(repaint_owner_released=1),
            lambda data: data[0].update(wgpu_submission_completed=False),
            lambda data: data[0].pop("temporary_oracle_rgba_bytes"),
            lambda data: data[0].pop("drained_registries"),
            lambda data: data[0].pop("window"),
            lambda data: data[0]["live_registries"].pop("queues"),
            lambda data: data[1]["window"].update(started_unix_ms=1500, completed_unix_ms=2500),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                malformed = copy.deepcopy(records)
                mutate(malformed)
                with self.assertRaises(ValueError):
                    aging.check_lifecycles(malformed, 2, 1)

    def test_os_commitment_peak_keeps_unsampled_transients_and_requires_final_receipt(self):
        with self.owned_directory() as directory:
            probe = Path(directory)
            binding = {
                "process_memory_schema": 1,
                "process_memory_method": "GetProcessMemoryInfo.PagefileUsage/PeakPagefileUsage",
            }
            manifest = {"process_memory_schema": 1}
            resources = [
                {"pid": 7, "phase": "starting", "unix_ms": 1000, "commitment_bytes": 200, "peak_commitment_bytes": 1000},
                {"pid": 7, "phase": "complete", "unix_ms": 2000, "commitment_bytes": 100, "peak_commitment_bytes": 2000},
            ]
            receipt = probe / "sampled.json"
            receipt.write_text(json.dumps(resources[-1]))
            result = aging.check_process_memory(binding, manifest, resources, probe)
            self.assertEqual(result["lifetime_peak_commitment_bytes"], 2000)
            self.assertEqual(result["maximum_sampled_commitment_bytes"], 200)
            self.assertEqual(result["peak_above_sampled_commitment_bytes"], 1800)
            mutations = [
                lambda data: data[0].pop("commitment_bytes"),
                lambda data: data[0].update(peak_commitment_bytes=True),
                lambda data: data[1].update(peak_commitment_bytes=999),
                lambda data: data[1].update(commitment_bytes=2001),
                lambda data: data[1].update(unix_ms=999),
                lambda data: data[-1].update(phase="rebuilt-idle"),
            ]
            for mutate in mutations:
                with self.subTest(mutation=mutate):
                    malformed = copy.deepcopy(resources)
                    mutate(malformed)
                    receipt.write_text(json.dumps(malformed[-1]))
                    with self.assertRaises(ValueError):
                        aging.check_process_memory(binding, manifest, malformed, probe)
            receipt.write_text(json.dumps(resources[0]))
            with self.assertRaisesRegex(ValueError, "receipt"):
                aging.check_process_memory(binding, manifest, resources, probe)
            receipt.unlink()
            with self.assertRaisesRegex(ValueError, "missing"):
                aging.check_process_memory(binding, manifest, resources, probe)
            for changed_binding, changed_manifest in [
                ({}, manifest), (binding, {}), ({**binding, "process_memory_schema": True}, manifest),
                ({**binding, "process_memory_method": "sampled-private-bytes"}, manifest),
                ({}, {}),
            ]:
                with self.subTest(binding=changed_binding, manifest=changed_manifest), self.assertRaises(ValueError):
                    aging.check_process_memory(changed_binding, changed_manifest, resources, probe)

    def test_complete_source_bound_matrix_keeps_adverse_results(self):
        with self.owned_directory() as temporary:
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
            self.assertNotIn("registry_observations", result)
            registry_records = self.registry_records([
                "fresh", "churn-1", "churned", *aging.TEARDOWN_POINTS[:2],
                "rebuilt", *aging.TEARDOWN_POINTS[2:],
            ])
            (probe / "registries.json").write_text(json.dumps(registry_records))
            with self.assertRaisesRegex(ValueError, "undeclared"):
                aging.validate(root)
            manifest.update(registry_schema=1, registry_interval=20)
            (probe / "manifest.json").write_text(json.dumps(manifest))
            (root / "resources.jsonl").write_text(
                "".join(json.dumps(item) + "\n" for item in [
                    resource,
                    {**resource, "phase": "fixture-dropped", "unix_ms": 1500},
                    {**resource, "phase": "fixture-dropped", "unix_ms": 2500, "private_bytes": 9000},
                ])
            )
            result = aging.validate(root)
            self.assertEqual(result["registry_observations"], registry_records)
            self.assertEqual(result["teardown_resources"]["fixture-dropped"]["sample_count"], 1)
            self.assertEqual(result["teardown_resources"]["fixture-dropped"]["private_bytes"]["median"], 200)
            (probe / "registries.json").write_text(json.dumps(registry_records[:-1]))
            with self.assertRaisesRegex(ValueError, "registry"):
                aging.validate(root)
            (probe / "registries.json").write_text(json.dumps(registry_records))
            binding.update(
                process_memory_schema=1,
                process_memory_method="GetProcessMemoryInfo.PagefileUsage/PeakPagefileUsage",
                lifecycle_repeats=2,
            )
            manifest.update(
                process_memory_schema=1, frame_resource_schema=1,
                lifecycle_schema=1, lifecycle_repeats=2, lifecycle_churn_cycles=2,
            )
            for path in probe.glob("*.jsonl"):
                samples = [json.loads(line) for line in path.read_text().splitlines()]
                for sample in samples:
                    sample["rendering"]["wgpu_submission_completed"] = True
                path.write_text("".join(json.dumps(sample) + "\n" for sample in samples))
            lifecycle_records = self.lifecycle_records()
            lifecycle_path = probe / "lifecycles.json"
            lifecycle_path.write_text(json.dumps(lifecycle_records))
            for index in range(2):
                (probe / f"lifecycle-{index}-normalized.png").write_bytes((probe / "fresh-normalized.png").read_bytes())
            resources = [
                {**resource, "commitment_bytes": 200, "peak_commitment_bytes": 1000},
                {**resource, "phase": "lifecycle-0-dropped", "unix_ms": 1500, "commitment_bytes": 200, "peak_commitment_bytes": 1000},
                {**resource, "phase": "lifecycle-1-dropped", "unix_ms": 2500, "commitment_bytes": 9000, "peak_commitment_bytes": 12000},
                {**resource, "phase": "complete", "unix_ms": 3500, "commitment_bytes": 100, "peak_commitment_bytes": 12000},
            ]
            (root / "source.json").write_text(json.dumps(binding))
            (probe / "manifest.json").write_text(json.dumps(manifest))
            (root / "resources.jsonl").write_text("".join(json.dumps(item) + "\n" for item in resources))
            (probe / "sampled.json").write_text(json.dumps(resources[-1]))
            result = aging.validate(root)
            self.assertEqual(result["process_memory"]["lifetime_peak_commitment_bytes"], 12000)
            self.assertEqual(result["lifecycle_resources"]["lifecycle-1-dropped"]["commitment_bytes"]["median"], 9000)
            self.assertEqual(result["lifecycle_observations"], lifecycle_records)
            for change in ({"lifecycle_repeats": 1}, {"lifecycle_schema": True}, {"lifecycle_churn_cycles": 3}):
                (probe / "manifest.json").write_text(json.dumps({**manifest, **change}))
                with self.subTest(change=change), self.assertRaisesRegex(ValueError, "lifecycle"):
                    aging.validate(root)
            (probe / "manifest.json").write_text(json.dumps(manifest))
            for repeats in (0, 9, True):
                (root / "source.json").write_text(json.dumps({**binding, "lifecycle_repeats": repeats}))
                with self.subTest(repeats=repeats), self.assertRaisesRegex(ValueError, "bounded lifecycle"):
                    aging.validate(root)
            (root / "source.json").write_text(json.dumps(binding))
            lifecycle_path.unlink()
            with self.assertRaisesRegex(ValueError, "missing lifecycle"):
                aging.validate(root)
            lifecycle_path.write_text(json.dumps(lifecycle_records))
            (probe / "lifecycle-1-normalized.png").unlink()
            with self.assertRaisesRegex(ValueError, "pixel oracles"):
                aging.validate(root)
            (probe / "lifecycle-1-normalized.png").write_bytes((probe / "fresh-normalized.png").read_bytes())
            Image.new("RGBA", (2058, 1658), (1, 0, 0, 255)).save(probe / "lifecycle-1-normalized.png")
            with self.assertRaisesRegex(ValueError, "PNG bytes"):
                aging.validate(root)
            (probe / "lifecycle-1-normalized.png").write_bytes((probe / "fresh-normalized.png").read_bytes())
            resources[2]["unix_ms"] = 3001
            (root / "resources.jsonl").write_text("".join(json.dumps(item) + "\n" for item in resources))
            with self.assertRaisesRegex(ValueError, "missing process samples"):
                aging.validate(root)
            resources[2]["unix_ms"] = 2500
            (root / "resources.jsonl").write_text("".join(json.dumps(item) + "\n" for item in resources))
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
