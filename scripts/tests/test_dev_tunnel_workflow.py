"""Contracts for the real-service campaign, without cloud credentials or networking."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("dev_tunnel_workflow", Path(__file__).parents[1] / "check_dev_tunnel_workflow.py")
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.configuration = self.root / "config.toml"
        self.configuration.touch(mode=0o600)
        self.runtime = self.root / "sessiond"
        self.runtime.mkdir(mode=0o700)
        self.record = {
            "schema_version": 1, "account": "fes-festerm1",
            "configuration": str(self.configuration), "runtime_root": str(self.runtime),
            "recipe_name": "Lab host", "shared_profile_name": "Lab shared",
            "private_profile_name": "Lab private",
            "generic_logical": "11111111-1111-4111-8111-111111111111",
            "denied_logical": "22222222-2222-4222-8222-222222222222",
            "creation_allowed": False, "allow_shell_input": True, "shell": "posix",
        }
        self.path = self.root / "operator.json"
        self.path.touch(mode=0o600)

    def write_record(self):
        self.path.write_text(json.dumps(self.record))

    def invoke(self, *arguments):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            status = runner.main(list(arguments))
        return status, output.getvalue()

    def test_without_opt_in_never_reads_fixture_or_runs_process(self):
        with patch.object(runner, "fixture_record") as record, patch.object(runner, "run_bounded") as run:
            status, output = self.invoke()
        self.assertEqual(status, 2)
        self.assertIn("status=blocked", output)
        record.assert_not_called()
        run.assert_not_called()

    def test_prior_evidence_is_never_overwritten(self):
        result = self.root / "evidence.txt"
        result.write_text("original evidence")
        status, output = self.invoke("--run", "--result", str(result))
        self.assertEqual(status, 2)
        self.assertIn("result-already-exists", output)
        self.assertEqual(result.read_text(), "original evidence")

    def test_operator_metadata_rejects_credentials_wrong_account_and_missing_input_consent(self):
        for change in ({"password": "DO_NOT_PRINT"}, {"account": "someone-else"}, {"allow_shell_input": False}, {"shell": "arbitrary-command"}):
            with self.subTest(change=list(change)):
                saved = dict(self.record)
                self.record.update(change)
                self.write_record()
                with self.assertRaises(runner.Blocked):
                    runner.fixture_record(self.path)
                self.record = saved

    def test_approved_metadata_is_exact_and_sources_are_outside_checkout(self):
        self.write_record()
        self.assertEqual(runner.fixture_record(self.path), self.record)
        self.record["configuration"] = str(runner.ROOT / "Cargo.toml")
        self.write_record()
        with self.assertRaises(runner.Blocked):
            runner.fixture_record(self.path)

    @unittest.skipIf(os.name == "nt", "Unix mode contract")
    def test_shared_readable_record_is_refused_without_permission_repair(self):
        self.write_record()
        self.path.chmod(0o644)
        with self.assertRaises(runner.Blocked):
            runner.fixture_record(self.path)
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o644)

    def test_child_diagnostics_are_redacted_and_blocked_is_not_pass(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner, "run_bounded", side_effect=[(0, b""), (101, b"SECRET_DO_NOT_PRINT FESTERM_DEV_TUNNEL_LAB_BLOCKED")]):
            status, output = self.invoke("--run")
        self.assertEqual(status, 2)
        self.assertIn("status=blocked", output)
        self.assertNotIn("SECRET_DO_NOT_PRINT", output)
        self.assertNotIn("status=pass", output)

    def test_passing_exit_without_exact_test_and_marker_is_not_evidence(self):
        for output in (b"0 passed; 0 failed", runner.MARKER + b"0 passed; 0 failed", b"1 passed; 0 failed"):
            with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                    runner, "run_bounded", side_effect=[(0, b""), (0, output)]):
                status, _ = self.invoke("--run")
            self.assertEqual(status, 1)

    def test_success_uses_fixed_cargo_selector_and_keeps_gui_unrun(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner, "run_bounded", side_effect=[(0, b""), (0, runner.MARKER + b"\n1 passed; 0 failed")]) as run:
            status, output = self.invoke("--run")
        self.assertEqual(status, 0)
        self.assertIn("scope=real-service-host-client", output)
        self.assertIn("gui_acceptance=unrun", output)
        self.assertEqual(run.call_args.args[0][-5:], [runner.TEST, "--", "--exact", "--ignored", "--nocapture"])
        self.assertNotIn("password", run.call_args.kwargs["env"])

    def test_real_process_is_bounded_and_private_output_does_not_block_pipe(self):
        code, output = runner.run_bounded([sys.executable, "-c", "print('x' * 200000)"], env=dict(os.environ), timeout=5)
        self.assertEqual(code, 0)
        self.assertLessEqual(len(output), 65536)
        code, _ = runner.run_bounded([sys.executable, "-c", "import time; time.sleep(30)"], env=dict(os.environ), timeout=0.05)
        self.assertIsNone(code)

    def test_owned_tree_cleans_descendants_after_parent_exit_and_timeout(self):
        for parent_exits in (True, False):
            marker = self.root / f"escaped-{parent_exits}.txt"
            child = ("import signal,time,pathlib; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                     f"time.sleep(1.5); pathlib.Path({str(marker)!r}).write_text('escaped')")
            parent = ("import subprocess,sys,time; "
                      f"subprocess.Popen([sys.executable,'-c',{child!r}]); "
                      + ("time.sleep(0.15)" if parent_exits else "time.sleep(30)"))
            started = time.monotonic()
            code, _ = runner.run_bounded([sys.executable, "-c", parent], env=dict(os.environ), timeout=0.3)
            self.assertEqual(code, 0 if parent_exits else None)
            self.assertLess(time.monotonic() - started, 2)
            time.sleep(1.6)
            self.assertFalse(marker.exists(), "owned descendant escaped parent exit/timeout cleanup")

    def test_continuous_output_cannot_starve_the_deadline(self):
        started = time.monotonic()
        code, output = runner.run_bounded(
            [sys.executable, "-c", "import os\nwhile True: os.write(1,b'x'*4096)"],
            env=dict(os.environ), timeout=0.1)
        self.assertIsNone(code)
        self.assertLess(time.monotonic() - started, 2)
        self.assertLessEqual(len(output), 65536)

    def test_windows_main_dispatches_to_owned_job_backend(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner.os, "name", "nt"), patch.object(runner, "run_bounded_windows",
                    side_effect=[(0, b""), (0, runner.MARKER + b"\n1 passed; 0 failed")]) as run:
            status, output = self.invoke("--run")
        self.assertEqual(status, 0)
        self.assertIn("status=pass", output)
        self.assertEqual(run.call_count, 2)

    def test_windows_capability_refusal_is_redacted_and_never_pass(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner, "run_bounded", side_effect=runner.Blocked("windows-owned-job-assignment-refused")):
            status, output = self.invoke("--run")
        self.assertEqual(status, 2)
        self.assertIn("reason=windows-owned-job-assignment-refused", output)
        self.assertNotIn("status=pass", output)


class FakeWindowsNative:
    def __init__(self, *, fail_assignment=False, fail_resume=False, running=False):
        self.events = []
        self.fail_assignment, self.fail_resume, self.running = fail_assignment, fail_resume, running

    def create_job(self):
        self.events.append("create-kill-on-close-job")
        return "job"

    def create_suspended(self, command, env):
        self.events.append("create-suspended-with-restricted-handles")
        return "process", "thread", "read"

    def assign(self, job, process):
        self.events.append("assign-and-verify-job")
        if self.fail_assignment:
            raise runner.Blocked("windows-owned-job-assignment-refused")

    def resume(self, thread):
        self.events.append("resume-owned-thread")
        if self.fail_resume:
            raise runner.Blocked("windows-owned-child-resume-failed")

    def poll(self, process):
        return None if self.running else 0

    def read_available(self, read):
        self.events.append("bounded-drain")
        return b"x" * 65536

    def terminate_job(self, job):
        self.events.append("terminate-exact-job")
        self.running = False

    def active_processes(self, job):
        self.events.append("verify-job-empty")
        return 0

    def terminate_suspended(self, process):
        self.events.append("terminate-exact-suspended-process")

    def close(self, handle):
        if handle is not None:
            self.events.append(f"close-{handle}")


class WindowsJobOwnershipTests(unittest.TestCase):
    def test_child_is_owned_before_resume_and_parent_exit_still_cleans_job(self):
        api = FakeWindowsNative()
        code, output = runner.run_bounded_windows(["fixed-child"], env={}, timeout=1, api=api)
        self.assertEqual(code, 0)
        self.assertLessEqual(len(output), 65536)
        self.assertLess(api.events.index("create-suspended-with-restricted-handles"), api.events.index("assign-and-verify-job"))
        self.assertLess(api.events.index("assign-and-verify-job"), api.events.index("resume-owned-thread"))
        self.assertLess(api.events.index("terminate-exact-job"), api.events.index("verify-job-empty"))
        self.assertLess(api.events.index("verify-job-empty"), api.events.index("close-job"))
        for handle in ("thread", "process", "read", "job"):
            self.assertEqual(api.events.count(f"close-{handle}"), 1)

    def test_assignment_failure_never_resumes_and_kills_only_suspended_child_handle(self):
        api = FakeWindowsNative(fail_assignment=True)
        with self.assertRaises(runner.Blocked):
            runner.run_bounded_windows(["fixed-child"], env={}, timeout=1, api=api)
        self.assertNotIn("resume-owned-thread", api.events)
        self.assertIn("terminate-exact-suspended-process", api.events)
        self.assertNotIn("terminate-exact-job", api.events)
        for handle in ("thread", "process", "read", "job"):
            self.assertEqual(api.events.count(f"close-{handle}"), 1)

    def test_resume_failure_closes_owned_job_and_handles(self):
        api = FakeWindowsNative(fail_resume=True)
        with self.assertRaises(runner.Blocked):
            runner.run_bounded_windows(["fixed-child"], env={}, timeout=1, api=api)
        self.assertNotIn("terminate-exact-suspended-process", api.events)
        self.assertIn("close-job", api.events)
        for handle in ("thread", "process", "read", "job"):
            self.assertEqual(api.events.count(f"close-{handle}"), 1)

    def test_timeout_returns_no_success_and_terminates_job_before_releasing_ownership(self):
        api = FakeWindowsNative(running=True)
        code, _ = runner.run_bounded_windows(["fixed-child"], env={}, timeout=0, api=api)
        self.assertIsNone(code)
        self.assertIn("terminate-exact-job", api.events)
        self.assertLess(api.events.index("terminate-exact-job"), api.events.index("close-job"))


    def test_post_create_stdio_failure_retains_and_cleans_suspended_child_before_transfer(self):
        for failed_release in ("stdout", "stdin"):
            with self.subTest(failed_release=failed_release):
                events = []
                failed = False
                api = runner._WindowsNative.__new__(runner._WindowsNative)
                api.winapi = SimpleNamespace(
                    CreatePipe=lambda *_: (1, 2),
                    CreateProcess=lambda *_: (11, 12, 123, 456),
                )

                def close_handle(handle):
                    nonlocal failed
                    if handle is None:
                        return
                    if failed_release == "stdout" and handle == 2 and not failed:
                        failed = True
                        events.append("stdout-release-failed")
                        raise OSError("private child diagnostic must not escape")
                    events.append(f"close-{handle}")

                def close_fd(fd):
                    nonlocal failed
                    if failed_release == "stdin" and not failed:
                        failed = True
                        events.append("stdin-release-failed")
                        raise OSError("private child diagnostic must not escape")
                    events.append(f"close-fd-{fd}")

                api.close = close_handle
                api.terminate_suspended = lambda process: events.append(f"terminate-and-wait-{process}")
                with patch.dict(sys.modules, {"msvcrt": SimpleNamespace(get_osfhandle=lambda _: 4)}), \
                        patch.object(runner.shutil, "which", return_value="fixed.exe"), \
                        patch.object(runner.os, "set_handle_inheritable", create=True), \
                        patch.object(runner.os, "open", return_value=3), \
                        patch.object(runner.os, "close", side_effect=close_fd), \
                        patch.object(runner.subprocess, "STARTUPINFO", SimpleNamespace, create=True), \
                        patch.object(runner.subprocess, "STARTF_USESTDHANDLES", 0x100, create=True), \
                        patch.object(runner.subprocess, "CREATE_NO_WINDOW", 0x08000000, create=True):
                    with self.assertRaises(OSError):
                        api.create_suspended(["fixed.exe"], {})
                self.assertIn("terminate-and-wait-11", events)
                for handle in (1, 2, 11, 12):
                    self.assertEqual(events.count(f"close-{handle}"), 1)
                self.assertEqual(events.count("close-fd-3"), 1)
                self.assertLess(events.index("terminate-and-wait-11"), events.index("close-11"))
                self.assertLess(events.index("terminate-and-wait-11"), events.index("close-12"))

    @unittest.skipUnless(os.name == "nt", "actual Win32 handle/ABI qualification")
    def test_native_windows_job_layout_and_noninherited_handle(self):
        import ctypes
        api = runner._WindowsNative()
        expected = 144 if ctypes.sizeof(ctypes.c_void_p) == 8 else 112
        self.assertEqual(ctypes.sizeof(api.ExtendedLimit), expected)
        self.assertEqual(ctypes.sizeof(api.Accounting), 48)
        job = api.create_job()
        try:
            self.assertFalse(os.get_handle_inheritable(job))
            self.assertEqual(api.active_processes(job), 0)
        finally:
            api.close(job)


if __name__ == "__main__":
    unittest.main()
