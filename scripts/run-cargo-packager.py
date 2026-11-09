#!/usr/bin/env python3
"""Retry cargo-packager only for transient upstream tool-download failures."""

import re
import subprocess
import sys
import time


TRANSIENT = re.compile(
    r"^ERROR https://github\.com/(?:tauri-apps|linuxdeploy)/\S+: status code (?:502|503|504)\s*$"
)
ANSI = re.compile(r"\x1b\[[0-9;]*m")
DELAYS = (5, 15, 30, 60, 120)


def execute(arguments):
    last_error = ""
    process = subprocess.Popen(
        ["cargo", "packager", *arguments], stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, errors="replace", bufsize=1,
    )
    with process.stdout:
        for line in process.stdout:
            print(line, end="", flush=True)
            clean = ANSI.sub("", line).strip()
            if clean.startswith("ERROR "):
                last_error = clean
    return process.wait(), last_error


def run(arguments):
    for attempt in range(len(DELAYS) + 1):
        code, error = execute(arguments)
        if code == 0:
            return 0
        if not TRANSIENT.fullmatch(error) or attempt == len(DELAYS):
            return code
        delay = DELAYS[attempt]
        print(f"Upstream packager download failed; retrying in {delay}s: {error}", file=sys.stderr, flush=True)
        time.sleep(delay)


if __name__ == "__main__":
    try:
        sys.exit(run(sys.argv[1:]))
    except OSError as error:
        sys.exit(f"Could not run cargo-packager: {error}")
