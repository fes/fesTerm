"""Non-destructive checks for the experimental ownership-probe workflow."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class OwnershipProbeWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text()
        cls.job = workflow.split("  windows-job-ownership-probe:", 1)[1].split(
            "\n  quality:", 1
        )[0]

    def test_context_gate_fails_before_checkout_and_opt_in(self):
        self.assertLess(
            self.job.index("- name: Require a GitHub-hosted Windows runner"),
            self.job.index("- uses: actions/checkout"),
        )
        self.assertNotIn(
            "FESTERM_WINDOWS_JOB_DISPOSABLE_VM", self.job.split("    steps:", 1)[0]
        )
        gate = self.job.split("      - name: Require a GitHub-hosted Windows runner", 1)[
            1
        ].split("\n      - uses: actions/checkout", 1)[0]
        self.assertIn("CONTEXT_RUNNER_ENVIRONMENT: ${{ runner.environment }}", gate)
        self.assertIn("CONTEXT_RUNNER_OS: ${{ runner.os }}", gate)
        self.assertIn("$env:CONTEXT_RUNNER_ENVIRONMENT -cne 'github-hosted'", gate)
        self.assertIn("$env:CONTEXT_RUNNER_OS -cne 'Windows'", gate)
        self.assertIn("$env:RUNNER_ENVIRONMENT -cne $env:CONTEXT_RUNNER_ENVIRONMENT", gate)
        self.assertIn("$env:RUNNER_OS -cne $env:CONTEXT_RUNNER_OS", gate)
        self.assertIn("$env:GITHUB_ACTIONS -cne 'true'", gate)
        self.assertIn("$env:GITHUB_JOB -cne 'windows-job-ownership-probe'", gate)
        self.assertIn("throw ", gate)
        self.assertNotIn("FESTERM_WINDOWS_JOB_DISPOSABLE_VM", gate)
        self.assertNotIn("continue-on-error", self.job)
        self.assertIn(
            "if: ${{ runner.environment == 'github-hosted' && runner.os == 'Windows' }}",
            self.job,
        )

    def test_exact_qualification_and_receipt_are_required(self):
        self.assertIn(
            "--test ownership_probe -- --ignored --exact "
            "windows_job_ownership_qualification --nocapture",
            self.job,
        )
        self.assertIn("FESTERM_WINDOWS_JOB_RUNNER_ENVIRONMENT: ${{ runner.environment }}", self.job)
        self.assertIn("FESTERM_WINDOWS_JOB_RUNNER_OS: ${{ runner.os }}", self.job)
        self.assertIn("$_ -ceq $receipt", self.job)
        self.assertIn(".Count -eq 1", self.job)
        self.assertIn("Test-ProbeReceipt -lines @($receipt, $receipt)", self.job)
        self.assertIn("if ($exitCode -ne 0)", self.job)
        self.assertIn("if (-not (Test-ProbeReceipt -lines $output))", self.job)


if __name__ == "__main__":
    unittest.main()
