"""Validate a complete saved native A/B/C series without rerunning the desktop."""

import argparse
from datetime import datetime
import hashlib
import json
import math
from pathlib import Path
import statistics


MODES = ("A", "B", "C", "C", "B", "A", "C", "B", "A", "A", "B", "C")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def distribution(values):
    ordered = sorted(values)
    return {
        "samples": values, "mean": statistics.mean(values),
        "median": statistics.median(values),
        "p95": ordered[math.ceil(0.95 * len(ordered)) - 1],
        "min": ordered[0], "max": ordered[-1],
    }


def summarize(directory):
    manifest = read_json(directory / "manifest.json")
    require(manifest["SchemaVersion"] == 1, "Unsupported native manifest")
    require(manifest["Modes"] == list(MODES), "Missing balanced/reversed A/B/C order")
    require(not manifest["DirtyChanges"], "Qualification source is dirty")
    require(manifest["Configuration"] == "release", "Qualification is not release")
    require(manifest["Architecture"] == "X64", "Qualification is not native x64")
    source = manifest["SourceSha"]
    require(len(source) == 40 and all(c in "0123456789abcdef" for c in source), "Invalid source SHA")
    runs = read_json(directory / "results.json")
    require(isinstance(runs, list) and len(runs) == len(manifest["Runs"]), "Incomplete native series")
    geometry = {}
    bytes_by_workload = {}
    previous_finish = None
    for expected, result in zip(manifest["Runs"], runs):
        name = f"{expected['Sequence']:02}-{expected['Mode']}-festerm-{expected['Workload']}"
        label = directory / name
        require(
            all(result[key] == expected[key] for key in ("Mode", "Sequence", "Workload", "Host")),
            f"{name}: ordered scenario changed",
        )
        require(not (label / "failure.json").exists(), f"{name}: failed attempt cannot be pooled")
        require(read_json(label / "result.json") == result, f"{name}: aggregate differs from raw result")
        cleanup = read_json(label / "cleanup.json")
        require(
            cleanup["NormalExit"] is True and cleanup["Forced"] is False
            and cleanup["ExitCode"] == 0 and not cleanup["RemainingDescendantPids"],
            f"{name}: graceful whole-tree shutdown was not demonstrated",
        )
        started = datetime.fromisoformat(result["StartedUtc"])
        finished = datetime.fromisoformat(cleanup["FinishedUtc"])
        require(
            started.tzinfo is not None and finished.tzinfo is not None and started <= finished
            and (previous_finish is None or previous_finish <= started),
            f"{name}: overlapping or reversed process intervals",
        )
        previous_finish = finished
        require(result["SourceSha"] == source, f"{name}: mixed sources")
        require(result["ExecutableSha256"] == manifest["FesTermSha256"], f"{name}: mixed app binaries")
        require(result["ProducerSha256"] == manifest["ProducerSha256"], f"{name}: mixed producers")
        require(result["LogicalProcessors"] == manifest["LogicalProcessors"], f"{name}: changed CPU capacity")
        require(
            result["Status"] == "valid" and not any(
                result[key] for key in ("InputChanged", "ForegroundChanged", "GeometryChanged")
            ),
            f"{name}: native guard failed",
        )
        require(result["SampleSeconds"] >= manifest["SampleSeconds"], f"{name}: incomplete sample")
        require(result["HostCopyRequested"] is (expected["Mode"] != "A"), f"{name}: wrong host-copy flag")
        require(result["RetainedCompositionRequested"] is (expected["Mode"] == "C"), f"{name}: wrong retention flag")
        require(result["OverlayControl"] is manifest["OverlayControl"], f"{name}: changed fallback control")
        producer = result["Producer"]
        require(
            producer["workload"] == expected["Workload"]
            and producer["frames"] == manifest["ProducerFrames"]
            and len(producer["completed_ms"]) == manifest["ProducerFrames"]
            and producer["interval_ms"] == 100
            and producer["geometry"] == {"columns": 120, "rows": 40}
            and producer["bytes"] > 0,
            f"{name}: incomplete or changed workload delivery",
        )
        previous = 0
        for index, value in enumerate(producer["completed_ms"], 1):
            require(
                isinstance(value, (int, float)) and math.isfinite(value)
                and value >= index * 100 and value >= previous,
                f"{name}: invalid producer cadence",
            )
            previous = value
        require(
            result["SampleStartedUnixMs"] >= producer["started_unix_ms"]
            and result["SampleStartedUnixMs"] + result["SampleSeconds"] * 1000
            <= producer["started_unix_ms"] + producer["completed_ms"][-1] + 1000,
            f"{name}: producer did not cover sampling",
        )
        workload = expected["Workload"]
        stable = (result["Metrics"][2:5], result["Font"])
        if workload not in geometry:
            geometry[workload] = stable
            bytes_by_workload[workload] = producer["bytes"]
        require(stable == geometry[workload], f"{name}: changed physical size, DPI or font")
        require(producer["bytes"] == bytes_by_workload[workload], f"{name}: unmatched workload bytes")
        require(result["Intervals"], f"{name}: missing interval evidence")
        for interval in result["Intervals"]:
            require(
                not interval["InputChanged"] and interval["Foreground"] == result["Window"]
                and interval["Metrics"] == result["Metrics"],
                f"{name}: interval guard failed",
            )
            require(
                all(isinstance(interval[key], (int, float)) and math.isfinite(interval[key])
                    and interval[key] >= 0 for key in ("CpuPercent", "PrivateBytes", "WorkingSetBytes", "Handles", "Threads")),
                f"{name}: invalid interval resources",
            )
        require(
            isinstance(result["CpuPercent"], (int, float)) and math.isfinite(result["CpuPercent"])
            and result["CpuPercent"] >= 0,
            f"{name}: invalid CPU result",
        )
        active = workload != "quiet"
        if active:
            require(result["Direct2DFramesPerSecond"] > 0, f"{name}: no native terminal frames")
            if expected["Mode"] != "A" and not manifest["OverlayControl"]:
                require(result["HostCopyFramesPerSecond"] > 0, f"{name}: no actual host copies")
            if expected["Mode"] == "C" and not manifest["OverlayControl"]:
                counter = "RetainedUiRebuildsPerSecond" if workload == "changing-chrome" else "RetainedUiFramesPerSecond"
                require(result[counter] > 0, f"{name}: no actual retained path")
        if manifest["OverlayControl"] and expected["Mode"] != "A":
            require(result["HostCopyFramesPerSecond"] == 0, f"{name}: overlay did not fall back")
            if expected["Mode"] == "C":
                require(result["RetainedUiFramesPerSecond"] == result["RetainedUiRebuildsPerSecond"] == 0,
                        f"{name}: ineligible prefix was retained")
        capture_path = label / "desktop-final.png"
        if capture_path.exists():
            capture = read_json(label / "desktop-final.json")
            require(
                hashlib.sha256(capture_path.read_bytes()).hexdigest().upper() == capture["Sha256"],
                f"{name}: changed desktop capture",
            )
    cases = {}
    for workload in geometry:
        selected = [item for item in runs if item["Workload"] == workload]
        values = {}
        for mode in ("A", "B", "C"):
            samples = [item for item in selected if item["Mode"] == mode]
            require(len(samples) == 4, f"{workload}: missing mode repetitions")
            values[mode] = {
                "cpu_percent": distribution([item["CpuPercent"] for item in samples]),
                "producer_fps": distribution([
                    item["Producer"]["frames"] * 1000 / item["Producer"]["completed_ms"][-1]
                    for item in samples
                ]),
                "private_bytes": distribution([item["PrivateBytes"] for item in samples]),
            }
        baseline = values["A"]["cpu_percent"]["mean"]
        values["C_vs_A_percent"] = 100 * (values["C"]["cpu_percent"]["mean"] / baseline - 1) if baseline else None
        values["B_vs_A_percent"] = 100 * (values["B"]["cpu_percent"]["mean"] / baseline - 1) if baseline else None
        values["ordered_blocks"] = [
            {mode: next(item["CpuPercent"] for item in selected
                        if item["Mode"] == mode and start <= item["Sequence"] < start + 3)
             for mode in ("A", "B", "C")}
            for start in (1, 4, 7, 10)
        ]
        cases[workload] = values
    return {
        "scope": "Single-host guarded native process CPU and producer delivery; no default-on approval",
        "manifest": manifest, "cases": cases,
        "display_boundary": manifest["CaptureBoundary"],
        "acceptance": "No new timing threshold or memory budget is inferred",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    arguments = parser.parse_args()
    print(json.dumps(summarize(arguments.directory), indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
