#!/usr/bin/env python3
"""Validate host dependency notices against real native Cargo metadata, without building."""

import json
import subprocess
import tomllib

from build_tunnel_bundle import ROOT, third_party_notices


def main():
    host = next(
        line.removeprefix("host: ")
        for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
        if line.startswith("host: ")
    )
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", host],
        cwd=ROOT, text=True,
    ))
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    notices = third_party_notices(metadata, version, ROOT, host=True)
    if not notices.strip():
        raise ValueError("empty host dependency notices")
    print(f"Host dependency notices: PASS ({host}; {len(notices)} bytes)")


if __name__ == "__main__":
    main()
