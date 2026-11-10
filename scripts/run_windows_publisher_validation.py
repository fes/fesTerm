#!/usr/bin/env python3
"""Opt-in public legacy-fixture acquisition and read-only Windows PKI evidence."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
import urllib.request
from urllib.parse import urlsplit
import zipfile


ROOT = Path(__file__).resolve().parents[1]
URL = (
    "https://github.com/fes/fesTerm/releases/download/v0.16.0/"
    "fesTerm-tunnel-0.16.0-windows-x86_64.zip"
)
ZIP_SHA256 = "3edfbf5a0725b70a0bc56448d81bb3745e25d745a2cd670978ecc1f33f51e896"
MEMBER = "festerm-sessiond.exe"
MEMBER_SHA256 = "4cd3e1b12026412717848cef2a872e110c50039bc0a6464ce8bb05a2781a481b"
TEST = (
    "host_publisher::tests::"
    "windows_publisher_native_opt_in_approved_signed_host_and_unsigned_rejection"
)
TEST_COMMAND = (
    "cargo", "test", "--locked", "-p", "festerm-windows-security",
    "--features", "host-service", "--lib", TEST,
    "--", "--ignored", "--exact", "--nocapture", "--test-threads=1",
)
MAX_ZIP = 128 * 1024 * 1024
MAX_MEMBER = 128 * 1024 * 1024
MAX_EXPANDED = 512 * 1024 * 1024
CHUNK = 64 * 1024


class ValidationFailure(Exception):
    def __init__(self, message, code=1):
        super().__init__(message)
        self.code = code


def require_opt_in():
    if sys.platform != "win32":
        raise ValidationFailure("Read-only native publisher validation requires Windows.")
    for name in ("FESTERM_RUN_OPTIONAL_VALIDATION", "FESTERM_RUN_ADR84_PUBLISHER_NATIVE"):
        if os.environ.get(name) != "1":
            raise ValidationFailure(f"Set {name}=1 explicitly after authorization.")


def fresh_directory(relative):
    relative = Path(relative)
    if relative.anchor or not relative.parts or any(
        part in (".", "..") for part in relative.parts
    ):
        raise ValidationFailure("Evidence directory must be a new checkout-relative path.")
    path = ROOT / relative
    for ancestor in (path, *path.parents):
        if ancestor == ROOT:
            break
        if ancestor.is_symlink() or (
            hasattr(ancestor, "is_junction") and ancestor.is_junction()
        ):
            raise ValidationFailure("Evidence path must not traverse a link or junction.")
    if not path.parent.is_dir():
        raise ValidationFailure("Evidence parent must already exist in the checkout.")
    path.mkdir(exist_ok=False)
    return path


def digest(path, maximum=None):
    value = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(CHUNK), b""):
            size += len(chunk)
            if maximum is not None and size > maximum:
                raise ValidationFailure("Fixture hash byte bound exceeded.")
            value.update(chunk)
    return value.hexdigest()


def require_digest(path, expected):
    actual = digest(path, MAX_MEMBER)
    if actual != expected:
        raise ValidationFailure(f"Pinned public fixture integrity mismatch: expected {expected}, actual {actual}.")
    return actual


class PublicReleaseRedirects(urllib.request.HTTPRedirectHandler):
    max_redirections = 3
    max_repeats = 1

    def redirect_request(self, request, fp, code, message, headers, new_url):
        target = urlsplit(new_url)
        if (
            target.scheme != "https"
            or target.netloc != "release-assets.githubusercontent.com"
            or target.fragment
            or any(ord(char) < 33 or ord(char) > 126 for char in new_url)
        ):
            raise ValidationFailure("Public release redirect authority refused.")
        return super().redirect_request(request, fp, code, message, headers, new_url)


def copy_bounded(source, destination, maximum, deadline):
    count = 0
    while True:
        if time.monotonic() >= deadline:
            raise ValidationFailure("Fixture acquisition deadline exceeded.")
        # read1 avoids waiting to fill a large buffer from a trickling peer.
        chunk = source.read1(CHUNK)
        if time.monotonic() >= deadline:
            raise ValidationFailure("Fixture acquisition deadline exceeded.")
        if not chunk:
            return count
        count += len(chunk)
        if count > maximum:
            raise ValidationFailure("Fixture byte bound exceeded.")
        destination.write(chunk)


def download(path):
    deadline = time.monotonic() + 120
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), PublicReleaseRedirects()
    )
    request = urllib.request.Request(URL, headers={"User-Agent": "fesTerm-public-PKI-fixture"})
    with opener.open(request, timeout=15) as response:
        if response.status != 200:
            raise ValidationFailure("Public release did not return HTTP 200.")
        length = response.headers.get("Content-Length")
        if length is not None and not 0 < int(length) <= MAX_ZIP:
            raise ValidationFailure("Public release length exceeds its bound.")
        with path.open("xb") as destination:
            actual = copy_bounded(response, destination, MAX_ZIP, deadline)
        if not actual or length is not None and actual != int(length):
            raise ValidationFailure("Public release body is empty or truncated.")
    require_digest(path, ZIP_SHA256)


def extract_member(archive, destination):
    require_digest(archive, ZIP_SHA256)
    if archive.stat().st_size > MAX_ZIP:
        raise ValidationFailure("Fixture archive byte bound exceeded.")
    with zipfile.ZipFile(archive) as bundle:
        entries = bundle.infolist()
        if not 1 <= len(entries) <= 64:
            raise ValidationFailure("Fixture archive entry bound exceeded.")
        seen = set()
        total = 0
        for entry in entries:
            name = entry.filename
            mode = entry.external_attr >> 16
            if (
                not name or entry.orig_filename != name or len(name) > 160 or "/" in name or "\\" in name
                or ":" in name or name in (".", "..") or name.rstrip(". ") != name
                or any(ord(char) < 32 or ord(char) > 126 for char in name)
                or name.casefold() in seen or entry.flag_bits & 1
                or not stat.S_ISREG(mode) or not 0 < entry.file_size <= MAX_MEMBER
            ):
                raise ValidationFailure("Unsafe, duplicate or unbounded ZIP entry.")
            seen.add(name.casefold())
            total += entry.file_size
        if total > MAX_EXPANDED or MEMBER not in bundle.namelist():
            raise ValidationFailure("Fixture ZIP total bound or exact member refused.")
        with bundle.open(MEMBER) as source, destination.open("xb") as output:
            actual = copy_bounded(source, output, MAX_MEMBER, time.monotonic() + 30)
        if actual != bundle.getinfo(MEMBER).file_size:
            raise ValidationFailure("Fixture ZIP member is truncated.")
    require_digest(destination, MEMBER_SHA256)


def acquire(relative):
    output = fresh_directory(relative)
    archive = output / "public-fixture.zip"
    download(archive)
    extract_member(archive, output / MEMBER)


def command(output, report, name, arguments, timeout, environment=None):
    log = output / f"{name}.log"
    entry = {"argv": list(arguments), "log": log.name, "exit_code": None, "timed_out": False}
    report["commands"].append(entry)
    started = time.monotonic()
    with log.open("xb") as stream:
        try:
            result = subprocess.run(
                arguments, cwd=ROOT, env=environment, stdin=subprocess.DEVNULL,
                stdout=stream, stderr=subprocess.STDOUT, timeout=timeout, check=False,
            )
            entry["exit_code"] = result.returncode
        except subprocess.TimeoutExpired:
            entry["timed_out"] = True
        finally:
            stream.flush()
            entry["elapsed_seconds"] = round(time.monotonic() - started, 3)
            entry["sha256"] = digest(log)
    if entry["timed_out"]:
        raise ValidationFailure(f"{name} timed out; no qualification.", 124)
    if entry["exit_code"] != 0:
        raise ValidationFailure(f"{name} failed; preserve its actual exit/log.", entry["exit_code"])
    return log


def run(relative):
    output = fresh_directory(relative)
    report = {
        "status": "failed", "source_head": None, "commands": [],
        "started_at_utc": datetime.now(timezone.utc).isoformat(),
        "fixture_url": URL, "zip_sha256": ZIP_SHA256, "member": MEMBER,
        "member_sha256": MEMBER_SHA256, "fixture_hashes_are_authentication": False,
        "native_pki_fixture_passed": False, "native_task_runtime_qualification": False,
        "ordinary_user_vm_qualification": False, "whole_bundle_release_qualification": False,
        "daemon_gui_qualification": False,
        "runner": {key: os.environ.get(key) for key in (
            "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA", "RUNNER_OS",
            "RUNNER_ARCH", "ImageOS", "ImageVersion",
        )},
    }
    code = 1
    try:
        head_log = command(output, report, "source-head", ("git", "rev-parse", "HEAD"), 30)
        head = head_log.read_text().strip()
        if not re.fullmatch(r"[a-f0-9]{40}", head):
            raise ValidationFailure("Source HEAD is not an exact commit.")
        report["source_head"] = head
        if os.environ.get("GITHUB_SHA") and os.environ["GITHUB_SHA"] != head:
            raise ValidationFailure("Checked-out source differs from dispatch SHA.")
        status = command(output, report, "source-status", (
            "git", "status", "--porcelain", "--untracked-files=no",
        ), 30)
        if status.read_text().strip():
            raise ValidationFailure("Tracked source is dirty; no exact-source qualification.")
        command(output, report, "rustc-version", ("rustc", "--version", "--verbose"), 30)
        command(output, report, "cargo-version", ("cargo", "--version"), 30)
        fixture_dir = output.relative_to(ROOT) / "fixture"
        # The child gives redirects, headers, body reads and decompression a hard
        # parent-owned deadline, without leaving an unbounded download thread.
        command(output, report, "acquisition", (
            sys.executable, str(Path(__file__).resolve()), "--acquire-only",
            "--output-directory", str(fixture_dir),
        ), 150)
        fixture = ROOT / fixture_dir / MEMBER
        report["verified_member_sha256"] = require_digest(fixture, MEMBER_SHA256)
        report["verified_zip_sha256"] = require_digest(
            ROOT / fixture_dir / "public-fixture.zip", ZIP_SHA256
        )
        environment = os.environ.copy()
        environment.update({
            "FESTERM_ADR84_SIGNED_HOST_FIXTURE": str(fixture),
            "CARGO_BUILD_JOBS": "1", "CARGO_PROFILE_DEV_DEBUG": "0",
            "CARGO_PROFILE_TEST_DEBUG": "0", "CARGO_INCREMENTAL": "0",
            "CARGO_TERM_COLOR": "never",
            "CARGO_TARGET_DIR": str(output / "target"),
        })
        native_log = command(output, report, "native-pki", TEST_COMMAND, 600, environment)
        text = native_log.read_text(encoding="utf-8", errors="replace")
        if (
            "test result: ok. 1 passed; 0 failed; 0 ignored;" not in text
            or TEST not in text
            or "native_task_runtime_qualification=false; fixture=read-only-publisher" not in text
        ):
            raise ValidationFailure("Native fixture did not prove exactly one passing selected test.")
        report["status"] = "passed"
        report["native_pki_fixture_passed"] = True
        code = 0
    except Exception as error:
        report["failure"] = type(error).__name__
        if isinstance(error, ValidationFailure):
            report["failure_reason"] = str(error)
        code = error.code if isinstance(error, ValidationFailure) else 1
        print(f"Windows public PKI fixture failed: {type(error).__name__}; retain evidence.", file=sys.stderr)
    finally:
        report["exit_code"] = code
        report["completed_at_utc"] = datetime.now(timezone.utc).isoformat()
        with (output / "report.json").open("x", encoding="utf-8") as stream:
            json.dump(report, stream, indent=2)
            stream.write("\n")
    return code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-directory", default=".windows-pki-evidence")
    parser.add_argument("--acquire-only", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    require_opt_in()
    if args.acquire_only:
        acquire(args.output_directory)
        return 0
    return run(args.output_directory)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        reason = str(error) if isinstance(error, ValidationFailure) else type(error).__name__
        print(f"Windows public PKI fixture refused: {reason}", file=sys.stderr)
        sys.exit(error.code if isinstance(error, ValidationFailure) else 1)
