#!/usr/bin/env python3
"""Build, sign and upload the fesTerm iOS app to TestFlight (manual, local).

--check     validate credentials, version, icon and plist offline
--setup-signing  one-time: ensure an Apple Distribution identity and App Store
            profile exist locally (created through the API key if missing)
--archive   release build, hand-assembled .xcarchive, Xcode cloud-signed .ipa
--upload    upload the .ipa with altool using the App Store Connect API key
--status    list this version's App Store Connect builds and processing state

Xcode's cloud-managed signing needs an Admin-level key, so signing is manual:
the distribution certificate and App Store profile come from --setup-signing.

Never run in CI. The API key path/ID/issuer come from arguments or the
APP_STORE_CONNECT_KEY_PATH, APP_STORE_CONNECT_KEY_ID and APPLE_API_ISSUER
environment variables; they are never printed or written to the output.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
from datetime import datetime, timezone
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
import time
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ios_spike_build", ROOT / "scripts/build-ios-spike.py")
spike = importlib.util.module_from_spec(spec)
spec.loader.exec_module(spike)

BUNDLE_ID = "com.feslabs.fesTerm"
TEAM_ID = "F884J4784J"
TARGET = "aarch64-apple-ios"
FEATURES = "live-ssh"
DISPLAY_NAME = "fesTerm"
ICON_SOURCE = ROOT / "assets/app-icon/app-icon-1024.png"
OUTPUT = ROOT / "target" / "ios-testflight"
IPA_NAME = "fesTerm.ipa"
PROFILE_NAME = "fesTerm App Store"
API = "https://api.appstoreconnect.apple.com"
PROFILE_DIR = Path.home() / "Library/MobileDevice/Provisioning Profiles"


def workspace_version(root: Path = ROOT) -> str:
    text = (root / "Cargo.toml").read_text()
    section = text.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    match = re.search(r'^version\s*=\s*"(\d+\.\d+\.\d+)"', section, re.M)
    if not match:
        raise ValueError("workspace version is not MAJOR.MINOR.PATCH")
    return match.group(1)


def default_build_number(now: float | None = None) -> str:
    """UTC yymmddHHMM: monotonically increasing and within Apple's integer bounds."""
    return time.strftime("%y%m%d%H%M", time.gmtime(now))


def validate_build_number(value: str) -> str:
    if not re.fullmatch(r"[1-9][0-9]{0,17}", value):
        raise ValueError("build number must be a positive integer without leading zeros")
    return value


def validate_team(team: str) -> str:
    if not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise ValueError("team ID must be ten uppercase letters/digits")
    return team


def api_credentials(key_path: str | None, key_id: str | None, issuer: str | None,
                    environ=os.environ) -> tuple[Path, str, str]:
    key_path = key_path or environ.get("APP_STORE_CONNECT_KEY_PATH")
    key_id = key_id or environ.get("APP_STORE_CONNECT_KEY_ID")
    issuer = issuer or environ.get("APPLE_API_ISSUER")
    if not (key_path and key_id and issuer):
        raise ValueError("set --key-path/--key-id/--issuer or APP_STORE_CONNECT_KEY_PATH, "
                         "APP_STORE_CONNECT_KEY_ID and APPLE_API_ISSUER")
    if not re.fullmatch(r"[A-Z0-9]{10}", key_id):
        raise ValueError("key ID must be ten uppercase letters/digits")
    if not re.fullmatch(r"[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", issuer):
        raise ValueError("issuer ID must be a UUID")
    path = Path(key_path).expanduser()
    if not path.is_file():
        raise ValueError("API key file is unavailable")
    if path.name != f"AuthKey_{key_id}.p8":
        # altool locates the key by this exact file name.
        raise ValueError(f"key file must be named AuthKey_{key_id}.p8")
    return path, key_id, issuer


def testflight_info(version: str, build: str, sdk: dict[str, str] | None = None) -> dict:
    info = spike.bundle_info(True)
    info.update(
        CFBundleDisplayName=DISPLAY_NAME, CFBundleName=DISPLAY_NAME, CFBundleIdentifier=BUNDLE_ID,
        CFBundleShortVersionString=version, CFBundleVersion=validate_build_number(build),
        CFBundleSupportedPlatforms=["iPhoneOS"], UIRequiredDeviceCapabilities=["arm64"],
        CFBundleIconName="AppIcon",
        # SSH uses only standard encryption; avoids the per-build compliance prompt.
        ITSAppUsesNonExemptEncryption=False,
    )
    info.update(sdk or {})
    return info


def validate_info(info: dict, version: str) -> None:
    problems = []
    if info.get("CFBundleIdentifier") != BUNDLE_ID:
        problems.append("bundle identifier")
    if info.get("CFBundleShortVersionString") != version:
        problems.append("marketing version does not match the workspace")
    if not re.fullmatch(r"[1-9][0-9]{0,17}", str(info.get("CFBundleVersion", ""))):
        problems.append("build number")
    if info.get("MinimumOSVersion") != spike.MINIMUM_IOS:
        problems.append("minimum iOS version")
    if info.get("UIDeviceFamily") != [1, 2]:
        problems.append("device family")
    if info.get("UIRequiredDeviceCapabilities") != ["arm64"]:
        problems.append("required capabilities")
    if info.get("ITSAppUsesNonExemptEncryption") is not False:
        problems.append("export compliance flag")
    if len(info.get("UISupportedInterfaceOrientations~ipad", [])) != 4:
        problems.append("iPad orientations")
    for key in ("NSFaceIDUsageDescription", "NSLocalNetworkUsageDescription", "CFBundleIconName"):
        if not info.get(key):
            problems.append(key)
    if info.get("CFBundleSupportedPlatforms") != ["iPhoneOS"]:
        problems.append("supported platforms")
    if problems:
        raise ValueError("Info.plist invalid: " + ", ".join(problems))


def verify_entitlements(entitlements: dict, team: str = TEAM_ID, bundle: str = BUNDLE_ID) -> None:
    """Check the entitlements Xcode embedded in the distribution-signed app."""
    problems = []
    if entitlements.get("application-identifier") != f"{team}.{bundle}":
        problems.append("application-identifier")
    if entitlements.get("com.apple.developer.team-identifier") != team:
        problems.append("team identifier")
    if entitlements.get("get-task-allow"):
        problems.append("get-task-allow must be absent")
    groups = entitlements.get("keychain-access-groups", [])
    if any(not (group == f"{team}.{bundle}" or group.startswith(f"{team}.")) for group in groups):
        problems.append("keychain access group outside the team prefix")
    if problems:
        raise ValueError("Distribution entitlements invalid: " + ", ".join(problems))


def export_options(team: str = TEAM_ID) -> dict:
    return {"method": "app-store-connect", "teamID": team, "destination": "export",
            "signingStyle": "manual", "signingCertificate": "Apple Distribution",
            "provisioningProfiles": {BUNDLE_ID: PROFILE_NAME},
            "uploadSymbols": True, "manageAppVersionAndBuildNumber": False}


def distribution_identity(listing: str, team: str = TEAM_ID) -> str | None:
    """SHA-1 of a valid local Apple Distribution identity for the team, if any."""
    for sha, name in re.findall(r'\)\s+([0-9A-Fa-f]{40})\s+"([^"]+)"', listing):
        if name.startswith("Apple Distribution:") and name.endswith(f"({team})"):
            return sha.upper()
    return None


def profile_request(bundle_resource: str, certificate_id: str) -> dict:
    return {"data": {"type": "profiles",
                     "attributes": {"name": PROFILE_NAME, "profileType": "IOS_APP_STORE"},
                     "relationships": {
                         "bundleId": {"data": {"type": "bundleIds", "id": bundle_resource}},
                         "certificates": {"data": [{"type": "certificates", "id": certificate_id}]}}}}


def build_filter_path(app_id: str, version: str) -> str:
    return (f"/v1/builds?filter[app]={app_id}&filter[preReleaseVersion.version]={version}"
            "&sort=-uploadedDate&limit=20&fields[builds]=version,processingState,uploadedDate,expired")


def api_token(key_path: Path, key_id: str, issuer: str, now: int | None = None) -> str:
    import jwt  # PyJWT is needed only for API calls, not the portable checks.
    now = int(time.time()) if now is None else now
    return jwt.encode({"iss": issuer, "iat": now, "exp": now + 900, "aud": "appstoreconnect-v1"},
                      key_path.read_text(), algorithm="ES256", headers={"kid": key_id})


def api(key: tuple[Path, str, str], path: str, method: str = "GET", body: dict | None = None) -> dict:
    import ssl
    import certifi
    request = urllib.request.Request(
        API + path, method=method, data=json.dumps(body).encode() if body else None,
        headers={"Authorization": f"Bearer {api_token(*key)}", "Content-Type": "application/json"})
    context = ssl.create_default_context(cafile=certifi.where())
    try:
        with urllib.request.urlopen(request, context=context, timeout=60) as response:
            return json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"App Store Connect API {method} {path.split('?')[0]} returned {error.code}") from None


def archive_info(version: str, build: str, team: str = TEAM_ID) -> dict:
    return {"ArchiveVersion": 2, "Name": DISPLAY_NAME, "SchemeName": DISPLAY_NAME,
            "CreationDate": datetime.now(timezone.utc),
            "ApplicationProperties": {
                "ApplicationPath": f"Applications/{DISPLAY_NAME}.app", "CFBundleIdentifier": BUNDLE_ID,
                "CFBundleShortVersionString": version, "CFBundleVersion": build,
                "SigningIdentity": "-", "Team": team}}


def check_icon(path: Path = ICON_SOURCE) -> None:
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError("icon is not a PNG")
    width, height = int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")
    if (width, height) != (1024, 1024):
        raise ValueError("icon must be 1024x1024")


def run(*args: str, timeout: int = 600, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, timeout=timeout, **kwargs)


def flatten_icon(destination: Path) -> None:
    """Fill the tile to the canvas edge on its own colour: App Store icons have no alpha."""
    from PIL import Image  # Pillow is needed only for packaging, not the portable checks.
    icon = Image.open(ICON_SOURCE).convert("RGBA")
    colour = icon.getpixel((512, 80))[:3]
    margin = round(1024 * (1 - 0.906) / 2)
    cropped = icon.crop((margin, margin, 1024 - margin, 1024 - margin)).resize((1024, 1024), Image.LANCZOS)
    canvas = Image.new("RGB", (1024, 1024), colour)
    canvas.paste(cropped, mask=cropped.getchannel("A"))
    canvas.save(destination, "PNG")


def build_assets(bundle: Path, work: Path, sdk_version: str) -> dict:
    catalog = work / "Assets.xcassets"
    iconset = catalog / "AppIcon.appiconset"
    iconset.mkdir(parents=True)
    flatten_icon(iconset / "icon-1024.png")
    (catalog / "Contents.json").write_text(json.dumps({"info": {"author": "xcode", "version": 1}}))
    (iconset / "Contents.json").write_text(json.dumps({
        "images": [{"filename": "icon-1024.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"}],
        "info": {"author": "xcode", "version": 1}}))
    partial = work / "assets-partial.plist"
    run("xcrun", "actool", str(catalog), "--compile", str(bundle), "--platform", "iphoneos",
        "--minimum-deployment-target", spike.MINIMUM_IOS, "--target-device", "iphone",
        "--target-device", "ipad", "--app-icon", "AppIcon", "--output-partial-info-plist", str(partial),
        "--output-format", "human-readable-text", "--notices", "--warnings", "--errors")
    return plistlib.loads(partial.read_bytes())


def sdk_keys() -> dict[str, str]:
    def out(*args):
        return run(*args, capture_output=True, text=True).stdout.strip()
    sdk = out("xcrun", "--sdk", "iphoneos", "--show-sdk-version")
    build = out("xcrun", "--sdk", "iphoneos", "--show-sdk-build-version")
    xcode_version, xcode_build = re.findall(r"Xcode (\S+)\s+Build version (\S+)", out("xcodebuild", "-version"))[0]
    return {"DTPlatformName": "iphoneos", "DTSDKName": f"iphoneos{sdk}", "DTPlatformVersion": sdk,
            "DTSDKBuild": build, "DTPlatformBuild": build, "DTXcodeBuild": xcode_build,
            "DTXcode": xcode_version.replace(".", "").rjust(4, "0"), "DTCompiler": "com.apple.compilers.llvm.clang.1_0",
            "BuildMachineOSBuild": out("sw_vers", "-buildVersion")}


def archive(args, key: tuple[Path, str, str], version: str, build: str) -> Path:
    spike.check_dependencies(TARGET, live_ssh=True)
    run("rustup", "target", "add", TARGET)
    sdk = run("xcrun", "--sdk", "iphoneos", "--show-sdk-path", capture_output=True, text=True).stdout.strip()
    env = dict(os.environ, SDKROOT=sdk, IPHONEOS_DEPLOYMENT_TARGET=spike.MINIMUM_IOS)
    run("cargo", "build", "--release", "--locked", "-p", "festerm-mobile", "--target", TARGET,
        "--features", FEATURES, "--target-dir", str(ROOT / "target"), env=env, timeout=3600)
    work = OUTPUT / f"{version}-{build}"
    if work.exists():
        raise ValueError(f"{work} exists; choose another build number")
    app = work / "fesTerm.xcarchive" / "Products" / "Applications" / f"{DISPLAY_NAME}.app"
    app.mkdir(parents=True)
    shutil.copy2(ROOT / "target" / TARGET / "release" / "festerm-mobile", app / "festerm-mobile")
    keys = sdk_keys()
    icons = build_assets(app, work / "assets", keys["DTPlatformVersion"])
    info = testflight_info(version, build, keys)
    info.update(icons)
    validate_info(info, version)
    (app / "Info.plist").write_bytes(plistlib.dumps(info))
    run("codesign", "--force", "--sign", "-", str(app))
    archive_dir = work / "fesTerm.xcarchive"
    (archive_dir / "Info.plist").write_bytes(plistlib.dumps(archive_info(version, build, args.team)))
    options = work / "ExportOptions.plist"
    options.write_bytes(plistlib.dumps(export_options(args.team)))
    path, key_id, issuer = key
    run("xcodebuild", "-exportArchive", "-archivePath", str(archive_dir), "-exportPath", str(work / "export"),
        "-exportOptionsPlist", str(options), "-allowProvisioningUpdates",
        "-authenticationKeyPath", str(path), "-authenticationKeyID", key_id,
        "-authenticationKeyIssuerID", issuer, timeout=1800)
    ipa = work / "export" / IPA_NAME
    if not ipa.is_file():
        found = sorted((work / "export").glob("*.ipa"))
        if not found:
            raise RuntimeError("export produced no .ipa")
        ipa = found[0]
    check_ipa(ipa, args.team, version, build)
    return ipa


def check_ipa(ipa: Path, team: str, version: str, build: str) -> None:
    with zipfile.ZipFile(ipa) as archive_file:
        names = [n for n in archive_file.namelist() if re.fullmatch(r"Payload/[^/]+\.app/Info\.plist", n)]
        if len(names) != 1:
            raise ValueError("ipa must contain exactly one app")
        info = plistlib.loads(archive_file.read(names[0]))
        app_dir = names[0].rsplit("/", 1)[0]
        extracted = OUTPUT / "verify"
        shutil.rmtree(extracted, ignore_errors=True)
        archive_file.extractall(extracted)
    if (info["CFBundleIdentifier"], info["CFBundleShortVersionString"], info["CFBundleVersion"]) != (BUNDLE_ID, version, build):
        raise ValueError("ipa Info.plist does not match the requested identity/version/build")
    app = extracted / app_dir
    run("codesign", "--verify", "--strict", str(app))
    result = run("codesign", "-d", "--entitlements", ":-", str(app), capture_output=True)
    verify_entitlements(plistlib.loads(result.stdout), team)
    shutil.rmtree(extracted, ignore_errors=True)


def upload(ipa: Path, key: tuple[Path, str, str]) -> None:
    path, key_id, issuer = key
    env = dict(os.environ, API_PRIVATE_KEYS_DIR=str(path.parent))
    run("xcrun", "altool", "--upload-app", "--type", "ios", "--file", str(ipa),
        "--apiKey", key_id, "--apiIssuer", issuer, env=env, timeout=3600)


def local_distribution_identity(team: str) -> str | None:
    listing = run("security", "find-identity", "-v", "-p", "codesigning", capture_output=True, text=True).stdout
    return distribution_identity(listing, team)


def setup_signing(key: tuple[Path, str, str], team: str) -> None:
    work = OUTPUT / "signing"
    work.mkdir(parents=True, exist_ok=True, mode=0o700)
    certificates = api(key, "/v1/certificates?filter[certificateType]=DISTRIBUTION&limit=50")["data"]
    by_sha = {hashlib.sha1(base64.b64decode(c["attributes"]["certificateContent"])).hexdigest().upper(): c["id"]
              for c in certificates}
    identity = local_distribution_identity(team)
    if identity in by_sha:
        certificate_id = by_sha[identity]
    else:
        # No usable local identity: create one. The private key lives only in the login keychain afterwards.
        run("openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", str(work / "dist.key"),
            "-out", str(work / "dist.csr"), "-subj", "/CN=fesTerm Distribution/O=fesTerm/C=US",
            capture_output=True)
        os.chmod(work / "dist.key", 0o600)
        created = api(key, "/v1/certificates", "POST", {"data": {"type": "certificates", "attributes": {
            "certificateType": "DISTRIBUTION", "csrContent": (work / "dist.csr").read_text()}}})["data"]
        certificate_id = created["id"]
        der = base64.b64decode(created["attributes"]["certificateContent"])
        (work / "dist.cer").write_bytes(der)
        run("openssl", "x509", "-inform", "DER", "-in", str(work / "dist.cer"), "-out", str(work / "dist.pem"))
        password = base64.b16encode(os.urandom(12)).decode()
        run("openssl", "pkcs12", "-export", "-legacy", "-inkey", str(work / "dist.key"), "-in", str(work / "dist.pem"),
            "-out", str(work / "dist.p12"), "-passout", f"pass:{password}", capture_output=True)
        run("security", "import", str(work / "dist.p12"), "-k", str(Path.home() / "Library/Keychains/login.keychain-db"),
            "-P", password, "-T", "/usr/bin/codesign", "-T", "/usr/bin/security", capture_output=True)
        shutil.rmtree(work)
        if not local_distribution_identity(team):
            raise RuntimeError("distribution identity was not imported into the keychain")
    bundle = api(key, f"/v1/bundleIds?filter[identifier]={BUNDLE_ID}&limit=5")["data"]
    bundle = [b for b in bundle if b["attributes"]["identifier"] == BUNDLE_ID]
    if not bundle:
        raise RuntimeError(f"{BUNDLE_ID} is not registered; create the app record first")
    profiles = api(key, f"/v1/bundleIds/{bundle[0]['id']}/profiles?limit=50")["data"]
    chosen = None
    for profile in profiles:
        attrs = profile["attributes"]
        if attrs["profileType"] != "IOS_APP_STORE" or attrs["name"] != PROFILE_NAME:
            continue
        ids = {c["id"] for c in api(key, f"/v1/profiles/{profile['id']}/certificates")["data"]}
        if attrs["profileState"] == "ACTIVE" and certificate_id in ids:
            chosen = profile
        else:
            api(key, f"/v1/profiles/{profile['id']}", "DELETE")
    if chosen is None:
        chosen = api(key, "/v1/profiles", "POST", profile_request(bundle[0]["id"], certificate_id))["data"]
    attrs = chosen["attributes"]
    PROFILE_DIR.mkdir(parents=True, exist_ok=True)
    (PROFILE_DIR / f"{attrs['uuid']}.mobileprovision").write_bytes(base64.b64decode(attrs["profileContent"]))
    print(f"Signing ready: distribution identity and profile '{PROFILE_NAME}' installed")


def show_status(key: tuple[Path, str, str], version: str) -> list[dict]:
    apps = api(key, f"/v1/apps?filter[bundleId]={BUNDLE_ID}&limit=5")["data"]
    if not apps:
        raise RuntimeError(f"no App Store Connect app for {BUNDLE_ID}")
    builds = api(key, build_filter_path(apps[0]["id"], version))["data"]
    for build in builds:
        attrs = build["attributes"]
        print(f"{version} ({attrs['version']}): {attrs['processingState']}, expired={attrs['expired']}")
    if not builds:
        print(f"No builds of {version} yet")
    return builds


def latest_ipa(version: str, build: str | None) -> Path:
    candidates = sorted(OUTPUT.glob(f"{version}-{build or '*'}/export/*.ipa"))
    if not candidates:
        raise ValueError("no exported .ipa found; run --archive first")
    return candidates[-1]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="offline preflight; no network or compile")
    parser.add_argument("--archive", action="store_true", help="build, sign and export the .ipa")
    parser.add_argument("--setup-signing", action="store_true", help="ensure distribution identity and App Store profile")
    parser.add_argument("--status", action="store_true", help="show App Store Connect processing state")
    parser.add_argument("--upload", action="store_true", help="upload the exported .ipa to App Store Connect")
    parser.add_argument("--build-number", default=None, help="CFBundleVersion (default: UTC yymmddHHMM)")
    parser.add_argument("--team", default=TEAM_ID)
    parser.add_argument("--key-path")
    parser.add_argument("--key-id")
    parser.add_argument("--issuer")
    args = parser.parse_args(argv)
    if not (args.check or args.archive or args.upload or args.setup_signing or args.status):
        parser.error("choose an action; see --help")
    if platform.system() != "Darwin":
        parser.error("TestFlight packaging requires macOS and full Xcode")
    validate_team(args.team)
    version = workspace_version()
    build = validate_build_number(args.build_number or default_build_number())
    key = api_credentials(args.key_path, args.key_id, args.issuer)
    check_icon()
    validate_info(testflight_info(version, build), version)
    if args.check:
        print(f"Preflight passed: {BUNDLE_ID} {version} ({build}), team {args.team}, key {key[1]}")
    if args.setup_signing:
        setup_signing(key, args.team)
    if args.archive:
        print(archive(args, key, version, build))
    if args.upload:
        ipa = latest_ipa(version, build if args.archive or args.build_number else None)
        upload(ipa, key)
        print(f"Uploaded {BUNDLE_ID} {version} ({ipa.parent.parent.name.split('-', 1)[1]})")
    if args.status:
        show_status(key, version)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError, RuntimeError) as error:
        # Subprocess arguments contain the key ID/issuer; report only the failing tool.
        detail = f"{Path(error.cmd[0]).name} exited {error.returncode}" if isinstance(error, subprocess.CalledProcessError) \
            else (type(error).__name__ if isinstance(error, (subprocess.TimeoutExpired, OSError)) else str(error))
        print(f"TestFlight build failed: {detail}", file=sys.stderr)
        sys.exit(1)
