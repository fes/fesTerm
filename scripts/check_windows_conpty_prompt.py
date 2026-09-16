#!/usr/bin/env python3
"""Repeat the timing-sensitive Windows ConPTY prompt-return regression."""

import argparse
import os
from pathlib import Path
import subprocess
import sys


TEST_NAME = (
    "session_controller::tests::"
    "conpty_default_shell_keeps_the_prompt_column_across_a_command_newline"
)


def run_cycles(cycles, *, repo, runner=subprocess.run):
    command = [
        "cargo",
        "test",
        "--quiet",
        "-p",
        "festerm",
        TEST_NAME,
        "--",
        "--exact",
    ]
    for cycle in range(1, cycles + 1):
        try:
            result = runner(command, cwd=repo, check=False, timeout=45)
        except subprocess.TimeoutExpired:
            print(
                f"windows-conpty-prompt: timed out cycle={cycle}/{cycles}",
                file=sys.stderr,
            )
            return 124
        if result.returncode:
            print(
                f"windows-conpty-prompt: failed cycle={cycle}/{cycles}",
                file=sys.stderr,
            )
            return result.returncode
    print(f"windows-conpty-prompt: passed cycles={cycles}")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cycles",
        type=int,
        default=os.environ.get("FESTERM_WINDOWS_CONPTY_PROMPT_CYCLES", 20),
        help="number of independent prompt-return attempts (default: 20)",
    )
    args = parser.parse_args()
    if not 1 <= args.cycles <= 100:
        parser.error("cycles must be 1..100")
    if sys.platform != "win32":
        parser.error("this qualification runner requires native Windows")
    repo = Path(__file__).resolve().parent.parent
    return run_cycles(args.cycles, repo=repo)


if __name__ == "__main__":
    raise SystemExit(main())
