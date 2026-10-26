#!/usr/bin/env python3
"""Verify and monitor fesTerm's embedded and optional terminal fonts."""

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
OPTIONAL_FAMILIES = ("iosevka-term", "julia-mono", "maple-mono")
NOTICE_NAME = re.compile(
    r"^(?:LICENSE(?:\..*)?|OFL(?:\..*)?|AUTHORS(?:\..*)?|NOTICE(?:\..*)?|COPYRIGHT(?:\..*)?)$",
    re.IGNORECASE,
)


class FontError(Exception):
    pass


def load_manifest(path: Path) -> dict[str, object]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1:
        raise FontError("font manifest schema_version must be 1")
    return manifest


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def optional_downloads(root: Path) -> list[dict[str, object]]:
    """Derive release assets from the checked-in catalog, never upstream archives."""
    assets = []
    for family in OPTIONAL_FAMILIES:
        manifest = load_manifest(root / "assets/fonts" / family / "manifest.json")
        verify(root, manifest)
        for entry in manifest["files"]:
            source = Path(entry["path"])
            if source.suffix.lower() == ".ttf":
                suffix = ".ttf"
            elif source.name.startswith("LICENSE"):
                suffix = ".license.txt"
            else:
                continue
            checksum = entry.get("sha256")
            size = entry.get("size_bytes")
            if not isinstance(checksum, str) or not re.fullmatch("[0-9a-f]{64}", checksum):
                raise FontError(f"download asset requires exact SHA256: {source}")
            if type(size) is not int or size <= 0:
                raise FontError(f"download asset requires exact size_bytes: {source}")
            assets.append({
                "family": manifest["family"],
                "path": entry["path"],
                "name": f"festerm-font-{checksum}{suffix}",
                "sha256": checksum,
                "size_bytes": size,
            })
    return sorted(assets, key=lambda asset: asset["name"])


def runtime_rust_source(source: str) -> str:
    """Ignore comments and cfg(test) items when checking production embeddings."""
    tokens = re.compile(
        r'//[^\n]*|/\*.*?\*/|r(?P<hashes>#+)".*?"(?P=hashes)|'
        r'r".*?"|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'',
        re.DOTALL,
    )
    masked = tokens.sub(lambda match: " " * len(match[0]), source)
    result = list(source)
    for match in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", masked):
        start = match.end()
        depth = 0
        # Skip any further attributes, then consume the complete item.
        while start < len(masked):
            if masked[start].isspace():
                start += 1
            elif masked.startswith("#[", start):
                end = masked.find("]", start)
                start = len(masked) if end < 0 else end + 1
            else:
                break
        end = start
        while end < len(masked):
            char = masked[end]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            elif char == ";" and depth == 0:
                end += 1
                break
            end += 1
        result[match.start():end] = " " * (end - match.start())
    # Comments must not masquerade as embeddings, but retain string arguments.
    uncommented = "".join(result)
    return tokens.sub(
        lambda match: " " * len(match[0])
        if match[0].startswith(("//", "/*")) else match[0],
        uncommented,
    )


def verify_runtime_embed_exclusion(root: Path) -> None:
    violations = []
    optional_path = "|".join(re.escape(family) for family in OPTIONAL_FAMILIES)
    embedding = re.compile(
        rf'include_bytes!\s*\(\s*(?:r#*)?"[^"]*assets/fonts/'
        rf'(?:{optional_path})/[^"]*\.ttf"',
        re.IGNORECASE,
    )
    for directory in ("app", "crates"):
        for path in sorted((root / directory).glob("*/src/**/*.rs")):
            if embedding.search(runtime_rust_source(path.read_text(encoding="utf-8"))):
                violations.append(str(path.relative_to(root)))
    if violations:
        raise FontError("optional fonts embedded in runtime: " + ", ".join(violations))


def production_rust_source(root: Path) -> str:
    return "\n".join(
        runtime_rust_source(path.read_text(encoding="utf-8"))
        for directory in ("app", "crates")
        for path in sorted((root / directory).glob("*/src/**/*.rs"))
    )


def runtime_embedded_font_families(production: str) -> set[str]:
    embedding = re.compile(
        r'include_bytes!\s*\(\s*(?:r#*)?"[^"]*assets/fonts/'
        r'(?P<family>[^/"]+)/[^"]*\.ttf"',
        re.IGNORECASE,
    )
    return {match.group("family") for match in embedding.finditer(production)}


def manifest_notice_entries(manifest: dict[str, object]) -> list[dict[str, object]]:
    return [
        entry
        for entry in manifest["files"]
        if isinstance(entry, dict) and NOTICE_NAME.fullmatch(Path(str(entry["path"])).name)
    ]


def embedded_font_notices(production: str) -> dict[str, tuple[str, str]]:
    notice = re.compile(
        r'FontLicenseNotice\s*\{\s*'
        r'family:\s*"(?P<family>[^"]+)",\s*'
        r'version:\s*"(?P<version>[^"]+)",\s*'
        r'notice:\s*"[^"]+",\s*'
        r'text:\s*include_str!\s*\(\s*(?:r#*)?"[^"]*'
        r'(?P<path>assets/fonts/[^"]+)"\s*\),\s*\}',
        re.DOTALL,
    )
    notices: dict[str, tuple[str, str]] = {}
    for match in notice.finditer(production):
        path = match.group("path")
        if path in notices:
            raise FontError(f"font notice embedded more than once: {path}")
        notices[path] = (match.group("family"), match.group("version"))
    return notices


def verify_runtime_license_embeddings(root: Path) -> None:
    production = production_rust_source(root)
    manifests = {
        path.parent.name: load_manifest(path)
        for path in sorted((root / "assets/fonts").glob("*/manifest.json"))
    }
    required_families = set(OPTIONAL_FAMILIES)
    required_families.update(runtime_embedded_font_families(production))
    embedded = embedded_font_notices(production)
    errors = []
    for family in sorted(required_families):
        manifest = manifests.get(family)
        if manifest is None:
            errors.append(f"runtime font family lacks a manifest: {family}")
            continue
        notices = manifest_notice_entries(manifest)
        if not notices:
            errors.append(f"font manifest lacks a complete notice: {family}")
            continue
        for entry in notices:
            relative = str(entry["path"])
            label = embedded.get(relative)
            if label is None:
                errors.append(f"font notice is not embedded in production: {relative}")
                continue
            expected = (str(manifest["family"]), str(manifest["pinned_version"]))
            if label != expected:
                errors.append(
                    f"runtime font notice label drift for {relative}: "
                    f"{label[0]} {label[1]}, expected {expected[0]} {expected[1]}"
                )
    if errors:
        raise FontError("\n".join(errors))


def stage_downloads(root: Path, destination: Path) -> dict[str, object]:
    assets = optional_downloads(root)
    names = {asset["name"] for asset in assets}
    if destination.exists():
        unexpected = sorted(path.name for path in destination.iterdir() if path.name not in names)
        if unexpected:
            raise FontError(f"unexpected files in download staging directory: {unexpected}")
    # Preflight every existing asset before writing anything; never clobber bad bytes.
    for asset in assets:
        target = destination / asset["name"]
        if target.is_symlink() or (target.exists() and (
            not target.is_file()
            or target.stat().st_size != asset["size_bytes"]
            or digest(target.read_bytes()) != asset["sha256"]
        )):
            raise FontError(f"staged download differs; refusing to overwrite: {target}")
    destination.mkdir(parents=True, exist_ok=True)
    for asset in assets:
        target = destination / asset["name"]
        if not target.exists():
            with target.open("xb") as output:
                output.write((root / asset["path"]).read_bytes())
    faces = [asset for asset in assets if asset["name"].endswith(".ttf")]
    return {
        "schema_version": 1,
        "face_count": len(faces),
        "font_size_bytes": sum(asset["size_bytes"] for asset in faces),
        "total_size_bytes": sum(asset["size_bytes"] for asset in assets),
        "assets": assets,
    }


def verify(root: Path, manifest: dict[str, object]) -> None:
    errors: list[str] = []
    source_kind = str(manifest.get("source_kind", "archive"))
    if source_kind not in {"archive", "direct"}:
        errors.append(f"unsupported source_kind: {source_kind!r}")
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
        source_url = str(entry.get("source_url", ""))
        expected = str(entry.get("sha256", ""))
        expected_size = entry.get("size_bytes")
        if relative in destinations:
            errors.append(f"duplicate destination: {relative}")
        source_identity = archive_path if source_kind == "archive" else source_url
        if source_identity in archive_paths:
            errors.append(f"duplicate upstream source: {source_identity}")
        destinations.add(relative)
        archive_paths.add(source_identity)
        if source_kind == "archive" and not archive_path:
            errors.append(f"missing archive_path for {relative}")
        if source_kind == "direct" and not source_url.startswith("https://"):
            errors.append(f"invalid source_url for {relative}")
        if (
            not relative.startswith("assets/fonts/")
            or "\\" in relative
            or not (root / relative).resolve().is_relative_to((root / "assets/fonts").resolve())
        ):
            errors.append(f"font destination escapes owned directory: {relative}")
            continue
        path = root / relative
        if not path.is_file():
            errors.append(f"bundled font file is missing: {relative}")
        elif digest(path.read_bytes()) != expected:
            errors.append(f"bundled font checksum differs: {relative}")
        elif expected_size is not None and path.stat().st_size != expected_size:
            errors.append(f"bundled font size differs: {relative}")

    version = str(manifest.get("pinned_version", ""))
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", version):
        errors.append(f"invalid pinned_version: {version!r}")
    if source_kind == "archive":
        release = str(manifest.get("pinned_release", ""))
        archive_url = str(manifest.get("archive_url", ""))
        if release != f"v{version}":
            errors.append("pinned_release must be 'v' plus pinned_version")
        if release not in archive_url:
            errors.append("archive_url does not match the pinned release/version")
    else:
        for entry in files:
            if not isinstance(entry, dict):
                continue
            commit = str(entry.get("source_commit", ""))
            if not re.fullmatch(r"[0-9a-f]{40}", commit):
                errors.append(
                    f"direct source requires a full source_commit: {entry.get('path', '')}"
                )
            elif commit not in str(entry.get("source_url", "")):
                errors.append(
                    f"direct source URL does not contain pinned commit: {entry.get('path', '')}"
                )
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


def latest_release(manifest: dict[str, object]) -> dict[str, object]:
    repository = str(manifest["upstream_repository"])
    payload = request_json(f"https://api.github.com/repos/{repository}/releases/latest")
    tag = str(payload.get("tag_name", ""))
    if not re.fullmatch(r"v[0-9]+(?:\.[0-9]+)+", tag):
        raise FontError(f"upstream latest release has unexpected tag: {tag!r}")
    version = tag.removeprefix("v")
    return {
        "tag": tag,
        "version": version,
        "release_url": str(payload.get("html_url", "")),
        "assets": payload.get("assets", []),
    }


def jetbrains_archive_url(release: dict[str, object]) -> str:
    version = str(release["version"])
    expected_asset = f"JetBrainsMono-{version}.zip"
    assets = release.get("assets", [])
    archive_url = next(
        (
            str(asset["browser_download_url"])
            for asset in assets
            if isinstance(asset, dict) and asset.get("name") == expected_asset
        ),
        "",
    )
    if not archive_url:
        raise FontError(f"upstream release {release['tag']} lacks {expected_asset}")
    return archive_url


def version_tuple(version: str) -> tuple[int, ...]:
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", version):
        raise FontError(f"unsupported release version: {version!r}")
    return tuple(int(part) for part in version.split("."))


def update_available(current: str, latest: str) -> bool:
    return version_tuple(latest) > version_tuple(current)


def download(url: str) -> bytes:
    return request_bytes(url, 60)


def verify_archive(manifest: dict[str, object]) -> None:
    if manifest.get("source_kind") == "direct":
        for entry in manifest["files"]:
            expected = str(entry.get("sha256", ""))
            actual = digest(download(str(entry["source_url"])))
            if actual != expected:
                raise FontError(
                    f"{manifest.get('family')} upstream source checksum differs: "
                    f"{entry.get('path')}"
                )
        return
    expected = str(manifest.get("archive_sha256", ""))
    if not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise FontError(f"{manifest.get('family')} lacks a valid archive_sha256")
    actual = digest(download(str(manifest["archive_url"])))
    if actual != expected:
        raise FontError(f"{manifest.get('family')} upstream archive checksum differs")


def update_assets(
    root: Path,
    manifest_path: Path,
    manifest: dict[str, object],
    release: dict[str, object],
) -> None:
    old_version = str(manifest["pinned_version"])
    archive_url = str(release["archive_url"])
    archive = download(archive_url)
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
            entry["size_bytes"] = len(data)

    manifest["pinned_release"] = str(release["tag"])
    manifest["pinned_version"] = str(release["version"])
    manifest["archive_url"] = archive_url
    manifest["archive_sha256"] = digest(archive)
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
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--check-upstream", action="store_true")
    parser.add_argument("--update-latest", action="store_true")
    parser.add_argument("--verify-archives", action="store_true")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument(
        "--stage-downloads", type=Path,
        help="stage optional TTFs and license notices under content-addressed release names",
    )
    parser.add_argument(
        "--downloads-report", type=Path,
        help="write deterministic JSON asset hashes, sizes and totals (requires --stage-downloads)",
    )
    args = parser.parse_args()
    if args.downloads_report and not args.stage_downloads:
        parser.error("--downloads-report requires --stage-downloads")
    if args.stage_downloads and (
        args.manifest or args.check_upstream or args.update_latest or args.verify_archives
    ):
        parser.error("--stage-downloads requires the complete local catalog only")
    try:
        if args.stage_downloads:
            report = stage_downloads(ROOT, args.stage_downloads)
            serialized = json.dumps(report, indent=2) + "\n"
            if args.downloads_report:
                args.downloads_report.write_text(serialized, encoding="utf-8")
            print(serialized, end="")
            return 0
        manifest_path = (args.manifest or DEFAULT_MANIFEST).resolve()
        root = ROOT if manifest_path.is_relative_to(ROOT) else manifest_path.parents[3]
        manifest = load_manifest(manifest_path)
        if not (args.check_upstream or args.update_latest or args.verify_archives):
            manifests = (
                [manifest_path]
                if args.manifest
                else sorted((ROOT / "assets/fonts").glob("*/manifest.json"))
            )
            summaries = []
            for path in manifests:
                candidate = load_manifest(path)
                verify(ROOT, candidate)
                summaries.append(f"{candidate['family']} {candidate['pinned_version']}")
            optional_downloads(ROOT)
            verify_runtime_embed_exclusion(ROOT)
            verify_runtime_license_embeddings(ROOT)
            print(f"bundled fonts: PASS ({', '.join(summaries)})")
            return 0
        if args.verify_archives:
            manifests = (
                [manifest_path]
                if args.manifest
                else sorted((ROOT / "assets/fonts").glob("*/manifest.json"))
            )
            for path in manifests:
                candidate = load_manifest(path)
                verify(ROOT, candidate)
                verify_archive(candidate)
            print(f"bundled font archives: PASS ({len(manifests)} verified)")
            return 0
        verify(root, manifest)
        if args.update_latest and manifest_path != DEFAULT_MANIFEST:
            raise FontError(
                "automatic asset staging currently supports JetBrains Mono only"
            )

        release = latest_release(manifest)
        available = update_available(
            str(manifest["pinned_version"]), str(release["version"])
        )
        values = {
            "family": str(manifest["family"]),
            "current_version": str(manifest["pinned_version"]),
            "latest_version": str(release["version"]),
            "latest_tag": str(release["tag"]),
            "release_url": str(release["release_url"]),
            "update_available": str(available).lower(),
        }
        if args.github_output:
            write_github_output(args.github_output, values)
        if args.report:
            update_instruction = (
                "`python scripts/manage_bundled_font.py --update-latest`"
                if manifest_path == DEFAULT_MANIFEST
                else "update the pinned assets and manifest deliberately from the official release"
            )
            args.report.write_text(
                f"# {values['family']} update available\n\n"
                f"fesTerm pins **{values['current_version']}**; upstream latest is "
                f"**{values['latest_version']}**.\n\n"
                f"Release: {values['release_url']}\n\n"
                f"Review the release and {update_instruction}, then "
                "review the resulting font metrics, glyph/fallback behavior, license, "
                "native DPI captures, terminal grid geometry, and full test suite. "
                "Do not merge this update automatically.\n",
                encoding="utf-8",
            )
        if args.update_latest:
            if not available:
                print(f"bundled font is current ({release['version']})")
                return 0
            release["archive_url"] = jetbrains_archive_url(release)
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
