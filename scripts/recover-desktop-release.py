#!/usr/bin/env python3
"""Validate a desktop release retry without moving its existing tag."""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ALLOWED_CHANGES = {
    ".github/workflows/ci.yml",
    ".github/workflows/release.yml",
    "scripts/build_tunnel_bundle.py",
    "scripts/recover-desktop-release.py",
    "scripts/tests/test_recover_desktop_release.py",
    "scripts/tests/test_tunnel_bundle.py",
}


def git(root, *args):
    return subprocess.run(
        ["git", *args], cwd=root, check=True, capture_output=True, text=True,
    ).stdout.rstrip("\n")


def validate(version, head, ref, main_head, runs, root=ROOT):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("version must be stable semver")
    if not re.fullmatch(r"[0-9a-f]{40}", head):
        raise ValueError("invalid workflow commit")
    if ref != "refs/heads/main" or main_head != head or git(root, "rev-parse", "HEAD") != head:
        raise ValueError("release recovery must run on the current main commit")
    if git(root, "status", "--porcelain"):
        raise ValueError("release recovery requires a clean checkout")
    source = git(root, "rev-parse", "--verify", f"refs/tags/v{version}^{{commit}}")
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", source, head], cwd=root, check=False,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if ancestor.returncode != 0:
        raise ValueError("release tag is not an ancestor of current main")
    changed = set(filter(None, git(
        root, "diff", "--name-only", "--no-renames", "-z", source, head,
    ).split("\0")))
    unexpected = changed - ALLOWED_CHANGES
    if unexpected:
        raise ValueError(f"release source has non-tooling changes: {', '.join(sorted(unexpected))}")
    for commit in {source, head}:
        latest = next((run for run in runs if run.get("headSha") == commit), None)
        if not latest or latest.get("status") != "completed" or latest.get("conclusion") != "success":
            raise ValueError(f"main CI is not green for {commit}")
    return source


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--repo", required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        raise ValueError("invalid repository")
    main_head = gh("api", f"repos/{args.repo}/branches/main", "--jq", ".commit.sha")
    runs = json.loads(gh(
        "run", "list", "--repo", args.repo, "--workflow", "ci.yml", "--branch", "main",
        "--event", "push", "--limit", "100", "--json", "headSha,status,conclusion",
    ))
    print(validate(args.version, args.head, args.ref, main_head, runs))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f"Desktop release recovery rejected: {error}")
