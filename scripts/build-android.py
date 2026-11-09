#!/usr/bin/env python3
"""Build a development APK, or an unsigned Play release AAB, using the
Android SDK/NDK and the shared Rust host.

Release AABs are signed separately by sign-android-bundle.py so the upload key
is never present while dependency build scripts run (ADR 0083)."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "app" / "festerm-android"
API = 35
MIN_API = 29
BUILD_TOOLS = "35.0.0"
NDK = "27.2.12479018"
TARGETS = {
    "arm64-v8a": ("aarch64-linux-android", "aarch64-linux-android"),
    "x86_64": ("x86_64-linux-android", "x86_64-linux-android"),
}
PACKAGE = "io.github.fes.festerm"
MAX_VERSION_CODE = 2_100_000_000
JVM_DEPENDENCY_LIMIT = 8 * 1024 * 1024
BUNDLETOOL_LIMIT = 64 * 1024 * 1024
# Keep Play-generated APKs equivalent to the development APK, whose manifest
# extracts native libraries rather than mapping them uncompressed.
BUNDLE_CONFIG = {"optimizations": {"uncompressNativeLibraries": {"enabled": False}}}
FORBIDDEN = {
    "festerm-pty", "festerm-sessiond", "festerm-serial", "portable-pty",
    "cargo-packager-updater", "zbus", "zbus-secret-service-keyring-store",
}


def run(*args, env=None, capture=False, timeout=None):
    return subprocess.run(
        [str(arg) for arg in args], cwd=ROOT, env=env, check=True,
        text=True, stdout=subprocess.PIPE if capture else None, timeout=timeout,
    ).stdout


def check_dependency_tree(tree):
    names = {line.split()[0] for line in tree.splitlines() if line.strip()}
    forbidden = sorted(names & FORBIDDEN)
    if forbidden:
        raise ValueError("Desktop-only Android dependencies: " + ", ".join(forbidden))
    for required in ("festerm-mobile", "festerm-core", "festerm-ui-egui"):
        if required not in names:
            raise ValueError("Missing shared Android dependency: " + required)

def download(url, limit, attempts=4):
    """Retries transient (5xx/network) failures; the checksum still decides trust."""
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(url, timeout=60) as response:
                return response.read(limit + 1)
        except urllib.error.HTTPError as error:
            error.close()
            if error.code < 500 or attempt == attempts:
                raise
        except (urllib.error.URLError, TimeoutError):
            if attempt == attempts:
                raise
        time.sleep(5 * 2 ** (attempt - 1))


def fetch_pinned(artifact, limit):
    directory = ROOT / "target" / "android-dependencies"
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / artifact["name"]
    if not path.exists():
        data = download(artifact["url"], limit)
        if len(data) > limit:
            raise ValueError("Android dependency exceeds the download bound")
        if hashlib.sha256(data).hexdigest() != artifact["sha256"]:
            raise ValueError("Android dependency checksum mismatch: " + artifact["name"])
        path.write_bytes(data)
    with path.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != artifact["sha256"]:
            raise ValueError("Android dependency checksum mismatch: " + artifact["name"])
    return path


def native_dependencies():
    manifest = json.loads((APP / "dependencies.json").read_text())
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    version = next(p["version"] for p in lock["package"] if p["name"] == "rustls-platform-verifier-android")
    if version != manifest["rustls_android_version"]:
        raise ValueError("Update the Android TLS artifact pins to match Cargo.lock")
    return [fetch_pinned(artifact, JVM_DEPENDENCY_LIMIT) for artifact in manifest["artifacts"]]


def bundletool():
    manifest = json.loads((APP / "dependencies.json").read_text())
    return fetch_pinned(manifest["bundletool"], BUNDLETOOL_LIMIT)


def check_release_version():
    """The Play bundle's user-visible version must be the workspace release."""
    manifest = (APP / "AndroidManifest.xml").read_text(encoding="utf-8")
    match = re.search(r'android:versionName="([^"]+)"', manifest)
    if match is None:
        raise ValueError("AndroidManifest.xml has no versionName")
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if match.group(1) != workspace:
        raise ValueError(
            f"AndroidManifest.xml versionName {match.group(1)} does not match workspace {workspace}")
    return workspace


def validate_arguments(args):
    if args.recording_validation and args.release:
        raise ValueError("Recording validation hooks are not allowed in release APKs")
    if args.version_code is not None and not 1 <= args.version_code <= MAX_VERSION_CODE:
        raise ValueError(f"--version-code must be between 1 and {MAX_VERSION_CODE}")
    if args.bundle:
        if not args.release:
            raise ValueError("--bundle requires --release")
        if args.version_code is None:
            raise ValueError("--bundle requires an explicit --version-code")
        conflicts = [flag for flag, enabled in (
            ("--abi", args.abi is not None), ("--offline", args.offline),
            ("--recording-validation", args.recording_validation), ("--install", args.install),
        ) if enabled]
        if conflicts:
            raise ValueError("--bundle cannot be combined with " + ", ".join(conflicts))


def write_base_module(proto, base, dex, libraries):
    """Rearranges aapt2's proto-format APK into bundletool's base-module layout."""
    with zipfile.ZipFile(proto) as source, \
            zipfile.ZipFile(base, "w", compression=zipfile.ZIP_DEFLATED) as module:
        for entry in source.infolist():
            name = entry.filename
            if entry.is_dir():
                continue
            if name == "AndroidManifest.xml":
                destination = "manifest/AndroidManifest.xml"
            elif name == "resources.pb" or name.startswith("res/"):
                destination = name
            else:
                raise ValueError(f"Unexpected aapt2 proto entry: {name}")
            module.writestr(destination, source.read(entry))
        module.write(dex / "classes.dex", "dex/classes.dex")
        for abi, library in libraries:
            module.write(library, f"lib/{abi}/libfesterm_android.so")
        module.write(APP / "NOTICE.txt", "assets/NOTICE.txt")
        module.write(ROOT / "vendor/egui-winit/LICENSE-APACHE", "assets/licenses/Apache-2.0.txt")


def bundletool_value(tool, bundle, xpath, env):
    return run("java", "-jar", tool, "dump", "manifest", f"--bundle={bundle}",
               f"--xpath={xpath}", env=env, capture=True).strip()


def build(args):
    validate_arguments(args)
    abis = list(TARGETS) if args.bundle else [args.abi or "arm64-v8a"]
    sdk = Path(args.sdk).expanduser().resolve()
    tools = sdk / "build-tools" / BUILD_TOOLS
    android_jar = sdk / "platforms" / f"android-{API}" / "android.jar"
    host = {"Darwin": "darwin-x86_64", "Linux": "linux-x86_64"}.get(platform.system())
    if host is None:
        raise ValueError("Use macOS or Linux for the Android development packager")
    llvm = sdk / "ndk" / NDK / "toolchains" / "llvm" / "prebuilt" / host / "bin"
    for path in (tools / "aapt2", tools / "d8", tools / "zipalign", tools / "apksigner", android_jar):
        if not path.is_file():
            raise ValueError(f"Missing Android tool: {path}")
    commands = ("cargo", "javac") + (("java",) if args.bundle else ("keytool",))
    for command in commands:
        if shutil.which(command) is None:
            raise ValueError(f"Missing build tool: {command}")
    version_name = None
    if args.bundle:
        version_name = check_release_version()
        if run("git", "status", "--porcelain", capture=True):
            raise ValueError("Play bundles must be built from a clean checkout")
    base_env = os.environ.copy()
    base_env["ANDROID_HOME"] = str(sdk)
    base_env["ANDROID_NDK_HOME"] = str(sdk / "ndk" / NDK)
    base_env["RUSTFLAGS"] = base_env.get("RUSTFLAGS", "") + " -C link-arg=-Wl,-z,max-page-size=16384"
    base_env.setdefault("CARGO_PROFILE_DEV_DEBUG", "0")
    features = ["--no-default-features"] if args.offline else []
    if args.recording_validation:
        features += ["--features", "native-recording-validation"]
    builds = []
    for abi in abis:
        target, clang_target = TARGETS[abi]
        env = base_env.copy()
        linker = llvm / f"{clang_target}{MIN_API}-clang"
        if not linker.is_file():
            raise ValueError(f"Missing Android NDK compiler: {linker}")
        key = target.replace("-", "_")
        env[f"CARGO_TARGET_{key.upper()}_LINKER"] = str(linker)
        env[f"CC_{key}"] = str(linker)
        env[f"AR_{key}"] = str(llvm / "llvm-ar")
        tree = run("cargo", "tree", "--locked", "-p", "festerm-android", "--target", target,
                   "--edges", "normal,build", "--prefix", "none", *features, env=env, capture=True)
        check_dependency_tree(tree)
        builds.append((abi, target, env))
    if args.check_dependencies:
        return
    dependencies = native_dependencies()
    mode = "release" if args.release else "debug"
    if args.bundle:
        output = ROOT / "target" / "android" / "bundle"
    else:
        output = ROOT / "target" / "android" / abis[0] / ("offline" if args.offline else "live")
    output.mkdir(parents=True, exist_ok=True)
    cargo_target = Path(base_env.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    libraries = []
    for abi, target, env in builds:
        run("cargo", "build", "--locked", "-p", "festerm-android", "--target", target,
            *features, *(["--release"] if args.release else []), env=env)
        libraries.append((abi, cargo_target / target / mode / "libfesterm_android.so"))
    env = base_env
    if not args.bundle:
        keystore = ROOT / "target" / "android" / "development.keystore"
        if not keystore.exists():
            run("keytool", "-genkeypair", "-keystore", keystore, "-storepass", "android",
                "-keypass", "android", "-alias", "androiddebugkey", "-keyalg", "RSA",
                "-keysize", "2048", "-validity", "10000",
                "-dname", "CN=fesTerm Android Development", env=env)
            keystore.chmod(0o600)
    version_args = (["--version-code", str(args.version_code), "--replace-version"]
                    if args.version_code is not None else [])
    artifact = output / ("fesTerm-unsigned.aab" if args.bundle else "fesTerm.apk")
    artifact.unlink(missing_ok=True)
    tool = bundletool() if args.bundle else None
    with tempfile.TemporaryDirectory(prefix="build-", dir=output) as directory:
        work = Path(directory)
        classes, dex = work / "classes", work / "dex"
        classes.mkdir()
        dex.mkdir()
        resource_dir = work / "res"
        shutil.copytree(APP / "res", resource_dir)
        jars = []
        for dependency in dependencies:
            if dependency.suffix == ".aar":
                with zipfile.ZipFile(dependency) as archive:
                    jar = work / (dependency.stem + ".jar")
                    jar.write_bytes(archive.read("classes.jar"))
                    jars.append(jar)
                    security = resource_dir / "xml" / "network_security_config.xml"
                    security.parent.mkdir(exist_ok=True)
                    security.write_bytes(archive.read("res/xml/network_security_config.xml"))
            else:
                jars.append(dependency)
        java = sorted((APP / "java").rglob("*.java"))
        if args.recording_validation:
            java += sorted((APP / "tests/hooks").rglob("*.java"))
        run("javac", "-encoding", "UTF-8", "-source", "11", "-target", "11",
            "-classpath", android_jar, "-d", classes,
            *java, env=env)
        run(tools / "d8", "--lib", android_jar, "--min-api", MIN_API,
            "--output", dex, *sorted(classes.rglob("*.class")), *jars, env=env)
        resources = work / "resources.zip"
        run(tools / "aapt2", "compile", "--dir", resource_dir, "-o", resources, env=env)
        if args.bundle:
            proto = work / "proto.apk"
            run(tools / "aapt2", "link", "--proto-format", "-I", android_jar,
                "--manifest", APP / "AndroidManifest.xml", *version_args,
                "-o", proto, resources, env=env)
            base = work / "base.zip"
            write_base_module(proto, base, dex, libraries)
            config = work / "BundleConfig.json"
            config.write_text(json.dumps(BUNDLE_CONFIG))
            unsigned = work / "unsigned.aab"
            run("java", "-jar", tool, "build-bundle", f"--modules={base}", f"--output={unsigned}",
                f"--config={config}", env=env)
            run("java", "-jar", tool, "validate", f"--bundle={unsigned}", env=env)
            for xpath, expected in (("/manifest/@package", PACKAGE),
                                    ("/manifest/@android:versionCode", str(args.version_code)),
                                    ("/manifest/@android:versionName", version_name)):
                actual = bundletool_value(tool, unsigned, xpath, env)
                if actual != expected:
                    raise ValueError(f"Bundle {xpath} is {actual!r}, expected {expected!r}")
            shutil.move(unsigned, artifact)
        else:
            unsigned = work / "unsigned.apk"
            run(tools / "aapt2", "link", "-I", android_jar, "--manifest", APP / "AndroidManifest.xml",
                *(["--debug-mode"] if not args.release else []), *version_args,
                "-o", unsigned, resources, env=env)
            with zipfile.ZipFile(unsigned, "a", compression=zipfile.ZIP_DEFLATED) as package:
                package.write(dex / "classes.dex", "classes.dex")
                package.write(libraries[0][1], f"lib/{abis[0]}/libfesterm_android.so")
                package.write(APP / "NOTICE.txt", "assets/NOTICE.txt")
                package.write(ROOT / "vendor/egui-winit/LICENSE-APACHE", "assets/licenses/Apache-2.0.txt")
            aligned = work / "aligned.apk"
            run(tools / "zipalign", "-P", "16", "-f", "4", unsigned, aligned, env=env)
            run(tools / "apksigner", "sign", "--ks", keystore, "--ks-key-alias", "androiddebugkey",
                "--ks-pass", "pass:android", "--key-pass", "pass:android", "--out", artifact, aligned, env=env)
    if not args.bundle:
        run(tools / "apksigner", "verify", artifact, env=env)
    with artifact.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    receipt = {
        "commit": run("git", "rev-parse", "HEAD", capture=True).strip(),
        "dirty": bool(run("git", "status", "--porcelain", capture=True)),
        "min_sdk": MIN_API, "target_sdk": API,
        "profile": mode, "ndk": NDK, "build_tools": BUILD_TOOLS,
        "sha256": digest,
    }
    if args.bundle:
        receipt |= {
            "format": "aab", "abis": abis, "live_ssh": True, "signing": "unsigned",
            "package": PACKAGE, "version_name": version_name, "version_code": args.version_code,
            "bundletool": json.loads((APP / "dependencies.json").read_text())["bundletool"]["name"],
        }
    else:
        receipt |= {
            "format": "apk", "abi": abis[0], "live_ssh": not args.offline,
            "signing": "development-only", "recording_validation": args.recording_validation,
        }
        if args.version_code is not None:
            receipt["version_code"] = args.version_code
    (output / "build-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(artifact)
    if args.install:
        adb = ["adb"] + (["-s", args.serial] if args.serial else [])
        devices = run(*adb, "devices", capture=True)
        if not args.serial and sum(line.endswith("\tdevice") for line in devices.splitlines()) != 1:
            raise ValueError("Select exactly one connected Android device with --serial")
        run(*adb, "install", "-r", artifact)
        run(*adb, "shell", "am", "start", "-W", "-n", f"{PACKAGE}/.FestermActivity")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", default=os.environ.get("ANDROID_HOME", str(Path.home() / "Library/Android/sdk")))
    parser.add_argument("--abi", choices=TARGETS, help="APK ABI (default arm64-v8a)")
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--bundle", action="store_true",
                        help="Build an unsigned multi-ABI Play AAB for sign-android-bundle.py")
    parser.add_argument("--version-code", type=int,
                        help="Override the manifest versionCode (required with --bundle)")
    parser.add_argument("--offline", action="store_true", help="Build the offline fixture instead of live SSH")
    parser.add_argument("--recording-validation", action="store_true",
                        help="Include native recording test hooks in a development APK")
    parser.add_argument("--check-dependencies", action="store_true")
    parser.add_argument("--install", action="store_true")
    parser.add_argument("--serial")
    try:
        build(parser.parse_args())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Android build failed: {error}\n")


if __name__ == "__main__":
    main()
