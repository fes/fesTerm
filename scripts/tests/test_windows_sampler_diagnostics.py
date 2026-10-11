"""Opt-in, bounded routing and failure preservation without GPU work."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class WorkflowContractTests(unittest.TestCase):
    def test_hosted_diagnostics_are_dispatch_only_and_keep_content_free_artifacts(self):
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        job = workflow.split("  windows-sampler-diagnostics:", 1)[1].split(
            "  # Platform-independent checks:", 1
        )[0]
        self.assertIn(
            "if: github.event_name == 'workflow_dispatch' && inputs.windows_sampler_diagnostics",
            job,
        )
        self.assertIn("default: false", workflow)
        self.assertIn('CARGO_PROFILE_TEST_DEBUG: "0"', job)
        self.assertIn("timeout-minutes: 45", job)
        self.assertIn("path: target/windows-sampler-diagnostics/*/*.json", job)
        self.assertIn("retention-days: 7", job)
        self.assertIn("if-no-files-found: error", job)
        self.assertNotIn("continue-on-error", job)
        self.assertIn("run-windows-sampler-diagnostics.ps1", job)


@unittest.skipUnless(sys.platform == "win32", "Windows PowerShell launcher")
class LauncherTests(unittest.TestCase):
    def execute(self, opt_in=True, failure=0, threads=2, rounds=3):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 required for Windows CI")
        environment = os.environ.copy()
        environment["FESTERM_SAMPLER_SCRIPT"] = str(
            ROOT / "scripts" / "run-windows-sampler-diagnostics.ps1"
        )
        environment["FESTERM_RUN_OPTIONAL_VALIDATION"] = "1"
        environment["FESTERM_RUN_WINDOWS_SAMPLER_DIAGNOSTICS"] = "1" if opt_in else "0"
        for name in ("THREADS", "ROUNDS", "INSTANCE", "WORKLOAD"):
            environment[f"FESTERM_SAMPLER_{name}"] = f"parent-{name}"
        environment["FESTERM_SAMPLER_FAKE_FAILURE"] = str(failure)
        environment["FESTERM_SAMPLER_TEST_THREADS"] = str(threads)
        environment["FESTERM_SAMPLER_TEST_ROUNDS"] = str(rounds)
        with tempfile.TemporaryDirectory() as directory:
            environment["FESTERM_SAMPLER_TEST_OUT"] = str(Path(directory) / "space in output")
            command = r"""
$ErrorActionPreference = 'Stop'
function cargo {
    Write-Output ('CALL:' + (ConvertTo-Json -Compress -InputObject @{
        arguments = $args
        threads = $env:FESTERM_SAMPLER_THREADS
        rounds = $env:FESTERM_SAMPLER_ROUNDS
        mode = $env:FESTERM_SAMPLER_INSTANCE
        workload = $env:FESTERM_SAMPLER_WORKLOAD
    }))
    if ($env:FESTERM_SAMPLER_INSTANCE -eq 'independent' -and $env:FESTERM_SAMPLER_WORKLOAD -eq 'sampler') {
        $global:LASTEXITCODE = [int]$env:FESTERM_SAMPLER_FAKE_FAILURE
    } else {
        $global:LASTEXITCODE = 0
    }
}
& $env:FESTERM_SAMPLER_SCRIPT -OutputDirectory $env:FESTERM_SAMPLER_TEST_OUT `
    -Threads ([int]$env:FESTERM_SAMPLER_TEST_THREADS) -Rounds ([int]$env:FESTERM_SAMPLER_TEST_ROUNDS)
$code = $LASTEXITCODE
Write-Output ('RESTORED:' + (ConvertTo-Json -Compress -InputObject @(
    $env:FESTERM_SAMPLER_THREADS, $env:FESTERM_SAMPLER_ROUNDS,
    $env:FESTERM_SAMPLER_INSTANCE, $env:FESTERM_SAMPLER_WORKLOAD
)))
exit $code
"""
            result = subprocess.run(
                [shell, "-NoProfile", "-NonInteractive", "-Command", command],
                env=environment, capture_output=True, text=True, timeout=30, check=False,
            )
        return result

    def test_four_distinct_controls_preserve_bounds_failure_and_parent_environment(self):
        result = self.execute(failure=2173)
        self.assertEqual(result.returncode, 2173, result.stdout + result.stderr)
        calls = [
            json.loads(line.removeprefix("CALL:"))
            for line in result.stdout.splitlines() if line.startswith("CALL:")
        ]
        self.assertEqual(
            [(call["workload"], call["mode"]) for call in calls],
            [("sampler", "independent"), ("sampler", "shared"),
             ("mixed", "independent"), ("mixed", "shared")],
        )
        for call in calls:
            self.assertEqual((call["threads"], call["rounds"]), ("2", "3"))
            self.assertEqual(call["arguments"][2:], [
                "test", "--locked", "-p", "festerm", "--test", "windows_sampler_lifecycles",
                "--", "concurrent_sampler_device_lifecycles", "--ignored", "--exact",
                "--nocapture", "--test-threads=1",
            ])
            self.assertIn("--timeout-seconds", call["arguments"][1])
            self.assertIn("600", call["arguments"][1])
        restored = next(
            line.removeprefix("RESTORED:") for line in result.stdout.splitlines()
            if line.startswith("RESTORED:")
        )
        self.assertEqual(json.loads(restored), [
            "parent-THREADS", "parent-ROUNDS", "parent-INSTANCE", "parent-WORKLOAD",
        ])

    def test_passing_controls_return_zero(self):
        result = self.execute()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.count("CALL:"), 4)

    def test_missing_opt_in_and_invalid_bounds_never_start_cargo(self):
        for settings in ({"opt_in": False}, {"threads": 0}, {"threads": 33},
                         {"rounds": 0}, {"rounds": 129}):
            with self.subTest(settings=settings):
                result = self.execute(**settings)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn("CALL:", result.stdout)


if __name__ == "__main__":
    unittest.main()
