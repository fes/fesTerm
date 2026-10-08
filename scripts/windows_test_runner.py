"""Cargo target runner: original status plus bounded, content-free evidence."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
import uuid

from windows_native_debug import NativeDebugger, cancel_reader


ROOT = Path(__file__).resolve().parents[1]
TEST_RESULT = re.compile(r"^test ([A-Za-z0-9_:.$#-]{1,240}) \.\.\. (ok|FAILED|ignored)\s*$")


def source_identity():
    identity = {}
    for key, arguments in (
        ("head", ["rev-parse", "HEAD"]),
        ("tree", ["rev-parse", "HEAD^{tree}"]),
        ("dirty", ["status", "--porcelain", "--untracked-files=no"]),
    ):
        result = subprocess.run(
            ["git", "-C", str(ROOT), *arguments], capture_output=True, timeout=10,
            check=False,
        )
        if result.returncode:
            identity[key] = "unavailable"
        else:
            identity[key] = bool(result.stdout) if key == "dirty" else result.stdout.decode().strip()
    return identity


def executable_identity(path):
    digest = hashlib.sha256()
    with path.open("rb") as binary:
        for chunk in iter(lambda: binary.read(1024 * 1024), b""):
            digest.update(chunk)
    return {"name": path.name, "sha256": digest.hexdigest(), "bytes": path.stat().st_size}


def encode_receipt(record):
    limit = 4 * 1024 * 1024
    text = json.dumps(record, indent=2) + "\n"
    if len(text.encode("utf-8")) <= limit:
        return text
    record["receipt_truncated"] = True
    for event in record["events"]:
        for stack in event.get("stacks", []):
            for frame in stack["frames"]:
                frame.pop("symbol", None)
        if len(event.get("stacks", [])) > 1:
            event["stacks"] = event["stacks"][:1]
            record["stacks_truncated"] = True
    record["test_context"]["reported_tests"] = record["test_context"]["reported_tests"][-1024:]
    record["test_context"]["truncated"] = True
    text = json.dumps(record, indent=2) + "\n"
    if len(text.encode("utf-8")) > limit:
        raise ValueError("metadata cannot fit its receipt bound")
    return text


class TestContext:
    def __init__(self):
        self.results = []
        self.truncated = False
        self.lock = threading.Lock()

    def observe(self, line):
        match = TEST_RESULT.fullmatch(line)
        if match:
            with self.lock:
                if len(self.results) < 8192:
                    self.results.append({"name": match[1], "result": match[2]})
                else:
                    self.truncated = True

    def record(self):
        with self.lock:
            return {
                "reported_tests": self.results.copy(), "truncated": self.truncated,
                "active_test_attribution": "unavailable_in_stable_parallel_libtest",
            }


def read_output(fd, destination, context):
    pending = b""
    try:
        while True:
            chunk = os.read(fd, 4096)
            if not chunk:
                break
            # Preserve ordinary CI output, but never copy it into artifacts.
            destination.buffer.write(chunk)
            destination.buffer.flush()
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                if len(line) <= 512:
                    context.observe(line.decode("utf-8", errors="replace").rstrip("\r"))
            if len(pending) > 512:
                pending = b""
        if pending:
            context.observe(pending.decode("utf-8", errors="replace").rstrip("\r"))
    except OSError:
        pass
    finally:
        os.close(fd)


def run_debugger(debugger, executable, arguments, timeout, context):
    import msvcrt

    readers, write_fds = [], []
    try:
        handles = []
        for stream in (sys.stdout, sys.stderr):
            read_fd, write_fd = os.pipe()
            os.set_inheritable(write_fd, True)
            write_fds.append(write_fd)
            handles.append(msvcrt.get_osfhandle(write_fd))
            thread = threading.Thread(
                target=read_output, args=(read_fd, stream, context), daemon=True,
            )
            thread.start()
            readers.append(thread)
        return debugger.run(
            str(executable), arguments, str(Path.cwd()), timeout, *handles,
            msvcrt.get_osfhandle(sys.stdin.fileno()),
        )
    finally:
        for fd in write_fds:
            os.close(fd)
        for thread in readers:
            thread.join(0.5)
            cancel_reader(thread)


def classify(record):
    if record.get("outcome") in ("timeout", "runner_error", "invalid_arguments"):
        return record["outcome"]
    if record["capture_status"] == "unavailable":
        return "diagnostic_tool_unavailable"
    code = record.get("exit_code_unsigned")
    if code == 0:
        return "passed"
    if any(e["kind"] == "exception" and not e["first_chance"] for e in record["events"]):
        return "native_exception"
    if code == 101:
        return "rust_test_failure"
    return "native_exit_unknown_cause"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-root", required=True, type=Path)
    parser.add_argument("--timeout-seconds", type=float, default=1800)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not 0 < args.timeout_seconds <= 14400 or not command:
        parser.error("requires a command and timeout in (0, 14400]")
    executable = Path(command[0]).resolve()
    if not executable.is_file():
        parser.error("executable does not exist")
    owned_roots = [ROOT]
    if os.environ.get("CARGO_TARGET_DIR"):
        owned_roots.append(Path(os.environ["CARGO_TARGET_DIR"]).resolve())
    if not any(executable.is_relative_to(root) for root in owned_roots):
        parser.error("only repository-owned build/test executables are accepted")
    args.output_root.mkdir(parents=True, exist_ok=True)
    receipt = args.output_root / f"{executable.stem}-{uuid.uuid4().hex}.json"
    record = {
        "schema": 1, "source": source_identity(),
        "executable": executable_identity(executable),
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "capture_status": "initializing", "events": [], "modules": [],
        "timeout_seconds": args.timeout_seconds,
        "ci": {name: os.environ[name] for name in (
            "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_SHA",
        ) if name in os.environ},
    }
    context = TestContext()
    started = time.monotonic()
    code = 125
    try:
        try:
            debugger = NativeDebugger(record)
        except (OSError, AttributeError, NotImplementedError) as error:
            # No success-shaped fallback or retry. The target has not started.
            record["capture_status"] = "unavailable"
            record["diagnostic_error"] = {"phase": "initialize", "type": type(error).__name__}
            record["cleanup"] = "not_started"
            print("ERROR: native diagnostics unavailable; target not started.", file=sys.stderr)
        else:
            record["capture_status"] = "captured"
            code = run_debugger(debugger, executable, command[1:], args.timeout_seconds, context)
            if record.get("outcome") == "timeout":
                code = 124
    except Exception as error:
        record["capture_status"] = "failed"
        record["outcome"] = "runner_error"
        record["diagnostic_error"] = {"phase": "run", "type": type(error).__name__}
        print("ERROR: native diagnostic runner failed; no successful result is inferred.", file=sys.stderr)
        code = 125
    finally:
        record["test_context"] = context.record()
        record["outcome"] = classify(record)
        status = record.get("exit_code_unsigned")
        if status is not None:
            record["exit_code_signed"] = status if status < 0x80000000 else status - 0x100000000
            record["exit_code_hex"] = f"0x{status:08x}"
        record["runner_exit_code"] = code & 0xffffffff
        record["duration_seconds"] = round(time.monotonic() - started, 3)
        try:
            receipt.write_text(encode_receipt(record), encoding="utf-8")
        except (OSError, ValueError):
            print("ERROR: native test receipt could not be persisted.", file=sys.stderr)
            code = 125
        print(
            f"native-test: {executable.name}: {record['outcome']}; "
            f"capture={record['capture_status']}; status={record.get('exit_code_hex', 'unknown')}",
            file=sys.stderr,
        )
    return code


if __name__ == "__main__":
    # Python's CRT exit binding accepts a signed C int; its bits remain the
    # original Windows DWORD, including NTSTATUS values above INT_MAX.
    sys.stdout.flush()
    sys.stderr.flush()
    result = main()
    sys.stdout.flush()
    sys.stderr.flush()
    unsigned = result & 0xffffffff
    os._exit(unsigned if unsigned < 0x80000000 else unsigned - 0x100000000)
