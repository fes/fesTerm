#!/usr/bin/env python3
"""Opt-in launch/relaunch evidence on fresh, test-owned iPhone/iPad Simulators.

Requires macOS, Xcode and an installed iOS runtime. Never boots, erases or
shuts down an existing Simulator. Screenshots require human visual review;
process survival is not evidence of correct rendering or touch behavior.
"""
from __future__ import annotations

import argparse
import json
import os
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


def choose_devices(inventory: dict, sdk_version: str) -> list[dict]:
    """Select both families from an installed runtime matching the selected SDK."""
    sdk_release = tuple(int(n) for n in sdk_version.split("."))[:2]
    runtimes = [r for r in inventory["runtimes"]
                if r.get("isAvailable") and ".iOS-" in r["identifier"]
                and tuple(int(n) for n in r["version"].split("."))[:2] == sdk_release]
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
    raise RuntimeError(f"Install an iOS {sdk_version} Simulator runtime with available iPhone and iPad device types")


class Runner:
    def __init__(self, log: Path):
        self.log = log

    def __call__(self, *args: str, timeout: int = 45, check: bool = True) -> str:
        print("[ios-smoke] " + shlex.join(args), flush=True)
        with self.log.open("a", encoding="utf-8") as stream:
            stream.write("$ " + shlex.join(args) + "\n")
            stream.flush()
            started = time.monotonic()
            try:
                result = subprocess.run(args, cwd=ROOT, text=True, stdout=subprocess.PIPE,
                                        stderr=subprocess.STDOUT, timeout=timeout)
            except subprocess.TimeoutExpired as error:
                output = error.stdout or ""
                if isinstance(output, bytes):
                    output = output.decode("utf-8", errors="replace")
                stream.write(output + f"\ntimeout={timeout}s elapsed={time.monotonic() - started:.3f}s\n")
                raise
            stream.write(result.stdout + f"\nexit={result.returncode} elapsed={time.monotonic() - started:.3f}s\n")
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
        run("xcrun", "simctl", "install", owned, str(bundle), timeout=120)
        for phase in ("launch", "relaunch"):
            stderr = output / f"{spec['family'].lower()}-{phase}.stderr.log"
            stdout = output / f"{spec['family'].lower()}-{phase}.stdout.log"
            launched = run("xcrun", "simctl", "launch", f"--stdout={stdout}",
                           f"--stderr={stderr}", owned, BUNDLE_ID, timeout=120)
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
            # A live process can have failed renderer initialization. Preserve
            # its screenshot and logs, but never pass without reaching the UI.
            first_ui = stderr.exists() and "festerm-mobile: first UI built" in stderr.read_text(errors="replace")
            result["captures"][-1].update(first_ui_built=first_ui, stderr=stderr.name)
            run("xcrun", "simctl", "terminate", owned, BUNDLE_ID)
            if not first_ui:
                raise RuntimeError(f"Application never built its first UI; inspect {stderr.name}")
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
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--run", action="store_true", help="opt in to creating temporary Simulators")
    mode.add_argument("--prepare-only", action="store_true",
                      help="initialize and validate installed runtimes without creating or launching devices")
    parser.add_argument("--build", action="store_true", help="build the Simulator bundle first")
    parser.add_argument("--bundle", type=Path, default=ROOT / "target/ios-spike/fesTermSpike.app")
    parser.add_argument("--output", type=Path, default=ROOT / "target/ios-smoke")
    args = parser.parse_args()
    if args.prepare_only and args.build:
        parser.error("--prepare-only cannot build or exercise an application")
    if platform.system() != "Darwin":
        parser.error("Simulator execution requires macOS and full Xcode")
    output = args.output.resolve() / uuid.uuid4().hex
    output.mkdir(parents=True, exist_ok=False)
    report = {"schema": 1, "status": "running", "devices": [],
              "scope": "install, launch survival, first UI callback, PNG capture, terminate/relaunch; visual review required",
              "unverified": ["rendering correctness", "native keyboard/IME", "touch gestures",
                             "background/resume", "physical-device behavior"]}
    if args.prepare_only:
        report["scope"] = "read-only CoreSimulator cache initialization; no application exercised"
    report["runner_image"] = {key: os.environ[key] for key in ("ImageOS", "ImageVersion")
                              if key in os.environ}
    run = Runner(output / "commands.log")
    manifest = output / "manifest.json"
    manifest.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    try:
        report["commit"] = run("git", "rev-parse", "HEAD").strip()
        report["xcode"] = run("xcodebuild", "-version").strip()
        report["host_arch"] = platform.machine()
        report["simulator_sdk"] = run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-version").strip()
        if args.prepare_only:
            # Cold service/runtime cache initialization is separate from the
            # unchanged 45s inventory and application-operation deadlines.
            inventory = json.loads(run("xcrun", "simctl", "list", "--json", timeout=180))
            report["available_devices"] = choose_devices(inventory, report["simulator_sdk"])
            report["status"] = "prepared"
            return 0
        if args.build:
            run(sys.executable, str(ROOT / "scripts/build-ios-spike.py"), timeout=1200)
        bundle = args.bundle.resolve()
        with (bundle / "Info.plist").open("rb") as stream:
            info = plistlib.load(stream)
        if info.get("CFBundleIdentifier") != BUNDLE_ID or info.get("CFBundleExecutable") != EXECUTABLE:
            raise RuntimeError("Only the offline fesTerm spike bundle may be exercised")
        inventory = json.loads(run("xcrun", "simctl", "list", "--json"))
        existing = {d["udid"] for group in inventory["devices"].values() for d in group}
        for spec in choose_devices(inventory, report["simulator_sdk"]):
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
