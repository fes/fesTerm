#!/usr/bin/env python3
"""Capture sanitized mobile widgets headlessly; never native iOS acceptance."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]


def emit_review_data(output):
    """Bounded API-readable transport for this controlled four-image gallery."""
    manifest_path = output / "manifest.json"
    if manifest_path.is_symlink():
        raise ValueError("review manifest must belong to this capture")
    with manifest_path.open("rb") as stream:
        manifest_bytes = stream.read(64 * 1024 + 1)
    if len(manifest_bytes) > 64 * 1024:
        raise ValueError("capture manifest exceeds the review transport bound")
    manifest = json.loads(manifest_bytes)
    expected = {f"{family}-{state}.png" for family in ("phone", "tablet")
                for state in ("terminal", "connect")}
    images = manifest["images"]
    if len(images) != 4 or {entry["image"] for entry in images} != expected:
        raise ValueError("review transport requires exactly the controlled gallery filenames")
    # Validate all images before printing any payload. No arbitrary paths/files.
    payloads = {}
    for entry in images:
        path = output / entry["image"]
        if path.is_symlink() or path.stat().st_size > 512 * 1024:
            raise ValueError("review image exceeds the transport policy")
        with path.open("rb") as stream:
            payload = stream.read(512 * 1024 + 1)
        if len(payload) > 512 * 1024:
            raise ValueError("review image exceeds the transport policy")
        if hashlib.sha256(payload).hexdigest() != entry["sha256"]:
            raise ValueError("review image hash does not match the capture manifest")
        payloads[entry["image"]] = base64.b64encode(payload).decode("ascii")
    print("MOBILE_UI_MANIFEST " + json.dumps(manifest))
    for name, encoded in payloads.items():
        for offset in range(0, len(encoded), 4096):
            print(f"MOBILE_UI_PNG {name} {offset} {encoded[offset:offset + 4096]}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="new output directory; existing paths are refused")
    parser.add_argument("--emit-review-data", action="store_true",
                        help="export only the controlled gallery manifest/PNGs to logs for API review")
    args = parser.parse_args(argv)
    output = (args.output or ROOT / "target/mobile-ui-captures" / uuid.uuid4().hex).resolve()
    if output.exists():
        parser.error("output already exists; retain previous captures and use a fresh directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    # The Rust generator only constructs controlled in-memory MobileApp fixtures.
    # It never constructs the native host or connects a session.
    subprocess.run(
        ["cargo", "test", "--locked", "-p", "festerm-mobile", "--features", "live-ssh",
         "--lib", "ui_gallery::capture_mobile_ui_gallery", "--", "--ignored", "--exact"],
        cwd=ROOT, env=dict(os.environ, FESTERM_MOBILE_UI_OUT=str(output)),
        check=True, timeout=600,
    )
    if args.emit_review_data:
        emit_review_data(output)
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
