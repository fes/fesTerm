#!/usr/bin/env python3
"""Opt-in launch/relaunch evidence on fresh, test-owned iPhone/iPad Simulators.

Requires macOS, Xcode and an installed iOS runtime. Never boots, erases or
shuts down an existing Simulator. Screenshots require human visual review;
process survival is not evidence of correct rendering or touch behavior.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import plistlib
import re
import shlex
import struct
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
BUNDLE_ID = "org.festerm.mobile-spike"
EXECUTABLE = "festerm-mobile"


def choose_devices(inventory: dict) -> list[dict]:
    """Select both families from one installed, available iOS runtime."""
    runtimes = [r for r in inventory["runtimes"]
                if r.get("isAvailable") and ".iOS-" in r["identifier"]]
    runtimes.sort(key=lambda r: tuple(int(n) for n in r["version"].split(".")), reverse=True)
    for runtime in runtimes:
        chosen = []
        devices = inventory["devices"].get(runtime["identifier"], [])
        for family in ("iPhone", "iPad"):
            matches = [d for d in devices if d.get("isAvailable")
                       and d["name"].startswith(family)
                       and d.get("deviceTypeIdentifier")]
            if matches:
                template = sorted(matches, key=lambda d: d["name"])[0]
                chosen.append({"family": family, "model": template["name"],
                               "device_type": template["deviceTypeIdentifier"],
                               "runtime": runtime["identifier"], "ios": runtime["version"]})
        if len(chosen) == 2:
            return chosen
    raise RuntimeError("Install an iOS Simulator runtime with available iPhone and iPad device types")


class Runner:
    def __init__(self, log: Path):
        self.log = log

    def __call__(self, *args: str, timeout: int = 45, check: bool = True) -> str:
        with self.log.open("a", encoding="utf-8") as stream:
            stream.write("$ " + shlex.join(args) + "\n")
            stream.flush()
            result = subprocess.run(args, cwd=ROOT, text=True, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, timeout=timeout)
            stream.write(result.stdout + f"\nexit={result.returncode}\n")
        if check and result.returncode:
            raise RuntimeError(f"Command failed ({result.returncode}): {shlex.join(args)}")
        return result.stdout


def png_dimensions(path: Path) -> tuple[int, int]:
    with path.open("rb") as stream:
        header = stream.read(24)
    if len(header) != 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise RuntimeError(f"Missing PNG screenshot header: {path.name}")
    width, height = struct.unpack(">II", header[16:24])
    if not width or not height:
        raise RuntimeError("Screenshot has empty dimensions")
    return width, height


def verify_process(run, pid: int) -> None:
    # Simulator processes share the host kernel. Match the executable as well
    # as the launch PID so an unrelated/reused PID cannot satisfy this check.
    command = run("ps", "-ww", "-p", str(pid), "-o", "comm=").strip()
    if Path(command).name != EXECUTABLE:
        raise RuntimeError(f"Launched process {pid} is no longer {EXECUTABLE}")


def exercise_device(run, spec: dict, bundle: Path, output: Path,
                    existing: set[str], pause=time.sleep) -> dict:
    result = dict(spec, status="running", captures=[], cleanup_errors=[])
    owned = None
    try:
        created = run("xcrun", "simctl", "create", f"fesTerm-smoke-{uuid.uuid4().hex[:12]}",
                      spec["device_type"], spec["runtime"]).strip()
        # Only the exact fresh UDID returned by this create may be mutated.
        if not re.fullmatch(r"[0-9A-Fa-f-]{36}", created) or created in existing:
            raise RuntimeError("simctl did not return a fresh test-owned device UDID")
        owned = created
        result["udid"] = owned
        run("xcrun", "simctl", "boot", owned)
        run("xcrun", "simctl", "bootstatus", owned, "-b", timeout=180)
        run("xcrun", "simctl", "install", owned, str(bundle))
        for phase in ("launch", "relaunch"):
            launched = run("xcrun", "simctl", "launch", owned, BUNDLE_ID)
            match = re.search(re.escape(BUNDLE_ID) + r":\s*(\d+)\s*$", launched)
            if not match or int(match[1]) <= 0:
                raise RuntimeError("simctl launch did not return a valid application PID")
            pid = int(match[1])
            pause(3)  # Bounded startup grace period, not a rendering assertion.
            verify_process(run, pid)
            screenshot = output / f"{spec['family'].lower()}-{phase}.png"
            run("xcrun", "simctl", "io", owned, "screenshot", "--type=png", str(screenshot))
            dimensions = png_dimensions(screenshot)
            verify_process(run, pid)
            result["captures"].append({"phase": phase, "pid": pid,
                                       "screenshot": screenshot.name, "pixels": dimensions})
            run("xcrun", "simctl", "terminate", owned, BUNDLE_ID)
        result["status"] = "pass"
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        result["status"] = "fail"
        result["error"] = str(error)
    finally:
        if owned is not None:
            try:
                run("xcrun", "simctl", "spawn", owned, "log", "show", "--last", "2m",
                    "--style", "compact", "--predicate", 'process == "festerm-mobile"',
                    timeout=20, check=False)
            except (OSError, subprocess.SubprocessError) as error:
                result["diagnostic_error"] = str(error)
            for operation in ("shutdown", "delete"):
                try:
                    run("xcrun", "simctl", operation, owned)
                except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                    result["cleanup_errors"].append(str(error))
            if result["cleanup_errors"]:
                result["status"] = "fail"
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="opt in to creating temporary Simulators")
    parser.add_argument("--build", action="store_true", help="build the Simulator bundle first")
    parser.add_argument("--bundle", type=Path, default=ROOT / "target/ios-spike/fesTermSpike.app")
    parser.add_argument("--output", type=Path, default=ROOT / "target/ios-smoke")
    args = parser.parse_args()
    if not args.run:
        parser.error("Pass --run to create and remove test-owned Simulators")
    if platform.system() != "Darwin":
        parser.error("Simulator execution requires macOS and full Xcode")
    output = args.output.resolve() / uuid.uuid4().hex
    output.mkdir(parents=True, exist_ok=False)
    report = {"schema": 1, "status": "running", "devices": [],
              "scope": "install, launch survival, PNG capture, terminate/relaunch; visual review required",
              "unverified": ["rendering correctness", "native keyboard/IME", "touch gestures",
                             "background/resume", "physical-device behavior"]}
    run = Runner(output / "commands.log")
    manifest = output / "manifest.json"
    manifest.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    try:
        report["commit"] = run("git", "rev-parse", "HEAD").strip()
        report["xcode"] = run("xcodebuild", "-version").strip()
        report["host_arch"] = platform.machine()
        if args.build:
            run(sys.executable, str(ROOT / "scripts/build-ios-spike.py"), timeout=1200)
        bundle = args.bundle.resolve()
        with (bundle / "Info.plist").open("rb") as stream:
            info = plistlib.load(stream)
        if info.get("CFBundleIdentifier") != BUNDLE_ID or info.get("CFBundleExecutable") != EXECUTABLE:
            raise RuntimeError("Only the offline fesTerm spike bundle may be exercised")
        inventory = json.loads(run("xcrun", "simctl", "list", "--json"))
        existing = {d["udid"] for group in inventory["devices"].values() for d in group}
        for spec in choose_devices(inventory):
            report["devices"].append(exercise_device(run, spec, bundle, output, existing))
            manifest.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        report["status"] = "pass" if all(d["status"] == "pass" for d in report["devices"]) else "fail"
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        report.update(status="fail", error=str(error))
    finally:
        manifest.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"Simulator smoke {report['status']}: {manifest}")
    return 0 if report["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
