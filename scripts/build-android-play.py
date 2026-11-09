#!/usr/bin/env python3
"""Build and verify an unsigned Android App Bundle for separate Play signing."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
import zipfile


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("android_build", ROOT / "scripts/build-android.py")
android = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(android)
ANDROID = "{http://schemas.android.com/apk/res/android}"
PACKAGE = "io.github.fes.festerm"
ABIS = ("arm64-v8a", "x86_64")
BUNDLE_CONFIG = {"optimizations": {"uncompressNativeLibraries": {"enabled": False}}}
SIGNING_ENV = (
    "ANDROID_UPLOAD_KEYSTORE", "ANDROID_UPLOAD_KEYSTORE_BASE64",
    "ANDROID_UPLOAD_KEYSTORE_PASSWORD", "PLAY_ACCESS_TOKEN",
)


def manifest_details(xml, expected_version):
    root = ET.fromstring(xml)
    if root.tag != "manifest" or root.get("package") != PACKAGE:
        raise ValueError("Android package identifier does not match the Play app")
    version = root.get(ANDROID + "versionName")
    code = root.get(ANDROID + "versionCode", "")
    if version != expected_version or not re.fullmatch(r"[1-9][0-9]*", code):
        raise ValueError("Android version name/code does not match the release")
    if int(code) > android.MAX_VERSION_CODE:
        raise ValueError("Android versionCode exceeds the Google Play limit")
    sdk = root.find("uses-sdk")
    app = root.find("application")
    if sdk is None or sdk.get(ANDROID + "minSdkVersion") != str(android.MIN_API) \
            or sdk.get(ANDROID + "targetSdkVersion") != str(android.API):
        raise ValueError("Android minimum or target SDK does not match the release toolchain")
    if app is None or app.get(ANDROID + "debuggable", "false") != "false" \
            or app.get(ANDROID + "extractNativeLibs") != "true":
        raise ValueError("Play bundles must not be debuggable and must extract native libraries")
    return version, int(code)


def bundletool(root=ROOT):
    pin = json.loads((root / "app/festerm-android/dependencies.json").read_text())["bundletool"]
    version = pin["version"]
    expected_url = f"https://github.com/google/bundletool/releases/download/{version}/bundletool-all-{version}.jar"
    if not re.fullmatch(r"\d+\.\d+\.\d+", version) \
            or pin["name"] != f"bundletool-all-{version}.jar" or pin["url"] != expected_url \
            or not re.fullmatch(r"[0-9a-f]{64}", pin["sha256"]):
        raise ValueError("Invalid bundletool version, URL, or checksum pin")
    path = root / "target/android-tools" / f"bundletool-all-{version}.jar"
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        for delay in (5, 15, 30, 60, 120, 0):
            try:
                with urllib.request.urlopen(pin["url"], timeout=120) as response:
                    data = response.read(64 * 1024 * 1024 + 1)
                break
            except urllib.error.HTTPError as error:
                if error.code not in (502, 503, 504) or delay == 0:
                    raise
                print(f"Bundletool download returned HTTP {error.code}; retrying in {delay}s",
                      file=sys.stderr, flush=True)
                time.sleep(delay)
        if len(data) > 64 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != pin["sha256"]:
            raise ValueError("Downloaded bundletool checksum mismatch")
        with path.open("xb") as destination:
            destination.write(data)
    if path.is_symlink():
        raise ValueError("Cached bundletool must be a regular file")
    with path.open("rb") as source:
        actual = hashlib.file_digest(source, "sha256").hexdigest()
    if actual != pin["sha256"]:
        raise ValueError("Cached bundletool checksum mismatch")
    return path, version


def run(*args, env=None, capture=False, timeout=600):
    return subprocess.run(
        [str(arg) for arg in args], cwd=ROOT, env=env, check=True, timeout=timeout,
        text=True, capture_output=capture,
    ).stdout


def verify_native_library(library, abi, sdk, env):
    host = {"Darwin": "darwin-x86_64", "Linux": "linux-x86_64"}.get(platform.system())
    if host is None or abi not in ABIS:
        raise ValueError(f"Unsupported Android packaging host or ABI: {abi}")
    readelf = sdk / "ndk" / android.NDK / "toolchains/llvm/prebuilt" / host / "bin/llvm-readelf"
    header = run(readelf, "-h", library, env=env, capture=True)
    machine = "AArch64" if abi == "arm64-v8a" else "Advanced Micro Devices X86-64"
    if not re.search(rf"(?m)^\s*Machine:\s*{re.escape(machine)}\s*$", header):
        raise ValueError(f"Wrong Android native library architecture for {abi}")
    segments = run(readelf, "-lW", library, env=env, capture=True)
    alignments = [
        int(line.split()[-1], 16)
        for line in segments.splitlines() if line.strip().startswith("LOAD ")
    ]
    if not alignments or any(value < 0x4000 or value % 0x4000 for value in alignments):
        raise ValueError(f"Android native library is not 16-KiB aligned: {abi}")


def make_module(path, resources, dex_files, libraries, root=ROOT):
    entries = {}
    with zipfile.ZipFile(resources) as archive:
        for info in archive.infolist():
            if info.is_dir():
                continue
            name = info.filename
            if name == "AndroidManifest.xml":
                target = "manifest/AndroidManifest.xml"
            elif name == "resources.pb" or name.startswith("res/"):
                target = name
            else:
                raise ValueError(f"Unexpected Android proto resource entry: {name}")
            if target in entries or ".." in Path(target).parts:
                raise ValueError(f"Duplicate or unsafe Android module entry: {target}")
            entries[target] = archive.read(info)
    if "manifest/AndroidManifest.xml" not in entries or "resources.pb" not in entries:
        raise ValueError("Android proto resources are incomplete")
    for dex in dex_files:
        entries[f"dex/{dex.name}"] = dex.read_bytes()
    for abi, library in libraries.items():
        entries[f"lib/{abi}/libfesterm_android.so"] = library.read_bytes()
    entries["assets/NOTICE.txt"] = (root / "app/festerm-android/NOTICE.txt").read_bytes()
    entries["assets/licenses/Apache-2.0.txt"] = (root / "vendor/egui-winit/LICENSE-APACHE").read_bytes()
    entries["assets/THIRD_PARTY_NOTICES.txt"] = (root / "packaging/THIRD_PARTY_NOTICES.txt").read_bytes()
    if not all(entries.values()):
        raise ValueError("Android module has an empty required entry")
    with zipfile.ZipFile(path, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, data in sorted(entries.items()):
            archive.writestr(name, data)


def verify_bundle_contents(bundle, libraries, root=ROOT):
    with zipfile.ZipFile(bundle) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError("Android bundle contains duplicate ZIP entries")
        expected = {
            "base/manifest/AndroidManifest.xml", "base/resources.pb", "base/dex/classes.dex",
            "base/assets/THIRD_PARTY_NOTICES.txt", "base/assets/NOTICE.txt",
            "base/assets/licenses/Apache-2.0.txt",
        }
        expected.update(f"base/lib/{abi}/libfesterm_android.so" for abi in libraries)
        if not expected <= set(names):
            raise ValueError("Android bundle is missing required application entries")
        if {name for name in names if name.startswith("base/lib/")} != \
                {f"base/lib/{abi}/libfesterm_android.so" for abi in libraries}:
            raise ValueError("Android bundle contains unexpected native libraries")
        for asset, path in (
            ("THIRD_PARTY_NOTICES.txt", "packaging/THIRD_PARTY_NOTICES.txt"),
            ("NOTICE.txt", "app/festerm-android/NOTICE.txt"),
            ("licenses/Apache-2.0.txt", "vendor/egui-winit/LICENSE-APACHE"),
        ):
            if archive.read("base/assets/" + asset) != (root / path).read_bytes():
                raise ValueError(f"Android bundle does not contain the reviewed notices: {asset}")
        for abi, library in libraries.items():
            with library.open("rb") as source:
                expected_hash = hashlib.file_digest(source, "sha256").digest()
            if hashlib.sha256(archive.read(f"base/lib/{abi}/libfesterm_android.so")).digest() != expected_hash:
                raise ValueError(f"Android bundle contains the wrong native library for {abi}")


def build(args):
    present = [name for name in SIGNING_ENV if os.environ.get(name)]
    if present:
        raise ValueError("Remove signing and Play credentials before building: " + ", ".join(present))
    if not 1 <= args.version_code <= android.MAX_VERSION_CODE:
        raise ValueError(f"--version-code must be between 1 and {android.MAX_VERSION_CODE}")
    env = dict(os.environ, LC_ALL="C")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    version, _ = manifest_details((android.APP / "AndroidManifest.xml").read_text(), version)
    code = args.version_code
    dirty = bool(run("git", "status", "--porcelain", capture=True).strip())
    if dirty and not args.validation_only:
        raise ValueError("Play release bundle requires a clean worktree")
    run(sys.executable, "scripts/build_app_notices.py", "--check", env=env, timeout=1200)
    tool, tool_version = bundletool()
    sdk = Path(args.sdk).expanduser().resolve()
    contexts = {abi: android.target_environment(sdk, abi) for abi in ABIS}
    for _, _, _, build_env, _ in contexts.values():
        build_env["LC_ALL"] = "C"
    dependencies = android.native_dependencies()
    libraries = {}
    for abi, (_, _, target, build_env, features) in contexts.items():
        libraries[abi] = android.build_library(target, build_env, features, True)
        verify_native_library(libraries[abi], abi, sdk, build_env)
    output = Path(args.output_dir).expanduser() if args.output_dir else (
        ROOT / "target/android-play" / ("validation" if args.validation_only else f"{version}-{code}")
    )
    output.mkdir(parents=True, exist_ok=True)
    destination = output / "fesTerm-unsigned.aab"
    receipt = output / "build-receipt.json"
    if destination.exists() or receipt.exists():
        raise ValueError("Refusing to overwrite an existing Android bundle or receipt")
    with tempfile.TemporaryDirectory(prefix="bundle-", dir=output) as directory:
        work = Path(directory)
        tools, android_jar, _, build_env, _ = contexts[ABIS[0]]
        resources, dex = android.compile_app(
            work, tools, android_jar, build_env, dependencies, proto_format=True, version_code=code,
        )
        module = work / "base.zip"
        make_module(module, resources, dex, libraries)
        config = work / "BundleConfig.json"
        config.write_text(json.dumps(BUNDLE_CONFIG))
        bundle = work / "fesTerm-unsigned.aab"
        run("java", "-jar", tool, "build-bundle", f"--modules={module}", f"--output={bundle}",
            f"--config={config}", env=env, timeout=1200)
        run("java", "-jar", tool, "validate", f"--bundle={bundle}", env=env, timeout=600)
        xml = run("java", "-jar", tool, "dump", "manifest", f"--bundle={bundle}", env=env, capture=True)
        if manifest_details(xml, version) != (version, code):
            raise ValueError("Android bundle manifest differs from the release manifest")
        verify_bundle_contents(bundle, libraries)
        os.link(bundle, destination)
    with destination.open("rb") as source:
        sha256 = hashlib.file_digest(source, "sha256").hexdigest()
    record = {
        "commit": run("git", "rev-parse", "HEAD", capture=True).strip(),
        "dirty": dirty, "validation_only": args.validation_only, "package": PACKAGE,
        "version_name": version, "version_code": code, "min_sdk": android.MIN_API,
        "target_sdk": android.API, "abis": list(ABIS),
        "signing": "unsigned", "bundletool": tool_version, "sha256": sha256,
    }
    receipt.write_text(json.dumps(record, indent=2) + "\n")
    print(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", default=os.environ.get("ANDROID_HOME", str(Path.home() / "Library/Android/sdk")))
    parser.add_argument("--version-code", type=int, required=True, help="unique Play versionCode")
    parser.add_argument("--output-dir", help="new output directory for the bundle and its receipt")
    parser.add_argument("--validation-only", action="store_true", help="allow a dirty worktree and mark the bundle test-only")
    build(parser.parse_args())


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, ET.ParseError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        print(f"Android Play bundle failed: {error}", file=sys.stderr)
        sys.exit(1)
