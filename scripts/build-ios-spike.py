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
DESKTOP_SECRET_BACKENDS = {
    "windows-native-keyring-store", "zbus-secret-service-keyring-store",
}
# ADR 0054: iOS may use only the app-sandboxed "protected" Keychain store. The
# macOS "keychain" feature is the desktop legacy login keychain and stays out.
IOS_KEYCHAIN_BACKEND = "apple-native-keyring-store"
IOS_KEYCHAIN_REQUIRED_FEATURE = "protected"
IOS_KEYCHAIN_FORBIDDEN_FEATURE = "keychain"


def run(*args: str, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def check_dependencies(target: str, package: str = "festerm-mobile", live_ssh: bool = False) -> None:
    result = run(
        "cargo", "tree", "--locked", "-p", package, "--target", target,
        *(["--features", "live-ssh"] if live_ssh else []),
        "--edges", "normal,build", "--prefix", "none", "--format", "{p} {f}",
        capture_output=True, text=True, timeout=120,
    )
    rows = [line.split() for line in result.stdout.splitlines() if line.strip()]
    names = {row[0] for row in rows}
    forbidden = names & (FORBIDDEN | DESKTOP_SECRET_BACKENDS)
    for row in rows:
        if row[0] == IOS_KEYCHAIN_BACKEND:
            features = set(row[2].split(",")) if len(row) > 2 else set()
            if IOS_KEYCHAIN_REQUIRED_FEATURE not in features or IOS_KEYCHAIN_FORBIDDEN_FEATURE in features:
                forbidden.add(f"{IOS_KEYCHAIN_BACKEND}[{','.join(sorted(features))}]")
    if forbidden:
        raise RuntimeError(f"Mobile dependency boundary violated: {sorted(forbidden)}")


def bundle_info(live_ssh: bool = False) -> dict:
    info = {
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
        # Required by LocalAuthentication (festerm-ios-lock); without it Face ID is denied.
        "NSFaceIDUsageDescription": "fesTerm uses Face ID to unlock the app when app lock is on.",
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
    if live_ssh:
        info["NSLocalNetworkUsageDescription"] = "Connect to SSH hosts you choose on your local network."
    return info


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="install and launch on a booted Simulator")
    parser.add_argument("--simulator", default="booted", help="Simulator UDID (default: booted)")
    parser.add_argument("--live-ssh", action="store_true", help="include the opt-in live SSH connection UI")
    parser.add_argument("--check-dependencies", action="store_true", help="check mobile/SSH graphs for both iOS targets only; works on Linux")
    args = parser.parse_args()
    if args.check_dependencies:
        for target in ("aarch64-apple-ios", "aarch64-apple-ios-sim"):
            check_dependencies(target)
            check_dependencies(target, "festerm-ssh")
            check_dependencies(target, "festerm-mobile-profiles")
            check_dependencies(target, "festerm-sftp")
            check_dependencies(target, "festerm-ios-lock")
            check_dependencies(target, "festerm-mobile-docs")
            if args.live_ssh:
                check_dependencies(target, "festerm-mobile", True)
        print("iOS mobile/SSH/SFTP normal/build dependency boundaries passed")
        return 0
    if platform.system() != "Darwin":
        parser.error("Simulator packaging requires macOS and full Xcode; use --check-dependencies here")
    target = "aarch64-apple-ios-sim" if platform.machine() == "arm64" else "x86_64-apple-ios"
    sdk = run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-path", capture_output=True, text=True).stdout.strip()
    run("rustup", "target", "add", target)
    check_dependencies(target, live_ssh=args.live_ssh)
    env = dict(os.environ, SDKROOT=sdk, IPHONEOS_DEPLOYMENT_TARGET=MINIMUM_IOS)
    run("cargo", "build", "--locked", "-p", "festerm-mobile", "--target", target,
        *(["--features", "live-ssh"] if args.live_ssh else []),
        "--target-dir", str(ROOT / "target"), env=env)
    output = "ios-spike-live" if args.live_ssh else "ios-spike"
    bundle = ROOT / "target" / output / "fesTermSpike.app"
    bundle.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "target" / target / "debug" / "festerm-mobile", bundle / "festerm-mobile")
    with (bundle / "Info.plist").open("wb") as stream:
        plistlib.dump(bundle_info(args.live_ssh), stream)
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
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, RuntimeError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
