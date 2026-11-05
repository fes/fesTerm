#!/usr/bin/env python3
"""Capture sanitized mobile widgets headlessly; never native iOS acceptance."""

import argparse
import os
from pathlib import Path
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="new output directory; existing paths are refused")
    args = parser.parse_args(argv)
    output = (args.output or ROOT / "target/mobile-ui-captures" / uuid.uuid4().hex).resolve()
    if output.exists():
        parser.error("output already exists; retain previous captures and use a fresh directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    # The Rust generator only constructs controlled in-memory MobileApp fixtures.
    # It never constructs the native host or connects a session.
    command = ["cargo", "test", "--locked", "-p", "festerm-mobile",
               "--features", "live-ssh", "--lib"]
    for test, variable, destination in [
        ("ui_gallery::capture_mobile_ui_gallery", "FESTERM_MOBILE_UI_OUT", output),
        ("documents::tests::capture_document_ui", "FESTERM_DOCUMENT_UI_OUT", output / "documents"),
    ]:
        subprocess.run(
            command + [test, "--", "--ignored", "--exact"],
            cwd=ROOT, env=dict(os.environ, **{variable: str(destination)}),
            check=True, timeout=600,
        )
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
