#!/usr/bin/env python3
"""Verify the pinned Unicode emoji corpus and optional upstream bytes."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = (
    ROOT / "tests" / "fixtures" / "unicode" / "emoji-15.1" / "manifest.json"
)


class EmojiDataError(RuntimeError):
    pass


def load_manifest(path: Path) -> dict:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1:
        raise EmojiDataError(f"unsupported schema_version in {path}")
    if not manifest.get("unicode_version") or not manifest.get("files"):
        raise EmojiDataError(f"incomplete Unicode emoji manifest {path}")
    return manifest


def verify(root: Path, manifest: dict, check_upstream: bool = False) -> None:
    version = manifest["unicode_version"]
    for entry in manifest["files"]:
        path = root / entry["path"]
        data = path.read_bytes()
        if len(data) != entry["size_bytes"]:
            raise EmojiDataError(f"{entry['path']} size differs from manifest")
        digest = hashlib.sha256(data).hexdigest()
        if digest != entry["sha256"]:
            raise EmojiDataError(f"{entry['path']} checksum differs from manifest")
        for mirror in entry.get("package_mirrors", []):
            if (root / mirror).read_bytes() != data:
                raise EmojiDataError(f"{mirror} differs from {entry['path']}")
        if path.name.startswith("emoji-"):
            marker = f"# Version: {version}".encode()
            if marker not in data:
                raise EmojiDataError(f"{entry['path']} does not declare version {version}")
        if check_upstream:
            with urllib.request.urlopen(entry["source_url"], timeout=60) as response:
                upstream = response.read()
            if upstream != data:
                raise EmojiDataError(f"{entry['path']} differs from upstream bytes")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--check-upstream", action="store_true")
    args = parser.parse_args()
    manifest_path = args.manifest.resolve()
    manifest = load_manifest(manifest_path)
    verify(ROOT, manifest, args.check_upstream)
    print(f"Unicode emoji {manifest['unicode_version']} data: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
