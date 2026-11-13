#!/usr/bin/env python3
"""Bind a screenshot run to unchanged widget sources and an isolated profile."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import platform
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def source_digest() -> str:
    files = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard",
         "--", "app", "crates", "vendor", "assets", "packaging",
         "Cargo.toml", "Cargo.lock", "rust-toolchain.toml"],
        cwd=ROOT,
    ).split(b"\0")
    digest = hashlib.sha256()
    for name in sorted(set(filter(None, files))):
        path = ROOT / name.decode()
        if path.is_file():
            digest.update(name + b"\0" + hashlib.sha256(path.read_bytes()).digest())
        else:
            digest.update(name + b"\0missing\0")
    return digest.hexdigest()


def finish(metadata: dict, manifest: dict, current_digest: str) -> dict:
    if metadata["source_tree_sha256"] != current_digest:
        raise ValueError("widget sources changed during capture; retain the attempt and rerun")
    manifest["capture"] = {
        **metadata,
        "completed_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("begin", "finish"))
    parser.add_argument("metadata", type=Path)
    parser.add_argument("--manifest", type=Path)
    args = parser.parse_args()
    if args.phase == "begin":
        if args.metadata.exists():
            parser.error("capture metadata already exists; use a fresh attempt")
        metadata = {
            "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "source_revision": subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
            ).strip(),
            "source_dirty": bool(subprocess.check_output(
                ["git", "status", "--porcelain"], cwd=ROOT
            ).strip()),
            "source_tree_sha256": source_digest(),
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "scope": "Production egui widgets, headless wgpu; not native compositor or usability acceptance",
            "isolation": "Fresh repository-owned HOME/XDG directories; synthetic fixtures, no personal profile/credentials/clipboard",
        }
        args.metadata.parent.mkdir(parents=True, exist_ok=True)
        args.metadata.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    else:
        if args.manifest is None:
            parser.error("finish requires --manifest")
        metadata = json.loads(args.metadata.read_text(encoding="utf-8"))
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
        args.manifest.write_text(
            json.dumps(finish(metadata, manifest, source_digest()), indent=2) + "\n",
            encoding="utf-8",
        )


if __name__ == "__main__":
    main()
