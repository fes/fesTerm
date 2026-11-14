#!/usr/bin/env python3
"""Explicit real-service HOST client campaign; never creates accounts or reads passwords."""

import argparse
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import selectors
import time

ROOT = Path(__file__).resolve().parents[1]
ACCOUNT = "fes-festerm1"
TEST = "controlled_github_host_service_client_workflow"
MARKER = b"FESTERM_DEV_TUNNEL_LAB_SERVICE_CLIENT_PASS"
RECORD = Path.home() / ".festerm-lab" / "dev-tunnel-workflow.json"


class Blocked(Exception):
    pass


def fixture_record(path=RECORD):
    """Read only nonsecret operator metadata. Never consult the password record."""
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 8192:
            raise Blocked("operator-record-missing-or-invalid")
        if os.name != "nt":
            info = path.stat()
            if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o600:
                raise Blocked("operator-record-needs-owner-only-permissions")
        record = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        raise Blocked("operator-record-missing-or-invalid") from None
    required = {"schema_version", "account", "configuration", "runtime_root", "recipe_name",
                "shared_profile_name", "private_profile_name", "generic_logical",
                "denied_logical", "creation_allowed", "allow_shell_input", "shell"}
    if (not isinstance(record, dict) or set(record) != required
            or record["schema_version"] != 1 or record["account"] != ACCOUNT
            or record["allow_shell_input"] is not True
            or not isinstance(record["creation_allowed"], bool)
            or record["shell"] not in ("posix", "powershell", "cmd")):
        raise Blocked("operator-record-does-not-authorize-campaign")
    for key in ("configuration", "runtime_root"):
        value = record[key]
        if not isinstance(value, str):
            raise Blocked("isolated-native-fixture-missing")
        target = Path(value)
        if (not target.is_absolute() or not target.exists() or target.resolve() != target
                or target == ROOT or ROOT in target.parents):
            raise Blocked("isolated-native-fixture-missing")
    return record


def run_bounded(command, *, env, timeout):
    """Bound the owned POSIX group and nonblocking drain, even if parent exits."""
    if os.name == "nt":
        # taskkill /T cannot own descendants after their parent exits. Refuse
        # instead of claiming the relay's watchdog covers direct optional runs.
        raise Blocked("windows-owned-job-capability-required")
    process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True)
    tail = bytearray()
    os.set_blocking(process.stdout.fileno(), False)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout

    def drain():
        # A chatty child must not monopolize deadline/grace checks.
        for _ in range(16):
            try:
                chunk = os.read(process.stdout.fileno(), 4096)
            except BlockingIOError:
                return
            if not chunk:
                return
            tail.extend(chunk)
            del tail[:-65536]

    stopped = False

    def no_live_group_members():
        # Darwin can return EPERM for a group containing only unreaped zombies.
        # Confirm that exceptional empty/dead-group state instead of suppressing
        # a real failure to signal a living owned descendant.
        result = subprocess.run(["ps", "-axo", "pgid=,stat="], capture_output=True,
                                text=True, timeout=2, check=True)
        return not any(fields[0] == str(process.pid) and not fields[1].startswith("Z")
                       for line in result.stdout.splitlines()
                       if len(fields := line.split()) == 2)

    def send_group(sig):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            return False
        except PermissionError:
            if no_live_group_members():
                return False
            raise
        return True

    def stop_group():
        nonlocal stopped
        if stopped:
            return
        # Kill the group regardless of direct-parent state: a descendant may
        # still own stdout and ignore SIGTERM after Cargo has already exited.
        if not send_group(signal.SIGTERM):
            stopped = True
            return
        grace = time.monotonic() + 0.25
        while time.monotonic() < grace:
            drain()
            time.sleep(0.01)
        process.poll()  # reap the direct child; descendants remain in our group
        send_group(signal.SIGKILL)
        stopped = True

    try:
        while process.poll() is None and time.monotonic() < deadline:
            selector.select(timeout=min(0.05, max(0, deadline - time.monotonic())))
            drain()
        code = process.poll()
        stop_group()
        process.wait(timeout=3)
        drain()
        return code, bytes(tail)
    finally:
        # Also clean up on keyboard interruption or a read/selector failure.
        stop_group()
        process.wait(timeout=3)
        selector.close()
        process.stdout.close()


def evidence(path, status, reason):
    # Only fixed classifications cross the artifact boundary; no raw child output.
    value = (f"scope=real-service-host-client\naccount={ACCOUNT}\n"
             f"gui_acceptance=unrun\nreason={reason}\nstatus={status}\n")
    if path is not None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
    print(value, end="")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="explicitly allow service calls and fixture shell input")
    parser.add_argument("--result", type=Path, help="content-free evidence destination")
    args = parser.parse_args(argv)
    if args.result is not None and (args.result.exists() or args.result.is_symlink()):
        evidence(None, "blocked", "result-already-exists")
        return 2
    if not args.run:
        evidence(args.result, "blocked", "explicit-opt-in-required")
        return 2
    try:
        record = fixture_record()
        if os.name == "nt":
            raise Blocked("windows-owned-job-capability-required")
    except Blocked as error:
        evidence(args.result, "blocked", str(error))
        return 2
    env = dict(os.environ)
    env["FESTERM_RUN_DEV_TUNNEL_LAB"] = "1"
    env["FESTERM_DEV_TUNNEL_LAB_RECORD"] = str(RECORD)
    env["FESTERM_SESSIOND_RUNTIME_ROOT"] = record["runtime_root"]
    # The private test itself selects the exact verified account/recipe. No login
    # password, OAuth token or native key is sent through the process environment.
    base = ["cargo", "test", "--locked", "-p", "festerm", "--test", "dev_tunnel_workflow_lab"]
    try:
        code, _ = run_bounded([*base, "--no-run"], env=env, timeout=900)
        if code != 0:
            evidence(args.result, "fail", "build-timeout" if code is None else "build-failed")
            return 1
        code, output = run_bounded([*base, TEST, "--", "--exact", "--ignored", "--nocapture"],
                                   env=env, timeout=180)
    except OSError:
        evidence(args.result, "blocked", "toolchain-or-process-unavailable")
        return 2
    if b"FESTERM_DEV_TUNNEL_LAB_BLOCKED" in output:
        evidence(args.result, "blocked", "native-account-host-approval-or-fixture-prerequisite")
        return 2
    if code != 0 or MARKER not in output or b"1 passed; 0 failed" not in output:
        evidence(args.result, "fail", "service-client-timeout" if code is None else "service-client-failed")
        return 1
    evidence(args.result, "pass", "scoped-fixtures-retained-detached-no-global-cleanup")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
