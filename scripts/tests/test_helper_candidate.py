import hashlib
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("record_helper_candidate", ROOT / "scripts/record_helper_candidate.py")
candidate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(candidate)


class HelperCandidateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.source = "a" * 40
        self.environment = dict(
            GITHUB_EVENT_NAME="workflow_dispatch", SOURCE_COMMIT=self.source,
            GITHUB_SHA=self.source, WORKFLOW_COMMIT=self.source,
            GITHUB_REPOSITORY="fes/fesTerm", GITHUB_RUN_ID="123", GITHUB_RUN_ATTEMPT="2",
            WORKFLOW_REF="fes/fesTerm/.github/workflows/release.yml@refs/heads/reviewed",
            VERSION="1.2.3", SECRET_SHOULD_NEVER_BE_RECORDED="private-placeholder",
        )
        for platform in ("linux", "macos", "windows"):
            extension = "zip" if platform == "windows" else "tar.gz"
            for arch in ("x86_64", "aarch64"):
                for family in ("tunnel", "host"):
                    archive = f"fesTerm-{family}-1.2.3-{platform}-{arch}.{extension}"
                    for suffix in ("", ".sha256", ".manifest.json", ".manifest.json.sig"):
                        (self.directory / (archive + suffix)).write_bytes(b"fixture\n")

    def record(self, **updates):
        return candidate.record(self.directory, {**self.environment, **updates}, self.source)

    def test_provenance_binds_all_artifacts_to_exact_run_attempt_source_and_workflow(self):
        result = self.record()
        self.assertEqual(result["source_commit"], self.source)
        self.assertEqual(result["workflow_commit"], self.source)
        self.assertEqual(result["run_attempt"], "2")
        self.assertEqual(len(result["artifacts"]), 48)
        self.assertEqual(result["artifacts"][0]["sha256"], hashlib.sha256(b"fixture\n").hexdigest())
        self.assertNotIn("SECRET_SHOULD_NEVER_BE_RECORDED", str(result))
        self.assertNotIn("private-placeholder", str(result))
        self.assertNotIn("update", result)

    def test_mixed_source_workflow_or_event_and_invalid_run_are_rejected(self):
        for updates in (
            {"GITHUB_EVENT_NAME": "push"}, {"GITHUB_SHA": "b" * 40},
            {"WORKFLOW_COMMIT": "b" * 40}, {"SOURCE_COMMIT": "not-a-sha"},
            {"GITHUB_RUN_ID": ""}, {"GITHUB_RUN_ATTEMPT": "0"},
            {"GITHUB_REPOSITORY": "someone/fesTerm"}, {"WORKFLOW_REF": "other.yml@main"},
            {"VERSION": "1.2.3-beta"},
        ):
            with self.subTest(updates=updates), self.assertRaises(ValueError):
                self.record(**updates)
        with self.assertRaises(ValueError):
            candidate.record(self.directory, self.environment, "b" * 40)

    def test_unexpected_missing_empty_or_nonregular_artifacts_are_rejected(self):
        extra = self.directory / "festerm-update.json"
        extra.write_bytes(b"extra")
        with self.assertRaises(ValueError):
            self.record()
        extra.unlink()
        target = next(self.directory.iterdir())
        target.unlink()
        with self.assertRaises(ValueError):
            self.record()
        target.write_bytes(b"")
        with self.assertRaises(ValueError):
            self.record()
        target.unlink()
        target.mkdir()
        with self.assertRaises(ValueError):
            self.record()
        target.rmdir()
        if os.name != "nt":
            target.symlink_to(next(self.directory.iterdir()))
            with self.assertRaises(ValueError):
                self.record()

    def test_explicit_candidate_skips_only_desktop_packaging_and_its_arm_feeder(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        for job in ("macos", "windows-arm-build", "windows", "linux"):
            header = workflow.split(f"  {job}:\n", 1)[1].split("    steps:\n", 1)[0]
            self.assertIn("if: needs.validate.outputs.signed_helpers_candidate != 'true'", header)
        for job in ("validate", "helper-bundles", "helper-windows-sign"):
            header = workflow.split(f"  {job}:\n", 1)[1].split("    steps:\n", 1)[0]
            self.assertNotIn("signed_helpers_candidate != 'true'", header)

    def test_candidate_job_cannot_publish_or_expose_signing_secrets_outside_sign_step(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        candidate_input = workflow.split("      signed_helpers_candidate:\n", 1)[1].split("      resume_publish:\n", 1)[0]
        self.assertIn("type: boolean", candidate_input)
        self.assertIn("default: false", candidate_input)
        authentication = workflow.split("  authenticate-helpers:\n", 1)[1].split("  publish:\n", 1)[0]
        header, steps = authentication.split("    steps:\n", 1)
        self.assertIn("environment: release", header)
        self.assertIn("contents: read", header)
        self.assertNotIn("contents: write", authentication)
        self.assertNotIn("id-token:", authentication)
        self.assertIn("needs.validate.outputs.signed_helpers_candidate == 'true'", header)
        self.assertNotIn("gh release", authentication)
        self.assertNotIn("generate_update_manifest", authentication)
        self.assertNotIn("run-id:", authentication)
        self.assertNotIn("github-token:", authentication)
        self.assertIn("pattern: release-tunnel-*", authentication)
        self.assertIn('test "$SOURCE_COMMIT" = "$GITHUB_SHA"', authentication)
        self.assertIn('test "$WORKFLOW_COMMIT" = "$GITHUB_SHA"', authentication)
        before, signing = steps.split("      - name: Sign standalone helper release metadata\n", 1)
        signing, after = signing.split("      - name: Authenticate standalone helper release metadata\n", 1)
        for section in (header, before, after):
            self.assertNotIn("secrets.", section)
        self.assertIn("secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY", signing)
        self.assertIn("packaging/updater.pub", before)
        publish = workflow.split("  publish:\n", 1)[1]
        self.assertIn("if: needs.validate.outputs.publish == 'true'", publish)
        self.assertIn("helper-windows-sign, authenticate-helpers]", publish)
        self.assertIn("name: authenticated-helper-metadata", publish)
        self.assertNotIn("cargo packager signer sign", publish)
        self.assertNotIn("secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY", publish)
        self.assertNotIn("name: release-", after)


if __name__ == "__main__":
    unittest.main()
