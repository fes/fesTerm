#!/usr/bin/env python3
"""Build a development-signed physical-device app using existing local credentials.

No enrollment, provisioning downloads, device installation or distribution occurs.
The output contains a provisioning profile: keep it local, outside CI artifacts.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ios_spike_build", ROOT / "scripts/build-ios-spike.py")
spike = importlib.util.module_from_spec(spec)
spec.loader.exec_module(spike)
TARGET = "aarch64-apple-ios"


def run(*args: str, timeout: int = 120, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, timeout=timeout,
                          capture_output=True, **kwargs)


def development_entitlements(profile: dict, bundle_id: str, team: str,
                             identity: str, device: str,
                             now: datetime | None = None) -> dict:
    """Admit an explicit development profile for this app, certificate and device.

    Decoding is not a trust/installation receipt. codesign and iOS perform their
    own verification. Never print or persist the decoded profile as diagnostics.
    """
    if not isinstance(profile, dict):
        raise ValueError("Provisioning payload is not a dictionary")
    if not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+", bundle_id):
        raise ValueError("Use an explicit reverse-DNS bundle ID (no wildcard)")
    if not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise ValueError("Use the ten-character Apple development team ID")
    if not re.fullmatch(r"[0-9a-fA-F]{40}", identity):
        raise ValueError("Use the exact SHA-1 signing identity from security find-identity")
    expiry = profile.get("ExpirationDate")
    if not isinstance(expiry, datetime):
        raise ValueError("Provisioning profile has no valid expiry")
    expiry = expiry.replace(tzinfo=timezone.utc) if expiry.tzinfo is None else expiry
    if expiry <= (now or datetime.now(timezone.utc)):
        raise ValueError("Provisioning profile has expired")
    if profile.get("Platform") != ["iOS"]:
        raise ValueError("An iOS development profile is required")
    if profile.get("TeamIdentifier") != [team]:
        raise ValueError("Provisioning profile belongs to another team")
    entitlements = profile.get("Entitlements", {})
    if not isinstance(entitlements, dict) or entitlements.get("get-task-allow") is not True:
        raise ValueError("A development profile with get-task-allow is required")
    if profile.get("ProvisionsAllDevices") or device not in profile.get("ProvisionedDevices", []):
        raise ValueError("The selected physical device is not provisioned for development")
    prefixes = profile.get("ApplicationIdentifierPrefix", [])
    if not isinstance(prefixes, list) or len(prefixes) != 1 or not isinstance(prefixes[0], str):
        raise ValueError("Provisioning profile has no unambiguous application prefix")
    application_id = f"{prefixes[0]}.{bundle_id}"
    if entitlements.get("application-identifier") != application_id:
        raise ValueError("Use an explicit profile matching the requested bundle ID")
    if entitlements.get("com.apple.developer.team-identifier") != team:
        raise ValueError("Provisioning entitlement team does not match")
    certificates = profile.get("DeveloperCertificates", [])
    if not isinstance(certificates, list) or not any(
        isinstance(cert, bytes) and hashlib.sha1(cert).hexdigest().upper() == identity.upper()
        for cert in certificates
    ):
        raise ValueError("Signing certificate is not included in the provisioning profile")
    # This prototype needs no extra capabilities or shared Keychain access.
    # Do not blindly copy all entitlements granted by the supplied profile.
    return {"application-identifier": application_id,
            "com.apple.developer.team-identifier": team, "get-task-allow": True}


def require_local_identity(identity: str) -> None:
    output = run("security", "find-identity", "-v", "-p", "codesigning", text=True).stdout
    valid = re.findall(r'\)\s+([0-9A-Fa-f]{40})\s+"', output)
    if identity.upper() not in {value.upper() for value in valid}:
        raise ValueError("Signing identity is not a valid local code-signing identity with a private key")


def device_bundle_info(bundle_id: str, live_ssh: bool = False) -> dict:
    info = spike.bundle_info(live_ssh)
    info.update(CFBundleIdentifier=bundle_id, CFBundleSupportedPlatforms=["iPhoneOS"],
                UIRequiredDeviceCapabilities=["arm64"])
    return info


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", type=Path, required=True, help="local .mobileprovision file")
    parser.add_argument("--identity", required=True, help="exact SHA-1 identity from security find-identity")
    parser.add_argument("--team", required=True, help="Apple development team ID")
    parser.add_argument("--bundle-id", required=True, help="explicit app ID registered for this profile")
    parser.add_argument("--device", required=True, help="provisioned physical device UDID; never logged in receipts")
    parser.add_argument("--preflight-only", action="store_true", help="validate prerequisites without building or signing")
    parser.add_argument("--live-ssh", action="store_true", help="include the opt-in transient SSH connection UI (default: offline fixture)")
    args = parser.parse_args(argv)
    if platform.system() != "Darwin":
        parser.error("Physical-device packaging requires macOS, full Xcode and local development signing")
    # Fail before compilation or artifact creation when signing setup is missing.
    if not args.profile.is_file():
        raise ValueError("Provisioning profile file is unavailable")
    with args.profile.open("rb") as stream:
        profile_bytes = stream.read(4 * 1024 * 1024 + 1)
    if len(profile_bytes) > 4 * 1024 * 1024:
        raise ValueError("Provisioning profile exceeds the 4 MiB input bound")
    # Decode the exact bytes that will be embedded, not a file that can change
    # between validation and packaging. The decoded payload stays in memory.
    decoded = run("security", "cms", "-D", input=profile_bytes).stdout
    entitlements = development_entitlements(plistlib.loads(decoded), args.bundle_id,
                                           args.team, args.identity, args.device)
    require_local_identity(args.identity)
    sdk = run("xcrun", "--sdk", "iphoneos", "--show-sdk-path", text=True).stdout.strip()
    sdk_version = run("xcrun", "--sdk", "iphoneos", "--show-sdk-version", text=True).stdout.strip()
    xcode = run("xcodebuild", "-version", text=True).stdout.strip()
    # Uses exactly the existing dependency exclusion policy; no desktop backend.
    spike.check_dependencies(TARGET, live_ssh=args.live_ssh)
    mode = "live-ssh" if args.live_ssh else "offline-fixture"
    features = ["live-ssh"] if args.live_ssh else []
    if args.preflight_only:
        print(f"Development profile, identity, device membership, Xcode SDK and {mode} dependency graph validated; no build or install")
        return 0
    run("rustup", "target", "add", TARGET, timeout=600)
    env = dict(os.environ, SDKROOT=sdk, IPHONEOS_DEPLOYMENT_TARGET=spike.MINIMUM_IOS)
    run("cargo", "build", "--locked", "-p", "festerm-mobile", "--target", TARGET,
        *(["--features", "live-ssh"] if args.live_ssh else []),
        "--target-dir", str(ROOT / "target"), env=env, timeout=1800)
    output = ROOT / "target" / "ios-device"
    output.mkdir(parents=True, exist_ok=True)
    # Fresh private output avoids stale Simulator signatures/resources or reuse
    # of another team's profile. No archive/upload of private device metadata.
    prefix = "development-live-ssh-" if args.live_ssh else "development-"
    directory = Path(tempfile.mkdtemp(prefix=prefix, dir=output))
    bundle = directory / "fesTermSpike.app"
    bundle.mkdir()
    executable = bundle / "festerm-mobile"
    shutil.copy2(ROOT / "target" / TARGET / "debug" / "festerm-mobile", executable)
    (bundle / "Info.plist").write_bytes(plistlib.dumps(device_bundle_info(args.bundle_id, args.live_ssh)))
    (bundle / "embedded.mobileprovision").write_bytes(profile_bytes)
    entitlement_file = directory / "development-entitlements.plist"
    entitlement_file.write_bytes(plistlib.dumps(entitlements))
    receipt = directory / "build-receipt.json"
    build_mode = {"prototype_mode": mode, "cargo_features": features}
    receipt.write_text(json.dumps({"status": "signing-pending", "native_validation": "not-run",
                                   **build_mode}) + "\n")
    run("codesign", "--force", "--sign", args.identity, "--entitlements", str(entitlement_file),
        "--generate-entitlement-der", str(bundle))
    run("codesign", "--verify", "--strict", str(bundle))
    source = run("git", "rev-parse", "HEAD", text=True).stdout.strip()
    dirty = bool(run("git", "status", "--porcelain", "--untracked-files=normal", text=True).stdout.strip())
    receipt.write_text(json.dumps({"status": "signed", "native_validation": "not-run",
                                   **build_mode,
                                   "source_commit": source, "source_dirty": dirty,
                                   "target": TARGET, "sdk_version": sdk_version, "xcode": xcode,
                                   "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest()},
                                  indent=2) + "\n")
    print(bundle)
    print(f"Development signed ({mode}) only; installation, rendering and SSH functionality are unverified.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError, RuntimeError) as error:
        # Never dump decoded provisioning content or subprocess stdout/stderr.
        detail = str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__
        print(f"Device build failed: {detail}. Check local prerequisites; tool output is not logged.",
              file=sys.stderr)
        sys.exit(1)
