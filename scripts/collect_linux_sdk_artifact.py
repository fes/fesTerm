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
    if (not binary.is_absolute() or binary.parent != root / "target/release/deps"
            or not re.fullmatch(TEST + r"-[0-9a-f]+", binary.name)
            or any(path.is_symlink() for path in (binary, *binary.parents))):
        raise ValueError("SDK executable must be an unaliased release test artifact")
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
    args = parser.parse_args()
    collect(ROOT, args.messages, args.output, os.environ)


if __name__ == "__main__":
    main()
