"""Validate complete owned aging evidence without asserting noisy CPU budgets."""

import argparse
import json
import math
from pathlib import Path
import re
import statistics

from PIL import Image

from compare_retained import digest

STATES = ("fresh", "churned", "rebuilt")
MODES = ("frozen", "active", "background", "idle")
TIMING_TOLERANCE_MS = 0.001
REGISTRIES = (
    "surfaces", "adapters", "devices", "queues", "pipeline_layouts", "shader_modules",
    "bind_group_layouts", "bind_groups", "command_encoders", "command_buffers",
    "render_bundles", "render_pipelines", "compute_pipelines", "pipeline_caches",
    "query_sets", "buffers", "textures", "texture_views", "external_textures",
    "samplers", "render_passes", "compute_passes", "render_bundle_encoders",
)
TEARDOWN_POINTS = (
    "renderer-dropped", "fixture-dropped",
    "rebuilt-renderer-dropped", "rebuilt-fixture-dropped",
)
RESOURCE_FIELDS = (
    "working_set_bytes", "private_bytes", "peak_working_set_bytes", "handles", "thread_count",
)
COMMITMENT_FIELDS = ("commitment_bytes", "peak_commitment_bytes")


def check_registry_report(registries):
    require(type(registries) is dict and set(registries) == set(REGISTRIES), "incomplete wgpu registry report")
    for registry in registries.values():
        require(
            type(registry) is dict and set(registry) == {
                "num_allocated", "num_kept_from_user", "num_released_from_user", "element_size",
            },
            "unsupported registry fields",
        )
        require(
            all(type(value) is int and value >= 0 for value in registry.values()),
            "registry counters must be nonnegative integers",
        )
        require(registry["element_size"] > 0, "registry element size must be positive")


def check_window(window, idle_seconds):
    require(
        type(window) is dict
        and {"started_unix_ms", "completed_unix_ms", "wall_seconds"}.issubset(window),
        "incomplete registry observation window",
    )
    started, completed = window["started_unix_ms"], window["completed_unix_ms"]
    require(
        type(started) is int and type(completed) is int and 0 <= started <= completed,
        "invalid registry observation window",
    )
    elapsed = number(window["wall_seconds"], "registry observation duration")
    require(elapsed >= idle_seconds, "truncated registry observation window")
    require(
        math.isclose(elapsed, (completed - started) / 1000, rel_tol=0, abs_tol=0.002),
        "inconsistent registry observation clocks",
    )


def check_registries(records, cycles, idle_seconds):
    churn_points = sorted(set(range(20, cycles + 1, 20)) | {cycles})
    expected = (
        ["fresh"] + [f"churn-{cycle}" for cycle in churn_points]
        + ["churned", *TEARDOWN_POINTS[:2], "rebuilt", *TEARDOWN_POINTS[2:]]
    )
    require(type(records) is list and len(records) == len(expected), "incomplete registry checkpoints")
    for record, name in zip(records, expected):
        require(record["name"] == name, "unordered registry checkpoint")
        check_registry_report(record["registries"])
        if name in TEARDOWN_POINTS:
            check_window(record["window"], idle_seconds)
    return records


def require(condition, message):
    if not condition:
        raise ValueError(message)


def number(value, name):
    require(
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(value)
        and value >= 0,
        f"{name} must be a finite nonnegative number",
    )
    return value


def distribution(values):
    return (
        {"minimum": min(values), "median": statistics.median(values), "maximum": max(values)}
        if values
        else None
    )


def check_phase(summary, samples, frames, idle_seconds, resource_schema=None):
    require(resource_schema is None or type(resource_schema) is int and resource_schema == 1, "unsupported frame resource schema")
    require(summary["schema"] == 1, "unsupported phase schema")
    mode = summary["mode"]
    require(mode in MODES, "unknown phase mode")
    require(summary["completed_frames"] == len(samples), "missing/extra completed frames")
    require(summary["event_driven"] == (mode == "idle"), "incorrect demand classification")
    elapsed = number(summary["wall_seconds"], "phase wall seconds")
    require(elapsed > 0, "empty phase")
    cpu = number(summary["process_cpu_ms"], "phase CPU")
    hz = number(summary["completed_hz"], "completed cadence")
    require(math.isclose(hz, len(samples) / elapsed), "incorrect completed cadence")
    if samples:
        mean = number(summary["cpu_ms_per_completed_frame"], "completed-frame CPU")
        require(math.isclose(mean, cpu / len(samples)), "incorrect completed-frame CPU")
    else:
        require(summary["cpu_ms_per_completed_frame"] is None, "empty idle CPU is not per-frame evidence")
    for field in ("immediate_repaint_callbacks", "delayed_repaint_callbacks"):
        number(summary[field], field)
    require(summary["pending_events"] == [0] * 6, "undrained synthetic events")
    if mode == "idle":
        require(elapsed >= idle_seconds, "truncated idle window")
        require(summary["requested_cadence_ms"] is None, "idle cannot be forcibly paced")
    else:
        require(len(samples) == frames, "truncated paced phase")
        require(summary["requested_cadence_ms"] == 100, "changed controlled cadence")
        require(
            elapsed * 1000 + TIMING_TOLERANCE_MS >= frames * summary["requested_cadence_ms"],
            "truncated declared paced window",
        )
    expected = {"active": 1, "background": 5, "frozen": 0, "idle": 0}[mode]
    require(summary["supplied_events"] == len(samples) * expected, "wrong phase event count")
    previous_completion_ms = 0
    for index, sample in enumerate(samples):
        require(sample["index"] == index, "out-of-order/duplicate frame")
        require(sample["supplied_events"] == expected, "wrong per-frame event count")
        for field in ("elapsed_ms", "work_ms", "cpu_ms", "dirty_rows"):
            number(sample[field], field)
        completion_ms = sample["elapsed_ms"]
        require(
            previous_completion_ms <= completion_ms + TIMING_TOLERANCE_MS,
            "reversed frame completion timestamps",
        )
        require(
            completion_ms <= elapsed * 1000 + TIMING_TOLERANCE_MS,
            "frame completion exceeds phase window",
        )
        if mode != "idle":
            require(
                completion_ms + TIMING_TOLERANCE_MS >= index * summary["requested_cadence_ms"],
                "frame completed before its declared paced deadline",
            )
        previous_completion_ms = completion_ms
        rendering = sample["rendering"]
        if resource_schema == 1:
            require(rendering.get("wgpu_submission_completed") is True, "missing/incomplete GPU submission observation")
        else:
            require("wgpu_submission_completed" not in rendering, "undeclared GPU submission observation")
        require(rendering["native_calls"] == 1, "ordinary fallback is not admitted evidence")
        require(rendering["host_copy"] is True, "native copy declined")
        updated = number(rendering["updated_pixels"], "native updated pixels")
        surface = number(rendering["surface_pixels"], "native surface pixels")
        require(0 < surface and updated <= surface, "invalid native damage")
        for field in (
            "retained_texture_bytes", "retained_signature_bytes", "font_atlas_bytes",
            "font_atlas_cloned_bytes", "uploaded_textures",
        ):
            number(rendering[field], field)
        for field in ("retained_reused", "retained_rebuilt"):
            require(type(rendering[field]) is bool, f"{field} must be a boolean")
        require(
            not (rendering["retained_reused"] and rendering["retained_rebuilt"]),
            "a frame cannot both reuse and rebuild retention",
        )
    return {
        "summary": summary,
        "cpu_ms_per_frame": distribution([sample["cpu_ms"] for sample in samples]),
        "work_ms_per_frame": distribution([sample["work_ms"] for sample in samples]),
        "updated_pixels": distribution([sample["rendering"]["updated_pixels"] for sample in samples]),
        "retained_texture_bytes": distribution([
            sample["rendering"]["retained_texture_bytes"] for sample in samples
        ]),
        "retained_signature_bytes": distribution([
            sample["rendering"]["retained_signature_bytes"] for sample in samples
        ]),
        "font_atlas_bytes": distribution([sample["rendering"]["font_atlas_bytes"] for sample in samples]),
        "font_atlas_cloned_bytes": sum(sample["rendering"]["font_atlas_cloned_bytes"] for sample in samples),
        "uploaded_textures": sum(sample["rendering"]["uploaded_textures"] for sample in samples),
        "retained_reused_frames": sum(sample["rendering"]["retained_reused"] for sample in samples),
        "retained_rebuilt_frames": sum(sample["rendering"]["retained_rebuilt"] for sample in samples),
    }


def check_process_memory(binding, manifest, resources, probe):
    declared = "process_memory_schema" in binding or "process_memory_schema" in manifest
    receipt_path = probe / "sampled.json"
    if not declared:
        require("process_memory_method" not in binding, "undeclared process memory method")
        require(not receipt_path.exists(), "undeclared final process memory receipt")
        require(
            all(not set(COMMITMENT_FIELDS).intersection(sample) for sample in resources),
            "undeclared commitment observations",
        )
        return None
    require(
        all(type(record.get("process_memory_schema")) is int and record["process_memory_schema"] == 1
            for record in (binding, manifest)),
        "incomplete/unsupported process memory declaration",
    )
    require(
        binding.get("process_memory_method") == "GetProcessMemoryInfo.PagefileUsage/PeakPagefileUsage",
        "unsupported process commitment method",
    )
    previous_peak = previous_time = 0
    for sample in resources:
        require(
            all(type(sample.get(field)) is int and sample[field] >= 0 for field in COMMITMENT_FIELDS),
            "incomplete/invalid process commitment counters",
        )
        require(
            sample["peak_commitment_bytes"] >= max(previous_peak, sample["commitment_bytes"]),
            "inconsistent process-lifetime commitment peak",
        )
        require(
            type(sample["unix_ms"]) is int and sample["unix_ms"] >= previous_time,
            "unordered commitment observations",
        )
        previous_peak, previous_time = sample["peak_commitment_bytes"], sample["unix_ms"]
    require(receipt_path.exists(), "missing final process memory receipt")
    receipt = json.loads(receipt_path.read_text(encoding="utf-8-sig"))
    require(
        receipt == resources[-1] and receipt["phase"] == "complete",
        "final commitment receipt does not match the last completed process observation",
    )
    sampled_maximum = max(sample["commitment_bytes"] for sample in resources)
    return {
        "lifetime_peak_commitment_bytes": previous_peak,
        "maximum_sampled_commitment_bytes": sampled_maximum,
        "peak_above_sampled_commitment_bytes": previous_peak - sampled_maximum,
        "final_observation": receipt,
        "classification": "OS-maintained process-lifetime high-water mark, not a per-phase peak, live cache size or GPU-byte counter",
    }


def window_resources(resources, name, window, fields, require_sample=False):
    selected = [
        sample for sample in resources
        if sample["phase"] == name
        and window["started_unix_ms"] <= sample["unix_ms"] <= window["completed_unix_ms"]
    ]
    require(not require_sample or selected, f"missing process samples in {name} held window")
    return {
        "sample_count": len(selected),
        **{field: distribution([number(sample[field], field) for sample in selected]) for field in fields},
    }


def check_lifecycles(records, repeats, idle_seconds):
    require(type(records) is list and len(records) == repeats, "incomplete whole-owner lifecycle observations")
    previous_end = 0
    for index, record in enumerate(records):
        require(type(record.get("index")) is int and record["index"] == index, "unordered lifecycle observation")
        require(
            type(record.get("churn_cycles")) is int and record["churn_cycles"] == 2
            and type(record.get("churn_submitted_frames")) is int and record["churn_submitted_frames"] == 12,
            "incomplete lifecycle churn",
        )
        for field in ("reporting_instance_dropped", "repaint_owner_released", "wgpu_submission_completed"):
            require(record.get(field) is True, f"incomplete lifecycle {field}")
        require(
            type(record.get("temporary_oracle_rgba_bytes")) is int
            and record["temporary_oracle_rgba_bytes"] == 2 * 2058 * 1658 * 4,
            "incorrect temporary CPU pixel oracle accounting",
        )
        check_registry_report(record.get("live_registries"))
        check_registry_report(record.get("drained_registries"))
        check_window(record.get("window"), idle_seconds)
        require(record["window"]["started_unix_ms"] >= previous_end, "overlapping lifecycle windows")
        previous_end = record["window"]["completed_unix_ms"]
    return records


def validate(directory):
    probe = directory / "probe"
    binding = json.loads((directory / "source.json").read_text(encoding="utf-8-sig"))
    require(binding["schema"] == 1 and binding["exit_code"] == 0, "failed or unsupported source receipt")
    for field in ("source_head", "source_tree"):
        require(re.fullmatch(r"[0-9a-f]{40}", binding[field]) is not None, f"invalid {field}")
    require(binding["profile"] in ("debug", "release"), "unknown build profile")
    executable = directory / "festerm-aging-probe.exe"
    require(
        digest(executable).upper() == binding["executable_sha256"],
        "executable hash mismatch",
    )
    manifest = json.loads((probe / "manifest.json").read_text())
    require(manifest["schema"] == 1, "unsupported probe schema")
    require(manifest["states"] == list(STATES) and manifest["modes"] == list(MODES), "incomplete matrix")
    require(manifest["session_count"] == 6, "not a six-session fixture")
    require(manifest["normalized_pixels_equal"] is True, "pixel oracle failed")
    require(manifest["installed_sessions_accessed"] is False, "installed sessions are out of scope")
    require(manifest["production_cadence_changed"] is False, "production cadence is out of scope")
    require(manifest["physical_size"] == [2058, 1658], "changed physical client size")
    require(manifest["measurement_scale"] == 2.0, "changed measurement DPI")
    require(manifest["churn_scales"] == [1.25, 2.0], "changed churn DPI matrix")
    png_hashes = {state: digest(probe / f"{state}-normalized.png") for state in STATES}
    require(len(set(png_hashes.values())) == 1, "normalized PNG bytes differ")
    for state in STATES:
        with Image.open(probe / f"{state}-normalized.png") as image:
            require(list(image.size) == manifest["physical_size"], "incorrect oracle geometry")
    for key, maximum in (("cycles", 2000), ("frames", 1000), ("idle_seconds", 300)):
        value = binding[key]
        require(type(value) is int and 1 <= value <= maximum, f"invalid bounded {key}")
    require(manifest["churn_cycles"] == binding["cycles"], "cycle mismatch")
    require(manifest["churn_submitted_frames"] == binding["cycles"] * 6, "churn frame mismatch")
    require(manifest["frames_per_paced_phase"] == binding["frames"], "frame declaration mismatch")
    require(manifest["idle_seconds"] == binding["idle_seconds"], "idle declaration mismatch")
    expected_files = {f"{state}-{mode}.json" for state in STATES for mode in MODES}
    require(
        {path.name for path in probe.glob("*-*.json")} == expected_files,
        "missing or unexpected phase summary",
    )
    require(
        {path.name for path in probe.glob("*.jsonl")} == {name + "l" for name in expected_files},
        "missing or unexpected frame log",
    )
    phases = {}
    frame_schema = manifest.get("frame_resource_schema")
    require(
        "frame_resource_schema" not in manifest or type(frame_schema) is int and frame_schema == 1,
        "unsupported frame resource schema",
    )
    for state in STATES:
        for mode in MODES:
            name = f"{state}-{mode}"
            summary = json.loads((probe / f"{name}.json").read_text())
            require(summary["phase"] == name and summary["mode"] == mode, "phase identity mismatch")
            samples = [json.loads(line) for line in (probe / f"{name}.jsonl").read_text().splitlines()]
            phases[name] = check_phase(summary, samples, binding["frames"], binding["idle_seconds"], frame_schema)
    resources = [json.loads(line) for line in (directory / "resources.jsonl").read_text().splitlines()]
    require(resources, "missing process resource observations")
    pids = {sample["pid"] for sample in resources}
    require(pids == {binding["process_id"]}, "mixed process resource observations")
    for sample in resources:
        for field in ("unix_ms", "elapsed_seconds", "process_cpu_ms"):
            number(sample[field], field)
        for field in RESOURCE_FIELDS:
            number(sample[field], field)
    process_memory = check_process_memory(binding, manifest, resources, probe)
    resource_phases = {}
    fields = RESOURCE_FIELDS + (COMMITMENT_FIELDS if process_memory else ())
    for name in phases:
        selected = [sample for sample in resources if sample["phase"] == name]
        # Short smoke windows may legitimately finish between the 500ms observations.
        resource_phases[name] = {
            "sample_count": len(selected),
            **{field: distribution([number(sample[field], field) for sample in selected]) for field in fields},
        }
    result = {
        "schema": 1, "source": binding, "manifest": manifest,
        "phases": phases, "resources": resource_phases,
        "normalized_png_sha256": png_hashes,
        "limitations": "No performance budget assertion. Non-idle frames are forced/paced offscreen work. Idle follows egui demand. Sampled process resources exclude GPU-specific and in-flight allocation accounting; rebuilt GUI is not a process restart or persistent-shell experiment.",
    }
    if process_memory:
        result["process_memory"] = process_memory
        result["limitations"] += (
            " Commitment high-water marks include unsampled transient commitment but are "
            "process-lifetime values: differences show new lifetime highs, not local phase peaks."
        )
    registry_path = probe / "registries.json"
    if "registry_schema" in manifest:
        require(type(manifest["registry_schema"]) is int and manifest["registry_schema"] == 1, "unsupported registry schema")
        require(manifest["registry_interval"] == 20, "changed registry checkpoint interval")
        result["registry_observations"] = check_registries(
            json.loads(registry_path.read_text()), binding["cycles"], binding["idle_seconds"],
        )
        result["teardown_resources"] = {}
        for name in TEARDOWN_POINTS:
            window = next(
                record["window"] for record in result["registry_observations"] if record["name"] == name
            )
            result["teardown_resources"][name] = window_resources(resources, name, window, fields)
        result["limitations"] += (
            " Registry reports count public wgpu IDs/vacant slots, not complete native, "
            "queued/in-flight allocations or GPU bytes; teardown retains the reporting instance."
        )
    else:
        require(not registry_path.exists(), "undeclared registry observations")
    lifecycle_path = probe / "lifecycles.json"
    lifecycle_pngs = {path.name for path in probe.glob("lifecycle-*.png")}
    if "lifecycle_schema" in manifest:
        require(
            type(manifest["lifecycle_schema"]) is int and manifest["lifecycle_schema"] == 1,
            "unsupported lifecycle schema",
        )
        require(process_memory and frame_schema == 1 and manifest.get("registry_schema") == 1, "incomplete lifecycle resource declarations")
        repeats = binding.get("lifecycle_repeats")
        require(type(repeats) is int and 1 <= repeats <= 8, "invalid bounded lifecycle repeats")
        require(
            type(manifest.get("lifecycle_repeats")) is int and manifest["lifecycle_repeats"] == repeats
            and type(manifest.get("lifecycle_churn_cycles")) is int and manifest["lifecycle_churn_cycles"] == 2,
            "lifecycle declaration mismatch",
        )
        require(lifecycle_path.exists(), "missing lifecycle observations")
        result["lifecycle_observations"] = check_lifecycles(
            json.loads(lifecycle_path.read_text()), repeats, binding["idle_seconds"],
        )
        require(
            lifecycle_pngs == {f"lifecycle-{index}-normalized.png" for index in range(repeats)},
            "incomplete/unexpected lifecycle pixel oracles",
        )
        result["lifecycle_resources"] = {}
        for record in result["lifecycle_observations"]:
            name = f"lifecycle-{record['index']}-dropped"
            require(
                digest(probe / f"lifecycle-{record['index']}-normalized.png") == png_hashes["fresh"],
                "whole-owner normalized PNG bytes differ",
            )
            result["lifecycle_resources"][name] = window_resources(
                resources, name, record["window"], fields, require_sample=True,
            )
        result["limitations"] += (
            " Repeated whole-owner windows also drop the reporting instance. Temporary oracle "
            "bytes count two CPU RGBA payloads only, not spare capacity/all temporary allocations; completed "
            "submissions and public registries do not measure driver-private/in-flight bytes."
        )
    else:
        require(
            not lifecycle_path.exists() and not lifecycle_pngs
            and "lifecycle_repeats" not in binding
            and not {"lifecycle_repeats", "lifecycle_churn_cycles"}.intersection(manifest),
            "undeclared lifecycle observations",
        )
    (directory / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    result = validate(args.directory)
    print(f"Validated {len(result['phases'])} complete six-session phases.")


if __name__ == "__main__":
    main()
