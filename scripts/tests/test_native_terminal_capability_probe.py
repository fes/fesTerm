import importlib.util
import ast
import copy
import json
import os
from pathlib import Path
import select
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "native_terminal_capability_probe.py"
spec = importlib.util.spec_from_file_location("capability_probe", SCRIPT)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
FIXTURE_REPLIES = (
    b"\x1b[?6c\x1b[>0;0;0c\x1b[32;1R"
    + b"".join(f"\x1b]4;{i};rgb:1234/abcd/0000\x1b\\".encode()
               for i in (0, 7, 15, 255))
    + b"".join(f"\x1b]{i};rgb:ffff/eeee/dddd\x07".encode() for i in (10, 11, 12))
)


def fixture_main(arguments):
    """Run the actual collector with narrowly injected test-only OS races."""
    import fcntl
    import termios
    mode, baseline_name, script_name, *probe_args = arguments
    baseline = Path(baseline_name)
    spec = importlib.util.spec_from_file_location("observed_probe", script_name)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    before = termios.tcgetattr(0)
    flags = {fd: fcntl.fcntl(fd, fcntl.F_GETFL) for fd in (0, 1)}
    baseline.write_text(repr((before, flags[0])))
    mutable = os.O_NONBLOCK | os.O_APPEND | getattr(os, "O_ASYNC", 0)
    if mode == "cleanup_signal":
        # Preserve a nonempty original mask too. All state is test-child local.
        original_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR1}) | {signal.SIGUSR1}

        def original_handler(_number, _frame):
            restored = {
                "terminal": module.attributes_restored(before, termios.tcgetattr(0)),
                "descriptors": all(fcntl.fcntl(fd, fcntl.F_GETFL) & mutable == value & mutable
                                   for fd, value in flags.items()),
                "mask": signal.pthread_sigmask(signal.SIG_BLOCK, set()) == original_mask,
            }
            baseline.with_name("signal-delivery.json").write_text(json.dumps(restored))

        signal.signal(signal.SIGTERM, original_handler)
        original_restore = termios.tcsetattr
        injected = False

        def inject_at_restore(fd, when, attributes):
            nonlocal injected
            if not injected and attributes == before:
                injected = True
                mask = signal.pthread_sigmask(signal.SIG_BLOCK, set())
                baseline.with_name("cleanup-mask.json").write_text(json.dumps({
                    "all_handled_blocked": set((signal.SIGINT, signal.SIGTERM, signal.SIGHUP)) <= mask,
                }))
                os.kill(os.getpid(), signal.SIGTERM)
            return original_restore(fd, when, attributes)

        termios.tcsetattr = inject_at_restore
    elif mode == "backpressure":
        original_observe = module.observe

        def fill_after_restore(*args, **kwargs):
            result = original_observe(*args, **kwargs)
            old = fcntl.fcntl(1, fcntl.F_GETFL)
            filled, blocked = 0, False
            fcntl.fcntl(1, fcntl.F_SETFL, old | os.O_NONBLOCK)
            try:
                for size in (4096, 1):
                    while filled < 2 * 1024 * 1024:
                        try:
                            filled += os.write(1, b"F" * size)
                        except BlockingIOError:
                            blocked = size == 1
                            break
                assert blocked, "controlled PTY did not reach output backpressure"
            finally:
                fcntl.fcntl(1, fcntl.F_SETFL, old)
            baseline.with_name("backpressure.json").write_text(json.dumps({"bytes": filled, "blocked": blocked}))
            return result

        module.observe = fill_after_restore
    code = module.main(probe_args)
    # This marker proves main returned while the PTY was still undrained.
    baseline.with_name("main-returned.json").write_text(json.dumps({"code": code}))
    deadline = time.monotonic() + 5
    while not baseline.with_name("release-fixture").exists() and time.monotonic() < deadline:
        time.sleep(0.005)
    return code


class ParserTests(unittest.TestCase):
    def test_exact_ten_read_only_queries_and_fragmented_actual_values(self):
        self.assertEqual(len(probe.QUERIES), 10)
        self.assertEqual([query for _, query in probe.QUERIES], [
            b"\x1b[c", b"\x1b[>c", b"\x1b[6n",
            b"\x1b]4;0;?\x1b\\", b"\x1b]4;7;?\x1b\\",
            b"\x1b]4;15;?\x1b\\", b"\x1b]4;255;?\x1b\\",
            b"\x1b]10;?\x1b\\", b"\x1b]11;?\x1b\\", b"\x1b]12;?\x1b\\",
        ])
        self.assertEqual(probe.MAX_SECONDS, 12)
        self.assertEqual(probe.MAX_BYTES, 4096)
        replies = probe.Replies()
        for byte in FIXTURE_REPLIES:
            replies.feed(bytes([byte]))
        self.assertFalse(replies.buffer)
        self.assertTrue(all(v["count"] == 1 for v in replies.report().values()))
        self.assertEqual(replies.report()["osc4_7"]["replies"][0]["rgb"], ["1234", "abcd", "0000"])
        self.assertEqual(replies.report()["dsr6"]["replies"], [{"row": 32, "column": 1}])

    def test_duplicate_is_counted_instead_of_silently_coalesced(self):
        replies = probe.Replies()
        replies.feed(FIXTURE_REPLIES + b"\x1b[?6c")
        self.assertEqual(replies.report()["da1"]["count"], 2)

    def test_unknown_input_never_enters_records_or_exception_text(self):
        replies = probe.Replies()
        with self.assertRaises(probe.Refused) as caught:
            replies.feed(b"PRIVATE_PASSWORD_TOKEN")
        safe = json.dumps(replies.report()) + str(caught.exception)
        self.assertNotIn("PRIVATE", safe)
        self.assertNotIn("PASSWORD", safe)
        self.assertEqual(caught.exception.count, len(b"PRIVATE_PASSWORD_TOKEN"))

    def test_admission_failures_do_not_write_to_terminal_stdout(self):
        from contextlib import redirect_stdout
        import io
        with redirect_stdout(io.StringIO()) as captured:
            self.assertEqual(probe.main(["--case", "invalid case", "--output", "unused"]), 2)
            self.assertEqual(probe.main(["--case", "case", "--output", "/missing/private/result"]), 2)
            self.assertEqual(probe.main([]), 2)
        self.assertEqual(captured.getvalue(), "")

    def test_unrequested_sequences_and_invalid_public_fields_are_refused(self):
        for frame in (b"\x1b]52;c;PRIVATE_TOKEN\x07", b"\x1b]4;1;rgb:0/0/0\x07",
                      b"\x1b[0;1R", b"\x1b[?65536c", b"\x1b[0n",
                      b"\x1b[?2004;1$y", b"\x1b]10;rgb:fffff/0/0\x07"):
            with self.subTest(frame=frame[:4]), self.assertRaises(probe.Refused):
                probe.Replies().feed(frame)

    def test_frame_and_total_input_budgets_are_enforced(self):
        with self.assertRaises(probe.Refused) as frame:
            probe.Replies().feed(b"\x1b]" + b"1" * 256)
        self.assertEqual(frame.exception.reason, "frame_budget")
        with self.assertRaises(probe.Refused) as total:
            probe.Replies().feed(b"x" * 4097)
        self.assertEqual(total.exception.reason, "input_budget")

    def test_environment_is_allowlisted_with_unset_and_empty_distinct(self):
        result = probe.selected_environment({"TERM": "", "LANG": "C.UTF-8",
                                             "LC_ALL": "PRIVATE\nTOKEN", "GITHUB_TOKEN": "PRIVATE_TOKEN"})
        self.assertEqual(set(result), set(probe.ENVIRONMENT))
        self.assertEqual(result["TERM"], {"set": True, "value": ""})
        self.assertEqual(result["TERM_PROGRAM"], {"set": False, "value": None})
        self.assertEqual(result["LANG"]["value"], "C.UTF-8")
        self.assertNotIn("PRIVATE", json.dumps(result))

    @unittest.skipUnless(os.name == "posix", "POSIX terminal attributes")
    def test_restoration_ignores_only_kernel_pendin_not_raw_flags_or_control_chars(self):
        import termios
        before = [1, 2, 3, termios.ECHO | termios.ICANON, 5, 6, [b"x"]]
        after = copy.deepcopy(before)
        after[3] |= getattr(termios, "PENDIN", 0)
        self.assertTrue(probe.attributes_restored(before, after))
        for flag in (termios.ECHO, termios.ICANON):
            changed = copy.deepcopy(after)
            changed[3] ^= flag
            self.assertFalse(probe.attributes_restored(before, changed))
        after[6][0] = b"y"
        self.assertFalse(probe.attributes_restored(before, after))

    @unittest.skipUnless(os.name == "posix", "POSIX private output")
    def test_output_is_private_exclusive_and_refuses_symlink_or_public_parent(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            destination = parent / "result.json"
            fd = probe.exclusive_output(destination)
            os.close(fd)
            self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o600)
            with self.assertRaises(FileExistsError):
                probe.exclusive_output(destination)
            alias = parent / "alias"
            alias.symlink_to(destination)
            with self.assertRaises(FileExistsError):
                probe.exclusive_output(alias)
            parent.chmod(0o755)
            with self.assertRaises(probe.Refused):
                probe.exclusive_output(parent / "public.json")


@unittest.skipUnless(os.name == "posix", "controlled POSIX PTY fixture")
class PtyTests(unittest.TestCase):
    # These responses are injected only by this unit fixture. They qualify
    # collector/restoration behavior, never a native terminal or Copilot.
    def run_probe(self, mode):
        import fcntl
        import struct
        import termios
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 100, 0, 0))
        before = termios.tcgetattr(slave)
        flags = fcntl.fcntl(slave, fcntl.F_GETFL)
        process = None
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            baseline = Path(directory) / "tty-before.txt"
            if mode.startswith("pending"):
                os.write(master, b"PRIVATE_PENDING" + (b"" if mode == "pending_partial" else b"\n"))

            def setup():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)
                os.tcsetpgrp(0, os.getpgrp())
                # The fixture child is its own TTY session leader. Closing
                # our master after completion must not turn its exit into
                # SIGHUP; observe() installs its own handler while active.
                signal.signal(signal.SIGHUP, signal.SIG_IGN)

            try:
                # Snapshot after interpreter startup as well as CTTY setup.
                # Python may add platform descriptor flags during startup.
                wrapper = ("import sys,runpy;module=runpy.run_path(sys.argv[1]);"
                           "raise SystemExit(module['fixture_main'](sys.argv[2:]))")
                process = subprocess.Popen([sys.executable, "-c", wrapper, __file__, mode, str(baseline), str(SCRIPT), "--case", "unit-fixture",
                                            "--output", str(output)], stdin=slave, stdout=slave,
                                           stderr=subprocess.PIPE, preexec_fn=setup,
                                           env={"PATH": os.defpath, "TERM": "fixture", "LANG": "C",
                                                "GITHUB_TOKEN": "PRIVATE_TOKEN"})
                received = bytearray()
                if not mode.startswith("pending"):
                    queries = b"".join(query for _, query in probe.QUERIES)
                    deadline = time.monotonic() + 5
                    while len(received) < len(queries) and time.monotonic() < deadline:
                        if select.select([master], [], [], 0.1)[0]:
                            received.extend(os.read(master, 4096))
                    self.assertEqual(bytes(received), queries)
                    if mode == "unknown":
                        os.write(master, b"PRIVATE_PASSWORD_TOKEN")
                    elif mode in ("signal", "signal_hup", "signal_int"):
                        process.send_signal({"signal": signal.SIGTERM, "signal_hup": signal.SIGHUP,
                                             "signal_int": signal.SIGINT}[mode])
                    elif mode != "timeout":
                        if mode == "resize":
                            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 33, 101, 0, 0))
                        replies = FIXTURE_REPLIES + (b"\x1b[?6c" if mode == "duplicate" else b"")
                        os.write(master, replies)
                # macOS can hold a controlling-PTY session leader in exit
                # until its master closes. Check the restored slave before
                # closing our fixture endpoints, then reap the owned child.
                deadline = time.monotonic() + 14
                while True:
                    try:
                        record_text = output.read_text()
                        record = json.loads(record_text)
                        returned = json.loads(baseline.with_name("main-returned.json").read_text())
                        break
                    except (FileNotFoundError, json.JSONDecodeError):
                        if time.monotonic() >= deadline:
                            self.fail("probe did not finish its bounded record")
                        time.sleep(0.02)
                self.assertNotIn("PRIVATE", record_text)
                saved_attributes, saved_flags = ast.literal_eval(baseline.read_text())
                self.assertTrue(probe.attributes_restored(saved_attributes, termios.tcgetattr(slave)))
                mutable = os.O_NONBLOCK | os.O_APPEND | getattr(os, "O_ASYNC", 0)
                self.assertEqual(fcntl.fcntl(slave, fcntl.F_GETFL) & mutable, saved_flags & mutable)
                if not mode.startswith("pending"):
                    self.assertTrue(record["descriptor_io_flags_restored"])
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
                if mode == "cleanup_signal":
                    self.assertTrue(json.loads(baseline.with_name("cleanup-mask.json").read_text())["all_handled_blocked"])
                    self.assertTrue(all(json.loads(baseline.with_name("signal-delivery.json").read_text()).values()))
                if mode == "backpressure":
                    self.assertTrue(json.loads(baseline.with_name("backpressure.json").read_text())["blocked"])
                    self.assertEqual(returned["code"], 0)
                if mode.startswith("pending"):
                    if mode == "pending_partial":
                        os.write(master, b"\n")
                    self.assertTrue(select.select([slave], [], [], 0)[0])
                    self.assertEqual(os.read(slave, 128), b"PRIVATE_PENDING\n")
                baseline.with_name("release-fixture").touch()
                os.close(master)
                os.close(slave)
                master = slave = None
                process.wait(timeout=2)
                stderr = process.stderr.read()
                self.assertNotIn(b"PRIVATE", stderr)
                return process.returncode, record
            finally:
                if slave is not None:
                    try:
                        termios.tcsetattr(slave, termios.TCSANOW, before)
                    except termios.error:
                        pass  # The fixture session leader may have exited.
                    finally:
                        os.close(slave)
                if master is not None:
                    os.close(master)
                if process is not None:
                    if process.poll() is None:
                        process.kill()  # Only this test-owned fixture child.
                        process.wait(timeout=2)
                    process.stderr.close()

    def test_success_restores_actual_pty_and_records_public_responses(self):
        code, record = self.run_probe("success")
        self.assertEqual(code, 0)
        self.assertEqual(record["status"], "complete")
        self.assertEqual(record["grid_before"], {"columns": 100, "rows": 32})
        self.assertTrue(record["terminal_restored"] and record["grid_stable"])

    def test_unknown_input_aborts_without_export_and_restores_pty(self):
        code, record = self.run_probe("unknown")
        self.assertEqual(code, 1)
        self.assertEqual(record["reason"], "unexpected_input")
        self.assertEqual(record["unknown_bytes"], len(b"PRIVATE_PASSWORD_TOKEN"))

    def test_signal_restores_actual_pty(self):
        for mode in ("signal", "signal_hup", "signal_int"):
            with self.subTest(mode=mode):
                code, record = self.run_probe(mode)
                self.assertEqual(code, 1)
                self.assertEqual(record["reason"], "interrupted")

    def test_signal_at_cleanup_is_deferred_until_state_and_original_mask_restore(self):
        code, record = self.run_probe("cleanup_signal")
        self.assertEqual(code, 1)
        self.assertEqual(record["reason"], "interrupted")
        self.assertTrue(record["terminal_restored"])

    def test_backpressured_stdout_after_restore_cannot_block_return(self):
        code, record = self.run_probe("backpressure")
        self.assertEqual(code, 0)
        self.assertEqual(record["status"], "complete")

    def test_timeout_is_incomplete_not_unsupported_and_restores_pty(self):
        code, record = self.run_probe("timeout")
        self.assertEqual(code, 1)
        self.assertEqual(record["status"], "incomplete")
        self.assertGreaterEqual(record["elapsed_ms"], 11900)
        self.assertLess(record["elapsed_ms"], 13000)
        self.assertNotIn("unsupported", json.dumps(record))

    def test_pending_input_is_neither_read_nor_flushed(self):
        for mode in ("pending", "pending_partial"):
            with self.subTest(mode=mode):
                code, record = self.run_probe(mode)
                self.assertEqual(code, 1)
                self.assertEqual(record["reason"], "pending_input")

    def test_duplicates_and_grid_changes_cannot_pass(self):
        code, record = self.run_probe("duplicate")
        self.assertEqual(code, 1)
        self.assertEqual(record["observations"]["da1"]["count"], 2)
        code, record = self.run_probe("resize")
        self.assertEqual(code, 1)
        self.assertFalse(record["grid_stable"])


if __name__ == "__main__":
    unittest.main()
