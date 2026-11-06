#!/usr/bin/env python3
"""Run opt-in Android native integration tests on an explicitly selected emulator."""

import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile

spec = importlib.util.spec_from_file_location("android_build", Path(__file__).with_name("build-android.py"))
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--sdk", default=os.environ.get("ANDROID_HOME", str(Path.home() / "Library/Android/sdk")))
    args = parser.parse_args()
    sdk = Path(args.sdk)
    tools = sdk / "build-tools" / build.BUILD_TOOLS
    android_jar = sdk / "platforms" / f"android-{build.API}" / "android.jar"
    adb = ["adb", "-s", args.serial]
    if build.run(*adb, "shell", "getprop", "ro.kernel.qemu", capture=True).strip() != "1":
        parser.error("Native smoke mutates test app data; select an isolated emulator, not a personal device")
    output = build.ROOT / "target" / "android-smoke"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="native-", dir=output) as directory:
        work = Path(directory)
        classes = work / "classes"
        classes.mkdir()
        build.run("javac", "-source", "11", "-target", "11", "-encoding", "UTF-8",
            "-classpath", android_jar, "-d", classes,
            *sorted((build.APP / "java").rglob("*.java")),
            *sorted((build.APP / "tests/java").rglob("*.java")))
        dex = work / "dex"
        dex.mkdir()
        # Do not package another copy of the target Activity into the test APK.
        build.run(tools / "d8", "--lib", android_jar, "--classpath", classes,
            "--min-api", build.MIN_API, "--output", dex,
            *sorted((classes / "io/github/fes/festerm/tests").rglob("*.class")))
        unsigned = work / "unsigned.apk"
        build.run(tools / "aapt2", "link", "-I", android_jar,
            "--manifest", build.APP / "tests/AndroidManifest.xml", "-o", unsigned)
        with zipfile.ZipFile(unsigned, "a") as archive:
            archive.write(dex / "classes.dex", "classes.dex")
        aligned = work / "aligned.apk"
        build.run(tools / "zipalign", "-f", "4", unsigned, aligned)
        apk = output / "native-tests.apk"
        build.run(tools / "apksigner", "sign", "--ks",
            build.ROOT / "target/android/development.keystore",
            "--ks-pass", "pass:android", "--out", apk, aligned)
    build.run(*adb, "install", "-r", apk)
    build.run(*adb, "shell", "am", "force-stop", "io.github.fes.festerm")
    try:
        result = build.run(*adb, "shell", "am", "instrument", "-w",
            "io.github.fes.festerm.tests/.PlatformSmoke", capture=True, timeout=90)
    except subprocess.TimeoutExpired:
        result = "FAIL: Android native instrumentation exceeded 90 seconds\n"
    finally:
        build.run(*adb, "shell", "am", "force-stop", "io.github.fes.festerm", timeout=15)
    (output / "native-results.txt").write_text(result)
    print(result)
    if "PASS:" not in result or "FAIL:" in result:
        raise SystemExit("Android native integration checks failed")


if __name__ == "__main__":
    main()
