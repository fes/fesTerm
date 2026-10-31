#!/usr/bin/env python3
"""Build, install and launch the festerm-ios-lock test host on ONE named Simulator.

Not the product app: it shows green when sensitive content is drawn and orange
when locked, and logs state to Documents/lock-host.log in the app container.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import plistlib
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ios_spike_build", ROOT / "scripts/build-ios-spike.py")
spike = importlib.util.module_from_spec(spec)
spec.loader.exec_module(spike)
TARGET = "aarch64-apple-ios-sim"
BUNDLE_ID = "dev.festerm.lockhost"


def run(*args: str, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulator", required=True, help="Simulator UDID (never 'booted': no guessing)")
    parser.add_argument("--grace", default="1m", choices=["always", "1m", "5m", "15m", "never"])
    parser.add_argument("--autounlock", action="store_true", help="start one unlock per lock episode")
    args = parser.parse_args()
    sdk = run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-path", capture_output=True, text=True).stdout.strip()
    env = dict(os.environ, SDKROOT=sdk, IPHONEOS_DEPLOYMENT_TARGET=spike.MINIMUM_IOS)
    run("cargo", "build", "--locked", "-p", "festerm-ios-lock", "--example", "lock_host", "--target", TARGET, env=env)
    bundle = ROOT / "target" / "ios-lock-host" / "LockHost.app"
    shutil.rmtree(bundle, ignore_errors=True)
    bundle.mkdir(parents=True)
    shutil.copy2(ROOT / "target" / TARGET / "debug" / "examples" / "lock_host", bundle / "lock_host")
    info = spike.bundle_info()
    info.update(CFBundleExecutable="lock_host", CFBundleIdentifier=BUNDLE_ID,
                CFBundleDisplayName="Lock Host", CFBundleName="Lock Host")
    with (bundle / "Info.plist").open("wb") as stream:
        plistlib.dump(info, stream)
    run("codesign", "--force", "--sign", "-", str(bundle))
    run("xcrun", "simctl", "install", args.simulator, str(bundle))
    child = {"SIMCTL_CHILD_LOCK_HOST_GRACE": args.grace, "SIMCTL_CHILD_LOCK_HOST_AUTOUNLOCK": "1" if args.autounlock else "0"}
    subprocess.run(["xcrun", "simctl", "terminate", args.simulator, BUNDLE_ID], check=False)
    run("xcrun", "simctl", "launch", args.simulator, BUNDLE_ID, env=dict(os.environ, **child))
    container = run("xcrun", "simctl", "get_app_container", args.simulator, BUNDLE_ID, "data",
                    capture_output=True, text=True).stdout.strip()
    print(f"log: {container}/Documents/lock-host.log")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
