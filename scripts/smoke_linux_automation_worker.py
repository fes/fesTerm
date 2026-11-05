#!/usr/bin/env python3
"""Probe a packaged Linux worker without sending workflow source or authority."""

import ctypes
import json
import os
import select
import selectors
import signal
import subprocess
import sys
import time
from collections import deque
from pathlib import Path

MAX_FRAME = 65_536
MAX_OUTPUT = 2 * (MAX_FRAME + 4)
TIMEOUT = 10.0
CLEANUP_TIMEOUT = 2.0
WAIT_SLICE = 0.1
MAX_DESCENDANTS = 512
MAX_THREADS = 512
MAX_CHILD_LIST_BYTES = MAX_DESCENDANTS * 16
MAX_REAPS_PER_SCAN = 64
PR_SET_CHILD_SUBREAPER = 36
READY = {"type": "ready", "profile": "linux-seccomp-stdio-rlimit-v1"}
PROTOCOL_FAILURE = {"type": "failed", "code": "protocol"}
# The packaged executable cannot start before its process has a pidfd.
LAUNCHER = (
    "import os, sys\n"
    "fd = int(sys.argv[1])\n"
    "try:\n"
    "    admitted = os.read(fd, 1)\n"
    "finally:\n"
    "    os.close(fd)\n"
    "if admitted != b'\\x01':\n"
    "    sys.exit(1)\n"
    "os.execve(sys.argv[2], [sys.argv[2], '--automation-worker'], "
    "{'MALLOC_ARENA_MAX': '1'})\n"
)


class SmokeFailure(Exception):
    pass


class IncompleteScan(SmokeFailure):
    def __init__(self, message, children):
        super().__init__(message)
        self.children = children


class StopRequest:
    def __init__(self):
        self.requested = False

    def __call__(self, _signal, _frame):
        self.requested = True

    def check(self):
        if self.requested:
            raise SmokeFailure("worker smoke interrupted")


def enable_subreaper():
    if sys.platform != "linux":
        raise SmokeFailure("worker smoke requires Linux")
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise SmokeFailure("worker smoke requires Linux pidfds")
    try:
        signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    except (OSError, ValueError) as error:
        raise SmokeFailure("cannot make worker children waitable") from error
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        raise SmokeFailure("cannot supervise worker descendants")
    verify_pidfd_cleanup()
    if child_pids(deadline=time.monotonic() + CLEANUP_TIMEOUT):
        raise SmokeFailure("unexpected children before worker launch")


def verify_pidfd_cleanup():
    try:
        child_read, parent_write = os.pipe()
    except OSError as error:
        raise SmokeFailure("cannot validate Linux pidfd cleanup") from error
    try:
        pid = os.fork()
    except OSError as error:
        os.close(child_read)
        os.close(parent_write)
        raise SmokeFailure("cannot validate Linux pidfd cleanup") from error
    if pid == 0:
        try:
            os.close(parent_write)
            os.read(child_read, 1)
        finally:
            os._exit(0)

    failure = None
    fd = None
    try:
        os.close(child_read)
    except OSError as error:
        failure = error
    try:
        fd = os.pidfd_open(pid)
        signal.pidfd_send_signal(fd, signal.SIGKILL)
    except OSError as error:
        failure = failure or error
    finally:
        if fd is not None:
            try:
                os.close(fd)
            except OSError as error:
                failure = failure or error
        try:
            os.close(parent_write)
        except OSError as error:
            failure = failure or error

    deadline = time.monotonic() + CLEANUP_TIMEOUT
    status = None
    while time.monotonic() < deadline:
        try:
            reaped, candidate = os.waitpid(pid, os.WNOHANG)
        except InterruptedError:
            continue
        except OSError as error:
            failure = failure or error
            break
        if reaped == pid:
            status = candidate
            break
        time.sleep(0.01)
    if failure or status is None or not (
        os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGKILL
    ):
        raise SmokeFailure("Linux pidfd cleanup is unavailable") from failure


def child_pids(parent=None, deadline=None):
    parent = os.getpid() if parent is None else parent
    tasks = Path(f"/proc/{parent}/task")
    children = set()
    failure = None
    try:
        with os.scandir(tasks) as threads:
            for index, thread in enumerate(threads):
                if index >= MAX_THREADS or (
                    deadline is not None and time.monotonic() >= deadline
                ):
                    failure = "worker descendant scan exceeded its bound"
                    break
                try:
                    with open(Path(thread.path) / "children", "rb") as children_file:
                        data = children_file.read(MAX_CHILD_LIST_BYTES + 1)
                except FileNotFoundError:
                    try:
                        os.stat(thread.path)
                    except FileNotFoundError:
                        continue
                    except OSError:
                        pass
                    failure = "cannot inspect worker descendants"
                    continue
                except OSError:
                    failure = "cannot inspect worker descendants"
                    continue
                if len(data) > MAX_CHILD_LIST_BYTES:
                    data = data[:MAX_CHILD_LIST_BYTES]
                    boundary = data.rfind(b" ")
                    data = data[: boundary + 1] if boundary >= 0 else b""
                    failure = "worker descendant list exceeded its bound"
                try:
                    for value in data.split():
                        if len(children) >= MAX_DESCENDANTS or (
                            deadline is not None and time.monotonic() >= deadline
                        ):
                            failure = "worker descendant list exceeded its bound"
                            break
                        children.add(int(value))
                except ValueError:
                    failure = "cannot inspect worker descendants"
    except FileNotFoundError:
        if parent != os.getpid():
            return sorted(children)
        raise SmokeFailure("cannot inspect worker descendants")
    except OSError as error:
        raise IncompleteScan("cannot inspect worker descendants", sorted(children)) from error
    if failure:
        raise IncompleteScan(failure, sorted(children))
    return sorted(children)


def pidfd_exited(fd):
    poller = select.poll()
    poller.register(fd, select.POLLIN)
    return bool(poller.poll(0))


def parent_pid(pid):
    try:
        with Path(f"/proc/{pid}/status").open("rb") as status:
            for _ in range(16):
                line = status.readline(512)
                if not line or not line.endswith(b"\n"):
                    break
                if line.startswith(b"PPid:"):
                    return int(line.split()[1])
    except FileNotFoundError:
        return None
    except (OSError, ValueError, IndexError) as error:
        raise SmokeFailure("cannot inspect worker descendant identity") from error
    raise SmokeFailure("cannot inspect worker descendant identity")


def owned_descendants(deadline):
    queue = deque([(os.getpid(), None)])
    observed = {os.getpid()}
    descendants = []
    failure = None
    found = False
    now = time.monotonic()
    scan_deadline = now + max(0, deadline - now) / 2
    while queue and time.monotonic() < scan_deadline:
        parent, parent_fd = queue.popleft()
        try:
            exited = parent_fd is not None and pidfd_exited(parent_fd)
        except OSError as error:
            failure = failure or error
            continue
        if exited:
            continue
        try:
            children = child_pids(parent, scan_deadline)
        except IncompleteScan as error:
            failure = failure or error
            children = error.children
        except SmokeFailure as error:
            failure = failure or error
            continue
        for pid in children:
            found = True
            if time.monotonic() >= deadline:
                failure = failure or SmokeFailure("worker descendant scan timed out")
                break
            if pid in observed:
                continue
            if len(observed) >= MAX_DESCENDANTS:
                failure = failure or SmokeFailure("too many worker descendants")
                break
            observed.add(pid)
            try:
                fd = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            except OSError as error:
                failure = failure or error
                continue
            try:
                owner = parent_pid(pid)
                if (
                    owner != parent
                    or pidfd_exited(fd)
                    or (parent_fd is not None and pidfd_exited(parent_fd))
                ):
                    os.close(fd)
                    continue
            except (OSError, SmokeFailure) as error:
                os.close(fd)
                failure = failure or error
                continue
            descendants.append((pid, fd))
            queue.append((pid, fd))
    if queue:
        failure = failure or SmokeFailure("worker descendant scan timed out")
    return descendants, found, failure


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate frame field")
        result[key] = value
    return result


def validate_frames(data):
    offset = 0
    for expected in (READY, PROTOCOL_FAILURE):
        if len(data) - offset < 4:
            raise SmokeFailure("worker did not report a complete frame")
        size = int.from_bytes(data[offset : offset + 4], "big")
        offset += 4
        if not 0 < size <= MAX_FRAME or len(data) - offset < size:
            raise SmokeFailure("worker frame is incomplete or outside its bound")
        try:
            payload = data[offset : offset + size].decode("utf-8")
            value = json.loads(payload, object_pairs_hook=unique_object)
        except (UnicodeDecodeError, ValueError) as error:
            raise SmokeFailure("worker frame is not strict UTF-8 JSON") from error
        if value != expected:
            raise SmokeFailure("worker did not report the expected qualification")
        offset += size
    if offset != len(data):
        raise SmokeFailure("worker emitted unexpected trailing data")


def read_output(pipe, deadline, stop):
    output = bytearray()
    with selectors.DefaultSelector() as selector:
        selector.register(pipe, selectors.EVENT_READ)
        while True:
            stop.check()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeFailure("worker output timed out")
            if not selector.select(min(remaining, WAIT_SLICE)):
                continue
            chunk = os.read(pipe.fileno(), min(8192, MAX_OUTPUT + 1 - len(output)))
            if not chunk:
                return bytes(output)
            output.extend(chunk)
            if len(output) > MAX_OUTPUT:
                raise SmokeFailure("worker output exceeded its bound")


def cleanup_children():
    observed = False
    cleanup_failure = None
    deadline = time.monotonic() + CLEANUP_TIMEOUT
    while True:
        children, found, scan_failure = owned_descendants(deadline)
        cleanup_failure = cleanup_failure or scan_failure
        if not found and scan_failure is None:
            if cleanup_failure:
                raise SmokeFailure("could not inspect or signal a worker descendant") from (
                    cleanup_failure
                )
            return observed
        observed |= found
        for _pid, fd in children:
            try:
                signal.pidfd_send_signal(fd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError as error:
                cleanup_failure = cleanup_failure or error
            finally:
                os.close(fd)
        for _ in range(MAX_REAPS_PER_SCAN):
            if time.monotonic() >= deadline:
                break
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                break
            if pid == 0:
                break
        if time.monotonic() >= deadline:
            raise SmokeFailure("worker descendants could not be reaped") from cleanup_failure
        time.sleep(0.01)


def wait_for_worker(process, deadline, stop):
    while True:
        stop.check()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise SmokeFailure("worker exit timed out")
        try:
            return process.wait(timeout=min(remaining, WAIT_SLICE))
        except subprocess.TimeoutExpired:
            continue


def probe(path, stop=None):
    stop = stop or StopRequest()
    stop.check()
    enable_subreaper()
    deadline = time.monotonic() + TIMEOUT
    process = None
    process_fd = None
    launch_read = None
    launch_write = None
    try:
        stop.check()
        launch_read, launch_write = os.pipe()
        process = subprocess.Popen(
            [sys.executable, "-I", "-S", "-c", LAUNCHER, str(launch_read), path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env={"MALLOC_ARENA_MAX": "1"},
            pass_fds=(launch_read,),
        )
        fd, launch_read = launch_read, None
        os.close(fd)
        process_fd = os.pidfd_open(process.pid)
        stop.check()
        if os.write(launch_write, b"\x01") != 1:
            raise SmokeFailure("cannot admit worker launch")
        fd, launch_write = launch_write, None
        os.close(fd)
        process.stdin.close()
        output = read_output(process.stdout, deadline, stop)
        status = wait_for_worker(process, deadline, stop)
        validate_frames(output)
        if status != 70:
            raise SmokeFailure("worker did not refuse the closed input pipe")
    finally:
        cleanup_failure = None
        for fd in (launch_read, launch_write):
            if fd is not None:
                try:
                    os.close(fd)
                except OSError as error:
                    cleanup_failure = cleanup_failure or error
        if process is not None:
            for pipe in (process.stdin, process.stdout):
                try:
                    pipe.close()
                except OSError as error:
                    cleanup_failure = cleanup_failure or error
        descendants = False
        if process is not None and process.returncode is None:
            try:
                if process_fd is not None:
                    signal.pidfd_send_signal(process_fd, signal.SIGKILL)
                else:
                    # The launcher cannot exec before admission, and its
                    # unreaped PID cannot have been reused.
                    os.kill(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError as error:
                cleanup_failure = cleanup_failure or error
        try:
            descendants = cleanup_children()
        except (OSError, SmokeFailure) as error:
            cleanup_failure = cleanup_failure or error
        if process is not None and process.returncode is None:
            try:
                process.wait(timeout=CLEANUP_TIMEOUT)
            except (OSError, subprocess.TimeoutExpired) as error:
                cleanup_failure = cleanup_failure or error
        if process_fd is not None:
            try:
                os.close(process_fd)
            except OSError as error:
                cleanup_failure = cleanup_failure or error
        if cleanup_failure:
            raise SmokeFailure("worker or descendants could not be cleaned up") from (
                cleanup_failure
            )
        if descendants:
            if stop.requested:
                raise SmokeFailure("worker smoke interrupted; unexpected descendants reaped")
            raise SmokeFailure("worker or unexpected descendants required cleanup")
    stop.check()


def main():
    if len(sys.argv) != 2 or not os.access(sys.argv[1], os.X_OK):
        raise SmokeFailure("expected the executable Linux automation worker path")
    stop = StopRequest()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    probe(sys.argv[1], stop)


if __name__ == "__main__":
    try:
        main()
    except (OSError, SmokeFailure) as error:
        print(f"Packaged automation worker smoke failed: {error}", file=sys.stderr)
        sys.exit(1)
