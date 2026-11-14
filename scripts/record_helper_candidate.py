#!/usr/bin/env python3
"""Bind run-scoped signed helper artifacts to the exact manually reviewed source."""
import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
from pathlib import Path


def record(directory, environment, checkout_commit):
    if environment.get("GITHUB_EVENT_NAME") != "workflow_dispatch":
        raise ValueError("candidate provenance requires manual dispatch")
    source = environment.get("SOURCE_COMMIT", "")
    if not re.fullmatch(r"[0-9a-f]{40}", source) or any(
        value != source for value in (
            environment.get("GITHUB_SHA"), environment.get("WORKFLOW_COMMIT"), checkout_commit,
        )
    ):
        raise ValueError("candidate source, workflow and checkout must be identical")
    version = environment.get("VERSION", "")
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ValueError("candidate version must match stable package identity")
    repository = environment.get("GITHUB_REPOSITORY", "")
    workflow_ref = environment.get("WORKFLOW_REF", "")
    if repository != "fes/fesTerm" or not workflow_ref.startswith(
        repository + "/.github/workflows/release.yml@"
    ):
        raise ValueError("candidate must use this repository's release workflow")
    for name in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"):
        if not re.fullmatch(r"[1-9][0-9]*", environment.get(name, "")):
            raise ValueError("candidate requires exact Actions run and attempt")
    expected = set()
    for platform in ("linux", "macos", "windows"):
        extension = "zip" if platform == "windows" else "tar.gz"
        for architecture in ("x86_64", "aarch64"):
            for family in ("tunnel", "host"):
                archive = f"fesTerm-{family}-{version}-{platform}-{architecture}.{extension}"
                expected.update(archive + suffix for suffix in (
                    "", ".sha256", ".manifest.json", ".manifest.json.sig",
                ))
    if {path.name for path in directory.iterdir()} != expected:
        raise ValueError("candidate requires exactly both authenticated helper families on six targets")
    artifacts = []
    for name in sorted(expected):
        path = directory / name
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or not 0 < metadata.st_size <= 256 * 1024 * 1024:
            raise ValueError("candidate artifact must be a bounded nonempty regular file")
        digest = hashlib.sha256()
        with path.open("rb") as artifact:
            for chunk in iter(lambda: artifact.read(1024 * 1024), b""):
                digest.update(chunk)
        artifacts.append({"filename": name, "length": metadata.st_size, "sha256": digest.hexdigest()})
    return {
        "schema": 1,
        "purpose": "signed-helper-candidate-no-publication",
        "repository": repository,
        "source_commit": source,
        "workflow_commit": source,
        "workflow_ref": workflow_ref,
        "run_id": environment["GITHUB_RUN_ID"],
        "run_attempt": environment["GITHUB_RUN_ATTEMPT"],
        "package_version": version,
        "artifacts": artifacts,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    value = record(args.directory, os.environ, commit)
    # Exclusive output retains every previous run receipt rather than overwriting it.
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")


if __name__ == "__main__":
    main()
