#!/usr/bin/env python3
"""Require only the owned-memory timestamp regression; never native PKI."""

from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
TEST = ("host_publisher::native::tests::"
        "windows_timestamp_filetime_copies_exact_bits_from_misaligned_sdk_storage")
COMMAND = (
    "cargo", "test", "--locked", "-p", "festerm-windows-security",
    "--features", "host-service", "--lib", TEST, "--", "--exact",
)


def main():
    if sys.platform != "win32":
        print("The timestamp alignment unit requires Windows.", file=sys.stderr)
        return 2
    listing = subprocess.run(
        [*COMMAND, "--list"], cwd=ROOT, capture_output=True, text=True, check=False,
    )
    sys.stdout.write(listing.stdout)
    sys.stderr.write(listing.stderr)
    if listing.returncode:
        return listing.returncode
    lines = [line.strip() for line in listing.stdout.splitlines() if line.strip()]
    if lines != [f"{TEST}: test", "1 test, 0 benchmarks"]:
        print("Exactly one pinned timestamp alignment test must be listed.", file=sys.stderr)
        return 1
    result = subprocess.run(
        COMMAND, cwd=ROOT, capture_output=True, text=True, check=False,
    )
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    if result.returncode:
        return result.returncode
    if result.stdout.splitlines().count(f"test {TEST} ... ok") != 1 or not re.search(
        r"(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;", result.stdout
    ):
        print("Exactly one timestamp alignment test must execute and pass.", file=sys.stderr)
        return 1
    print("timestamp_alignment_unit_passed=true; native_pki_qualification=false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
