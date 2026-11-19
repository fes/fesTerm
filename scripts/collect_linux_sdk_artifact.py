#!/usr/bin/env python3
"""Retain only the compiled Linux ARM64 controlled SDK test; never execute it."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

ROOT = Path(__file__).resolve().parents[1]
TEST = "dev_tunnel_workflow_lab"


class BuildPathError(ValueError):
    def __init__(self, reason, paths):
        super().__init__(reason)
        self.paths = {"reason": reason, **paths}


def validate_build_path(root, binary):
    expected = root / "target/release/deps"
    def canonical(path):
        try:
            return str(path.resolve())[:2048]
        except (OSError, RuntimeError):
            return None
    aliases = [str(path)[:2048] for path in (binary, *binary.parents) if path.is_symlink()]
    paths = {
        "checkout_root": str(root)[:2048],
        "expected_parent": str(expected)[:2048],
        "canonical_expected_parent": canonical(expected),
        "executable": str(binary)[:2048],
        "canonical_executable": canonical(binary),
        "alias_paths": aliases[:16],
    }
    if not binary.is_absolute():
        raise BuildPathError("sdk.executable_relative", paths)
    if binary.parent == expected:
        layout = "release-deps"
    else:
        try:
            parent = binary.parent.relative_to(root / "target/release").as_posix()
        except ValueError:
            parent = ""
        package = re.fullmatch(r"build/festerm/([0-9a-f]{16})/out", parent)
        if package is None:
            raise BuildPathError("sdk.release_parent_mismatch", paths)
        if binary.name != f"{TEST}-{package[1]}":
            raise BuildPathError("sdk.package_hash_mismatch", paths)
        layout = "release-package-build"
        paths["expected_parent"] = str(binary.parent)[:2048]
        paths["canonical_expected_parent"] = canonical(binary.parent)
    if not re.fullmatch(TEST + r"-[0-9a-f]+", binary.name):
        raise BuildPathError("sdk.test_basename_mismatch", paths)
    if aliases:
        raise BuildPathError("sdk.path_alias", paths)
    paths["build_layout"] = layout
    return paths


def collect(root, messages, output, environment):
    root = root.resolve()
    selected = []
    for line in messages.read_text(encoding="utf-8").splitlines():
        message = json.loads(line)
        target = message.get("target", {})
        if (message.get("reason") == "compiler-artifact"
                and target.get("name") == TEST and target.get("kind") == ["test"]
                and target.get("src_path") == str(root / "app/festerm/tests" / f"{TEST}.rs")
                and message.get("profile", {}).get("test") is True
                and message.get("executable")):
            selected.append(message)
    if len(selected) != 1:
        raise ValueError("SDK build requires exactly one test executable")
    artifact = selected[0]
    binary = Path(artifact["executable"])
    build_paths = validate_build_path(root, binary)
    info = binary.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or not info.st_mode & 0o111:
        raise ValueError("SDK executable must be a regular executable without hard links")
    data = binary.read_bytes()
    if len(data) < 20 or data[:6] != b"\x7fELF\x02\x01" or data[18:20] != b"\xb7\x00":
        raise ValueError("SDK executable must be little-endian AArch64 ELF64")
    features = artifact.get("features")
    if (not isinstance(features, list) or not all(isinstance(value, str) for value in features)
            or not {"experimental-dev-tunnels", "local-host-settings"}.issubset(features)):
        raise ValueError("SDK executable lacks the normal HOST client features")
    head = environment["DEVELOPMENT_SOURCE_HEAD"]
    run, attempt = environment["GITHUB_RUN_ID"], environment["GITHUB_RUN_ATTEMPT"]
    if not re.fullmatch(r"[0-9a-f]{40}", head) or not run.isdecimal() or not attempt.isdecimal():
        raise ValueError("SDK provenance requires exact source and run identifiers")
    def git(*args):
        return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
    receipt = {
        "purpose": "unsigned-development-controlled-sdk-test-not-executed",
        "checkout_commit": git("rev-parse", "HEAD"),
        "checkout_tree": git("rev-parse", "HEAD^{tree}"),
        "candidate_head": head, "run_id": run, "run_attempt": attempt,
        "runner": "ubuntu-24.04-arm", "target": "aarch64-unknown-linux-gnu",
        "test_target": TEST, "features": sorted(features),
        "build": "cargo test --locked --release -p festerm --test dev_tunnel_workflow_lab --no-run --message-format=json-render-diagnostics",
        "binary": TEST, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
        "mode": 0o755,
        "build_layout": build_paths["build_layout"],
        "build_paths": build_paths,
    }
    output.mkdir()  # Exclusive: retained output is never repaired or overwritten.
    destination = output / TEST
    with destination.open("xb") as file:
        file.write(data)
    destination.chmod(0o755)
    with (output / "development-sdk-provenance.json").open("x", encoding="utf-8") as file:
        file.write(json.dumps(receipt, indent=2) + "\n")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--messages", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--path-diagnostic", type=Path, required=True)
    args = parser.parse_args()
    try:
        collect(ROOT, args.messages, args.output, os.environ)
    except BuildPathError as error:
        # Cargo build paths only: no environment dump, credentials or SDK output.
        with args.path_diagnostic.open("x", encoding="utf-8") as file:
            file.write(json.dumps(error.paths, indent=2) + "\n")
        raise


if __name__ == "__main__":
    main()
