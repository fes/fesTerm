#!/usr/bin/env python3
"""Package only the standalone helpers; never the GUI or a ConPTY sidecar.

Checksums provide integrity, not an independent publisher signature. macOS and
Windows executables must be signed and verified by the protected release job
before this script packages them.
"""

import argparse
import gzip
import hashlib
import io
import json
import re
import stat
import tarfile
import tomllib
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("festerm-sessiond", "festerm-tunnel-bridge", "festerm-tunnel-setup")
PLATFORMS = ("linux", "macos", "windows")
ARCHITECTURES = ("x86_64", "aarch64")
# These exact published crates omit their license files. Select their declared
# Apache-2.0 option and retain attribution; an upgrade requires renewed review.
APACHE_DECLARATIONS = {
    ("delegate", "0.13.5"): "Godfrey Chan; Jakub Beránek",
    ("pageant", "0.2.1"): "Declared author: Eugene; https://github.com/warp-tech/russh",
    ("russh-cryptovec", "0.62.0"): "Copyright 2016 Pierre-Étienne Meunier",
    ("russh-util", "0.52.0"): "russh contributors, https://github.com/warp-tech/russh",
}
MIT_LICENSE = """fesTerm standalone helpers
Source: https://github.com/fes/fesTerm
License declaration: workspace.package.license in Cargo.toml (MIT).
This file reproduces the MIT terms; it does not invent a copyright attribution.

MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def validate_version(version, root):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ValueError("version must be an exact stable semver")
    workspace = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    if workspace["workspace"]["package"]["version"] != version:
        raise ValueError("version does not match Cargo.toml")
    if workspace["workspace"]["package"]["license"] != "MIT":
        raise ValueError("workspace license changed; update the bundle license")


def third_party_notices(metadata, version, root):
    """Retain dependency license/notice files, including vendored native code."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = []
    for name in BINARIES[:2]:
        matches = [p for p in packages.values() if p["name"] == name]
        if len(matches) != 1 or matches[0]["version"] != version:
            raise ValueError(f"missing or wrong-version package metadata: {name}")
        pending.append(matches[0]["id"])
    visited = set()
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        pending.extend(
            dependency["pkg"]
            for dependency in nodes[package_id]["deps"]
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"])
        )
    notices = [
        "Third-party licenses and notices for the standalone fesTerm helpers.\n"
        "Collected from resolved dependency sources; this is not a legal audit.\n"
    ]
    for package in sorted(
        (packages[package_id] for package_id in visited),
        key=lambda package: (package["name"], package["version"], package["id"]),
    ):
        directory = Path(package["manifest_path"]).resolve().parent
        if directory.is_relative_to(root.resolve() / "crates"):
            if package["license"] != "MIT":
                raise ValueError(f"unexpected first-party license: {package['name']}")
            continue
        files = {
            path
            for path in directory.rglob("*")
            if path.is_file()
            and re.fullmatch(
                r"(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE|COPYRIGHT)([._-].*)?",
                path.name,
                re.IGNORECASE,
            )
        }
        if package.get("license_file"):
            files.add(directory / package["license_file"])
        notices.append(f"\n=== {package['name']} {package['version']} ===\n")
        notices.append(f"Declared license: {package.get('license') or 'see retained license file'}\n")
        if package.get("repository"):
            notices.append(f"Source repository: {package['repository']}\n")
        if not files:
            declaration = APACHE_DECLARATIONS.get((package["name"], package["version"]))
            if not declaration or package["license"] not in (
                "Apache-2.0", "MIT OR Apache-2.0",
            ):
                raise ValueError(f"missing dependency license/notice: {package['name']}")
            apache = (root / "vendor/russh/LICENSE-APACHE").read_text(encoding="utf-8")
            if "Apache License" not in apache or "Version 2.0" not in apache:
                raise ValueError("missing canonical Apache-2.0 license text")
            notices.append(f"{declaration}\nLicensed under Apache-2.0.\n{apache}\n")
        for path in sorted(files):
            if path.is_symlink() or not path.resolve().is_relative_to(directory):
                raise ValueError(f"unsafe dependency license path: {path}")
            text = path.read_text(encoding="utf-8")
            if not text.strip():
                raise ValueError(f"empty dependency license/notice: {path}")
            notices.append(f"\n--- {path.relative_to(directory).as_posix()} ---\n{text}\n")
    return "".join(notices).encode("utf-8")


def entry_names(platform):
    suffix = ".exe" if platform == "windows" else ""
    return tuple(name + suffix for name in BINARIES) + (
        "VERSION", "LICENSE", "THIRD_PARTY_NOTICES.txt",
    )


def verify_bundle(archive, version, platform):
    expected = entry_names(platform)
    if platform != "windows":
        with tarfile.open(archive, "r:gz") as bundle:
            if sorted(bundle.getnames()) != sorted(expected):
                raise ValueError("bundle must contain exactly the allowlisted entries")
            for entry in bundle.getmembers():
                expected_mode = 0o755 if entry.name in expected[:3] else 0o644
                if not entry.isfile() or entry.mode != expected_mode:
                    raise ValueError(f"unexpected bundle entry mode/type: {entry.name}")
                content = bundle.extractfile(entry).read()
                if not content:
                    raise ValueError(f"empty bundle entry: {entry.name}")
                if entry.name == "VERSION" and content != f"{version}\n".encode("ascii"):
                    raise ValueError("bundle VERSION does not match release version")
    else:
        with zipfile.ZipFile(archive) as bundle:
            if sorted(bundle.namelist()) != sorted(expected):
                raise ValueError("bundle must contain exactly the allowlisted entries")
            if bundle.read("VERSION") != f"{version}\n".encode("ascii"):
                raise ValueError("bundle VERSION does not match release version")
            for entry in bundle.infolist():
                mode = entry.external_attr >> 16
                expected_mode = 0o755 if entry.filename in expected[:3] else 0o644
                if not stat.S_ISREG(mode) or stat.S_IMODE(mode) != expected_mode:
                    raise ValueError(f"unexpected bundle entry mode: {entry.filename}")
                if not bundle.read(entry):
                    raise ValueError(f"empty bundle entry: {entry.filename}")
            if bundle.testzip() is not None:
                raise ValueError("bundle CRC verification failed")


def build_bundle(root, binary_dir, output_dir, version, platform, arch, metadata=None, notices=None):
    validate_version(version, root)
    if platform not in PLATFORMS or arch not in ARCHITECTURES:
        raise ValueError("unsupported platform or architecture")
    names = entry_names(platform)
    entries = {}
    for name in names[:3]:
        path = binary_dir / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing regular helper binary: {name}")
        entries[name] = path.read_bytes()
        if not entries[name]:
            raise ValueError(f"empty helper binary: {name}")
    entries["VERSION"] = f"{version}\n".encode("ascii")
    entries["LICENSE"] = MIT_LICENSE.encode("utf-8")
    if (metadata is None) == (notices is None):
        raise ValueError("provide exactly one of dependency metadata or staged notices")
    entries["THIRD_PARTY_NOTICES.txt"] = (
        third_party_notices(metadata, version, root) if metadata is not None else notices
    )
    if not entries["THIRD_PARTY_NOTICES.txt"].strip():
        raise ValueError("empty dependency notices")
    output_dir.mkdir(parents=True, exist_ok=True)
    extension = "zip" if platform == "windows" else "tar.gz"
    archive = output_dir / f"fesTerm-tunnel-{version}-{platform}-{arch}.{extension}"
    checksum = archive.with_name(archive.name + ".sha256")
    if archive.exists() or checksum.exists():
        raise ValueError("refusing to overwrite an existing bundle or checksum")
    archive_file = archive.open("xb")
    owns_checksum = False
    try:
        if platform == "windows":
            with zipfile.ZipFile(archive_file, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
                for name in sorted(entries):
                    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                    info.create_system = 3
                    mode = 0o755 if name in names[:3] else 0o644
                    info.external_attr = (stat.S_IFREG | mode) << 16
                    bundle.writestr(
                        info, entries[name], compress_type=zipfile.ZIP_DEFLATED,
                        compresslevel=9,
                    )
        else:
            with gzip.GzipFile(filename="", mode="wb", fileobj=archive_file, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as bundle:
                    for name in sorted(entries):
                        info = tarfile.TarInfo(name)
                        info.mode = 0o755 if name in names[:3] else 0o644
                        info.size = len(entries[name])
                        bundle.addfile(info, io.BytesIO(entries[name]))
        archive_file.close()
        verify_bundle(archive, version, platform)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        with checksum.open("x", encoding="ascii", newline="\n") as output:
            owns_checksum = True
            output.write(f"{digest}  {archive.name}\n")
    except Exception:
        archive_file.close()
        archive.unlink(missing_ok=True)
        if owns_checksum:
            checksum.unlink(missing_ok=True)
        raise
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True, choices=PLATFORMS)
    parser.add_argument("--arch", required=True, choices=ARCHITECTURES)
    parser.add_argument("--binary-dir", type=Path, default=ROOT / "target/release")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--metadata", type=Path,
                        help="cargo metadata --locked --format-version 1 --filter-platform HOST")
    source.add_argument("--notices", type=Path,
                        help="dependency notices retained from the native build before signing")
    args = parser.parse_args()
    try:
        build_bundle(
            ROOT, args.binary_dir, args.output_dir, args.version, args.platform,
            args.arch,
            metadata=json.loads(args.metadata.read_text(encoding="utf-8")) if args.metadata else None,
            notices=args.notices.read_bytes() if args.notices else None,
        )
    except (ValueError, KeyError, OSError, zipfile.BadZipFile, tarfile.TarError) as error:
        parser.exit(1, f"tunnel bundle failed: {error}\n")


if __name__ == "__main__":
    main()
