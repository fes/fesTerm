#!/usr/bin/env python3
"""Produce synthetic interoperability bundles, including historical Windows HOST8.

Other layouts use the real release producer. Current Windows HOST production
requires genuine pinned Microsoft bytes; its synthetic pin seam is tested
separately. No download, native signing or release credentials participate.
"""

import argparse
import hashlib
import stat
import zipfile
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
        for name in build_tunnel_bundle.binary_names(platform, host=True):
            (binaries / name).write_bytes(
                f"Repository-owned synthetic helper: {name}\n".encode("ascii")
            )
    output = destination / "dist"
    for platform in build_tunnel_bundle.PLATFORMS:
        for arch in build_tunnel_bundle.ARCHITECTURES:
            for host in (False, True):
                if platform == "windows" and host:
                    # Historical schema-2 HOST8 interoperability fixture. New
                    # production HOSTs require genuine Microsoft-pinned bytes;
                    # these synthetic fixtures must not counterfeit those pins.
                    output.mkdir(exist_ok=True)
                    archive = output / f"fesTerm-host-{version}-windows-{arch}.zip"
                    entries = {name: (binaries / name).read_bytes()
                               for name in build_tunnel_bundle.binary_names(platform, host=True)}
                    entries.update(VERSION=f"{version}\n".encode(), LICENSE=build_tunnel_bundle.MIT_LICENSE.encode())
                    entries["THIRD_PARTY_NOTICES.txt"] = b"Repository-owned synthetic dependency notice.\n"
                    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as bundle:
                        for name, data in sorted(entries.items()):
                            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                            info.create_system = 3
                            mode = 0o755 if name in build_tunnel_bundle.binary_names(platform, host=True) else 0o644
                            info.external_attr = (stat.S_IFREG | mode) << 16
                            bundle.writestr(info, data, compress_type=zipfile.ZIP_DEFLATED)
                    archive.with_name(archive.name + ".sha256").write_text(
                        f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
                else:
                    archive = build_tunnel_bundle.build_bundle(
                        destination, binaries, output, version, platform, arch,
                        notices=b"Repository-owned synthetic dependency notice.\n", host=host,
                    )
                build_host_manifest.build_manifest(archive, version, platform, arch, host=host)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    produce(args.output_dir, args.version)


if __name__ == "__main__":
    main()
