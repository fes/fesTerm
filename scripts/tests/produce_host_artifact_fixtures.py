#!/usr/bin/env python3
"""Produce synthetic bundles through the real Python release producers.

Invoked by Rust interoperability tests. No downloaded binaries, native signing,
release credentials or mutable upstream metadata participate.
"""

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_host_manifest
import build_tunnel_bundle


def produce(destination, version):
    destination.mkdir()
    (destination / "Cargo.toml").write_text(
        f'[workspace.package]\nversion = "{version}"\nlicense = "MIT"\n',
        encoding="utf-8",
    )
    binaries = destination / "bin"
    binaries.mkdir()
    for platform in build_tunnel_bundle.PLATFORMS:
        for name in build_tunnel_bundle.binary_names(platform):
            (binaries / name).write_bytes(
                f"Repository-owned synthetic helper: {name}\n".encode("ascii")
            )
    output = destination / "dist"
    for platform in build_tunnel_bundle.PLATFORMS:
        for arch in build_tunnel_bundle.ARCHITECTURES:
            archive = build_tunnel_bundle.build_bundle(
                destination, binaries, output, version, platform, arch,
                notices=b"Repository-owned synthetic dependency notice.\n",
            )
            build_host_manifest.build_manifest(archive, version, platform, arch)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    produce(args.output_dir, args.version)


if __name__ == "__main__":
    main()
