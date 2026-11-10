import hashlib
import importlib.util
import json
import shutil
import sys
import unittest
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
SPEC = importlib.util.spec_from_file_location(
    "build_host_manifest", ROOT / "scripts/build_host_manifest.py"
)
manifest = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(manifest)
import build_tunnel_bundle as bundle


class HostManifestTests(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / ".test-work" / f"host-manifest-{uuid.uuid4().hex}"
        self.root.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.root)
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "1.2.3"\nlicense = "MIT"\n'
        )
        self.binary_dir = self.root / "bin"
        self.binary_dir.mkdir()
        for platform in bundle.PLATFORMS:
            for name in bundle.binary_names(platform):
                (self.binary_dir / name).write_bytes(f"synthetic {name}".encode())

    def archive(self, platform="linux", arch="x86_64"):
        return bundle.build_bundle(
            self.root, self.binary_dir, self.root / "dist", "1.2.3",
            platform, arch, notices=b"synthetic third party notice\n",
        )

    def test_six_manifests_bind_exact_identity_length_hash_and_preserve_legacy_assets(self):
        for platform in bundle.PLATFORMS:
            for arch in bundle.ARCHITECTURES:
                with self.subTest(platform=platform, arch=arch):
                    archive = self.archive(platform, arch)
                    original = archive.read_bytes()
                    checksum = archive.with_name(archive.name + ".sha256").read_bytes()
                    output = manifest.build_manifest(archive, "1.2.3", platform, arch)
                    encoded = output.read_bytes()
                    self.assertLessEqual(len(encoded), manifest.MAX_MANIFEST_BYTES)
                    self.assertEqual(json.loads(encoded), {
                        "schema": 1,
                        "package": "festerm-helpers",
                        "version": "1.2.3",
                        "platform": platform,
                        "architecture": arch,
                        "filename": archive.name,
                        "length": len(original),
                        "sha256": hashlib.sha256(original).hexdigest(),
                    })
                    self.assertEqual(archive.read_bytes(), original)
                    self.assertEqual(
                        archive.with_name(archive.name + ".sha256").read_bytes(), checksum
                    )
                    self.assertEqual(output.name, archive.name + ".manifest.json")
                    with self.assertRaises(FileExistsError):
                        manifest.build_manifest(archive, "1.2.3", platform, arch)
                    self.assertEqual(output.read_bytes(), encoded)

    def test_mismatched_target_version_and_invalid_archive_fail_without_manifest(self):
        archive = self.archive()
        for version, platform, arch in [
            ("1.2.4", "linux", "x86_64"),
            ("1.2.3", "linux", "aarch64"),
            ("1.2.3", "macos", "x86_64"),
            ("v1.2.3", "linux", "x86_64"),
            ("01.2.3", "linux", "x86_64"),
            ("1.2.3-rc.1", "linux", "x86_64"),
            ("../1.2.3", "linux", "x86_64"),
        ]:
            with self.subTest(version=version, platform=platform, arch=arch):
                with self.assertRaises(ValueError):
                    manifest.build_manifest(archive, version, platform, arch)
        archive.write_bytes(b"")
        with self.assertRaisesRegex(ValueError, "length"):
            manifest.build_manifest(archive, "1.2.3", "linux", "x86_64")
        self.assertFalse(archive.with_name(archive.name + ".manifest.json").exists())

    def test_signed_metadata_workflow_uses_existing_key_and_verifies_before_publication(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        step = workflow.split("      - name: Prepare standalone helper metadata and verifier without signing secrets\n")[1]
        step = step.split("      - name: Generate release notes")[0]
        self.assertIn("secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY", step)
        self.assertIn('test "$FESTERM_UPDATE_PUBLIC_KEY" = "$(cat packaging/updater.pub)"', step)
        self.assertIn('for platform in linux macos windows', step)
        self.assertIn('for arch in x86_64 aarch64', step)
        self.assertLess(step.index("build_host_manifest.py"), step.index("cargo packager signer sign"))
        self.assertLess(step.index("cargo packager signer sign"), step.index("target/release/festerm-verify-artifact"))
        self.assertLess(workflow.index("Authenticate standalone helper release metadata"),
                        workflow.index("gh release upload"))
        self.assertIn("cargo install cargo-packager --version \"$CARGO_PACKAGER_VERSION\" --locked",
                      workflow)

    def test_signing_secrets_are_absent_from_build_generation_and_verification_steps(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        publish_environment = workflow.split("  publish:\n", 1)[1].split("    steps:\n", 1)[0]
        self.assertNotIn("CARGO_PACKAGER_SIGN_PRIVATE_KEY", publish_environment)
        installation, remainder = workflow.split(
            "      - name: Install pinned cargo-packager for helper metadata signatures\n", 1
        )[1].split(
            "      - name: Prepare standalone helper metadata and verifier without signing secrets\n", 1
        )
        preparation, remainder = remainder.split(
            "      - name: Sign standalone helper release metadata\n", 1
        )
        signing, remainder = remainder.split(
            "      - name: Authenticate standalone helper release metadata\n", 1
        )
        verification = remainder.split("      - name: Generate release notes", 1)[0]
        for step in (installation, preparation, verification):
            self.assertNotIn("secrets.", step)
            self.assertNotIn("CARGO_PACKAGER_SIGN_PRIVATE_KEY", step)
        self.assertIn("cargo build --locked", preparation)
        self.assertIn("build_host_manifest.py", preparation)
        self.assertIn('test "$FESTERM_UPDATE_PUBLIC_KEY" = "$(cat packaging/updater.pub)"',
                      preparation)
        self.assertIn("secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY", signing)
        self.assertIn("secrets.CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD", signing)
        self.assertIn("cargo packager signer sign", signing)
        for command in ("cargo build", "cargo install", "python", "target/release/", "find "):
            self.assertNotIn(command, signing)
        self.assertIn("target/release/festerm-verify-artifact", verification)

    def test_normal_ci_runs_cross_language_and_real_pinned_signer_checks(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        step = workflow.split(
            "      - name: Validate authenticated helper artifacts without desktop dependencies\n", 1
        )[1].split("      - name:", 1)[0]
        self.assertIn("\n        shell: bash\n", "\n" + step)
        self.assertIn("cargo test --locked -p festerm-host-setup --features artifacts", workflow)
        job = workflow.split("  helper-signature-compatibility:\n", 1)[1]
        job = job.split("  conformance:\n", 1)[0]
        self.assertIn("runs-on: ubuntu-latest", job)
        self.assertIn("cargo install cargo-packager --version 0.11.8 --locked", job)
        self.assertIn("pinned_cargo_packager_signatures_authenticate_python_produced_manifest", job)
        self.assertIn("-- --exact --ignored", job)
        self.assertNotIn("secrets.", job)
