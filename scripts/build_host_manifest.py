#!/usr/bin/env python3
"""Generate bounded helper metadata; protected release signs the exact bytes.

This script holds no signing key. cargo-packager 0.11.8 signs the output using
the same protected updater identity as the desktop release.
"""

import argparse
import hashlib
import json
import re
from pathlib import Path

from build_tunnel_bundle import ARCHITECTURES, PLATFORMS, verify_bundle


MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_MANIFEST_BYTES = 4096
SCHEMA = 2


def build_manifest(archive, version, platform, arch):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ValueError("version must be exact stable semver")
    if len(version) > 32 or platform not in PLATFORMS or arch not in ARCHITECTURES:
        raise ValueError("unsupported target")
    extension = "zip" if platform == "windows" else "tar.gz"
    expected = f"fesTerm-tunnel-{version}-{platform}-{arch}.{extension}"
    if archive.name != expected or archive.is_symlink() or not archive.is_file():
        raise ValueError("expected regular exact-target archive")
    if not 0 < archive.stat().st_size <= MAX_ARCHIVE_BYTES:
        raise ValueError("archive length exceeds bounds")
    verify_bundle(archive, version, platform)
    with archive.open("rb") as source:
        data = source.read(MAX_ARCHIVE_BYTES + 1)
    if not 0 < len(data) <= MAX_ARCHIVE_BYTES:
        raise ValueError("archive length exceeds bounds")
    manifest = {
        "schema": SCHEMA,
        "package": "festerm-helpers",
        "version": version,
        "platform": platform,
        "architecture": arch,
        "filename": expected,
        "length": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }
    encoded = (json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")
    if len(encoded) > MAX_MANIFEST_BYTES:
        raise ValueError("manifest length exceeds bounds")
    output = archive.with_name(archive.name + ".manifest.json")
    with output.open("xb") as destination:
        destination.write(encoded)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True, choices=PLATFORMS)
    parser.add_argument("--arch", required=True, choices=ARCHITECTURES)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    try:
        build_manifest(args.archive, args.version, args.platform, args.arch)
    except (ValueError, OSError) as error:
        parser.exit(1, f"helper manifest failed: {error}\n")


if __name__ == "__main__":
    main()
