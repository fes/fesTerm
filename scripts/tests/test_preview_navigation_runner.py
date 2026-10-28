"""Exercise only the optional navigation dispatch; never build or render."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = (ROOT / "scripts" / "run-optional-validation.ps1").read_text(
    encoding="utf-8"
)
START = RUNNER.index("if ($env:OS -ne 'Windows_NT' -or")
END = RUNNER.index("if ($env:FESTERM_RUN_SURFACE_PROFILE", START)
DISPATCH = RUNNER[START:END]


@unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 required")
class PreviewNavigationDispatchTests(unittest.TestCase):
    def run_dispatch(self, *, enabled=True, platform="Windows_NT",
                     architecture="AMD64", contains=True, exit_code=0, receipt=True,
                     complete=True, timeout=False):
        scratch = ROOT / "target" / "preview-navigation-runner-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            directory = Path(directory)
            fake = directory / "probe.ps1"
            fake.write_text(
                "if ($args -contains '--list') {\n"
                + ("  'direct2d::preview_navigation::profile_small_preview_navigation: test'\n"
                   if contains else "  'some_other_test: test'\n")
                + "  $global:LASTEXITCODE = 0\n"
                "  return\n"
                "}\n"
                + ("'{\"kind\":\"small-preview-navigation-complete\",\"status\":\"pass\","
                   "\"measured_frames\":1408,\"completed_draws\":1760}'\n"
                   if complete else "'incomplete output'\n")
                + f"$global:LASTEXITCODE = {exit_code}\n",
                encoding="utf-8",
            )
            result = directory / "result.txt"
            environment = {
                **os.environ,
                "OS": platform,
                "PROCESSOR_ARCHITECTURE": architecture,
                "FESTERM_RUN_PREVIEW_NAVIGATION_PROFILE": "1" if enabled else "",
                "FESTERM_PREVIEW_NAVIGATION_TEST_EXE": str(fake),
                "FESTERM_PREVIEW_NAVIGATION_BUILD_RECEIPT": "receipt.json" if receipt else "",
                "NAVIGATION_TEST_RESULT": str(result),
                "NAVIGATION_TEST_TIMEOUT": "1" if timeout else "",
            }
            program = (
                "$ErrorActionPreference = 'Stop'\n"
                "$ResultPath = $env:NAVIGATION_TEST_RESULT\n"
                "$status = 'pass'\n"
                "function Invoke-NativeCommand { param([scriptblock] $Command); & $Command }\n"
                "function Write-OptionalResult { param([string] $Text); "
                "Add-Content $ResultPath $Text }\n"
                "function Start-Process {\n"
                "  param($FilePath, $ArgumentList, $RedirectStandardOutput, "
                "$RedirectStandardError, [switch]$PassThru, $ErrorAction)\n"
                "  & $FilePath @ArgumentList | Set-Content $RedirectStandardOutput\n"
                "  Set-Content $RedirectStandardError ''\n"
                "  $process = [pscustomobject]@{ Id = 0; ExitCode = $global:LASTEXITCODE }\n"
                "  $process | Add-Member -MemberType ScriptMethod -Name WaitForExit "
                "-Value { param($timeout); return $env:NAVIGATION_TEST_TIMEOUT -ne '1' }\n"
                "  return $process\n"
                "}\n"
                "function Stop-Process { param($Id) }\n"
                + DISPATCH
                + "\nAdd-Content $ResultPath \"status=$status\"\n"
            )
            completed = subprocess.run(
                ["pwsh", "-NoProfile", "-NonInteractive", "-Command", program],
                env=environment, capture_output=True, text=True, timeout=30,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            return result.read_text(encoding="utf-8-sig")

    def test_explicit_opt_in_and_supported_platform_are_required(self):
        self.assertIn("reason=explicit-opt-in", self.run_dispatch(enabled=False))
        self.assertIn("reason=windows-x64-required", self.run_dispatch(platform="Unix"))
        self.assertIn("reason=windows-x64-required", self.run_dispatch(architecture="ARM64"))

    def test_success_and_failure_propagate(self):
        self.assertIn("suite=small-preview-navigation status=pass", self.run_dispatch())
        self.assertIn("status=fail", self.run_dispatch(exit_code=1))

    def test_missing_receipt_and_zero_matching_tests_are_not_passes(self):
        self.assertIn("status=fail", self.run_dispatch(receipt=False))
        self.assertIn("status=fail", self.run_dispatch(contains=False))

    def test_incomplete_output_and_timed_out_process_are_not_passes(self):
        self.assertIn("status=fail", self.run_dispatch(complete=False))
        self.assertIn("status=fail", self.run_dispatch(timeout=True))

    def test_unix_aggregation_reports_an_honest_platform_skip(self):
        source = (ROOT / "scripts" / "run-optional-validation.sh").read_text()
        self.assertIn(
            "suite=small-preview-navigation status=skipped reason=windows-x64-required",
            source,
        )
        self.assertNotIn("profile_small_preview_navigation --", source)
