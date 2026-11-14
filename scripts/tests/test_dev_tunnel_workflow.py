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

    @unittest.skipIf(os.name == "nt", "service process execution is explicitly blocked on Windows")
    def test_child_diagnostics_are_redacted_and_blocked_is_not_pass(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner, "run_bounded", side_effect=[(0, b""), (101, b"SECRET_DO_NOT_PRINT FESTERM_DEV_TUNNEL_LAB_BLOCKED")]):
            status, output = self.invoke("--run")
        self.assertEqual(status, 2)
        self.assertIn("status=blocked", output)
        self.assertNotIn("SECRET_DO_NOT_PRINT", output)
        self.assertNotIn("status=pass", output)

    @unittest.skipIf(os.name == "nt", "service process execution is explicitly blocked on Windows")
    def test_passing_exit_without_exact_test_and_marker_is_not_evidence(self):
        for output in (b"0 passed; 0 failed", runner.MARKER + b"0 passed; 0 failed", b"1 passed; 0 failed"):
            with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                    runner, "run_bounded", side_effect=[(0, b""), (0, output)]):
                status, _ = self.invoke("--run")
            self.assertEqual(status, 1)

    @unittest.skipIf(os.name == "nt", "service process execution is explicitly blocked on Windows")
    def test_success_uses_fixed_cargo_selector_and_keeps_gui_unrun(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner, "run_bounded", side_effect=[(0, b""), (0, runner.MARKER + b"\n1 passed; 0 failed")]) as run:
            status, output = self.invoke("--run")
        self.assertEqual(status, 0)
        self.assertIn("scope=real-service-host-client", output)
        self.assertIn("gui_acceptance=unrun", output)
        self.assertEqual(run.call_args.args[0][-5:], [runner.TEST, "--", "--exact", "--ignored", "--nocapture"])
        self.assertNotIn("password", run.call_args.kwargs["env"])

    @unittest.skipIf(os.name == "nt", "POSIX process-group implementation; Windows deliberately blocked")
    def test_real_process_is_bounded_and_private_output_does_not_block_pipe(self):
        code, output = runner.run_bounded([sys.executable, "-c", "print('x' * 200000)"], env=dict(os.environ), timeout=5)
        self.assertEqual(code, 0)
        self.assertLessEqual(len(output), 65536)
        code, _ = runner.run_bounded([sys.executable, "-c", "import time; time.sleep(30)"], env=dict(os.environ), timeout=0.05)
        self.assertIsNone(code)

    @unittest.skipIf(os.name == "nt", "POSIX descendant process-group ownership")
    def test_parent_exit_and_timeout_kill_ignoring_pipe_holding_descendants(self):
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

    @unittest.skipIf(os.name == "nt", "POSIX process-group execution")
    def test_continuous_output_cannot_starve_the_deadline(self):
        started = time.monotonic()
        code, output = runner.run_bounded(
            [sys.executable, "-c", "import os\nwhile True: os.write(1,b'x'*4096)"],
            env=dict(os.environ), timeout=0.1)
        self.assertIsNone(code)
        self.assertLess(time.monotonic() - started, 2)
        self.assertLessEqual(len(output), 65536)

    def test_windows_main_is_blocked_before_build_or_service_process(self):
        with patch.object(runner, "fixture_record", return_value=self.record), patch.object(
                runner.os, "name", "nt"), patch.object(runner, "run_bounded") as run:
            status, output = self.invoke("--run")
        self.assertEqual(status, 2)
        self.assertIn("windows-owned-job-capability-required", output)
        run.assert_not_called()

    def test_windows_without_owned_job_capability_never_starts_a_process(self):
        with patch.object(runner.os, "name", "nt"), patch.object(runner.subprocess, "Popen") as spawn:
            with self.assertRaises(runner.Blocked):
                runner.run_bounded(["unreachable"], env={}, timeout=1)
        spawn.assert_not_called()


if __name__ == "__main__":
    unittest.main()
