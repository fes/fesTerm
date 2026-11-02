"""Keep privilege-required security assertions separate and mandatory."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
PRIVILEGED = {
    "security_privilege_is_scoped_to_the_saving_thread",
    "security_copy_preserves_an_explicit_mandatory_integrity_label",
    "security_copy_preserves_an_explicit_audit_sacl",
    "conditional_replacement_preserves_audit_metadata_and_private_dacl",
}


class SecurityValidationPolicyTests(unittest.TestCase):
    def test_only_the_four_privileged_assertions_are_explicitly_gated(self):
        source = (ROOT / "crates/festerm-windows-security/src/lib.rs").read_text()
        ignored = re.findall(
            r'#\[test\]\s*#\[ignore = "([^"]+)"\]\s*fn (\w+)\(', source
        )
        self.assertEqual({name for _, name in ignored}, PRIVILEGED)
        for reason, _ in ignored:
            self.assertIn("requires SeSecurityPrivilege", reason)

    def test_private_publication_and_missing_privilege_remain_default_tests(self):
        source = (ROOT / "crates/festerm-windows-security/src/lib.rs").read_text()
        for name in (
            "private_creation_and_conditional_publication_keep_the_current_user_only_dacl",
            "missing_security_privilege_refuses_capture_without_mutation",
        ):
            with self.subTest(name=name):
                self.assertRegex(source, rf"#\[test\]\s*fn {name}\(")

    def test_ci_requires_the_privileged_suite_without_a_conditional_or_waiver(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job = re.search(
            r"(?ms)^  windows-security-privileged:\n(.*?)(?=^  [\w-]+:|\Z)",
            workflow,
        )
        self.assertIsNotNone(job, "privileged security CI job must exist")
        body = job.group(1)
        self.assertIn("runs-on: windows-latest", body)
        self.assertIn(
            "run: cargo test --locked -p festerm-windows-security --lib -- --ignored --nocapture",
            body,
        )
        self.assertNotRegex(body, r"(?m)^\s*(if|continue-on-error):")


if __name__ == "__main__":
    unittest.main()
