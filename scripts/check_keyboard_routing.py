#!/usr/bin/env python3
"""Opt-in keyboard routing checks; --native additionally injects real OS input."""
import argparse
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", action="store_true",
                        help="also run the platform OS-input driver on an isolated test desktop")
    args = parser.parse_args()
    if args.native and os.environ.get("FESTERM_ISOLATED_TEST_DESKTOP") != "1":
        parser.error(
            "--native replaces the test desktop clipboard; run it only with "
            "FESTERM_ISOLATED_TEST_DESKTOP=1 after disabling clipboard sharing"
        )
    root = Path(__file__).resolve().parent.parent
    result = subprocess.run(
        ["cargo", "test", "-p", "festerm", "-p", "festerm-config",
         "-p", "festerm-ui-egui", "keyboard_"], cwd=root, check=False)
    if result.returncode:
        return result.returncode
    if not args.native:
        print("keyboard-routing: synthesized application/controller checks passed; no OS-input claim")
        return 0
    environment = dict(os.environ, FESTERM_NATIVE_KEYBOARD_ROUTING_SMOKE="1")
    result_path = root / "keyboard-routing-native-result.txt"
    if sys.platform == "darwin":
        command = ["sh", "scripts/run-macos-os-input-smoke.sh", str(result_path)]
    elif sys.platform == "win32":
        command = ["pwsh", "-NoProfile", "-File", "scripts/run-windows-os-input-smoke.ps1",
                   "-ResultPath", str(result_path)]
    elif sys.platform.startswith("linux"):
        command = ["sh", "scripts/run-linux-os-input-smoke.sh", str(result_path)]
    else:
        print("keyboard-routing: native platform unsupported", file=sys.stderr)
        return 2
    return subprocess.run(command, cwd=root, env=environment, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
