#!/usr/bin/env python3
"""Generate cargo-packager-updater metadata from signed release artifacts."""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse

SUPPORTED_ARTIFACTS = {
    "macos-x86_64": {"app": ".app.tar.gz"},
    "macos-aarch64": {"app": ".app.tar.gz"},
    "linux-x86_64": {"appimage": ".AppImage"},
    "linux-aarch64": {"appimage": ".AppImage"},
    "windows-x86_64": {"nsis": ".exe"},
}


class ManifestError(Exception):
    pass


def parse_artifact(value: str) -> tuple[str, Path, str, str]:
    parts = value.split("=", 3)
    if len(parts) != 4:
        raise argparse.ArgumentTypeError(
            "artifact must be TARGET=PATH=URL=FORMAT"
        )
    target, path, url, update_format = parts
    if target not in SUPPORTED_ARTIFACTS:
        raise argparse.ArgumentTypeError(f"unsupported updater target: {target}")
    if update_format not in SUPPORTED_ARTIFACTS[target]:
        raise argparse.ArgumentTypeError(f"unsupported updater format: {update_format}")
    parsed = urlparse(url)
    if parsed.scheme != "https" or parsed.netloc != "github.com" or parsed.query:
        raise argparse.ArgumentTypeError(
            "artifact URL must be an immutable github.com HTTPS URL"
        )
    return target, Path(path), url, update_format


def validate_artifact(
    version: str,
    target: str,
    path: Path,
    url: str,
    update_format: str,
) -> None:
    formats = SUPPORTED_ARTIFACTS.get(target)
    if formats is None:
        raise ManifestError(f"unsupported updater target: {target}")
    suffix = formats.get(update_format)
    if suffix is None:
        raise ManifestError(
            f"updater target {target} does not support format {update_format}"
        )
    if not path.name.endswith(suffix):
        raise ManifestError(
            f"{target} {update_format} artifact must end with {suffix}: {path}"
        )
    parsed = urlparse(url)
    release_prefix = f"/fes/fesTerm/releases/download/v{version}/"
    if (
        parsed.scheme != "https"
        or parsed.netloc != "github.com"
        or parsed.query
        or not parsed.path.startswith(release_prefix)
        or not parsed.path.endswith(suffix)
    ):
        raise ManifestError(
            f"artifact URL must be an immutable fesTerm v{version} GitHub Release "
            f"URL ending with {suffix}: {url}"
        )


def generate(
    version: str,
    notes: str,
    artifacts: list[tuple[str, Path, str, str]],
    published_at: str,
) -> dict[str, object]:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ManifestError("version must be MAJOR.MINOR.PATCH")
    platforms: dict[str, object] = {}
    for target, path, url, update_format in artifacts:
        if target in platforms:
            raise ManifestError(f"duplicate updater target: {target}")
        validate_artifact(version, target, path, url, update_format)
        if not path.is_file():
            raise ManifestError(f"signed updater artifact is missing: {path}")
        signature_path = Path(f"{path}.sig")
        if not signature_path.is_file():
            raise ManifestError(f"updater signature is missing: {signature_path}")
        signature = signature_path.read_text(encoding="utf-8").strip()
        if not signature:
            raise ManifestError(f"updater signature is empty: {signature_path}")
        platforms[target] = {
            "signature": signature,
            "url": url,
            "format": update_format,
        }
    if not platforms:
        raise ManifestError("at least one signed updater artifact is required")
    return {
        "version": f"v{version}",
        "notes": notes,
        "pub_date": published_at,
        "platforms": platforms,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--notes-file", type=Path, required=True)
    parser.add_argument("--artifact", action="append", type=parse_artifact, default=[])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--published-at",
        default=datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
    )
    args = parser.parse_args()
    try:
        notes = args.notes_file.read_text(encoding="utf-8").strip()
        manifest = generate(args.version, notes, args.artifact, args.published_at)
        args.output.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    except (ManifestError, OSError) as error:
        print(f"update manifest: FAIL\n{error}", file=sys.stderr)
        return 1
    print(f"update manifest: PASS ({len(args.artifact)} targets)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
