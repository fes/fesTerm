#!/usr/bin/env python3
"""Build a development APK using the Android SDK/NDK and the shared Rust host."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "app" / "festerm-android"
API = 36
MIN_API = 29
BUILD_TOOLS = "36.0.0"
NDK = "27.2.12479018"
MAX_VERSION_CODE = 2_100_000_000
TARGETS = {
    "arm64-v8a": ("aarch64-linux-android", "aarch64-linux-android"),
    "x86_64": ("x86_64-linux-android", "x86_64-linux-android"),
}
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
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(url, timeout=60) as response:
                return response.read(limit + 1)
        except urllib.error.HTTPError as error:
            error.close()
            if error.code < 500 or attempt == attempts:
                raise
            reason = f"HTTP {error.code}"
        except (urllib.error.URLError, TimeoutError) as error:
            if attempt == attempts:
                raise
            reason = str(error)
        delay = 5 * 2 ** (attempt - 1)
        print(f"Android dependency download failed ({reason}); retrying in {delay}s",
              file=sys.stderr, flush=True)
        time.sleep(delay)


def native_dependencies():
    manifest = json.loads((APP / "dependencies.json").read_text())
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    version = next(p["version"] for p in lock["package"] if p["name"] == "rustls-platform-verifier-android")
    if version != manifest["rustls_android_version"]:
        raise ValueError("Update the Android TLS artifact pins to match Cargo.lock")
    directory = ROOT / "target" / "android-dependencies"
    directory.mkdir(parents=True, exist_ok=True)
    paths = []
    for artifact in manifest["artifacts"]:
        path = directory / artifact["name"]
        if not path.exists():
            data = download(artifact["url"], 8 * 1024 * 1024)
            if len(data) > 8 * 1024 * 1024:
                raise ValueError("Android dependency exceeds the download bound")
            if hashlib.sha256(data).hexdigest() != artifact["sha256"]:
                raise ValueError("Android dependency checksum mismatch: " + artifact["name"])
            path.write_bytes(data)
        with path.open("rb") as source:
            if hashlib.file_digest(source, "sha256").hexdigest() != artifact["sha256"]:
                raise ValueError("Android dependency checksum mismatch: " + artifact["name"])
        paths.append(path)
    return paths


def target_environment(sdk_path, abi, offline=False, recording_validation=False):
    sdk = Path(sdk_path).expanduser().resolve()
    tools = sdk / "build-tools" / BUILD_TOOLS
    android_jar = sdk / "platforms" / f"android-{API}" / "android.jar"
    host = {"Darwin": "darwin-x86_64", "Linux": "linux-x86_64"}.get(platform.system())
    if host is None:
        raise ValueError("Use macOS or Linux for the Android development packager")
    llvm = sdk / "ndk" / NDK / "toolchains" / "llvm" / "prebuilt" / host / "bin"
    for path in (tools / "aapt2", tools / "d8", tools / "zipalign", tools / "apksigner", android_jar):
        if not path.is_file():
            raise ValueError(f"Missing Android tool: {path}")
    for command in ("cargo", "javac", "keytool"):
        if shutil.which(command) is None:
            raise ValueError(f"Missing build tool: {command}")
    target, clang_target = TARGETS[abi]
    env = os.environ.copy()
    env["ANDROID_HOME"] = str(sdk)
    env["ANDROID_NDK_HOME"] = str(sdk / "ndk" / NDK)
    linker = llvm / f"{clang_target}{MIN_API}-clang"
    if not linker.is_file():
        raise ValueError(f"Missing Android NDK compiler: {linker}")
    key = target.replace("-", "_")
    env[f"CARGO_TARGET_{key.upper()}_LINKER"] = str(linker)
    env[f"CC_{key}"] = str(linker)
    env[f"AR_{key}"] = str(llvm / "llvm-ar")
    env["RUSTFLAGS"] = env.get("RUSTFLAGS", "") + " -C link-arg=-Wl,-z,max-page-size=16384"
    env.setdefault("CARGO_PROFILE_DEV_DEBUG", "0")
    features = ["--no-default-features"] if offline else []
    if recording_validation:
        features += ["--features", "native-recording-validation"]
    tree = run("cargo", "tree", "--locked", "-p", "festerm-android", "--target", target,
               "--edges", "normal,build", "--prefix", "none", *features, env=env, capture=True)
    check_dependency_tree(tree)
    return tools, android_jar, target, env, features


def build_library(target, env, features, release):
    mode = "release" if release else "debug"
    cargo_target = Path(env.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    run("cargo", "build", "--locked", "-p", "festerm-android", "--target", target,
        *features, *(["--release"] if release else []), env=env)
    library = cargo_target / target / mode / "libfesterm_android.so"
    if not library.is_file() or not library.stat().st_size:
        raise ValueError(f"Missing Android native library: {library}")
    return library


def compile_app(work, tools, android_jar, env, dependencies, recording_validation=False,
                debug=False, proto_format=False, version_code=None):
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
    if recording_validation:
        java += sorted((APP / "tests/hooks").rglob("*.java"))
    run("javac", "-encoding", "UTF-8", "-source", "11", "-target", "11",
        "-classpath", android_jar, "-d", classes, *java, env=env)
    run(tools / "d8", "--lib", android_jar, "--min-api", MIN_API,
        "--output", dex, *sorted(classes.rglob("*.class")), *jars, env=env)
    dex_files = sorted(dex.glob("classes*.dex"))
    if not dex_files:
        raise ValueError("Android compiler produced no dex files")
    resources = work / "resources.zip"
    unsigned = work / "unsigned.apk"
    run(tools / "aapt2", "compile", "--dir", resource_dir, "-o", resources, env=env)
    version_args = (["--version-code", str(version_code), "--replace-version"]
                    if version_code is not None else [])
    run(tools / "aapt2", "link", "-I", android_jar, "--manifest", APP / "AndroidManifest.xml",
        *(["--debug-mode"] if debug else []), *(["--proto-format"] if proto_format else []),
        *version_args, "-o", unsigned, resources, env=env)
    return unsigned, dex_files


def build(args):
    if args.recording_validation and args.release:
        raise ValueError("Recording validation hooks are not allowed in release APKs")
    tools, android_jar, target, env, features = target_environment(
        args.sdk, args.abi, args.offline, args.recording_validation,
    )
    if args.check_dependencies:
        return
    dependencies = native_dependencies()
    mode = "release" if args.release else "debug"
    output = ROOT / "target" / "android" / args.abi / ("offline" if args.offline else "live")
    output.mkdir(parents=True, exist_ok=True)
    library = build_library(target, env, features, args.release)
    keystore = ROOT / "target" / "android" / "development.keystore"
    if not keystore.exists():
        run("keytool", "-genkeypair", "-keystore", keystore, "-storepass", "android",
            "-keypass", "android", "-alias", "androiddebugkey", "-keyalg", "RSA",
            "-keysize", "2048", "-validity", "10000",
            "-dname", "CN=fesTerm Android Development", env=env)
        keystore.chmod(0o600)
    apk = output / "fesTerm.apk"
    with tempfile.TemporaryDirectory(prefix="build-", dir=output) as directory:
        work = Path(directory)
        unsigned, dex_files = compile_app(
            work, tools, android_jar, env, dependencies,
            recording_validation=args.recording_validation, debug=not args.release,
        )
        with zipfile.ZipFile(unsigned, "a", compression=zipfile.ZIP_DEFLATED) as package:
            for dex_file in dex_files:
                package.write(dex_file, dex_file.name)
            package.write(library, f"lib/{args.abi}/libfesterm_android.so")
            package.write(APP / "NOTICE.txt", "assets/NOTICE.txt")
            package.write(ROOT / "vendor/egui-winit/LICENSE-APACHE", "assets/licenses/Apache-2.0.txt")
            package.write(ROOT / "packaging/THIRD_PARTY_NOTICES.txt", "assets/THIRD_PARTY_NOTICES.txt")
        aligned = work / "aligned.apk"
        run(tools / "zipalign", "-P", "16", "-f", "4", unsigned, aligned, env=env)
        run(tools / "apksigner", "sign", "--ks", keystore, "--ks-key-alias", "androiddebugkey",
            "--ks-pass", "pass:android", "--key-pass", "pass:android", "--out", apk, aligned, env=env)
    run(tools / "apksigner", "verify", apk, env=env)
    with apk.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    receipt = {
        "commit": run("git", "rev-parse", "HEAD", capture=True).strip(),
        "dirty": bool(run("git", "status", "--porcelain", capture=True)),
        "abi": args.abi, "min_sdk": MIN_API, "target_sdk": API,
        "live_ssh": not args.offline, "signing": "development-only",
        "recording_validation": args.recording_validation,
        "profile": mode, "ndk": NDK, "build_tools": BUILD_TOOLS,
        "sha256": digest,
    }
    (output / "build-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(apk)
    if args.install:
        adb = ["adb"] + (["-s", args.serial] if args.serial else [])
        devices = run(*adb, "devices", capture=True)
        if not args.serial and sum(line.endswith("\tdevice") for line in devices.splitlines()) != 1:
            raise ValueError("Select exactly one connected Android device with --serial")
        run(*adb, "install", "-r", apk)
        run(*adb, "shell", "am", "start", "-W", "-n", "io.github.fes.festerm/.FestermActivity")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", default=os.environ.get("ANDROID_HOME", str(Path.home() / "Library/Android/sdk")))
    parser.add_argument("--abi", choices=TARGETS, default="arm64-v8a")
    parser.add_argument("--release", action="store_true")
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
