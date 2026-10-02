#!/usr/bin/env python3
"""Build an ad-hoc-signed Simulator app; optionally install and launch it.

Uses the selected Xcode installation and an already booted Simulator. This is
development packaging, not device provisioning or a store release pipeline.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
BUNDLE_ID = "org.festerm.mobile-spike"
MINIMUM_IOS = "15.0"
FORBIDDEN = {"festerm", "festerm-pty", "festerm-sessiond", "festerm-serial", "cargo-packager-updater"}


def run(*args: str, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def check_dependencies(target: str) -> None:
    result = run(
        "cargo", "tree", "--locked", "-p", "festerm-mobile", "--target", target,
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}",
        capture_output=True, text=True,
    )
    names = {line.split()[0] for line in result.stdout.splitlines() if line.strip()}
    forbidden = names & FORBIDDEN
    if forbidden:
        raise RuntimeError(f"Mobile dependency boundary violated: {sorted(forbidden)}")


def bundle_info() -> dict:
    return {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": "fesTerm Spike",
        "CFBundleExecutable": "festerm-mobile",
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "fesTerm Spike",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "0.1.0",
        "CFBundleVersion": "1",
        "CFBundleSupportedPlatforms": ["iPhoneSimulator"],
        "MinimumOSVersion": MINIMUM_IOS,
        "LSRequiresIPhoneOS": True,
        "UIDeviceFamily": [1, 2],
        "UILaunchScreen": {},
        "UISupportedInterfaceOrientations": [
            "UIInterfaceOrientationPortrait",
            "UIInterfaceOrientationLandscapeLeft",
            "UIInterfaceOrientationLandscapeRight",
        ],
        "UISupportedInterfaceOrientations~ipad": [
            "UIInterfaceOrientationPortrait",
            "UIInterfaceOrientationPortraitUpsideDown",
            "UIInterfaceOrientationLandscapeLeft",
            "UIInterfaceOrientationLandscapeRight",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="install and launch on a booted Simulator")
    parser.add_argument("--simulator", default="booted", help="Simulator UDID (default: booted)")
    parser.add_argument("--check-dependencies", action="store_true", help="check both iOS target graphs only; works on Linux")
    args = parser.parse_args()
    if args.check_dependencies:
        for target in ("aarch64-apple-ios", "aarch64-apple-ios-sim"):
            check_dependencies(target)
        print("iOS normal/build dependency boundaries passed")
        return 0
    if platform.system() != "Darwin":
        parser.error("Simulator packaging requires macOS and full Xcode; use --check-dependencies here")
    target = "aarch64-apple-ios-sim" if platform.machine() == "arm64" else "x86_64-apple-ios"
    sdk = run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-path", capture_output=True, text=True).stdout.strip()
    run("rustup", "target", "add", target)
    check_dependencies(target)
    env = dict(os.environ, SDKROOT=sdk, IPHONEOS_DEPLOYMENT_TARGET=MINIMUM_IOS)
    run("cargo", "build", "--locked", "-p", "festerm-mobile", "--target", target,
        "--target-dir", str(ROOT / "target"), env=env)
    bundle = ROOT / "target" / "ios-spike" / "fesTermSpike.app"
    bundle.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "target" / target / "debug" / "festerm-mobile", bundle / "festerm-mobile")
    with (bundle / "Info.plist").open("wb") as stream:
        plistlib.dump(bundle_info(), stream)
    run("codesign", "--force", "--sign", "-", str(bundle))
    run("codesign", "--verify", "--strict", str(bundle))
    # Preserve executable permissions when downloaded from Actions artifacts.
    shutil.make_archive(str(bundle.parent / "fesTermSpike-simulator"), "gztar",
                        root_dir=bundle.parent, base_dir=bundle.name)
    print(bundle)
    if args.run:
        # Resolve the alias so multiple booted devices never receive a guessed install.
        devices = json.loads(run("xcrun", "simctl", "list", "devices", "booted", "--json",
                                 capture_output=True, text=True).stdout)
        booted = [device["udid"] for group in devices["devices"].values() for device in group]
        udid = args.simulator
        if udid == "booted":
            if len(booted) != 1:
                parser.error("boot exactly one Simulator or pass its UDID with --simulator")
            udid = booted[0]
        if udid not in booted:
            parser.error("the selected Simulator must already be booted")
        run("xcrun", "simctl", "install", udid, str(bundle))
        run("xcrun", "simctl", "launch", udid, BUNDLE_ID)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.CalledProcessError, RuntimeError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
