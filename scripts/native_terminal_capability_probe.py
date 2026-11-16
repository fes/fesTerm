#!/usr/bin/env python3
"""Opt-in POSIX terminal query observations; never inject terminal replies."""
import argparse
import json
import os
from pathlib import Path
import re
import select
import signal
import stat
import time
if os.name == "posix":
    import fcntl
    import termios
    import tty

MAX_BYTES = 4096
MAX_FRAME = 256
MAX_SECONDS = 12.0
QUIET_SECONDS = 0.25
ENVIRONMENT = ("TERM", "TERM_PROGRAM", "COLORTERM", "LANG", "LC_ALL", "LC_CTYPE")
QUERIES = (
    ("da1", b"\x1b[c"), ("da2", b"\x1b[>c"), ("dsr6", b"\x1b[6n"),
    *((f"osc4_{i}", f"\x1b]4;{i};?\x1b\\".encode()) for i in (0, 7, 15, 255)),
    *((f"osc{i}", f"\x1b]{i};?\x1b\\".encode()) for i in (10, 11, 12)),
)


class Refused(Exception):
    def __init__(self, reason, count=0):
        self.reason, self.count = reason, count
        super().__init__(reason)  # Never retain or format received bytes.


class QuietParser(argparse.ArgumentParser):
    def error(self, _message):
        raise Refused("arguments")


class Replies:
    def __init__(self):
        self.buffer = bytearray()
        self.received = 0
        self.values = {name: [] for name, _ in QUERIES}

    def _refuse(self, reason):
        count = len(self.buffer)
        self.buffer.clear()
        raise Refused(reason, count)

    def feed(self, data):
        self.received += len(data)
        if self.received > MAX_BYTES:
            raise Refused("input_budget", len(data))
        self.buffer.extend(data)
        while self.buffer:
            if self.buffer[0] != 27:
                self._refuse("unexpected_input")
            if len(self.buffer) < 2:
                return
            kind = self.buffer[1]
            if kind == ord("["):
                end = next((i + 1 for i in range(2, len(self.buffer))
                            if 0x40 <= self.buffer[i] <= 0x7e), None)
            elif kind == ord("]"):
                end = None
                for i in range(2, len(self.buffer)):
                    if self.buffer[i] == 7:
                        end = i + 1
                        break
                    if i + 1 < len(self.buffer) and self.buffer[i:i + 2] == b"\x1b\\":
                        end = i + 2
                        break
            else:
                self._refuse("unexpected_input")
            if end is None:
                if len(self.buffer) > MAX_FRAME:
                    self._refuse("frame_budget")
                return
            if end > MAX_FRAME:
                self._refuse("frame_budget")
            frame = bytes(self.buffer[:end])
            del self.buffer[:end]
            parsed = self.recognize(frame)
            if parsed is None:
                # Count the rejected frame without copying it to any record.
                raise Refused("unexpected_reply", len(frame))
            name, value = parsed
            self.values[name].append(value)

    @staticmethod
    def recognize(frame):
        match = re.fullmatch(rb"\x1b\[([?>])([0-9;]{1,128})c", frame)
        if match:
            parts = match[2].split(b";")
            if not 1 <= len(parts) <= 32 or any(not p or len(p) > 5 for p in parts):
                return None
            values = [int(p) for p in parts]
            if any(v > 65535 for v in values):
                return None
            return ("da1" if match[1] == b"?" else "da2"), {"parameters": values}
        match = re.fullmatch(rb"\x1b\[([0-9]{1,5});([0-9]{1,5})R", frame)
        if match and all(1 <= int(v) <= 65535 for v in match.groups()):
            return "dsr6", {"row": int(match[1]), "column": int(match[2])}
        match = re.fullmatch(
            rb"\x1b\](4;(0|7|15|255)|10|11|12);rgb:([0-9a-fA-F]{1,4})/"
            rb"([0-9a-fA-F]{1,4})/([0-9a-fA-F]{1,4})(\x07|\x1b\\)", frame)
        if match:
            name = "osc4_" + match[2].decode() if match[2] else "osc" + match[1].decode()
            return name, {"rgb": [part.decode().lower() for part in match.group(3, 4, 5)]}
        return None

    def report(self):
        return {name: {"count": len(values), "replies": values}
                for name, values in self.values.items()}


def selected_environment(environ):
    result = {}
    for name in ENVIRONMENT:
        value = environ.get(name)
        if value is None:
            result[name] = {"set": False, "value": None}
        elif re.fullmatch(r"[A-Za-z0-9_.@:+/-]{0,128}", value):
            result[name] = {"set": True, "value": value}
        else:
            result[name] = {"set": True, "omitted": "invalid_or_overlength"}
    return result


def exclusive_output(path):
    parent = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(parent)
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
            raise Refused("output_parent_not_private")
        return os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                       0o600, dir_fd=parent)
    finally:
        os.close(parent)


def attributes_restored(before, after):
    # macOS sets this kernel-owned "retype pending input (state)" bit on
    # return to canonical mode. Do not read/flush input just to clear it.
    left, right = list(before), list(after)
    pendin = getattr(termios, "PENDIN", 0)
    left[3] &= ~pendin
    right[3] &= ~pendin
    return left == right


def observe(input_fd=0, output_fd=1):
    if not all(os.isatty(fd) for fd in (input_fd, output_fd)):
        raise Refused("tty_required")
    if os.fstat(input_fd).st_rdev != os.fstat(output_fd).st_rdev:
        raise Refused("same_tty_required")
    if os.tcgetpgrp(input_fd) != os.getpgrp():
        raise Refused("foreground_required")
    if select.select([input_fd], [], [], 0)[0]:
        raise Refused("pending_input")  # Do not read or flush existing input.
    attributes = termios.tcgetattr(input_fd)
    flags = {fd: fcntl.fcntl(fd, fcntl.F_GETFL) for fd in (input_fd, output_fd)}
    replies = Replies()
    before = os.get_terminal_size(input_fd)
    result = {"grid_before": {"columns": before.columns, "rows": before.lines}}
    start = time.monotonic()
    handlers = {}
    cancellation_requested = False
    handled = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)

    def interrupted(_number, _frame):
        # Never raise asynchronously: a handler can run at the boundary
        # between observation and cleanup, before cleanup blocks signals.
        nonlocal cancellation_requested
        cancellation_requested = True

    try:
        for number in handled:
            handlers[number] = signal.signal(number, interrupted)
        tty.setraw(input_fd, when=termios.TCSANOW)
        for fd, previous in flags.items():
            fcntl.fcntl(fd, fcntl.F_SETFL, previous | os.O_NONBLOCK)
        if select.select([input_fd], [], [], 0)[0]:
            # Noncanonical mode can expose existing partial typeahead which
            # was not readable in canonical mode. Preserve it, without reading.
            raise Refused("pending_input")
        pending = b"".join(query for _, query in QUERIES)
        quiet_since = None
        deadline = start + MAX_SECONDS
        while time.monotonic() < deadline:
            if cancellation_requested:
                raise Refused("interrupted")
            now = time.monotonic()
            if now >= deadline:
                break
            if not pending and all(replies.values.values()) and quiet_since is not None:
                if now - quiet_since >= QUIET_SECONDS:
                    break
            readable, writable, _ = select.select(
                [input_fd], [output_fd] if pending else [], [], min(0.05, deadline - now))
            if writable:
                try:
                    pending = pending[os.write(output_fd, pending):]
                except BlockingIOError:
                    pass
            if readable:
                try:
                    data = os.read(input_fd, min(512, MAX_BYTES - replies.received + 1))
                except BlockingIOError:
                    continue
                if not data:
                    raise Refused("tty_closed")
                replies.feed(data)
                quiet_since = time.monotonic()
        if replies.buffer:
            raise Refused("incomplete_reply", len(replies.buffer))
        result["status"] = "complete" if all(len(v) == 1 for v in replies.values.values()) else "incomplete"
        if pending:
            result["status"] = "incomplete"
            result["reason"] = "query_write_timeout"
    except Refused as error:
        result.update(status="aborted", reason=error.reason, unknown_bytes=error.count)
    finally:
        previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, handled)
        cleanup_error = None
        try:
            try:
                termios.tcsetattr(input_fd, termios.TCSANOW, attributes)
            except BaseException as error:
                cleanup_error = error
            for fd, previous in flags.items():
                try:
                    fcntl.fcntl(fd, fcntl.F_SETFL, previous)
                except BaseException as error:
                    cleanup_error = cleanup_error or error
            # Observe without consuming pending cancellation. Delivery retains
            # the caller's original disposition when its mask is restored.
            if set(handled).intersection(signal.sigpending()):
                cancellation_requested = True
            for number, previous in handlers.items():
                signal.signal(number, previous)
        finally:
            # Pending signals now use the caller's original handlers, after
            # every terminal/descriptor restoration has been attempted.
            signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)
        if cleanup_error is not None:
            raise cleanup_error
    if cancellation_requested:
        result.update(status="aborted", reason="interrupted")
    after = os.get_terminal_size(input_fd)
    restored = termios.tcgetattr(input_fd)
    # fcntl(2) documents these mutable I/O flags. macOS may expose additional
    # kernel status bits after operating on a controlling PTY; report that
    # difference separately instead of pretending those bits are restored.
    io_flags = os.O_NONBLOCK | os.O_APPEND | getattr(os, "O_ASYNC", 0)
    current_flags = {fd: fcntl.fcntl(fd, fcntl.F_GETFL) for fd in flags}
    result.update(grid_after={"columns": after.columns, "rows": after.lines},
                  grid_stable=before == after, terminal_restored=attributes_restored(attributes, restored),
                  descriptor_io_flags_restored=all(current_flags[fd] & io_flags == previous & io_flags
                                                   for fd, previous in flags.items()),
                  other_descriptor_status_bits_changed=any((current_flags[fd] ^ previous) & ~io_flags
                                                          for fd, previous in flags.items()),
                  kernel_pendin_changed=bool((attributes[3] ^ restored[3]) & getattr(termios, "PENDIN", 0)),
                  input_bytes=replies.received, elapsed_ms=round((time.monotonic() - start) * 1000),
                  observations=replies.report())
    return result


def main(argv=None):
    if os.name != "posix":
        return 2
    parser = QuietParser(description=__doc__, add_help=False)
    parser.add_argument("--case", required=True)
    parser.add_argument("--output", required=True, type=Path)
    try:
        args = parser.parse_args(argv)
    except Refused:
        return 2
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", args.case):
        return 2
    try:
        destination = exclusive_output(args.output)
    except (OSError, Refused):
        return 2
    try:
        try:
            result = observe()
        except Refused as error:
            result = {"status": "aborted", "reason": error.reason, "unknown_bytes": error.count}
        except (OSError, termios.error):
            result = {"status": "aborted", "reason": "tty_io"}
        record = {"schema": 1, "case": args.case, "platform": "POSIX",
                  "environment": selected_environment(os.environ), **result}
        with os.fdopen(destination, "w", encoding="utf-8") as output:
            json.dump(record, output, sort_keys=True)
            output.write("\n")
        return 0 if (result["status"] == "complete" and result.get("grid_stable")
                     and result.get("terminal_restored") and result.get("descriptor_io_flags_restored")) else 1
    except BaseException:
        # Do not print exception text: a platform error could include private input.
        try:
            os.close(destination)
        except OSError:
            pass
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
