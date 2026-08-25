#!/usr/bin/env python3
"""Validate repository-owned native packaging metadata."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONFIGS = {
    "macos": ROOT / "packaging/macos.toml",
    "windows": ROOT / "packaging/windows.toml",
    "linux": ROOT / "packaging/linux.toml",
}
EXPECTED_FORMATS = {
    "macos": ["dmg"],
    "windows": ["nsis"],
    "linux": ["appimage", "deb"],
}
EXPECTED_MACOS_ICONS = [
    f"../assets/app-icon/app-icon-{size}.png"
    for size in (16, 32, 128, 256, 512)
]


class PackagingError(Exception):
    pass


def load_toml(path: Path) -> dict[str, object]:
    with path.open("rb") as source:
        return tomllib.load(source)


def workspace_version() -> str:
    cargo = load_toml(ROOT / "Cargo.toml")
    return str(cargo["workspace"]["package"]["version"])


def verify() -> None:
    version = workspace_version()
    errors: list[str] = []
    updater_public_key_path = ROOT / "packaging/updater.pub"
    try:
        updater_public_key = updater_public_key_path.read_text(encoding="ascii").strip()
    except OSError as error:
        errors.append(f"cannot read packaging/updater.pub: {error}")
    else:
        if not updater_public_key or "\n" in updater_public_key:
            errors.append("packaging/updater.pub must contain one non-empty encoded key")

    for platform, path in CONFIGS.items():
        config = load_toml(path)
        if config.get("version") != version:
            errors.append(f"{path.relative_to(ROOT)} does not pin version {version}")
        if config.get("product-name") != "fesTerm":
            errors.append(f"{path.relative_to(ROOT)} has the wrong product name")
        if config.get("name") != "festerm":
            errors.append(f"{path.relative_to(ROOT)} has the wrong package name")
        if config.get("identifier") != "dev.fes.festerm":
            errors.append(f"{path.relative_to(ROOT)} has the wrong application identifier")
        if config.get("formats") != EXPECTED_FORMATS[platform]:
            errors.append(f"{path.relative_to(ROOT)} has unexpected package formats")
        binaries = config.get("binaries")
        if binaries != [{"path": "festerm", "main": True}]:
            errors.append(f"{path.relative_to(ROOT)} must package only the fesTerm binary")

    macos = load_toml(CONFIGS["macos"])
    if macos.get("icons") != EXPECTED_MACOS_ICONS:
        errors.append("macOS packaging has unsupported or incomplete ICNS source sizes")

    windows = load_toml(CONFIGS["windows"])
    expected_windows_icon = "../assets/app-icon/festerm.ico"
    if windows.get("icons") != [expected_windows_icon]:
        errors.append("Windows packaging must use the generated ICO container")
    if windows.get("nsis", {}).get("installer-icon") != expected_windows_icon:
        errors.append("NSIS packaging must use the generated installer icon")
    resources = windows.get("resources", [])
    expected_runtime = {
        "src": "../target/release/runtime/conpty",
        "target": "runtime/conpty",
    }
    if expected_runtime not in resources:
        errors.append("Windows packaging does not own the required ConPTY sidecar")

    if errors:
        raise PackagingError("\n".join(errors))


def main() -> int:
    argparse.ArgumentParser().parse_args()
    try:
        verify()
    except (KeyError, OSError, PackagingError, tomllib.TOMLDecodeError) as error:
        print(f"packaging metadata: FAIL\n{error}", file=sys.stderr)
        return 1
    print(f"packaging metadata: PASS ({workspace_version()})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
