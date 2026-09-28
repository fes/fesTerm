#!/usr/bin/env python3
"""Validate repository-owned native packaging metadata."""

from __future__ import annotations

import argparse
import difflib
import plistlib
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
EXPECTED_BINARIES = [
    {"path": "festerm", "main": True},
    {"path": "festerm-sessiond", "main": False},
]
EXPECTED_MARKDOWN_ASSOCIATION = {
    "extensions": ["md", "markdown"],
    "mime-type": "text/markdown",
    "name": "Markdown Document",
    "description": "Markdown document",
    "role": "viewer",
}
EXPECTED_OPEN_COMMAND = '"$\\"$INSTDIR\\${MAINBINARYNAME}.exe$\\" --open -- $\\"%1$\\""'
NSIS_TEMPLATE_NOTICE = """; Derived from cargo-packager 0.11.8 src/package/nsis/installer.nsi.
; Upstream: https://github.com/crabnebula-dev/cargo-packager
; Copyright (c) 2023 - Present CrabNebula Ltd.
; Copyright (c) 2019 - 2023 Tauri Programme within The Commons Conservancy
; Copyright (c) 2017 - 2019 Cargo-Bundle developers
; SPDX-License-Identifier: MIT
;
; fesTerm carries this copy only because cargo-packager 0.11.8's built-in
; FileAssociation.nsh path assigns extension defaults. The only intended
; functional change is replacing that default-seizing association block with
; current-user Markdown OpenWithProgids registration/removal that invokes
; `festerm --open -- "%1"`.

"""
UPSTREAM_NSIS_INSTALL_ASSOCIATIONS = """  ; Create file associations
  {{#each file_associations as |association| ~}}
    {{#each association.extensions as |ext| ~}}
       !insertmacro APP_ASSOCIATE "{{ext}}" "{{or association.name ext}}" "{{association-description association.description ext}}" "$INSTDIR\\${MAINBINARYNAME}.exe,0" "Open with ${PRODUCTNAME}" "$INSTDIR\\${MAINBINARYNAME}.exe $\\"%1$\\""
    {{/each}}
  {{/each}}
"""
FESTERM_NSIS_INSTALL_ASSOCIATIONS = """  ; Register fesTerm as an optional Markdown Open With handler without replacing defaults.
  WriteRegStr SHCTX "Software\\Classes\\fesTerm.Markdown" "" "Markdown Document"
  WriteRegStr SHCTX "Software\\Classes\\fesTerm.Markdown\\DefaultIcon" "" "$\\"$INSTDIR\\${MAINBINARYNAME}.exe$\\",0"
  WriteRegStr SHCTX "Software\\Classes\\fesTerm.Markdown\\shell" "" "open"
  WriteRegStr SHCTX "Software\\Classes\\fesTerm.Markdown\\shell\\open" "" "Open with ${PRODUCTNAME}"
  WriteRegStr SHCTX "Software\\Classes\\fesTerm.Markdown\\shell\\open\\command" "" "$\\"$INSTDIR\\${MAINBINARYNAME}.exe$\\" --open -- $\\"%1$\\""
  WriteRegStr SHCTX "Software\\Classes\\.md\\OpenWithProgids" "fesTerm.Markdown" ""
  WriteRegStr SHCTX "Software\\Classes\\.markdown\\OpenWithProgids" "fesTerm.Markdown" ""
  !insertmacro UPDATEFILEASSOC
"""
UPSTREAM_NSIS_UNINSTALL_ASSOCIATIONS = """  ; Delete app associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
      !insertmacro APP_UNASSOCIATE "{{ext}}" "{{or association.name ext}}"
    {{/each}}
  {{/each}}
"""
FESTERM_NSIS_UNINSTALL_ASSOCIATIONS = """  ; Remove only fesTerm-owned Markdown Open With entries.
  ReadRegStr $R7 SHCTX "Software\\Classes\\fesTerm.Markdown\\shell\\open\\command" ""
  ${If} $R7 == "$\\"$INSTDIR\\${MAINBINARYNAME}.exe$\\" --open -- $\\"%1$\\""
    DeleteRegValue SHCTX "Software\\Classes\\.md\\OpenWithProgids" "fesTerm.Markdown"
    DeleteRegValue SHCTX "Software\\Classes\\.markdown\\OpenWithProgids" "fesTerm.Markdown"
    DeleteRegKey SHCTX "Software\\Classes\\fesTerm.Markdown"
    !insertmacro UPDATEFILEASSOC
  ${EndIf}
"""


class PackagingError(Exception):
    pass


def load_toml(path: Path) -> dict[str, object]:
    with path.open("rb") as source:
        return tomllib.load(source)


def load_plist(path: Path) -> dict[str, object]:
    with path.open("rb") as source:
        value = plistlib.load(source)
    if not isinstance(value, dict):
        raise PackagingError(f"{path.relative_to(ROOT)} must contain a plist dictionary")
    return value


def workspace_version() -> str:
    cargo = load_toml(ROOT / "Cargo.toml")
    return str(cargo["workspace"]["package"]["version"])


def substituted_linux_desktop_template(template: str, exec_value: str) -> str:
    rendered = template.replace("{{categories}}", "Utility;TerminalEmulator;")
    rendered = rendered.replace("{{comment}}", "A compact terminal.")
    rendered = rendered.replace("{{exec}}", exec_value)
    rendered = rendered.replace("{{icon}}", "festerm")
    rendered = rendered.replace("{{name}}", "fesTerm")
    rendered = rendered.replace("{{#if mime_type}}\n", "")
    rendered = rendered.replace("{{/if}}\n", "")
    return rendered.replace("{{mime_type}}", "text/markdown")


def verify_nsis_template_against_upstream_if_available(
    nsis_template: str, errors: list[str]
) -> None:
    registry = Path.home() / ".cargo/registry/src"
    candidates = sorted(registry.glob("*/cargo-packager-0.11.8/src/package/nsis/installer.nsi"))
    if not candidates:
        return
    upstream = candidates[0].read_text(encoding="utf-8")
    expected = upstream.replace(
        UPSTREAM_NSIS_INSTALL_ASSOCIATIONS,
        FESTERM_NSIS_INSTALL_ASSOCIATIONS,
        1,
    ).replace(
        UPSTREAM_NSIS_UNINSTALL_ASSOCIATIONS,
        FESTERM_NSIS_UNINSTALL_ASSOCIATIONS,
        1,
    )
    expected = NSIS_TEMPLATE_NOTICE + expected
    if nsis_template != expected:
        diff = "\n".join(
            difflib.unified_diff(
                expected.splitlines(),
                nsis_template.splitlines(),
                fromfile="expected cargo-packager-0.11.8 + fesTerm OpenWith",
                tofile="packaging/windows-open-with.nsi",
                lineterm="",
                n=3,
            )
        )
        errors.append(
            "Windows NSIS template differs from the pinned cargo-packager 0.11.8 "
            f"template outside the intended Open With replacement:\n{diff[:4000]}"
        )


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
        expected_binaries = (
            [{"path": "festerm", "main": True}]
            if platform == "windows"
            else EXPECTED_BINARIES
        )
        if binaries != expected_binaries:
            errors.append(
                f"{path.relative_to(ROOT)} has unexpected packaged binaries"
            )

    macos = load_toml(CONFIGS["macos"])
    if macos.get("icons") != EXPECTED_MACOS_ICONS:
        errors.append("macOS packaging has unsupported or incomplete ICNS source sizes")
    if macos.get("file-associations") or macos.get("file_associations"):
        errors.append("macOS Markdown associations must come from the reviewed plist")
    if macos.get("macos", {}).get("info-plist-path") != "../packaging/macos-info.plist":
        errors.append("macOS packaging must merge the reviewed Markdown Info.plist")
    try:
        macos_plist = load_plist(ROOT / "packaging/macos-info.plist")
    except (OSError, plistlib.InvalidFileException, PackagingError) as error:
        errors.append(f"cannot read macOS Markdown Info.plist: {error}")
    else:
        document_types = macos_plist.get("CFBundleDocumentTypes")
        imported_types = macos_plist.get("UTImportedTypeDeclarations")
        expected_document_type = {
            "CFBundleTypeExtensions": ["md", "markdown"],
            "CFBundleTypeName": "Markdown Document",
            "CFBundleTypeRole": "Viewer",
            "LSHandlerRank": "Alternate",
            "LSItemContentTypes": [
                "net.daringfireball.markdown",
                "public.markdown",
            ],
        }
        if document_types != [expected_document_type]:
            errors.append(
                "macOS Markdown document type must be Viewer/Alternate for .md/.markdown"
            )
        expected_imported_type = {
            "UTTypeConformsTo": ["public.plain-text", "public.text"],
            "UTTypeDescription": "Markdown Document",
            "UTTypeIdentifier": "net.daringfireball.markdown",
            "UTTypeTagSpecification": {
                "public.filename-extension": ["md", "markdown"],
                "public.mime-type": "text/markdown",
            },
        }
        if imported_types != [expected_imported_type]:
            errors.append("macOS packaging must import the Markdown UTI without owning it")

    windows = load_toml(CONFIGS["windows"])
    expected_windows_icon = "../assets/app-icon/festerm.ico"
    if windows.get("icons") != [expected_windows_icon]:
        errors.append("Windows packaging must use the generated ICO container")
    if windows.get("nsis", {}).get("installer-icon") != expected_windows_icon:
        errors.append("NSIS packaging must use the generated installer icon")
    if windows.get("nsis", {}).get("installMode") != "currentUser":
        errors.append("NSIS packaging must use cargo-packager's current-user install mode")
    if windows.get("file-associations") or windows.get("file_associations"):
        errors.append("Windows packaging must not use default-seizing file-associations")
    if windows.get("nsis", {}).get("template") != "../packaging/windows-open-with.nsi":
        errors.append("Windows packaging must use the reviewed Open With NSIS template")
    try:
        nsis_template = (ROOT / "packaging/windows-open-with.nsi").read_text(
            encoding="utf-8"
        )
    except OSError as error:
        errors.append(f"cannot read Windows Open With NSIS template: {error}")
    else:
        if not nsis_template.startswith(NSIS_TEMPLATE_NOTICE):
            errors.append("Windows NSIS template must preserve upstream attribution/license")
        license_text = (ROOT / "packaging/cargo-packager-template.LICENSE-MIT").read_text(
            encoding="utf-8"
        )
        if "MIT License" not in license_text or "CrabNebula Ltd." not in license_text:
            errors.append("cargo-packager NSIS template MIT license notice is incomplete")
        required_nsis_snippets = [
            'WriteRegStr SHCTX "Software\\Classes\\fesTerm.Markdown\\shell\\open\\command" "" '
            + EXPECTED_OPEN_COMMAND,
            'WriteRegStr SHCTX "Software\\Classes\\.md\\OpenWithProgids" "fesTerm.Markdown" ""',
            'WriteRegStr SHCTX "Software\\Classes\\.markdown\\OpenWithProgids" "fesTerm.Markdown" ""',
            'DeleteRegValue SHCTX "Software\\Classes\\.md\\OpenWithProgids" "fesTerm.Markdown"',
            'DeleteRegValue SHCTX "Software\\Classes\\.markdown\\OpenWithProgids" "fesTerm.Markdown"',
        ]
        for snippet in required_nsis_snippets:
            if snippet not in nsis_template:
                errors.append(f"Windows NSIS template is missing: {snippet}")
        forbidden_snippets = [
            "!insertmacro APP_ASSOCIATE",
            "!insertmacro APP_UNASSOCIATE",
            '"$INSTDIR\\${MAINBINARYNAME}.exe $"%1$"',
            'WriteRegStr SHCTX "Software\\Classes\\.md" ""',
            'WriteRegStr SHCTX "Software\\Classes\\.markdown" ""',
        ]
        for snippet in forbidden_snippets:
            if snippet in nsis_template:
                errors.append(f"Windows NSIS template must not contain: {snippet}")
        rendered_nsis = nsis_template.replace("${MAINBINARYNAME}", "festerm")
        if '"$\\"$INSTDIR\\festerm.exe$\\" --open -- $\\"%1$\\""' not in rendered_nsis:
            errors.append("rendered Windows NSIS command must preserve executable/path quoting")
        verify_nsis_template_against_upstream_if_available(nsis_template, errors)
    resources = windows.get("resources", [])
    expected_sessiond = {
        "src": f"../target/release/festerm-sessiond-{version}.exe",
        "target": f"festerm-sessiond-{version}.exe",
    }
    if expected_sessiond not in resources:
        errors.append(
            "Windows packaging must own an immutable release-versioned session daemon"
        )
    if any(
        resource.get("target") == "festerm-sessiond.exe"
        for resource in resources
        if isinstance(resource, dict)
    ):
        errors.append(
            "Windows packaging must not own the legacy stable session daemon resource"
        )
    expected_runtime = {
        "src": "../target/release/runtime/conpty",
        "target": "runtime/conpty",
    }
    if expected_runtime not in resources:
        errors.append("Windows packaging does not own the required ConPTY sidecar")

    linux = load_toml(CONFIGS["linux"])
    if linux.get("file-associations") != [EXPECTED_MARKDOWN_ASSOCIATION]:
        errors.append("Linux packaging must advertise only text/markdown for .md/.markdown")
    if linux.get("deb", {}).get("desktop-template") != "../packaging/linux-desktop-entry.desktop":
        errors.append("Linux packaging must use the reviewed desktop entry template")
    try:
        linux_desktop = (ROOT / "packaging/linux-desktop-entry.desktop").read_text(
            encoding="utf-8"
        )
    except OSError as error:
        errors.append(f"cannot read Linux desktop entry template: {error}")
    else:
        if "Exec={{exec}} --open -- %F" not in linux_desktop:
            errors.append("Linux desktop entry must call festerm --open -- %F")
        if "MimeType={{mime_type}}" not in linux_desktop:
            errors.append("Linux desktop entry must include cargo-packager MIME types")
        substituted_desktop = substituted_linux_desktop_template(linux_desktop, '"fes Term"')
        if 'Exec="fes Term" --open -- %F' not in substituted_desktop:
            errors.append("Linux desktop template must preserve quoted executable paths")
        if "MimeType=text/markdown" not in substituted_desktop:
            errors.append("Linux desktop template must advertise text/markdown")

    package_smoke = (ROOT / ".github/workflows/package-smoke.yml").read_text(
        encoding="utf-8"
    )
    release_workflow = (ROOT / ".github/workflows/release.yml").read_text(
        encoding="utf-8"
    )
    staging_command = "./scripts/stage-windows-sessiond.ps1 -Configuration Release"
    if staging_command not in package_smoke:
        errors.append("Windows package smoke does not stage the immutable session daemon")
    if staging_command not in release_workflow:
        errors.append("Windows release packaging does not stage the immutable session daemon")

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
