#!/usr/bin/env python3
"""Run the shared Maestro UI flows against the mobile app.

The flows drive the app only through the platform accessibility tree
(ADR 0075), so they also guard against regressions in what assistive
technology can see and operate.

The flows expect a fresh app with no saved profiles, so the app is
uninstalled and reinstalled first. iOS: without --device, a fresh test-owned
Simulator is created and deleted; an explicitly named Simulator loses only
this app's data. Android: --device must name an emulator; without --build or --apk the
installed app's data is cleared instead.
Exit status 3 means the suite was skipped because Maestro is unavailable.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parents[1]
FLOWS = ROOT / "app/festerm-mobile/maestro"
APP_IDS = {"ios": "org.festerm.mobile-spike", "android": "io.github.fes.festerm"}
SKIPPED = 3

_spec = importlib.util.spec_from_file_location("ios_smoke", ROOT / "scripts/smoke-ios-simulator.py")
ios_smoke = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ios_smoke)


def find_maestro(env: dict) -> str | None:
    found = shutil.which("maestro", path=env.get("PATH"))
    if found:
        return found
    fallback = Path(env.get("HOME", str(Path.home()))) / ".maestro/bin/maestro"
    return str(fallback) if fallback.is_file() else None


def maestro_env(env: dict) -> dict:
    env = dict(env, MAESTRO_CLI_NO_ANALYTICS="1", MAESTRO_CLI_ANALYSIS_NOTIFICATION_DISABLED="true")
    if "JAVA_HOME" not in env:
        brewed = Path("/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home")
        if brewed.is_dir():
            env["JAVA_HOME"] = str(brewed)
    return env


def maestro_command(maestro: str, platform_name: str, device: str, output: Path) -> list[str]:
    return [maestro, "--device", device, "test",
            "-e", f"APP_ID={APP_IDS[platform_name]}",
            "--format", "junit", "--output", str(output / "report.xml"),
            "--test-output-dir", str(output / "artifacts"),
            str(FLOWS)]


def run_ios(run, args, maestro_run, output: Path) -> None:
    if args.build:
        run(sys.executable, str(ROOT / "scripts/build-ios-spike.py"), timeout=1200)
    bundle = args.bundle.resolve()
    if args.device:
        run("xcrun", "simctl", "uninstall", args.device, APP_IDS["ios"], check=False)
        run("xcrun", "simctl", "install", args.device, str(bundle), timeout=120)
        maestro_run(args.device)
        return
    inventory = json.loads(run("xcrun", "simctl", "list", "--json"))
    existing = {d["udid"] for group in inventory["devices"].values() for d in group}
    sdk = run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-version").strip()
    phone = next(spec for spec in ios_smoke.choose_devices(inventory, sdk) if spec["family"] == "iPhone")
    created = run("xcrun", "simctl", "create", f"fesTerm-ui-{uuid.uuid4().hex[:12]}",
                  phone["device_type"], phone["runtime"]).strip()
    if len(created) != 36 or created in existing:
        raise RuntimeError("simctl did not return a fresh test-owned device UDID")
    try:
        run("xcrun", "simctl", "boot", created)
        run("xcrun", "simctl", "bootstatus", created, "-b", timeout=180)
        run("xcrun", "simctl", "install", created, str(bundle), timeout=120)
        maestro_run(created)
    finally:
        for operation in ("shutdown", "delete"):
            run("xcrun", "simctl", operation, created, check=False)


def run_android(run, args, maestro_run) -> None:
    if not args.device:
        raise RuntimeError("--device must name an isolated Android emulator serial")
    adb = ["adb", "-s", args.device]
    if run(*adb, "shell", "getprop", "ro.kernel.qemu").strip() != "1":
        raise RuntimeError("UI flows replace app data; select an isolated emulator, not a personal device")
    if args.build or args.apk:
        # CI and local builds use different signing keys, so replace rather than update.
        run(*adb, "uninstall", APP_IDS["android"], check=False)
    else:
        run(*adb, "shell", "pm", "clear", APP_IDS["android"])
    if args.build:
        run(sys.executable, str(ROOT / "scripts/build-android.py"), "--install",
            "--serial", args.device, timeout=3600)
    elif args.apk:
        run(*adb, "install", str(args.apk.resolve()), timeout=300)
    maestro_run(args.device)


def main(argv=None, env=None, runner_factory=ios_smoke.Runner) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=sorted(APP_IDS), required=True)
    parser.add_argument("--device", help="iOS Simulator UDID or Android emulator serial")
    parser.add_argument("--build", action="store_true", help="build and install the app first")
    parser.add_argument("--bundle", type=Path, default=ROOT / "target/ios-spike/fesTermSpike.app")
    parser.add_argument("--apk", type=Path, help="install this APK before running")
    parser.add_argument("--output", type=Path, default=ROOT / "target/maestro")
    args = parser.parse_args(argv)
    if args.platform == "ios" and platform.system() != "Darwin":
        parser.error("iOS flows require macOS and full Xcode")
    env = maestro_env(os.environ if env is None else env)
    maestro = find_maestro(env)
    if maestro is None:
        print("suite=mobile-ui-flows status=skipped reason=maestro-unavailable")
        return SKIPPED
    output = args.output.resolve() / f"{args.platform}-{uuid.uuid4().hex}"
    output.mkdir(parents=True)
    run = runner_factory(output / "commands.log")

    def maestro_run(device: str) -> None:
        command = maestro_command(maestro, args.platform, device, output)
        print("[ui-flows] " + " ".join(command), flush=True)
        if subprocess.run(command, cwd=output, env=env).returncode:
            raise RuntimeError(f"Maestro flows failed; see {output / 'report.xml'}")

    try:
        if args.platform == "ios":
            run_ios(run, args, maestro_run, output)
        else:
            run_android(run, args, maestro_run)
    except (RuntimeError, OSError, ValueError, KeyError, StopIteration,
            subprocess.SubprocessError) as error:
        print(f"UI flows failed: {error}", file=sys.stderr)
        return 1
    print(f"UI flows passed: {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
