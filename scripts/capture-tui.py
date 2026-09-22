#!/usr/bin/env python3
"""Record the raw byte stream a real terminal program writes to its pty.

The captures this produces are committed as test fixtures and replayed against
a headless `festerm_core::Terminal`, so that our escape-sequence coverage is
answerable to what programs actually emit rather than to what we imagined they
emit. See `crates/festerm-core/tests/tui_capture.rs`.

Usage:

    scripts/capture-tui.py --list
    scripts/capture-tui.py vim less
    scripts/capture-tui.py --all

Recording is deliberately *not* part of any test run or CI job. A capture is
evidence of one moment on one machine; re-recording it casually would quietly
replace the evidence the assertions were written against. Re-record only when
you mean to, and read the diff.

Determinism: the replay is exact because the bytes are frozen, but the
*recording* is not reproducible - htop draws live CPU meters, and every program
draws whatever the clock said. Assertions in the replay tests must therefore key
off structure (alternate screen, cursor position, styling, box drawing) and not
off any value that changes between runs.

Leaks: rather than scrubbing the capture afterwards - which corrupts the column
alignment a TUI capture depends on - every program is run inside a throwaway
HOME at a fixed path with a fixed user name and no user config. The capture is
then checked for the real user name, host name and home directory, and the run
fails if any of them appear. Fix the environment, do not edit the bytes.
"""

from __future__ import annotations

import argparse
import fcntl
import getpass
import os
import pty
import re
import select
import shutil
import signal
import socket
import struct
import sys
import termios
import time
from dataclasses import dataclass, field
from pathlib import Path

COLUMNS = 120
ROWS = 40

REPOSITORY = Path(__file__).resolve().parent.parent
FIXTURES = REPOSITORY / "crates" / "festerm-core" / "tests" / "fixtures" / "tui"

# A fixed path, so that anything a program prints about its own working
# directory is identical on every machine that re-records.
SANDBOX = Path("/tmp/festerm-capture")
HOME = SANDBOX / "home"
WORK = SANDBOX / "work"
USER = "festerm"

# Large enough that a capture is never truncated mid-sequence, small enough
# that a runaway program cannot fill the disk.
MAXIMUM_CAPTURE_BYTES = 4 * 1024 * 1024

NOTES = """\
# Nimbus Relay

The relay accepts webhook events and republishes them onto an internal queue.

## Configuration

    listen = "0.0.0.0:8443"
    workers = 4

## Operational Notes

1. Restart the service with `systemctl restart nimbus-relay`.
2. Queue depth is exposed on `/metrics` for the operator role.
3. See the failover runbook before draining a node.

## Known Issues

- Queue draining is slow under heavy backpressure.
- The health endpoint does not report per-queue status.
"""

# `less` needs more than a screenful before it will scroll, and a word worth
# searching for so the match highlight (reverse video) is exercised.
LEDGER = "".join(
    f"{index:04d}  relay-{index % 7}  "
    + ("queue drained cleanly" if index % 11 else "BACKPRESSURE observed")
    + "\n"
    for index in range(1, 400)
)

# Identity columns are omitted deliberately: see the htop scenario. The meters
# are pinned so that a re-recording differs only in the numbers, not the layout.
# tmux's defaults put the host name in `status-right` (pane_title resolves to
# it) and in the window title, and the login shell's prompt carries user@host.
# All three are pinned here so the capture is of tmux, not of this machine.
TMUX_CONF = """\
set -g status-left "[capture] "
set -g status-right " nimbus relay "
set -g set-titles off
set -g default-command "env PS1='$ ' /bin/sh"
set -g automatic-rename off
set -g allow-rename off
set -g default-terminal "xterm-256color"
set -g mouse on
"""

HTOPRC = """\
fields=0 18 39 2 46 47 49 1
sort_key=47
hide_kernel_threads=1
hide_userland_threads=1
tree_view=0
header_margin=1
show_program_path=0
highlight_base_name=1
color_scheme=6
left_meters=AllCPUs Memory Swap
left_meter_modes=1 1 1
right_meters=Tasks LoadAverage Uptime
right_meter_modes=2 2 2
"""

CANDIDATES = "".join(f"crates/festerm-core/src/module_{index:03d}.rs\n" for index in range(1, 150))


@dataclass
class Scenario:
    """One program, the keys to drive it with, and why it is worth recording."""

    name: str
    covers: str
    argv: list[str]
    steps: list[tuple[float, bytes]]
    settle: float = 1.5
    environment: dict[str, str] = field(default_factory=dict)
    # Sent after the steps to bring the program down on its own terms, so the
    # capture ends with the alternate screen being left rather than mid-frame.
    # Same shape as `steps`, because a confirmation prompt has to be drawn
    # before its answer is sent or the answer lands in the wrong reader.
    teardown: list[tuple[float, bytes]] = field(default_factory=list)


ESCAPE = b"\x1b"
CONTROL_X = b"\x18"


def scenarios() -> list[Scenario]:
    notes = str(WORK / "NOTES.md")
    ledger = str(WORK / "ledger.txt")
    return [
        Scenario(
            name="vim",
            covers=(
                "alternate screen enter/leave, cursor save and restore, the "
                "status line's reverse video, and visual-block selection"
            ),
            # No user config and no plugins: the capture is of vim, not of
            # whoever last recorded it.
            argv=["vim", "-u", "NONE", "-U", "NONE", "-N", "-i", "NONE", notes],
            steps=[
                (1.5, b":set number ruler laststatus=2\r"),
                (0.8, b"G"),
                (0.5, b"gg"),
                (0.5, b"\x16jjjll"),  # CTRL-V, then extend the block
                (0.8, ESCAPE),
                (0.5, b"/Known\r"),
                (0.8, b""),
            ],
            teardown=[(0.5, b":q!\r")],
        ),
        Scenario(
            name="htop",
            covers="256-colour meters, wide-character bars, full-screen repaint on an interval",
            # htop draws the whole machine's process list, which on a developer
            # workstation means other people's user names and command lines. So
            # it is pinned to one process we started ourselves, and the sandbox
            # htoprc drops the identity columns entirely. `-d 30` is three
            # seconds between refreshes, which keeps this to a few frames.
            argv=["sh", "-c", "sleep 120 & htop -d 30 -p $!"],
            steps=[
                (3.0, b""),
                (0.5, b"t"),  # tree view: box drawing between processes
                (3.0, b""),
            ],
            teardown=[(0.5, b"q")],
        ),
        Scenario(
            name="tmux",
            covers=(
                "a nested terminal, its own status line, SGR mouse tracking, and "
                "pane dividers drawn with the DEC special graphics charset"
            ),
            argv=[
                "tmux",
                "-f",
                str(SANDBOX / "tmux.conf"),
                "-L",
                "festerm-capture",
                "new-session",
                "-s",
                "capture",
                "-n",
                "relay",
            ],
            steps=[
                (2.0, b'\x02"'),  # CTRL-B ": split horizontally
                (1.5, b"\x02%"),  # CTRL-B %: split the lower pane vertically
                (1.5, b"echo nimbus relay\r"),
                (1.0, b"\x02o"),  # CTRL-B o: move between panes
                (1.0, b""),
            ],
            # CTRL-B &, then the confirmation, once tmux has drawn the prompt.
            teardown=[(0.5, b"\x02&"), (1.0, b"y")],
            environment={"LANG": "C", "LC_ALL": "C"},
        ),
        Scenario(
            name="less",
            covers="reverse-video search highlight, ESC[K line clears, and scroll region use",
            argv=["less", "-M", ledger],
            steps=[
                (1.2, b" "),  # page down
                (0.6, b" "),
                (0.6, b"/BACKPRESSURE\r"),  # highlights every match
                (1.0, b"n"),  # next match
                (0.8, b"b"),  # page back up
                (0.8, b""),
            ],
            teardown=[(0.5, b"q")],
            environment={"LESSHISTFILE": "-", "LESSSECURE": "1"},
        ),
        Scenario(
            name="nano",
            covers="the inverse-video shortcut bar and a modal prompt over the text",
            # No options at all: on macOS `nano` is a symlink to UW pico, which
            # does not take GNU nano's long options and silently treats them as
            # further file names to open. The keys below are the ones both
            # programs share, so a re-recording on Linux captures GNU nano
            # doing the same things rather than falling over.
            argv=["nano", notes],
            steps=[
                (1.5, b"\x17Known\r"),  # CTRL-W: where is / search
                (1.0, b"appended by the capture harness"),
                (1.0, b"\x0f"),  # CTRL-O: write out, which opens a prompt
                (1.2, b"\x03"),  # CTRL-C: cancel the prompt
                (0.8, b""),
            ],
            # CTRL-X, then "n" once the save prompt is on screen.
            teardown=[(0.5, CONTROL_X), (1.0, b"n")],
        ),
        Scenario(
            name="fzf",
            covers="alternate screen plus incremental redraw plus a live match counter",
            argv=["sh", "-c", f"fzf --height=100% --no-mouse < {WORK / 'candidates.txt'}"],
            steps=[
                (1.5, b"core"),
                (0.8, b"_01"),
                (0.8, b"\x15"),  # CTRL-U: clear the query, redrawing the list
                (1.0, b""),
            ],
            teardown=[(0.5, ESCAPE)],
        ),
    ]


def answer_capability_probes(chunk: bytes) -> bytes:
    """Reply as a plausible xterm would.

    Programs block on these. Without answers the capture is a few bytes of
    probe and nothing else, which is exactly the failure mode that made the
    first Copilot capture attempt useless.
    """
    replies = b""
    if b"\x1b[?12$p" in chunk:
        replies += b"\x1b[?12;1$y"
    if b"\x1b[?1007$p" in chunk:
        replies += b"\x1b[?1007;2$y"
    if b"\x1b[?996n" in chunk:
        replies += b"\x1b[?997;1n"
    if b"\x1b[?u" in chunk:
        replies += b"\x1b[?0u"
    if b"\x1b[>q" in chunk:
        replies += b"\x1bP>|xterm(370)\x1b\\"
    if b"\x1b[c" in chunk:
        replies += b"\x1b[?62;4;6;22c"
    if b"\x1b[>c" in chunk:
        replies += b"\x1b[>41;370;0c"
    if b"\x1b[5n" in chunk:
        replies += b"\x1b[0n"
    if b"\x1b]10;?" in chunk:
        replies += b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"
    if b"\x1b]11;?" in chunk:
        replies += b"\x1b]11;rgb:0000/0000/0000\x1b\\"
    for match in re.finditer(rb"\x1b\]4;(\d+);\?", chunk):
        replies += b"\x1b]4;" + match.group(1) + b";rgb:8080/8080/8080\x1b\\"
    if b"\x1b[6n" in chunk:
        replies += b"\x1b[1;1R"
    return replies


def prepare_sandbox() -> None:
    if SANDBOX.exists():
        shutil.rmtree(SANDBOX)
    WORK.mkdir(parents=True)
    HOME.mkdir(parents=True)
    (WORK / "NOTES.md").write_text(NOTES)
    (WORK / "ledger.txt").write_text(LEDGER)
    (WORK / "candidates.txt").write_text(CANDIDATES)

    (SANDBOX / "tmux.conf").write_text(TMUX_CONF)

    htoprc = HOME / ".config" / "htop"
    htoprc.mkdir(parents=True)
    (htoprc / "htoprc").write_text(HTOPRC)


def child_environment(scenario: Scenario) -> dict[str, str]:
    environment = {
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        "HOME": str(HOME),
        "USER": USER,
        "LOGNAME": USER,
        "SHELL": "/bin/sh",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        "LANG": "en_US.UTF-8",
        "LC_ALL": "en_US.UTF-8",
        # Anything that would print a differing value on every run.
        "COLUMNS": str(COLUMNS),
        "LINES": str(ROWS),
        "TZ": "UTC",
        "PS1": "$ ",
    }
    environment.update(scenario.environment)
    return environment


def pump(master: int, deadline: float, sink: bytearray) -> bool:
    """Drain output until `deadline`, answering probes. False once the pty closes."""
    while time.time() < deadline:
        readable, _, _ = select.select([master], [], [], 0.05)
        if not readable:
            continue
        try:
            data = os.read(master, 65536)
        except OSError:
            return False
        if not data:
            return False
        sink.extend(data)
        if len(sink) > MAXIMUM_CAPTURE_BYTES:
            raise RuntimeError(
                f"capture exceeded {MAXIMUM_CAPTURE_BYTES} bytes; the program is "
                "probably redrawing in a loop rather than waiting for input"
            )
        reply = answer_capability_probes(data)
        if reply:
            os.write(master, reply)
    return True


def record(scenario: Scenario) -> bytes:
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))

    pid = os.fork()
    if pid == 0:
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        for descriptor in (0, 1, 2):
            os.dup2(slave, descriptor)
        if slave > 2:
            os.close(slave)
        os.close(master)
        os.chdir(WORK)
        try:
            os.execvpe(scenario.argv[0], scenario.argv, child_environment(scenario))
        finally:
            os._exit(127)

    os.close(slave)
    captured = bytearray()

    def send(keys: bytes) -> bool:
        """Writes keys, reporting whether the program is still there to read them."""
        try:
            os.write(master, keys)
        except OSError:
            return False
        return True

    try:
        alive = True
        for wait, keys in scenario.steps:
            if not pump(master, time.time() + wait, captured):
                alive = False
                break
            if keys and not send(keys):
                alive = False
                break
        if alive:
            for wait, keys in scenario.teardown:
                if not pump(master, time.time() + wait, captured):
                    alive = False
                    break
                if keys and not send(keys):
                    alive = False
                    break
        if alive:
            pump(master, time.time() + scenario.settle, captured)
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
        os.close(master)
    return bytes(captured)


def check_for_leaks(name: str, capture: bytes) -> list[str]:
    """Report anything identifying that survived into the capture.

    Matched on word boundaries, because a short user name is otherwise a
    substring of innocent text - "fes" occurs inside "festerm-capture", which
    is the sandbox path this harness creates itself.
    """
    host = socket.gethostname()
    candidates = {
        "the real user name": getpass.getuser(),
        "the real home directory": os.path.expanduser("~"),
        "the host name": host,
        "the short host name": host.split(".")[0],
    }
    found = []
    for description, value in candidates.items():
        if not value or len(value) < 3 or value in (USER, str(HOME)):
            continue
        pattern = rb"(?<![A-Za-z0-9_.-])" + re.escape(value.encode()) + rb"(?![A-Za-z0-9_-])"
        if re.search(pattern, capture):
            found.append(f"{description} ({value!r}) appears in the capture")
    return found


def cleanup_tmux() -> None:
    """Leave no server behind; a stray one changes what the next capture records."""
    if shutil.which("tmux"):
        os.system("tmux -L festerm-capture kill-server >/dev/null 2>&1")


def main() -> int:
    available = {scenario.name: scenario for scenario in scenarios()}
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("names", nargs="*", help="scenarios to record")
    parser.add_argument("--all", action="store_true", help="record every scenario")
    parser.add_argument("--list", action="store_true", help="list scenarios and exit")
    arguments = parser.parse_args()

    if arguments.list:
        width = max(len(name) for name in available)
        for scenario in available.values():
            print(f"{scenario.name:<{width}}  {scenario.covers}")
        return 0

    chosen = list(available) if arguments.all else arguments.names
    if not chosen:
        parser.error("name at least one scenario, or pass --all")
    unknown = [name for name in chosen if name not in available]
    if unknown:
        parser.error(f"unknown scenario(s): {', '.join(unknown)}")

    missing = [name for name in chosen if not shutil.which(available[name].argv[0])]
    if missing:
        print(f"not installed, skipping: {', '.join(missing)}", file=sys.stderr)
        chosen = [name for name in chosen if name not in missing]

    FIXTURES.mkdir(parents=True, exist_ok=True)
    failures = []
    for name in chosen:
        scenario = available[name]
        prepare_sandbox()
        cleanup_tmux()
        capture = record(scenario)
        cleanup_tmux()

        leaks = check_for_leaks(name, capture)
        if leaks:
            failures.extend(f"{name}: {leak}" for leak in leaks)
            print(f"{name}: NOT written", file=sys.stderr)
            for leak in leaks:
                print(f"  {leak}", file=sys.stderr)
            continue
        if len(capture) < 512:
            failures.append(f"{name}: captured only {len(capture)} bytes; it never drew")
            print(f"{name}: captured only {len(capture)} bytes; it never drew", file=sys.stderr)
            continue

        destination = FIXTURES / f"{name}.raw"
        destination.write_bytes(capture)
        print(f"{name}: {len(capture):,} bytes -> {destination.relative_to(REPOSITORY)}")

    shutil.rmtree(SANDBOX, ignore_errors=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
