#!/usr/bin/env python3
"""Verify, check, and deliberately update fesTerm's bundled terminal font."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "assets/fonts/jetbrains-mono/manifest.json"
USER_AGENT = "fesTerm bundled-font update checker"


class FontError(Exception):
    pass


def load_manifest(path: Path) -> dict[str, object]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1:
        raise FontError("font manifest schema_version must be 1")
    return manifest


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def verify(root: Path, manifest: dict[str, object]) -> None:
    errors: list[str] = []
    files = manifest.get("files")
    if not isinstance(files, list) or not files:
        raise FontError("font manifest files must be a non-empty list")
    destinations: set[str] = set()
    archive_paths: set[str] = set()
    for entry in files:
        if not isinstance(entry, dict):
            errors.append("font manifest file entry must be an object")
            continue
        relative = str(entry.get("path", ""))
        archive_path = str(entry.get("archive_path", ""))
        expected = str(entry.get("sha256", ""))
        if relative in destinations:
            errors.append(f"duplicate destination: {relative}")
        if archive_path in archive_paths:
            errors.append(f"duplicate archive path: {archive_path}")
        destinations.add(relative)
        archive_paths.add(archive_path)
        if not relative.startswith("assets/fonts/jetbrains-mono/"):
            errors.append(f"font destination escapes owned directory: {relative}")
            continue
        path = root / relative
        if not path.is_file():
            errors.append(f"bundled font file is missing: {relative}")
        elif digest(path.read_bytes()) != expected:
            errors.append(f"bundled font checksum differs: {relative}")

    version = str(manifest.get("pinned_version", ""))
    release = str(manifest.get("pinned_release", ""))
    archive_url = str(manifest.get("archive_url", ""))
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", version):
        errors.append(f"invalid pinned_version: {version!r}")
    if release != f"v{version}":
        errors.append("pinned_release must be 'v' plus pinned_version")
    if release not in archive_url or f"JetBrainsMono-{version}.zip" not in archive_url:
        errors.append("archive_url does not match the pinned release/version")
    for marker in manifest.get("version_markers", []):
        path = root / str(marker)
        if not path.is_file():
            errors.append(f"version marker file is missing: {marker}")
        elif version not in path.read_text(encoding="utf-8"):
            errors.append(f"version marker {marker} does not name {version}")
    if errors:
        raise FontError("\n".join(errors))


def request_bytes(url: str, timeout: int) -> bytes:
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/vnd.github+json", "User-Agent": USER_AGENT},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.read()
    except urllib.error.URLError as urllib_error:
        # Some macOS Python installations are not connected to the native
        # trust store. Fall back to the platform curl binary, which still
        # performs ordinary TLS certificate and HTTP-status verification.
        result = subprocess.run(
            [
                "curl",
                "-fLsS",
                "--max-time",
                str(timeout),
                "-A",
                USER_AGENT,
                url,
            ],
            check=False,
            capture_output=True,
        )
        if result.returncode != 0:
            detail = result.stderr.decode("utf-8", errors="replace").strip()
            raise FontError(detail or str(urllib_error)) from urllib_error
        return result.stdout


def request_json(url: str) -> dict[str, object]:
    return json.loads(request_bytes(url, 30))


def latest_release(manifest: dict[str, object]) -> dict[str, str]:
    repository = str(manifest["upstream_repository"])
    payload = request_json(f"https://api.github.com/repos/{repository}/releases/latest")
    tag = str(payload.get("tag_name", ""))
    if not re.fullmatch(r"v[0-9]+(?:\.[0-9]+)+", tag):
        raise FontError(f"upstream latest release has unexpected tag: {tag!r}")
    version = tag.removeprefix("v")
    expected_asset = f"JetBrainsMono-{version}.zip"
    assets = payload.get("assets", [])
    archive_url = next(
        (
            str(asset["browser_download_url"])
            for asset in assets
            if isinstance(asset, dict) and asset.get("name") == expected_asset
        ),
        "",
    )
    if not archive_url:
        raise FontError(f"upstream release {tag} lacks {expected_asset}")
    return {
        "tag": tag,
        "version": version,
        "archive_url": archive_url,
        "release_url": str(payload.get("html_url", "")),
    }


def version_tuple(version: str) -> tuple[int, ...]:
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", version):
        raise FontError(f"unsupported release version: {version!r}")
    return tuple(int(part) for part in version.split("."))


def update_available(current: str, latest: str) -> bool:
    return version_tuple(latest) > version_tuple(current)


def download(url: str) -> bytes:
    return request_bytes(url, 60)


def update_assets(
    root: Path,
    manifest_path: Path,
    manifest: dict[str, object],
    release: dict[str, str],
) -> None:
    old_version = str(manifest["pinned_version"])
    archive = download(release["archive_url"])
    with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
        names = set(bundle.namelist())
        for entry in manifest["files"]:
            source = str(entry["archive_path"])
            if source not in names:
                raise FontError(f"release archive lacks required file: {source}")
        for entry in manifest["files"]:
            data = bundle.read(str(entry["archive_path"]))
            destination = root / str(entry["path"])
            destination.write_bytes(data)
            entry["sha256"] = digest(data)

    manifest["pinned_release"] = release["tag"]
    manifest["pinned_version"] = release["version"]
    manifest["archive_url"] = release["archive_url"]
    for marker in manifest.get("version_markers", []):
        path = root / str(marker)
        text = path.read_text(encoding="utf-8")
        if old_version not in text:
            raise FontError(f"cannot update missing {old_version} marker in {marker}")
        path.write_text(text.replace(old_version, release["version"]), encoding="utf-8")
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    verify(root, manifest)


def write_github_output(path: Path, values: dict[str, str]) -> None:
    with path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            if "\n" in value:
                raise FontError(f"GitHub output {key} contains a newline")
            output.write(f"{key}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--check-upstream", action="store_true")
    parser.add_argument("--update-latest", action="store_true")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    try:
        manifest_path = args.manifest.resolve()
        root = ROOT if manifest_path == DEFAULT_MANIFEST else manifest_path.parents[3]
        manifest = load_manifest(manifest_path)
        verify(root, manifest)
        if not (args.check_upstream or args.update_latest):
            print(f"bundled font: PASS ({manifest['family']} {manifest['pinned_version']})")
            return 0

        release = latest_release(manifest)
        available = update_available(str(manifest["pinned_version"]), release["version"])
        values = {
            "current_version": str(manifest["pinned_version"]),
            "latest_version": release["version"],
            "latest_tag": release["tag"],
            "release_url": release["release_url"],
            "update_available": str(available).lower(),
        }
        if args.github_output:
            write_github_output(args.github_output, values)
        if args.report:
            args.report.write_text(
                "# JetBrains Mono update available\n\n"
                f"fesTerm pins **{values['current_version']}**; upstream latest is "
                f"**{values['latest_version']}**.\n\n"
                f"Release: {values['release_url']}\n\n"
                "Run `python scripts/manage_bundled_font.py --update-latest`, then "
                "review the resulting font metrics, glyph/fallback behavior, license, "
                "native DPI captures, terminal grid geometry, and full test suite. "
                "Do not merge this update automatically.\n",
                encoding="utf-8",
            )
        if args.update_latest:
            if not available:
                print(f"bundled font is current ({release['version']})")
                return 0
            update_assets(root, manifest_path, manifest, release)
            print(f"updated bundled font to {release['version']}; review is required")
        else:
            print(json.dumps(values, indent=2))
        return 0
    except (FontError, OSError, urllib.error.URLError, zipfile.BadZipFile) as error:
        print(f"bundled font: FAIL\n{error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
