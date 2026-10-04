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


def check_registries(records, cycles):
    churn_points = sorted(set(range(20, cycles + 1, 20)) | {cycles})
    expected = (
        ["fresh"] + [f"churn-{cycle}" for cycle in churn_points]
        + ["churned", *TEARDOWN_POINTS[:2], "rebuilt", *TEARDOWN_POINTS[2:]]
    )
    require(type(records) is list and len(records) == len(expected), "incomplete registry checkpoints")
    for record, name in zip(records, expected):
        require(record["name"] == name, "unordered registry checkpoint")
        registries = record["registries"]
        require(set(registries) == set(REGISTRIES), "incomplete wgpu registry report")
        for registry in registries.values():
            require(
                set(registry) == {
                    "num_allocated", "num_kept_from_user", "num_released_from_user", "element_size",
                },
                "unsupported registry fields",
            )
            require(
                all(type(value) is int and value >= 0 for value in registry.values()),
                "registry counters must be nonnegative integers",
            )
            require(registry["element_size"] > 0, "registry element size must be positive")
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


def check_phase(summary, samples, frames, idle_seconds):
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
    for state in STATES:
        for mode in MODES:
            name = f"{state}-{mode}"
            summary = json.loads((probe / f"{name}.json").read_text())
            require(summary["phase"] == name and summary["mode"] == mode, "phase identity mismatch")
            samples = [json.loads(line) for line in (probe / f"{name}.jsonl").read_text().splitlines()]
            phases[name] = check_phase(summary, samples, binding["frames"], binding["idle_seconds"])
    resources = [json.loads(line) for line in (directory / "resources.jsonl").read_text().splitlines()]
    require(resources, "missing process resource observations")
    pids = {sample["pid"] for sample in resources}
    require(pids == {binding["process_id"]}, "mixed process resource observations")
    for sample in resources:
        for field in ("unix_ms", "elapsed_seconds", "process_cpu_ms"):
            number(sample[field], field)
    resource_phases = {}
    fields = ("working_set_bytes", "private_bytes", "peak_working_set_bytes", "handles", "thread_count")
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
    registry_path = probe / "registries.json"
    if "registry_schema" in manifest:
        require(manifest["registry_schema"] == 1, "unsupported registry schema")
        require(manifest["registry_interval"] == 20, "changed registry checkpoint interval")
        result["registry_observations"] = check_registries(
            json.loads(registry_path.read_text()), binding["cycles"],
        )
        result["teardown_resources"] = {}
        for name in TEARDOWN_POINTS:
            selected = [sample for sample in resources if sample["phase"] == name]
            result["teardown_resources"][name] = {
                "sample_count": len(selected),
                **{
                    field: distribution([number(sample[field], field) for sample in selected])
                    for field in fields
                },
            }
        result["limitations"] += (
            " Registry reports count public wgpu IDs/vacant slots, not complete native, "
            "queued/in-flight allocations or GPU bytes; teardown retains the reporting instance."
        )
    else:
        require(not registry_path.exists(), "undeclared registry observations")
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
