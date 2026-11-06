#!/usr/bin/env python3
"""Validate repository-owned native packaging metadata."""

from __future__ import annotations

import argparse
import difflib
import plistlib
import re
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
    {"path": "festerm-tunnel-setup", "main": False},
    {"path": "festerm-tunnel-bridge", "main": False},
]
TUNNEL_HELPERS = ("festerm-tunnel-setup", "festerm-tunnel-bridge")
WINDOWS_TUNNEL_HELPERS = (*TUNNEL_HELPERS, "festerm-tunnel-worker")
TUNNEL_BUILD_COMMAND = (
    "cargo build --locked --release -p festerm-tunnel-bridge "
    "--no-default-features --features relay --bins"
)
EXPECTED_LINUX_BINARIES = [
    *EXPECTED_BINARIES,
    {"path": "festerm-automation-js-worker", "main": False},
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


def workflow_step(workflow: str, name: str) -> str:
    marker = f"      - name: {name}\n"
    if marker not in workflow:
        return ""
    return re.split(r"\n      - ", workflow.split(marker, 1)[1], maxsplit=1)[0]


def verify_desktop_helper_workflows(
    package_smoke: str, release: str, errors: list[str]
) -> None:
    for workflow, label, names in (
        (
            package_smoke,
            "package smoke",
            (
                "Build unsigned macOS package",
                "Build unsigned Windows package with verified ConPTY sidecar",
                "Build unsigned Linux AppImage",
                "Rebuild package-managed Linux binary and Debian package",
            ),
        ),
        (
            release,
            "release",
            (
                "Build, sign, and notarize",
                "Build and stage verified ARM64 ConPTY runtime",
                "Build and stage verified x86_64 ConPTY runtime",
                "Build AppImage updater artifact",
                "Build package-managed Debian artifact",
            ),
        ),
    ):
        for name in names:
            if TUNNEL_BUILD_COMMAND not in workflow_step(workflow, name):
                errors.append(f"{label} step {name!r} must build relay-enabled tunnel helpers")

    signing = workflow_step(release, "Sign application binary")
    architecture = workflow_step(
        release, "Verify package input architecture and ConPTY signatures"
    )
    publisher = workflow_step(release, "Stage immutable Windows session daemon")
    arm_input = release.split("\n  windows:\n", 1)[0].split(
        "\n  windows-arm-build:\n", 1
    )[-1]
    for helper in WINDOWS_TUNNEL_HELPERS:
        if f"\\target\\release\\{helper}.exe" not in signing:
            errors.append(f"Windows release must Authenticode-sign {helper}")
        if f"'target/release/{helper}.exe'" not in architecture:
            errors.append(f"Windows release must check the architecture of {helper}")
        if f"            target/release/{helper}.exe\n" not in arm_input:
            errors.append(f"Windows ARM64 package input must include {helper}")
        if f"'{helper}'" not in publisher:
            errors.append(f"Windows release must verify the publisher of {helper}")
    if (
        "$signature.Status -ne 'Valid'" not in publisher
        or "$signature.SignerCertificate.Subject -ne "
        "$applicationSignature.SignerCertificate.Subject" not in publisher
    ):
        errors.append("Windows release must require valid helpers with the application publisher")

    notarize = workflow_step(release, "Build, sign, and notarize")
    for helper in TUNNEL_HELPERS:
        if helper not in notarize:
            errors.append(f"macOS release must verify the bundled signature of {helper}")
    for snippet in (
        "codesign --verify --strict -R='anchor apple generic' --verbose=2 \"$binary\"",
        '"target/release/fesTerm.app/Contents/MacOS/$helper"',
        'test "$(printf \'%s\\n\' "$helper_signature" | sed -n \'s/^TeamIdentifier=//p\')" = "$app_team"',
        "grep -q 'flags=.*runtime'",
        "xcrun stapler validate target/release/fesTerm.app",
    ):
        if snippet not in notarize:
            errors.append("macOS release must verify signed, same-team, hardened helpers in the notarized app")
            break
    cli_smoke = workflow_step(
        package_smoke, "Smoke reviewed macOS CLI signature prerequisites"
    )
    for snippet in (
        "if: matrix.platform == 'macos'",
        "timeout-minutes: 6",
        "crates/festerm-tunnel-bridge/devtunnel-cli-pins.json",
        '["aarch64", "x86_64"]',
        'hashlib.sha256(archive.read_bytes()).hexdigest() != pin["archive_sha256"]',
        'hashlib.sha256(executable_bytes).hexdigest() != pin["executable_sha256"]',
        "len(entries) != 1",
        'entry.filename != "devtunnel"',
        "stat.S_ISLNK(entry.external_attr >> 16)",
        'tempfile.TemporaryDirectory(prefix=".macos-cli-smoke-", dir=Path.cwd())',
        '"--connect-timeout", "15", "--max-time", "90"',
        '["/usr/bin/codesign", "-dv", str(executable)]',
        '"/usr/bin/codesign", "--verify", "--strict",\n'
        '                          "-R=anchor apple generic", str(executable)',
    ):
        if snippet not in cli_smoke:
            errors.append(
                "macOS package smoke must verify both pinned CLI archives and "
                f"their runtime signature prerequisite: missing {snippet}"
            )

    macos_smoke = workflow_step(package_smoke, "Build unsigned macOS package")
    appimage_smoke = workflow_step(package_smoke, "Build unsigned Linux AppImage")
    deb_smoke = workflow_step(
        package_smoke, "Rebuild package-managed Linux binary and Debian package"
    )
    windows_smoke = workflow_step(
        package_smoke, "Build unsigned Windows package with verified ConPTY sidecar"
    )
    for helper in TUNNEL_HELPERS:
        for script, snippet in (
            (macos_smoke, f"test -x target/release/fesTerm.app/Contents/MacOS/{helper}"),
            (appimage_smoke, f"test -x squashfs-root/usr/bin/{helper}"),
            (deb_smoke, f"grep -q 'usr/bin/{helper}$'"),
            (deb_smoke, f"test -x /usr/bin/{helper}"),
            (deb_smoke, f"test ! -e /usr/bin/{helper}"),
        ):
            if snippet not in script:
                errors.append(f"package smoke must check tunnel helper payload: {snippet}")
    for helper in WINDOWS_TUNNEL_HELPERS:
        if f"'{helper}.exe'" not in windows_smoke:
            errors.append(f"Windows package smoke must check installed {helper}")
    for snippet in (
        "foreach ($helper in $installedTunnelHelpers)",
        "Test-Path $helper -PathType Leaf",
        "NSIS uninstall left tunnel helper installed",
    ):
        if snippet not in windows_smoke:
            errors.append("Windows package smoke must check tunnel helper install/uninstall")


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
        if platform == "windows":
            expected_binaries = [{"path": "festerm", "main": True}]
        elif platform == "linux":
            expected_binaries = EXPECTED_LINUX_BINARIES
        else:
            expected_binaries = EXPECTED_BINARIES
        if binaries != expected_binaries:
            errors.append(
                f"{path.relative_to(ROOT)} has unexpected packaged binaries"
            )
        if platform != "windows" and config.get("resources"):
            errors.append(f"{path.relative_to(ROOT)} has unexpected packaged resources")
        if config.get("external-binaries"):
            errors.append(f"{path.relative_to(ROOT)} has unexpected external binaries")

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
    expected_resources = [
        expected_sessiond,
        *(
            {"src": f"../target/release/{helper}.exe", "target": f"{helper}.exe"}
            for helper in WINDOWS_TUNNEL_HELPERS
        ),
        expected_runtime,
    ]
    if resources != expected_resources:
        errors.append(
            "Windows packaging must contain exactly the versioned daemon, sibling "
            "tunnel helpers, and ConPTY resources (not the downloaded CLI)"
        )

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
    verify_desktop_helper_workflows(package_smoke, release_workflow, errors)

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
