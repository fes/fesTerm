import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import textwrap
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
            for name in bundle.binary_names(platform, host=True):
                (self.binary_dir / name).write_bytes(f"synthetic {name}".encode())

    def archive(self, platform="linux", arch="x86_64", *, host=True):
        return bundle.build_bundle(
            self.root, self.binary_dir, self.root / "dist", "1.2.3",
            platform, arch, notices=b"synthetic third party notice\n", host=host,
        )

    def test_six_manifests_bind_exact_identity_length_hash_and_preserve_legacy_assets(self):
        for platform in bundle.PLATFORMS:
            for arch in bundle.ARCHITECTURES:
                with self.subTest(platform=platform, arch=arch):
                    archive = self.archive(platform, arch)
                    original = archive.read_bytes()
                    checksum = archive.with_name(archive.name + ".sha256").read_bytes()
                    output = manifest.build_manifest(archive, "1.2.3", platform, arch, host=True)
                    encoded = output.read_bytes()
                    self.assertLessEqual(len(encoded), manifest.MAX_MANIFEST_BYTES)
                    self.assertEqual(json.loads(encoded), {
                        "schema": 2,
                        "package": "festerm-host",
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
                        manifest.build_manifest(archive, "1.2.3", platform, arch, host=True)
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
                    manifest.build_manifest(archive, version, platform, arch, host=True)
        archive.write_bytes(b"")
        with self.assertRaisesRegex(ValueError, "length"):
            manifest.build_manifest(archive, "1.2.3", "linux", "x86_64", host=True)
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

    @unittest.skipIf(os.name == "nt" or not shutil.which("sha256sum"), "Unix release payload admission")
    def test_production_release_payload_consumer_accepts_both_families_and_refuses_substitution(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        step = workflow.split("      - name: Verify release payload\n", 1)[1].split("      - uses:", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        output = self.root / "dist"
        for platform in bundle.PLATFORMS:
            for arch in bundle.ARCHITECTURES:
                for host in (False, True):
                    bundle.build_bundle(
                        self.root, self.binary_dir, output, "1.2.3", platform, arch,
                        notices=b"synthetic notices\n", host=host,
                    )
        for name in re.findall(r'^\s+"([^"]+)"$', script, re.MULTILINE):
            (output / name.replace("${VERSION}", "1.2.3")).write_bytes(b"synthetic desktop/installer payload")
        (self.root / "scripts").symlink_to(ROOT / "scripts", target_is_directory=True)
        environment = {"PATH": os.environ["PATH"], "TMPDIR": str(self.root),
                       "VERSION": "1.2.3", "HEADLESS_HELPERS": "true"}

        def verify():
            return subprocess.run(["bash", "-c", script], cwd=self.root, env=environment,
                                  capture_output=True, text=True, timeout=30)

        result = verify()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        host = output / "fesTerm-host-1.2.3-linux-x86_64.tar.gz"
        host.write_bytes((output / "fesTerm-tunnel-1.2.3-linux-x86_64.tar.gz").read_bytes())
        result = verify()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exactly", result.stderr)

    def test_host_is_required_by_manifest_producer_and_native_signer_paths(self):
        legacy = bundle.build_bundle(
            self.root, self.binary_dir, self.root / "legacy", "1.2.3",
            "linux", "x86_64", notices=b"synthetic notice\n",
        )
        with self.assertRaisesRegex(ValueError, "exact-target"):
            manifest.build_manifest(legacy, "1.2.3", "linux", "x86_64", host=True)
        self.assertFalse(legacy.with_name(legacy.name + ".manifest.json").exists())
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        helpers = workflow.split("  helper-bundles:\n", 1)[1]
        self.assertIn("packages+=(-p festerm-host)", helpers)
        self.assertIn("binaries+=(festerm-host)", helpers)
        self.assertIn("'festerm-host.exe'", helpers)
        self.assertIn(r"format('{0}\helper-input\festerm-host.exe', github.workspace)", helpers)
        self.assertIn("$names += 'festerm-host'", helpers)

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
        self.assertIn("cargo test --locked -p festerm-host-setup --features acquisition -p festerm-host", workflow)
        job = workflow.split("  helper-signature-compatibility:\n", 1)[1]
        job = job.split("  conformance:\n", 1)[0]
        self.assertIn("runs-on: ubuntu-latest", job)
        self.assertIn("cargo install cargo-packager --version 0.11.8 --locked", job)
        self.assertIn("pinned_cargo_packager_signatures_authenticate_python_produced_manifest", job)
        self.assertIn("-- --exact --ignored", job)
        self.assertNotIn("secrets.", job)
