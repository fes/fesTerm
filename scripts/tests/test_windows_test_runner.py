"""Non-GPU debug-event, exit propagation and exact owned-tree regressions."""

import contextlib
import ctypes
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import unittest
from unittest.mock import patch
import uuid
import tomllib


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import windows_test_runner as runner
from windows_native_debug import NativeDebugger


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.directory = ROOT / "target" / "native-exit-tests" / uuid.uuid4().hex
        self.directory.mkdir(parents=True)
        self.executable = self.directory / "owned-fixture.exe"
        self.executable.write_bytes(b"not executed")

    def tearDown(self):
        shutil.rmtree(self.directory)

    def test_unavailable_tool_is_explicit_nonzero_and_never_launches_target(self):
        with patch.object(runner, "NativeDebugger", side_effect=OSError("private error")), \
             patch.object(runner, "run_debugger") as execute, \
             contextlib.redirect_stderr(io.StringIO()):
            code = runner.main([
                "--output-root", str(self.directory), "--", str(self.executable), "not-run",
            ])
        self.assertEqual(code, 125)
        execute.assert_not_called()
        text = next(self.directory.glob("*.json")).read_text()
        receipt = json.loads(text)
        self.assertEqual(receipt["outcome"], "diagnostic_tool_unavailable")
        self.assertEqual(receipt["cleanup"], "not_started")
        self.assertNotIn("exit_code_unsigned", receipt)
        self.assertNotIn("private error", text)
        self.assertNotIn(str(Path.home()), text)

    def test_diagnostic_run_failure_cannot_look_like_a_pass(self):
        with patch.object(runner, "NativeDebugger"), \
             patch.object(runner, "run_debugger", side_effect=OSError("private exception")), \
             contextlib.redirect_stderr(io.StringIO()):
            code = runner.main(["--output-root", str(self.directory), "--", str(self.executable)])
        receipt = json.loads(next(self.directory.glob("*.json")).read_text())
        self.assertEqual(code, 125)
        self.assertEqual(receipt["outcome"], "runner_error")
        self.assertEqual(receipt["capture_status"], "failed")
        self.assertNotIn("private exception", json.dumps(receipt))

    def test_invalid_arguments_never_launch_a_target(self):
        for extra in ([], ["--timeout-seconds", "0", "--", sys.executable],
                      ["--", str(self.directory / "missing.exe")], ["--", sys.executable]):
            with self.subTest(extra=extra), patch.object(runner, "NativeDebugger") as debugger, \
                 contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                runner.main(["--output-root", str(self.directory), *extra])
            self.assertEqual(error.exception.code, 2)
            debugger.assert_not_called()
        self.assertFalse(list(self.directory.glob("*.json")))

    def test_parallel_context_discards_arbitrary_output_and_bounds_names(self):
        context = runner.TestContext()
        context.observe("secret user payload")
        context.observe("test fixture::first ... ok")
        context.observe("test fixture::second ... FAILED")
        for number in range(8200):
            context.observe(f"test fixture::t{number} ... ok")
        record = context.record()
        self.assertTrue(record["truncated"])
        self.assertEqual(len(record["reported_tests"]), 8192)
        self.assertEqual(record["reported_tests"][1]["result"], "FAILED")
        self.assertNotIn("secret", json.dumps(record))
        self.assertEqual(record["active_test_attribution"], "unavailable_in_stable_parallel_libtest")

    def test_oversized_evidence_keeps_failure_and_faulting_stack_with_explicit_truncation(self):
        record = {
            "exit_code_unsigned": 2173,
            "events": [{"kind": "termination", "stacks": [
                {"tid": tid, "frames": [{"pc": hex(frame), "symbol": "x" * 512}
                                       for frame in range(24)]} for tid in range(64)
            ]} for _ in range(8)],
            "test_context": {"reported_tests": [
                {"name": "test_" + str(i) + "x" * 200, "result": "ok"} for i in range(8192)
            ]},
        }
        text = runner.encode_receipt(record)
        self.assertLessEqual(len(text.encode()), 4 * 1024 * 1024)
        reduced = json.loads(text)
        self.assertEqual(reduced["exit_code_unsigned"], 2173)
        self.assertTrue(reduced["receipt_truncated"])
        self.assertTrue(reduced["test_context"]["truncated"])
        self.assertEqual(reduced["events"][0]["stacks"][0]["tid"], 0)
        self.assertTrue(reduced["test_context"]["reported_tests"][-1]["name"].startswith("test_8191"))


@unittest.skipUnless(sys.platform == "win32", "Windows debug event API")
class NativeRunnerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = ROOT / "target" / "native-exit-tests" / uuid.uuid4().hex
        cls.directory.mkdir(parents=True)
        compiler = Path(os.environ["WINDIR"]) / "Microsoft.NET" / "Framework64" / "v4.0.30319" / "csc.exe"
        if not compiler.is_file():
            shutil.rmtree(cls.directory)
            raise AssertionError("Installed Windows Framework compiler required by calibration tests")
        cls.executable = cls.directory / "exit-probe.exe"
        build = subprocess.run(
            [str(compiler), "/nologo", "/platform:x64", f"/out:{cls.executable}",
             str(ROOT / "scripts" / "diagnose-windows-exit-probe.cs")],
            capture_output=True, text=True, timeout=30, check=False,
        )
        if build.returncode:
            shutil.rmtree(cls.directory)
            raise AssertionError(build.stdout + build.stderr)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.directory)

    def execute(self, mode, timeout=20, environment=None):
        output = self.directory / uuid.uuid4().hex
        result = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "windows_test_runner.py"),
             "--output-root", str(output), "--timeout-seconds", str(timeout),
             "--", str(self.executable), mode],
            cwd=ROOT, env=environment, capture_output=True, text=True, timeout=40, check=False,
        )
        receipts = list(output.glob("*.json"))
        self.assertEqual(len(receipts), 1, result.stdout + result.stderr)
        text = receipts[0].read_text()
        receipt = json.loads(text)
        self.assertEqual(receipt["capture_status"], "captured", result.stdout + result.stderr + text)
        self.assertNotIn("private-payload", text)
        self.assertNotIn(str(self.directory), text)
        self.assertEqual(receipt["executable"]["name"], "exit-probe.exe")
        self.assertRegex(receipt["executable"]["sha256"], r"^[0-9a-f]{64}$")
        return result, receipt

    def test_debugger_startup_preserves_bare_windows_heap_policy(self):
        bare = subprocess.run(
            [str(self.executable), "heap"], capture_output=True, text=True,
            timeout=20, check=False,
        )
        self.assertEqual(bare.returncode, 0, bare.stderr)
        wrapped, receipt = self.execute("heap")
        bare_flags = re.search(r"DEBUG_HEAP_FLAGS=(\d+)", bare.stdout)[1]
        wrapped_flags = re.search(r"DEBUG_HEAP_FLAGS=(\d+)", wrapped.stdout)[1]
        self.assertEqual(wrapped_flags, bare_flags, "the observer changed native heap allocation behavior")
        self.assertEqual(wrapped.returncode, bare.returncode)
        self.assertIn("debug_heap_policy", receipt)

    def test_child_environment_preserves_unicode_values_without_changing_parent_or_receipt(self):
        with patch.dict(os.environ, {
            "_NO_DEBUG_HEAP": "0",
            "FESTERM_DIAGNOSTIC_ENV_CONTROL": "private-env-\U0001f680-after",
        }):
            result, receipt = self.execute("environment", environment=os.environ.copy())
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(os.environ["_NO_DEBUG_HEAP"], "0")
            self.assertNotIn("private-env", json.dumps(receipt))

    def test_native_environment_preserves_hidden_entries_after_surrogate_pairs(self):
        record = {"events": [], "modules": []}
        debugger = NativeDebugger(record)
        entries = ["=C:=C:\\owned-\U0001f680", "ALPHA=private-env-\U0001f680",
                   "_no_debug_heap=0", "ZETA=still-present"]
        text = "\0".join(entries) + "\0"
        native = ctypes.create_unicode_buffer(text, len(text.encode("utf-16-le")) // 2 + 1)
        with patch.object(debugger, "get_environment", return_value=ctypes.addressof(native)), \
             patch.object(debugger, "free_environment", return_value=True) as free:
            copied = debugger.normal_heap_environment()
        copied_entries = ctypes.string_at(
            ctypes.addressof(copied), ctypes.sizeof(copied),
        ).decode("utf-16-le").rstrip("\0").split("\0")
        self.assertEqual(set(copied_entries), set(entries[:2] + entries[3:] + ["_NO_DEBUG_HEAP=1"]))
        free.assert_called_once_with(ctypes.addressof(native))
        self.assertNotIn("private-env", json.dumps(record))

    def test_pass_and_rust_failure_preserve_status_and_completed_context(self):
        for mode, code, outcome in (("pass", 0, "passed"), ("failure", 101, "rust_test_failure")):
            with self.subTest(mode=mode):
                result, receipt = self.execute(mode)
                self.assertEqual(result.returncode, code, result.stderr)
                self.assertEqual(receipt["outcome"], outcome)
                self.assertEqual(receipt["exit_code_unsigned"], code)
                self.assertEqual(receipt["test_context"]["reported_tests"][0]["name"], "fixture::completed")

    def test_exit_2173_keeps_original_status_and_native_termination_stacks(self):
        result, receipt = self.execute("exit")
        self.assertEqual(result.returncode, 2173, result.stderr)
        self.assertEqual(receipt["outcome"], "native_exit_unknown_cause")
        self.assertEqual(receipt["exit_code_hex"], "0x0000087d")
        exits = [e for e in receipt["events"] if e["kind"] == "termination"]
        self.assertTrue(exits, receipt)
        self.assertEqual(exits[0]["exit_code"], 2173)
        self.assertIn(exits[0]["api"], ("RtlExitUserProcess", "NtTerminateProcess"))
        self.assertTrue(any(len(s["frames"]) > 1 for s in exits[0]["stacks"]), exits)
        self.assertTrue(any(m["name"].lower() == "ntdll.dll" for m in receipt["modules"]))

    def test_unsigned_native_status_and_external_style_termination(self):
        result, receipt = self.execute("terminate")
        self.assertEqual(result.returncode & 0xffffffff, 0xc0000005, result.stderr)
        self.assertEqual(receipt["exit_code_signed"], -1073741819)
        self.assertEqual(receipt["exit_code_hex"], "0xc0000005")
        self.assertTrue(any(e["kind"] == "termination" and e["api"] == "NtTerminateProcess"
                            for e in receipt["events"]))

    def test_real_unhandled_native_exception_keeps_second_chance_evidence(self):
        result, receipt = self.execute("exception")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(receipt["outcome"], "native_exception", receipt)
        exceptions = [e for e in receipt["events"] if e["kind"] == "exception" and not e["first_chance"]]
        self.assertTrue(exceptions)
        self.assertTrue(exceptions[0]["stacks"])
        self.assertEqual(receipt["exit_code_unsigned"], result.returncode & 0xffffffff)

    def test_timeout_removes_owned_child_but_not_unrelated_sentinel(self):
        sentinel = subprocess.Popen(
            [str(self.executable), "sleep"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            result, receipt = self.execute("tree", timeout=2)
            self.assertEqual(result.returncode, 124, result.stderr)
            self.assertEqual(receipt["outcome"], "timeout")
            child = int(re.search(r"FIXTURE_CHILD_PID=(\d+)", result.stdout)[1])
            self.assertTrue(any(e["kind"] == "owned_descendant_terminated" and e["pid"] == child
                                for e in receipt["events"]))
            kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel.OpenProcess.restype = ctypes.c_void_p
            handle = kernel.OpenProcess(0x00100000, False, child)
            if handle:
                try:
                    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
                    self.assertEqual(kernel.WaitForSingleObject(handle, 0), 0)
                finally:
                    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
                    kernel.CloseHandle(handle)
            self.assertIsNone(sentinel.poll(), "unrelated same-name process was killed")
        finally:
            sentinel.kill()
            sentinel.wait(timeout=10)

    def test_real_debugger_error_cleans_up_root_and_records_failure(self):
        output = self.directory / uuid.uuid4().hex
        original = NativeDebugger.module

        def fail_after_metadata(debugger, *arguments):
            original(debugger, *arguments)
            raise OSError("private failure")

        with patch.object(NativeDebugger, "module", fail_after_metadata), \
             contextlib.redirect_stderr(io.StringIO()):
            code = runner.main([
                "--output-root", str(output), "--", str(self.executable), "sleep",
            ])
        receipt = json.loads(next(output.glob("*.json")).read_text())
        self.assertEqual(code, 125)
        self.assertEqual(receipt["outcome"], "runner_error")
        self.assertEqual(receipt["cleanup"], "owned_tree_exited_after_runner_error")
        self.assertNotIn("exit_code_unsigned", receipt, "forced cleanup is not an original target exit")

    def test_powershell_cargo_routing_keeps_argument_boundaries_and_failure(self):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 required for Windows CI runner")
        environment = os.environ.copy()
        environment["FESTERM_TEST_RUNNER"] = str(ROOT / "scripts" / "run-windows-tests.ps1")
        environment["FESTERM_TEST_OUTPUT"] = str(self.directory / "space in output")
        command = r"""
function cargo {
    ConvertTo-Json -InputObject $args -Compress
    $global:LASTEXITCODE = 2173
}
& $env:FESTERM_TEST_RUNNER -OutputDirectory $env:FESTERM_TEST_OUTPUT -CargoArguments @('test','--workspace')
exit $LASTEXITCODE
"""
        result = subprocess.run(
            [shell, "-NoProfile", "-NonInteractive", "-Command", command],
            env=environment, capture_output=True, text=True, timeout=30, check=False,
        )
        self.assertEqual(result.returncode, 2173, result.stdout + result.stderr)
        arguments = json.loads(result.stdout)
        self.assertEqual(arguments[0], "--config")
        configuration = tomllib.loads(arguments[1])
        route = configuration["target"]["x86_64-pc-windows-msvc"]["runner"]
        self.assertEqual(route[-3:], ["--timeout-seconds", "1800", "--"])
        self.assertEqual(route[3], environment["FESTERM_TEST_OUTPUT"])
        self.assertEqual(arguments[2:], ["test", "--workspace"])


if __name__ == "__main__":
    unittest.main()
