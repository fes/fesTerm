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
import shutil
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



class _WindowsNative:
    """Narrow owned-Job APIs, matching festerm-windows-job's kill-on-close policy.

    CPython's native CreateProcess binding constructs the restricted handle list.
    No child code runs until assignment to our non-breakaway Job is verified.
    """
    def __init__(self):
        import ctypes as c
        from ctypes import wintypes as w
        import _winapi
        self.c, self.w, self.winapi = c, w, _winapi
        self.kernel = c.WinDLL("kernel32", use_last_error=True)

        class BasicLimit(c.Structure):
            _fields_ = [("process_time", c.c_int64), ("job_time", c.c_int64),
                        ("flags", w.DWORD), ("minimum_working_set", c.c_size_t),
                        ("maximum_working_set", c.c_size_t), ("active_limit", w.DWORD),
                        ("affinity", c.c_size_t), ("priority", w.DWORD), ("scheduling", w.DWORD)]

        class IoCounters(c.Structure):
            _fields_ = [(name, c.c_uint64) for name in
                        ("read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")]

        class ExtendedLimit(c.Structure):
            _fields_ = [("basic", BasicLimit), ("io", IoCounters),
                        ("process_memory", c.c_size_t), ("job_memory", c.c_size_t),
                        ("peak_process_memory", c.c_size_t), ("peak_job_memory", c.c_size_t)]

        class Accounting(c.Structure):
            _fields_ = [(name, c.c_int64) for name in
                        ("user_time", "kernel_time", "period_user_time", "period_kernel_time")] + [
                        (name, w.DWORD) for name in
                        ("page_faults", "total_processes", "active_processes", "terminated_processes")]

        self.ExtendedLimit, self.Accounting = ExtendedLimit, Accounting
        declarations = {
            "CreateJobObjectW": ([c.c_void_p, w.LPCWSTR], w.HANDLE),
            "SetInformationJobObject": ([w.HANDLE, c.c_int, c.c_void_p, w.DWORD], w.BOOL),
            "AssignProcessToJobObject": ([w.HANDLE, w.HANDLE], w.BOOL),
            "IsProcessInJob": ([w.HANDLE, w.HANDLE, c.POINTER(w.BOOL)], w.BOOL),
            "ResumeThread": ([w.HANDLE], w.DWORD),
            "TerminateJobObject": ([w.HANDLE, w.UINT], w.BOOL),
            "QueryInformationJobObject": ([w.HANDLE, c.c_int, c.c_void_p, w.DWORD, c.c_void_p], w.BOOL),
            "PeekNamedPipe": ([w.HANDLE, c.c_void_p, w.DWORD, c.c_void_p, c.POINTER(w.DWORD), c.c_void_p], w.BOOL),
            "ReadFile": ([w.HANDLE, c.c_void_p, w.DWORD, c.POINTER(w.DWORD), c.c_void_p], w.BOOL),
            "CloseHandle": ([w.HANDLE], w.BOOL),
        }
        for name, (arguments, result) in declarations.items():
            function = getattr(self.kernel, name)
            function.argtypes, function.restype = arguments, result

    def check(self, success):
        if not success:
            raise self.c.WinError(self.c.get_last_error())

    def close(self, handle):
        if handle is not None:
            self.check(self.kernel.CloseHandle(handle))

    def create_job(self):
        handle = self.kernel.CreateJobObjectW(None, None)
        if not handle:
            raise Blocked("windows-owned-job-unavailable")
        try:
            limits = self.ExtendedLimit()
            limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; no breakaway.
            self.check(self.kernel.SetInformationJobObject(
                handle, 9, self.c.byref(limits), self.c.sizeof(limits)))
            return handle
        except BaseException:
            self.close(handle)
            raise

    def create_suspended(self, command, env):
        import msvcrt
        executable = shutil.which(command[0], path=env.get("PATH"))
        if executable is None:
            raise Blocked("toolchain-or-process-unavailable")
        read = write = null = created = None
        try:
            read, write = self.winapi.CreatePipe(None, 0)
            os.set_handle_inheritable(read, False)
            os.set_handle_inheritable(write, True)
            null = os.open(os.devnull, os.O_RDONLY)
            stdin = msvcrt.get_osfhandle(null)
            os.set_handle_inheritable(stdin, True)
            startup = subprocess.STARTUPINFO()
            startup.dwFlags = subprocess.STARTF_USESTDHANDLES
            startup.hStdInput, startup.hStdOutput, startup.hStdError = stdin, write, write
            startup.lpAttributeList = {"handle_list": [stdin, write]}
            flags = 0x4 | subprocess.CREATE_NO_WINDOW  # CREATE_SUSPENDED; never break away.
            created = self.winapi.CreateProcess(
                executable, subprocess.list2cmdline([executable, *command[1:]]),
                None, None, True, flags, env, str(ROOT), startup)
            # Keep all process/thread/read ownership local until *every* stdio
            # release succeeds. Nothing that can raise runs after the transfer.
            self.close(write)
            write = None
            os.close(null)
            null = None
            return created[0], created[1], read
        except BaseException:
            cleanup_failed = False
            if created is not None:
                try:
                    self.terminate_suspended(created[0])
                except BaseException:
                    cleanup_failed = True
            for handle in ((created[1], created[0]) if created is not None else ()) + (read, write):
                try:
                    self.close(handle)
                except BaseException:
                    cleanup_failed = True
            if null is not None:
                try:
                    os.close(null)
                except BaseException:
                    cleanup_failed = True
            if cleanup_failed:
                raise Blocked("windows-suspended-child-cleanup-unconfirmed") from None
            raise

    def assign(self, job, process):
        if not self.kernel.AssignProcessToJobObject(job, process):
            raise Blocked("windows-owned-job-assignment-refused")
        assigned = self.w.BOOL()
        self.check(self.kernel.IsProcessInJob(process, job, self.c.byref(assigned)))
        if not assigned.value:
            raise Blocked("windows-owned-job-assignment-unverified")

    def resume(self, thread):
        if self.kernel.ResumeThread(thread) != 1:
            raise Blocked("windows-owned-child-resume-failed")

    def poll(self, process):
        if self.winapi.WaitForSingleObject(process, 0) == self.winapi.WAIT_TIMEOUT:
            return None
        return self.winapi.GetExitCodeProcess(process)

    def read_available(self, read):
        tail = bytearray()
        for _ in range(16):
            available = self.w.DWORD()
            if not self.kernel.PeekNamedPipe(read, None, 0, None, self.c.byref(available), None):
                if self.c.get_last_error() in (109, 232):  # closed writer, no data
                    break
                self.check(False)
            if not available.value:
                break
            size = min(4096, available.value)
            buffer = self.c.create_string_buffer(size)
            received = self.w.DWORD()
            self.check(self.kernel.ReadFile(read, buffer, size, self.c.byref(received), None))
            tail.extend(buffer.raw[:received.value])
        return bytes(tail)

    def terminate_job(self, job):
        self.check(self.kernel.TerminateJobObject(job, 1))

    def active_processes(self, job):
        accounting = self.Accounting()
        self.check(self.kernel.QueryInformationJobObject(
            job, 1, self.c.byref(accounting), self.c.sizeof(accounting), None))
        return accounting.active_processes

    def terminate_suspended(self, process):
        self.winapi.TerminateProcess(process, 1)
        if self.winapi.WaitForSingleObject(process, 3000) == self.winapi.WAIT_TIMEOUT:
            raise Blocked("windows-suspended-child-cleanup-unconfirmed")


def run_bounded_windows(command, *, env, timeout, api=None):
    """Own the complete child Job before resuming its primary thread."""
    if api is None:
        try:
            api = _WindowsNative()
        except (ImportError, AttributeError):
            raise Blocked("windows-native-job-api-unavailable") from None
    job = process = thread = read = None
    assigned = False
    empty = False
    tail = bytearray()

    def drain():
        if read is not None:
            tail.extend(api.read_available(read))
            del tail[:-65536]

    try:
        job = api.create_job()
        process, thread, read = api.create_suspended(command, env)
        api.assign(job, process)
        assigned = True
        api.resume(thread)
        api.close(thread)
        thread = None
        deadline = time.monotonic() + timeout
        code = api.poll(process)
        while code is None and time.monotonic() < deadline:
            drain()
            time.sleep(min(0.01, max(0, deadline - time.monotonic())))
            code = api.poll(process)
        # A normal Cargo exit may leave descendants. Terminate/query the exact
        # owned Job in both success and timeout cases before returning evidence.
        api.terminate_job(job)
        cleanup_deadline = time.monotonic() + 3
        while api.active_processes(job):
            if time.monotonic() >= cleanup_deadline:
                raise Blocked("windows-owned-job-cleanup-unconfirmed")
            drain()
            time.sleep(0.01)
        empty = True
        drain()
        return code, bytes(tail)
    finally:
        # Closing the non-inherited Job handle is the final kill-on-close fence.
        # Assignment failure leaves only our suspended child: terminate that
        # exact process handle, never a PID or taskkill tree inferred afterward.
        try:
            if process is not None and not assigned:
                api.terminate_suspended(process)
            elif assigned and not empty:
                api.terminate_job(job)
                cleanup_deadline = time.monotonic() + 3
                while api.active_processes(job):
                    if time.monotonic() >= cleanup_deadline:
                        raise Blocked("windows-owned-job-cleanup-unconfirmed")
                    time.sleep(0.01)
        finally:
            errors = []
            for handle in (job, thread, process, read):
                try:
                    api.close(handle)
                except OSError as error:
                    errors.append(error)
            if errors:
                raise Blocked("windows-owned-handles-cleanup-unconfirmed") from None


def run_bounded(command, *, env, timeout):
    """Bound owned native process groups/Jobs and private output drains."""
    if os.name == "nt":
        return run_bounded_windows(command, env=env, timeout=timeout)
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
    except Blocked as error:
        evidence(args.result, "blocked", str(error))
        return 2
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
