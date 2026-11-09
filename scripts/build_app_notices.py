#!/usr/bin/env python3
"""Generate or check the third-party notices shipped with the applications."""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FALLBACKS = ROOT / "packaging/license-fallbacks.toml"
FALLBACK_DIR = ROOT / "packaging/license-fallbacks"
REPORT = ROOT / "packaging/THIRD_PARTY_NOTICES.txt"
INDEX = ROOT / "packaging/third-party-notices.json"
TARGETS = {
    "aarch64-apple-darwin": ("desktop",),
    "x86_64-unknown-linux-gnu": ("desktop",),
    "aarch64-unknown-linux-gnu": ("desktop",),
    "x86_64-pc-windows-msvc": ("desktop",),
    "aarch64-pc-windows-msvc": ("desktop",),
    "aarch64-apple-ios": ("ios",),
    "aarch64-linux-android": ("android",),
    "x86_64-linux-android": ("android",),
}
DESKTOP_PACKAGES = ("festerm", "festerm-sessiond", "festerm-tunnel-bridge", "festerm-automation-js")
NOTICE_NAME = re.compile(
    r"(?i)(?:(?:license|licence|copying|notice|unlicense|copyright|ofl|ufl)"
    r"(?:[._-].*)?|.+[-_](?:license|licence)(?:[._-].*)?)"
)
PRIMARY_NAME = re.compile(r"(?i)(?:license|licence|copying|unlicense)(?:[._-].*)?")
TREE_ENTRY = re.compile(r"^([A-Za-z0-9_-]+) v(\S+)(?:\s|$)")
FONT_NOTICES = (
    "assets/fonts/jetbrains-mono/OFL.txt",
    "assets/fonts/jetbrains-mono/AUTHORS.txt",
    "assets/fonts/iosevka-term/LICENSE.md",
    "assets/fonts/julia-mono/LICENSE",
    "assets/fonts/maple-mono/LICENSE.txt",
    "assets/fonts/noto-emoji/OFL.txt",
)
OTHER_INPUTS = (
    "app/festerm-android/dependencies.json",
    "third_party/conpty/manifest.json",
)
DEFAULT_FONT_NOTICES = (
    "fonts/Hack-Regular.txt",
    "fonts/emoji-icon-font-mit-license.txt",
    "fonts/OFL.txt",
    "fonts/UFL.txt",
)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git_blob(data):
    return hashlib.sha1(b"blob " + str(len(data)).encode("ascii") + b"\0" + data).hexdigest()


def cargo(*args):
    return subprocess.run(
        ["cargo", *args], cwd=ROOT, check=True, capture_output=True, text=True, timeout=600,
    ).stdout


def tree_packages(text):
    packages = set()
    for line in text.splitlines():
        if not line.strip():
            continue
        match = TREE_ENTRY.match(line)
        if not match:
            raise ValueError(f"unrecognized cargo tree entry: {line}")
        packages.add((match.group(1), match.group(2)))
    if not packages:
        raise ValueError("empty cargo dependency tree")
    return packages


def tree_for(target, kind):
    if kind == "desktop":
        selection = [arg for package in DESKTOP_PACKAGES for arg in ("-p", package)]
    elif kind == "ios":
        selection = ["-p", "festerm-mobile", "--features", "live-ssh"]
    elif kind == "android":
        selection = ["-p", "festerm-android"]
    else:
        raise ValueError(f"unsupported release target kind: {kind}")
    return tree_packages(cargo(
        "tree", "--locked", *selection, "--target", target,
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}",
    ))


def release_trees():
    return {target: tree_for(target, kinds[0]) for target, kinds in TARGETS.items()}


def workspace_packages():
    metadata = json.loads(cargo("metadata", "--locked", "--format-version", "1", "--no-deps"))
    members = set(metadata["workspace_members"])
    return {
        (package["name"], package["version"])
        for package in metadata["packages"] if package["id"] in members
    }


def locked_packages(root=ROOT):
    result = {}
    for package in tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))["package"]:
        key = (package["name"], package["version"])
        if key in result:
            raise ValueError(f"ambiguous name/version in Cargo.lock: {key}")
        result[key] = {
            "source": package.get("source", "path"),
            "checksum": package.get("checksum"),
        }
    return result


def load_fallbacks(root=ROOT):
    config = tomllib.loads((root / "packaging/license-fallbacks.toml").read_text(encoding="utf-8"))
    sources = {}
    for source in config["source"]:
        name = source["name"]
        if not re.fullmatch(r"[a-z0-9-]+", name) or name in sources:
            raise ValueError(f"invalid or duplicate fallback source: {name}")
        if ("url" in source) == ("path" in source):
            raise ValueError(f"fallback source must have exactly one location: {name}")
        if "url" in source and not re.fullmatch(
            r"https://raw\.githubusercontent\.com/[^/]+/[^/]+/[0-9a-f]{40}/.+", source["url"]
        ):
            raise ValueError(f"fallback URL must use a pinned GitHub revision: {name}")
        sources[name] = source
    overrides = {}
    for override in config["override"]:
        if not override["sources"] or not set(override["sources"]) <= set(sources):
            raise ValueError("fallback references an unknown or empty source")
        for package in override["packages"]:
            if package in overrides:
                raise ValueError(f"duplicate fallback for {package}")
            overrides[package] = override
    return sources, overrides


def source_file(source, root=ROOT):
    if "url" in source:
        path = root / "packaging/license-fallbacks" / f"{source['name']}.txt"
    else:
        path = root / source["path"]
    if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
        raise ValueError(f"unsafe fallback path: {path}")
    return path


def read_source(source, root=ROOT):
    path = source_file(source, root)
    data = path.read_bytes()
    if not data.strip():
        raise ValueError(f"empty fallback source: {path}")
    if source.get("git-blob") and git_blob(data) != source["git-blob"]:
        raise ValueError(f"upstream fallback blob mismatch: {path}")
    data.decode("utf-8")
    return data


def refresh_fallbacks(sources):
    for source in sources.values():
        if "url" not in source:
            continue
        result = subprocess.run(
            ["curl", "--fail", "--location", "--silent", "--show-error", "--proto", "=https",
             "--proto-redir", "=https", "--max-time", "90", "--max-filesize", "1048576",
             source["url"]],
            check=True, capture_output=True, timeout=100,
        )
        data = result.stdout
        if len(data) > 1024 * 1024 or not data.strip() or git_blob(data) != source.get("git-blob"):
            raise ValueError(f"downloaded fallback blob mismatch: {source['name']}")
        data.decode("utf-8")
        destination = source_file(source)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        print(f"Verified {source['name']}")


def package_documents(package, sources, overrides, root=ROOT):
    directory = Path(package["manifest_path"]).resolve().parent
    found = set()
    for path in directory.rglob("*"):
        if path.is_file() and NOTICE_NAME.fullmatch(path.name):
            found.add(path)
    if package.get("license_file"):
        found.add(directory / package["license_file"])
    if (package["name"], package["version"]) == ("epaint_default_fonts", "0.36.1"):
        found.update(directory / name for name in DEFAULT_FONT_NOTICES)
    documents = []
    primary = False
    for path in sorted(found):
        if path.is_symlink() or not path.resolve().is_relative_to(directory):
            raise ValueError(f"unsafe dependency license path: {path}")
        data = path.read_bytes()
        if not data.strip():
            raise ValueError(f"empty dependency license/notice: {path}")
        data.decode("utf-8")
        relative = path.relative_to(directory).as_posix()
        documents.append((relative, data))
        primary |= (path.parent == directory and bool(PRIMARY_NAME.fullmatch(path.name)))
        primary |= relative == package.get("license_file")
    key = f"{package['name']}@{package['version']}"
    if not primary:
        override = overrides.get(key)
        if not override or package.get("license") not in override["licenses"]:
            raise ValueError(f"missing reviewed primary license for {key}")
        for name in override["sources"]:
            source = sources[name]
            location = source.get("url", source.get("path"))
            documents.append((f"reviewed fallback: {location}", read_source(source, root)))
    return documents


def metadata_for_trees(trees, workspace):
    packages = {}
    for target, selected in trees.items():
        metadata = json.loads(cargo(
            "metadata", "--locked", "--format-version", "1", "--filter-platform", target,
        ))
        found = {}
        for package in metadata["packages"]:
            key = (package["name"], package["version"])
            if key not in selected or key in workspace:
                continue
            if key in found:
                raise ValueError(f"ambiguous dependency metadata: {key}")
            found[key] = package
        missing = selected - workspace - set(found)
        if missing:
            raise ValueError(f"dependency metadata missing for {target}: {sorted(missing)}")
        for key, package in found.items():
            existing = packages.get(key)
            if existing and existing["id"] != package["id"]:
                raise ValueError(f"dependency source differs by target: {key}")
            packages[key] = package
    return packages


def add_document(texts, data):
    text = data.decode("utf-8").replace("\r\n", "\n").replace("\r", "\n")
    data = ("\n".join(line.rstrip(" \t") for line in text.split("\n")).rstrip("\n") + "\n").encode("utf-8")
    key = digest(data)
    old = texts.setdefault(key, data)
    if old != data:
        raise ValueError("license text digest collision")
    return key


def report_for(packages, sources, overrides, root=ROOT):
    texts = {}
    inputs = {}
    sections = [
        "fesTerm third-party licenses and notices\n",
        "This collection covers the resolved desktop and mobile release dependency graphs, "
        "including build dependencies and components for other supported platforms. "
        "Some entries may not be present in a particular package. License alternatives "
        "are identified by each component's declaration. Retained texts have normalized "
        "line endings and trailing whitespace. This is not a legal audit.\n",
    ]
    for key, package in sorted(packages.items()):
        documents = package_documents(package, sources, overrides, root)
        sections.append(f"\n=== {key[0]} {key[1]} ===\n")
        sections.append(f"Declared license: {package.get('license') or 'see retained license file'}\n")
        if package.get("authors"):
            sections.append("Declared authors: " + "; ".join(package["authors"]) + "\n")
        if package.get("repository"):
            sections.append(f"Source repository: {package['repository']}\n")
        if key == ("epaint_default_fonts", "0.36.1"):
            sections.append(
                "NotoEmoji-Regular.ttf: Copyright 2013 Google Inc. All Rights Reserved. "
                "Noto is a trademark of Google Inc.\n"
                "Ubuntu-Light.ttf: Copyright 2011 Canonical Ltd. Licensed under the Ubuntu "
                "Font Licence 1.0. Ubuntu and Canonical are registered trademarks of Canonical Ltd.\n"
            )
        for label, data in documents:
            sections.append(f"  {label}: SHA-256 {add_document(texts, data)}\n")
        directory = Path(package["manifest_path"]).resolve().parent
        if directory.is_relative_to(root.resolve() / "vendor"):
            manifest = directory / "Cargo.toml"
            inputs[manifest.relative_to(root.resolve()).as_posix()] = digest(manifest.read_bytes())
            for label, data in documents:
                path = directory / label
                if path.is_file() and path.resolve().is_relative_to(root.resolve()):
                    inputs[path.relative_to(root.resolve()).as_posix()] = digest(data)

    sections.append("\n=== Bundled font notices ===\n")
    for name in FONT_NOTICES:
        data = (root / name).read_bytes()
        if not data.strip():
            raise ValueError(f"empty font notice: {name}")
        sections.append(f"  {name}: SHA-256 {add_document(texts, data)}\n")
        inputs[name] = digest(data)
    for path in sorted((root / "assets/fonts").glob("*/manifest.json")):
        inputs[path.relative_to(root).as_posix()] = digest(path.read_bytes())

    android = json.loads((root / OTHER_INPUTS[0]).read_text(encoding="utf-8"))
    conpty = json.loads((root / OTHER_INPUTS[1]).read_text(encoding="utf-8"))
    android_artifacts = {item["name"]: item for item in android["artifacts"]}
    expected_android = {"rustls-platform-verifier-0.2.0.aar", "kotlin-stdlib-2.0.21.jar"}
    if set(android_artifacts) != expected_android or conpty["license"] != "MIT":
        raise ValueError("review changed Android or ConPTY components before updating notices")
    mit = read_source(sources["declared-mit-terms"], root)
    apache = (root / "vendor/egui-winit/LICENSE-APACHE").read_bytes()
    sections.append(
        f"\n=== {conpty['package']} {conpty['version']} (Windows) ===\n"
        f"Source: {conpty['source_url']}\n"
        f"Package SHA-512: {conpty['sha512']}\n"
        "Copyright \u00a9 Microsoft Corporation; declared license: MIT.\n"
        f"  MIT terms: SHA-256 {add_document(texts, mit)}\n"
    )
    sections.append("\n=== Android JVM components ===\n")
    for name, attribution in (
        ("rustls-platform-verifier-0.2.0.aar", "Copyright rustls-platform-verifier contributors"),
        ("kotlin-stdlib-2.0.21.jar", "Copyright JetBrains s.r.o. and Kotlin Programming Language contributors"),
    ):
        item = android_artifacts[name]
        if item["license"] not in ("Apache-2.0", "MIT OR Apache-2.0"):
            raise ValueError(f"review changed Android component license: {name}")
        sections.append(
            f"{name}: {attribution}; declared license: {item['license']}\n"
            f"  Source: {item['url']}; SHA-256 {item['sha256']}\n"
            f"  Apache-2.0 terms: SHA-256 {add_document(texts, apache)}\n"
        )
    for name in OTHER_INPUTS:
        inputs[name] = digest((root / name).read_bytes())

    sections.append("\n=== Retained license and notice texts ===\n")
    for key, data in sorted(texts.items()):
        sections.append(f"\n--- SHA-256 {key} ---\n")
        sections.append(data.decode("utf-8"))
        if not data.endswith(b"\n"):
            sections.append("\n")
    return "".join(sections).encode("utf-8"), inputs


def generate():
    sources, overrides = load_fallbacks()
    trees = release_trees()
    workspace = workspace_packages()
    packages = metadata_for_trees(trees, workspace)
    locked = locked_packages()
    report, inputs = report_for(packages, sources, overrides)
    inputs["packaging/license-fallbacks.toml"] = digest(FALLBACKS.read_bytes())
    for source in sources.values():
        path = source_file(source)
        read_source(source)
        inputs[path.relative_to(ROOT).as_posix()] = digest(path.read_bytes())
    vendor_roots = sorted({
        Path(package["manifest_path"]).resolve().parent.relative_to(ROOT).as_posix()
        for package in packages.values()
        if Path(package["manifest_path"]).resolve().is_relative_to(ROOT / "vendor")
    })
    vendor_notices = {
        directory: sorted(
            path.relative_to(ROOT).as_posix()
            for path in (ROOT / directory).rglob("*")
            if path.is_file() and NOTICE_NAME.fullmatch(path.name)
        )
        for directory in vendor_roots
    }
    index = {
        "schema": 1,
        "report_sha256": digest(report),
        "packages": {f"{name}@{version}": locked[(name, version)] for name, version in sorted(packages)},
        "inputs": dict(sorted(inputs.items())),
        "vendor_notices": vendor_notices,
    }
    REPORT.write_bytes(report)
    INDEX.write_text(json.dumps(index, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"Generated notices for {len(packages)} dependencies")


def check_index(index, report, trees, workspace, locked, root=ROOT):
    if index.get("schema") != 1 or index.get("report_sha256") != digest(report):
        raise ValueError("third-party notice report is missing, corrupt, or does not match its index")
    font_manifests = {
        path.relative_to(root).as_posix() for path in (root / "assets/fonts").glob("*/manifest.json")
    }
    indexed_fonts = {
        name for name in index["inputs"] if re.fullmatch(r"assets/fonts/[^/]+/manifest\.json", name)
    }
    if font_manifests != indexed_fonts:
        raise ValueError("bundled font catalog changed; regenerate and review notices")
    for directory, expected in index.get("vendor_notices", {}).items():
        path = root / directory
        if path.is_symlink() or not path.resolve().is_relative_to(root.resolve() / "vendor"):
            raise ValueError(f"unsafe vendor notice directory: {directory}")
        found = sorted(
            file.relative_to(root).as_posix()
            for file in path.rglob("*")
            if file.is_file() and NOTICE_NAME.fullmatch(file.name)
        )
        if found != expected:
            raise ValueError(f"vendored notice files changed; regenerate and review: {directory}")
    for name, expected in index["inputs"].items():
        path = root / name
        if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
            raise ValueError(f"unsafe notice input path: {name}")
        if digest(path.read_bytes()) != expected:
            raise ValueError(f"notice input changed; regenerate and review: {name}")
    selected = set().union(*trees.values()) - workspace
    for name, version in sorted(selected):
        key = f"{name}@{version}"
        if key not in index["packages"] or locked.get((name, version)) != index["packages"][key]:
            raise ValueError(f"dependency notices are missing or stale for {key}")


def check():
    index = json.loads(INDEX.read_text(encoding="utf-8"))
    check_index(index, REPORT.read_bytes(), release_trees(), workspace_packages(), locked_packages())
    print("Third-party notice report and release dependency coverage verified")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--refresh-fallbacks", action="store_true", help="download pinned upstream license texts")
    action.add_argument("--generate", action="store_true", help="review inputs and regenerate the notice report")
    action.add_argument("--check", action="store_true", help="check the report and every release dependency graph")
    args = parser.parse_args()
    if args.refresh_fallbacks:
        sources, _ = load_fallbacks()
        refresh_fallbacks(sources)
    elif args.generate:
        generate()
    else:
        check()


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, UnicodeError, subprocess.SubprocessError) as error:
        print(f"Third-party notice generation failed: {error}", file=sys.stderr)
        sys.exit(1)
