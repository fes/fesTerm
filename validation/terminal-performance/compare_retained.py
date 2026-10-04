"""Run or verify a balanced, completed-work retained-prefix comparison."""

import argparse
from datetime import datetime, timezone
import hashlib
import io
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys

from PIL import Image


CASES = (
    "frozen-all",
    "localized-all",
    "localized-without-composition",
    "frozen-all-repeat",
)
RUNS = tuple(
    (f"{order}-{index:02}-{mode}", mode)
    for order, modes in (
        ("abba", ("off", "on", "on", "off")),
        ("baab", ("on", "off", "off", "on")),
    )
    for index, mode in enumerate(modes, 1)
)
STABLE_FIELDS = (
    "adapter", "grid", "physical_size", "pixels_per_point", "target_format",
    "logical_processors", "interval_ms", "primitives", "removed_fill_triangles",
)
MAX_TEXTURE_BYTES = 64 * 1024 * 1024
MAX_SIGNATURE_BYTES = 1024 * 1024
TEST = "direct2d::profile::profile_terminal_residual_cpu"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, allow_nan=False)
        stream.write("\n")


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(chunk)
    return checksum.hexdigest()


def finite_number(value):
    return type(value) in (int, float) and math.isfinite(value)


def summarize(directory):
    manifest = read_json(directory / "manifest.json")
    require(manifest["schema_version"] == 1, "Unsupported comparison manifest")
    expected_sha = manifest["executable_sha256"]
    require(
        isinstance(expected_sha, str)
        and len(expected_sha) == 64
        and all(c in "0123456789abcdef" for c in expected_sha),
        "Invalid executable SHA256",
    )
    require(
        isinstance(manifest["source_label"], str) and manifest["source_label"].strip(),
        "Missing declared source label",
    )
    reference = None
    invariants = None
    runs = []
    previous_finish = None
    for name, mode in RUNS:
        metadata = read_json(directory / f"{name}.metadata.json")
        report = read_json(directory / name / "profile.json")
        stdout = (directory / f"{name}.stdout.log").read_text(encoding="utf-8")
        require(metadata["exit_code"] == 0, f"{name}: probe did not succeed")
        require(metadata["verified"] is True, f"{name}: process or executable verification failed")
        require("1 passed; 0 failed;" in stdout, f"{name}: missing successful test result")
        require(metadata["mode"] == mode, f"{name}: wrong comparison mode")
        require(metadata["executable_sha256"] == expected_sha, f"{name}: mixed executables")
        require(metadata["source_label"] == manifest["source_label"], f"{name}: mixed sources")
        expected_reference = None if not runs else RUNS[0][0]
        require(metadata["reference_run"] == expected_reference, f"{name}: wrong reference")
        started = datetime.fromisoformat(metadata["started_utc"])
        finished = datetime.fromisoformat(metadata["finished_utc"])
        require(
            started.tzinfo is not None and finished.tzinfo is not None
            and started <= finished and (previous_finish is None or previous_finish <= started),
            f"{name}: overlapping or reversed process intervals",
        )
        previous_finish = finished
        require(report["scene"] == "application", f"{name}: wrong scene")
        require(report["grid"] == [120, 40], f"{name}: wrong grid")
        require(report["interval_ms"] == 100, f"{name}: wrong requested cadence")
        require(
            type(report["logical_processors"]) is int and report["logical_processors"] > 0,
            f"{name}: invalid CPU capacity",
        )
        require(
            "device_type: Cpu" in report["adapter"] and "backend: Dx12" in report["adapter"],
            f"{name}: not DX12/WARP",
        )
        require(report["target_format"] == "Bgra8Unorm", f"{name}: wrong target format")
        require(report["host_copy_probe"] is True, f"{name}: host-copy must stay enabled")
        require(report["direct_copy_probe"] is False, f"{name}: diagnostic copy is enabled")
        require(
            report["retained_composition_probe"] is (mode == "on"),
            f"{name}: wrong retention flag",
        )
        require(
            report["exact_sampled_pixels"] is True and report["exact_interpolated_pixels"] is True,
            f"{name}: pixel oracle failed",
        )
        current = {key: report[key] for key in STABLE_FIELDS}
        image = (directory / name / "original.png").read_bytes()
        if reference is None:
            invariants = current
            reference = image
            with Image.open(io.BytesIO(image)) as decoded:
                require(decoded.format == "PNG" and decoded.mode == "RGBA", f"{name}: not an RGBA PNG")
                require(list(decoded.size) == report["physical_size"], f"{name}: wrong PNG size")
                require(
                    0 < decoded.width * decoded.height * 4 <= MAX_TEXTURE_BYTES,
                    f"{name}: image exceeds the cache-owned pixel budget",
                )
                decoded.load()
        else:
            require(current == invariants, f"{name}: scene metadata changed")
            require(image == reference, f"{name}: reference PNG bytes changed")
        measurements = report["measurements"]
        require([item["case"] for item in measurements] == list(CASES), f"{name}: missing cases")
        for item in measurements:
            label = f"{name}/{item['case']}"
            require(item["frames"] == 100, f"{label}: incomplete frame count")
            for key in (
                "cpu_ms", "cpu_ms_per_frame", "cpu_percent", "wall_ms",
                "frames_per_second", "completed_draw_ms_per_frame",
            ):
                require(finite_number(item[key]) and item[key] >= 0, f"{label}: invalid {key}")
            require(9.95 <= item["frames_per_second"] <= 10.05, f"{label}: cadence changed")
            require(item["wall_ms"] > 0, f"{label}: empty timing interval")
            require(
                math.isclose(item["frames_per_second"], 100000 / item["wall_ms"], rel_tol=1e-9),
                f"{label}: inconsistent wall time",
            )
            require(
                math.isclose(item["cpu_ms_per_frame"], item["cpu_ms"] / 100, rel_tol=1e-9),
                f"{label}: inconsistent CPU time",
            )
            require(
                math.isclose(
                    item["cpu_percent"],
                    100 * item["cpu_ms"] / item["wall_ms"] / report["logical_processors"],
                    rel_tol=1e-9,
                ),
                f"{label}: inconsistent normalized CPU",
            )
            retained = item["retained_prefix"]
            for key in ("texture_bytes", "signature_bytes", "reused_frames", "rebuilt_frames"):
                require(type(retained[key]) is int and retained[key] >= 0, f"{label}: invalid {key}")
            require(retained["texture_bytes"] <= MAX_TEXTURE_BYTES, f"{label}: texture budget exceeded")
            require(retained["signature_bytes"] <= MAX_SIGNATURE_BYTES, f"{label}: signature budget exceeded")
            require(retained["decline_reason"] is None, f"{label}: retention declined")
            composed = mode == "on" and item["case"] != "localized-without-composition"
            if composed:
                require(
                    retained["reused_frames"] > 0
                    and retained["reused_frames"] + retained["rebuilt_frames"] == 100,
                    f"{label}: missing actual reuse or incomplete accounting",
                )
                require(
                    retained["texture_bytes"] == report["physical_size"][0] * report["physical_size"][1] * 4
                    and retained["signature_bytes"] > 0,
                    f"{label}: missing retained image/signature",
                )
                if item["case"].startswith("frozen"):
                    require(retained["reused_frames"] == 100, f"{label}: frozen prefix rebuilt")
            else:
                require(
                    retained["reused_frames"] == retained["rebuilt_frames"] == 0,
                    f"{label}: control unexpectedly used retention",
                )
        runs.append({"name": name, "mode": mode, "metadata": metadata, "measurements": measurements})
    cases = {}
    for case in CASES:
        values = {}
        for mode in ("off", "on"):
            samples = [
                item for run in runs if run["mode"] == mode
                for item in run["measurements"] if item["case"] == case
            ]
            cpu = [item["cpu_ms_per_frame"] for item in samples]
            values[mode] = {
                "cpu_ms_per_frame": cpu,
                "cpu_mean": statistics.mean(cpu),
                "cpu_min": min(cpu),
                "cpu_max": max(cpu),
                "completed_draw_ms_mean": statistics.mean(item["completed_draw_ms_per_frame"] for item in samples),
                "fps_min": min(item["frames_per_second"] for item in samples),
                "fps_max": max(item["frames_per_second"] for item in samples),
            }
        require(values["off"]["cpu_mean"] > 0, f"{case}: cannot compute a percentage from zero CPU")
        values["change_percent"] = 100 * (values["on"]["cpu_mean"] / values["off"]["cpu_mean"] - 1)
        cases[case] = values
    return {
        "scope": "Completed offscreen work, not native presentation or input latency",
        "manifest": manifest,
        "invariants": {**invariants, "original_png_sha256": hashlib.sha256(reference).hexdigest()},
        "runs": runs,
        "cases": cases,
    }


def run_comparison(probe, directory, source_label):
    require(
        platform.system() == "Windows" and platform.machine().upper() in ("AMD64", "X86_64"),
        "Running this comparison requires native Windows x64; checking saved results is portable",
    )
    require(os.environ.get("FESTERM_RUN_OPTIONAL_VALIDATION") == "1", "Set FESTERM_RUN_OPTIONAL_VALIDATION=1")
    require(source_label.strip(), "Declare the measured binary's source, not an assumed current HEAD")
    probe = probe.resolve(strict=True)
    directory = directory.resolve()
    require(not directory.exists(), "Use a fresh evidence directory; failed runs are never overwritten")
    sha = digest(probe)
    directory.mkdir(parents=True)
    write_json(directory / "manifest.json", {
        "schema_version": 1, "executable": str(probe), "executable_sha256": sha,
        "source_label": source_label,
    })
    environment = dict(os.environ)
    environment.update({
        "FESTERM_TUI_PROFILE_SCENE": "application",
        "FESTERM_TUI_PROFILE_CASES": ",".join(CASES),
        "FESTERM_TUI_PROFILE_HOST_COPY": "1",
    })
    for key in ("FESTERM_TUI_PROFILE_COPY", "FESTERM_TUI_PROFILE_SAMPLER", "FESTERM_TUI_PROFILE_REFERENCE"):
        environment.pop(key, None)
    for index, (name, mode) in enumerate(RUNS):
        require(digest(probe) == sha, "Probe executable changed during the comparison")
        environment["FESTERM_TUI_PROFILE_RETAINED_COMPOSITION"] = "1" if mode == "on" else "0"
        environment["FESTERM_TUI_PROFILE_OUT"] = str(directory / name)
        if index:
            environment["FESTERM_TUI_PROFILE_REFERENCE"] = str(directory / RUNS[0][0] / "original.png")
        metadata = {
            "mode": mode, "executable_sha256": sha, "source_label": source_label,
            "reference_run": RUNS[0][0] if index else None,
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "exit_code": None, "verified": False,
        }
        print(f"{index + 1}/{len(RUNS)}: {name}", flush=True)
        try:
            with (directory / f"{name}.stdout.log").open("xb") as stdout, (directory / f"{name}.stderr.log").open("xb") as stderr:
                result = subprocess.run(
                    [str(probe), TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                    cwd=Path(__file__).resolve().parents[2], env=environment,
                    stdout=stdout, stderr=stderr, timeout=180, check=False,
                )
            metadata["exit_code"] = result.returncode
            require(result.returncode == 0, f"{name}: probe failed; stopped without retry, see preserved logs")
            require(digest(probe) == sha, "Probe executable changed during the comparison")
            metadata["verified"] = True
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            metadata["error"] = str(error)
            raise
        finally:
            metadata["finished_utc"] = datetime.now(timezone.utc).isoformat()
            write_json(directory / f"{name}.metadata.json", metadata)
    summary = summarize(directory)
    write_json(directory / "summary.json", summary)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="Run one ABBA and BAAB; never retry a failed process")
    run.add_argument("--probe", type=Path, required=True, help="Already-built release festerm test executable")
    run.add_argument("--directory", type=Path, required=True)
    run.add_argument("--source-label", required=True, help="Explicit binary source provenance, including dirty changes")
    check = commands.add_parser("check", help="Revalidate a saved series without running or changing it")
    check.add_argument("directory", type=Path)
    args = parser.parse_args()
    try:
        summary = run_comparison(args.probe, args.directory, args.source_label) if args.command == "run" else summarize(args.directory)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"retained comparison failed: {error}", file=sys.stderr)
        return 1
    for case, values in summary["cases"].items():
        print(f"{case}: {values['off']['cpu_mean']:.6f} -> {values['on']['cpu_mean']:.6f} CPU-ms/frame ({values['change_percent']:+.2f}%)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
