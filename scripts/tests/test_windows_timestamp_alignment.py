"""Mock Cargo only: no Windows, Rust build, memory fixture or PKI execution."""

import contextlib
import importlib.util
import io
from pathlib import Path
import re
import subprocess
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "timestamp_alignment", ROOT / "scripts/run_windows_timestamp_alignment_test.py"
)
ALIGNMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ALIGNMENT)


class TimestampAlignmentContracts(unittest.TestCase):
    def invoke(self, listing=None, list_exit=0, execution=None, execute_exit=0):
        if listing is None:
            listing = f"{ALIGNMENT.TEST}: test\n\n1 test, 0 benchmarks\n"
        if execution is None:
            execution = (
                f"running 1 test\ntest {ALIGNMENT.TEST} ... ok\n\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;\n"
            )
        with mock.patch.object(ALIGNMENT.sys, "platform", "win32"), \
                mock.patch.object(ALIGNMENT.subprocess, "run", side_effect=[
                    subprocess.CompletedProcess([], list_exit, listing, "listing diagnostic\n"),
                    subprocess.CompletedProcess([], execute_exit, execution, "execution diagnostic\n"),
                ]) as cargo, contextlib.redirect_stdout(io.StringIO()) as stdout, \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            code = ALIGNMENT.main()
        return code, cargo.call_args_list, stdout.getvalue(), stderr.getvalue()

    def test_exact_unit_is_listed_then_executed_once_without_ignored(self):
        expected = (
            "cargo", "test", "--locked", "-p", "festerm-windows-security",
            "--features", "host-service", "--lib",
            "host_publisher::native::tests::"
            "windows_timestamp_filetime_copies_exact_bits_from_misaligned_sdk_storage",
            "--", "--exact",
        )
        self.assertEqual(ALIGNMENT.COMMAND, expected)
        code, calls, stdout, stderr = self.invoke()
        self.assertEqual(code, 0)
        self.assertEqual(calls, [
            mock.call([*expected, "--list"], cwd=ROOT, capture_output=True, text=True, check=False),
            mock.call(expected, cwd=ROOT, capture_output=True, text=True, check=False),
        ])
        self.assertIn("timestamp_alignment_unit_passed=true; native_pki_qualification=false", stdout)
        self.assertIn("listing diagnostic", stderr)
        self.assertIn("execution diagnostic", stderr)
        self.assertNotIn("--ignored", expected)

    def test_empty_duplicate_foreign_and_incomplete_listings_never_execute(self):
        entry = f"{ALIGNMENT.TEST}: test\n"
        for listing in ("", "0 tests, 0 benchmarks\n", entry,
                        entry * 2 + "\n2 tests, 0 benchmarks\n",
                        entry + "foreign: test\n\n2 tests, 0 benchmarks\n",
                        "foreign: test\n\n1 test, 0 benchmarks\n"):
            with self.subTest(listing=listing):
                code, calls, _, _ = self.invoke(listing=listing)
                self.assertEqual(code, 1)
                self.assertEqual(len(calls), 1)

    def test_listing_error_preserves_real_exit_even_with_matching_text(self):
        for exit_code in (37, 101, 3221226505):
            with self.subTest(exit_code=exit_code):
                code, calls, _, _ = self.invoke(list_exit=exit_code)
                self.assertEqual(code, exit_code)
                self.assertEqual(len(calls), 1)

    def test_execution_failure_is_preserved_without_retry(self):
        for exit_code in (37, 101, 3221226505):
            with self.subTest(exit_code=exit_code):
                code, calls, _, _ = self.invoke(execute_exit=exit_code)
                self.assertEqual(code, exit_code)
                self.assertEqual(len(calls), 2)

    def test_zero_skipped_or_wrong_success_output_is_not_execution_proof(self):
        for execution in ("", "test result: ok. 0 passed; 0 failed; 1 ignored;\n",
                          "test foreign ... ok\n"
                          "test result: ok. 1 passed; 0 failed; 0 ignored;\n"):
            with self.subTest(execution=execution):
                code, calls, _, _ = self.invoke(execution=execution)
                self.assertEqual(code, 1)
                self.assertEqual(len(calls), 2)

    def test_non_windows_refuses_before_cargo(self):
        with mock.patch.object(ALIGNMENT.sys, "platform", "darwin"), \
                mock.patch.object(ALIGNMENT.subprocess, "run") as cargo, \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(ALIGNMENT.main(), 2)
        cargo.assert_not_called()

    def test_cargo_launch_error_is_not_swallowed_or_retried(self):
        with mock.patch.object(ALIGNMENT.sys, "platform", "win32"), \
                mock.patch.object(ALIGNMENT.subprocess, "run",
                                  side_effect=OSError("owned mock")) as cargo, \
                self.assertRaises(OSError):
            ALIGNMENT.main()
        self.assertEqual(cargo.call_count, 1)

    def test_default_windows_quality_step_precedes_heavy_workspace_work(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        workspace = re.search(
            r"(?ms)^  workspace:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)",
            workflow,
        ).group(1)
        step = re.search(
            r"(?ms)^      - name: Require exact Windows timestamp alignment memory regression\n"
            r"(.*?)(?=^      - |\Z)", workspace,
        )
        self.assertIsNotNone(step)
        self.assertIn("if: matrix.os == 'windows-latest'", step.group(1))
        self.assertIn("run: python scripts/run_windows_timestamp_alignment_test.py", step.group(1))
        self.assertNotIn("inputs.", step.group(1))
        self.assertNotIn("continue-on-error", step.group(1))
        self.assertLess(step.start(), workspace.index("run: cargo clippy --workspace"))
        self.assertLess(workspace.index("python-version: '3.12'"), step.start())
        self.assertNotIn("run_windows_timestamp_alignment_test.py",
                         workflow.split("\n  windows-native-pki:\n", 1)[1].split(
                             "\n  windows-sampler-diagnostics:\n", 1
                         )[0])


if __name__ == "__main__":
    unittest.main()
