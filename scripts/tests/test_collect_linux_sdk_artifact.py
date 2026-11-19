"""Artifact admission and workflow contracts; no compilation or SDK execution."""
import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("collect_sdk", ROOT / "scripts/collect_linux_sdk_artifact.py")
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.binary = self.root / "target/release/deps/dev_tunnel_workflow_lab-0123456789abcdef"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b"\x7fELF\x02\x01" + bytes(12) + b"\xb7\x00" + b"test artifact")
        self.binary.chmod(0o755)
        self.message = {
            "reason": "compiler-artifact",
            "target": {"name": collector.TEST, "kind": ["test"],
                       "src_path": str(self.root / "app/festerm/tests/dev_tunnel_workflow_lab.rs")},
            "profile": {"test": True}, "executable": str(self.binary),
            "features": ["default", "experimental-dev-tunnels", "local-host-settings"],
        }
        self.messages = self.root / "messages.jsonl"
        self.output = self.root / "export"
        self.environment = {"DEVELOPMENT_SOURCE_HEAD": "a" * 40, "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "2"}

    def collect(self, messages=None):
        self.messages.write_text("\n".join(json.dumps(value) for value in (messages or [self.message])))
        with patch.object(collector.subprocess, "check_output", side_effect=["b" * 40, "c" * 40]):
            return collector.collect(self.root, self.messages, self.output, self.environment)

    def test_exports_only_exact_binary_and_source_bound_receipt_without_execution(self):
        receipt = self.collect([{"reason": "build-finished", "success": True}, self.message])
        self.assertEqual(sorted(path.name for path in self.output.iterdir()),
                         [collector.TEST, "development-sdk-provenance.json"])
        self.assertEqual((self.output / collector.TEST).read_bytes(), self.binary.read_bytes())
        self.assertEqual(receipt["candidate_head"], "a" * 40)
        self.assertEqual(receipt["checkout_commit"], "b" * 40)
        self.assertEqual(receipt["checkout_tree"], "c" * 40)
        self.assertEqual(receipt["run_attempt"], "2")
        self.assertEqual(receipt["features"], self.message["features"])
        self.assertEqual(receipt["build_layout"], "release-deps")
        self.assertEqual(receipt["build_paths"]["canonical_executable"], str(self.binary))
        self.assertEqual(receipt["build_paths"]["alias_paths"], [])

    def test_exports_exact_modern_package_build_layout(self):
        # Actual Cargo layout from run44589993880, artifact14563977465.
        modern = self.root / "target/release/build/festerm/f6ab84ea3b491ff1/out/dev_tunnel_workflow_lab-f6ab84ea3b491ff1"
        modern.parent.mkdir(parents=True)
        self.binary.rename(modern)
        self.message["executable"] = str(modern)
        receipt = self.collect()
        self.assertEqual(receipt["build_layout"], "release-package-build")
        self.assertEqual(receipt["build_paths"]["expected_parent"], str(modern.parent))
        self.assertEqual(receipt["build_paths"]["canonical_executable"], str(modern))
        self.assertEqual((self.output / collector.TEST).read_bytes(), modern.read_bytes())

    def test_modern_layout_refuses_mismatched_hash_package_profile_and_outside_root(self):
        valid = self.root / "target/release/build/festerm/f6ab84ea3b491ff1/out/dev_tunnel_workflow_lab-f6ab84ea3b491ff1"
        for binary, reason in [
            (valid.with_name("dev_tunnel_workflow_lab-0123456789abcdef"), "sdk.package_hash_mismatch"),
            (Path(str(valid).replace("/build/festerm/", "/build/foreign/")), "sdk.release_parent_mismatch"),
            (Path(str(valid).replace("/target/release/", "/target/debug/")), "sdk.release_parent_mismatch"),
            (self.root / "outside/build/festerm/f6ab84ea3b491ff1/out" / valid.name, "sdk.release_parent_mismatch"),
            (Path(str(valid).replace("f6ab84ea3b491ff1", "f6ab")), "sdk.release_parent_mismatch"),
        ]:
            with self.subTest(binary=binary):
                with self.assertRaises(collector.BuildPathError) as failure:
                    collector.validate_build_path(self.root, binary)
                self.assertEqual(str(failure.exception), reason)
                self.assertFalse(self.output.exists())

    def test_rejects_missing_duplicate_wrong_target_and_non_test(self):
        for mutation in ["missing", "duplicate", "name", "source", "kind", "profile"]:
            with self.subTest(mutation=mutation):
                message = json.loads(json.dumps(self.message))
                messages = [message]
                if mutation == "missing": messages = [{"reason": "build-finished"}]
                elif mutation == "duplicate": messages *= 2
                elif mutation == "name": message["target"]["name"] = "other"
                elif mutation == "source": message["target"]["src_path"] = "/foreign/test.rs"
                elif mutation == "kind": message["target"]["kind"] = ["bin"]
                elif mutation == "profile": message["profile"]["test"] = False
                with self.assertRaises(ValueError): self.collect(messages)
                self.assertFalse(self.output.exists())

    def test_rejects_wrong_elf_features_and_provenance(self):
        original = self.binary.read_bytes()
        for mutation in ["machine", "truncated", "features", "head", "run"]:
            with self.subTest(mutation=mutation):
                self.binary.write_bytes(original)
                self.message["features"] = ["experimental-dev-tunnels", "local-host-settings"]
                self.environment.update(DEVELOPMENT_SOURCE_HEAD="a" * 40, GITHUB_RUN_ID="123")
                if mutation == "machine": self.binary.write_bytes(original[:18] + b"\x3e\x00" + original[20:])
                elif mutation == "truncated": self.binary.write_bytes(b"\x7fELF")
                elif mutation == "features": self.message["features"] = []
                elif mutation == "head": self.environment["DEVELOPMENT_SOURCE_HEAD"] = "main"
                elif mutation == "run": self.environment["GITHUB_RUN_ID"] = "../123"
                with self.assertRaises(ValueError): self.collect()
                self.assertFalse(self.output.exists())

    def test_rejects_wrong_path_symlink_and_hardlink(self):
        other = self.root / "foreign"
        self.binary.rename(other)
        self.message["executable"] = str(other)
        with self.assertRaises(ValueError): self.collect()
        self.message["executable"] = str(self.binary)
        self.binary.symlink_to(other)
        with self.assertRaises(ValueError): self.collect()
        self.binary.unlink()
        os.link(other, self.binary)
        with self.assertRaises(ValueError): self.collect()

    def test_existing_output_is_never_overwritten(self):
        self.output.mkdir()
        sentinel = self.output / "keep"
        sentinel.write_text("original")
        with self.assertRaises(FileExistsError): self.collect()
        self.assertEqual(sentinel.read_text(), "original")

    def test_path_failures_identify_each_closed_boundary(self):
        for binary, reason in [
            (Path("target/release/deps") / self.binary.name, "sdk.executable_relative"),
            (self.root / self.binary.name, "sdk.release_parent_mismatch"),
            (self.binary.with_name("dev_tunnel_workflow_lab"), "sdk.test_basename_mismatch"),
        ]:
            with self.subTest(reason=reason):
                with self.assertRaises(collector.BuildPathError) as failure:
                    collector.validate_build_path(self.root, binary)
                self.assertEqual(str(failure.exception), reason)
                self.assertEqual(failure.exception.paths["executable"], str(binary))
        original = self.root / "original"
        self.binary.rename(original)
        self.binary.symlink_to(original)
        with self.assertRaises(collector.BuildPathError) as failure:
            collector.validate_build_path(self.root, self.binary)
        self.assertEqual(str(failure.exception), "sdk.path_alias")
        self.assertEqual(failure.exception.paths["canonical_executable"], str(original))
        self.assertEqual(failure.exception.paths["alias_paths"], [str(self.binary)])

    def test_cli_retains_only_build_path_projection_before_raising(self):
        diagnostic = self.root / "path-diagnostic.json"
        self.message["executable"] = str(self.root / self.binary.name)
        self.messages.write_text(json.dumps(self.message))
        with patch.object(collector, "ROOT", self.root), patch.object(collector.os, "environ", self.environment), patch("sys.argv", [
            "collector", "--messages", str(self.messages), "--output", str(self.output),
            "--path-diagnostic", str(diagnostic),
        ]):
            with self.assertRaises(collector.BuildPathError): collector.main()
        projection = json.loads(diagnostic.read_text())
        self.assertEqual(projection["reason"], "sdk.release_parent_mismatch")
        self.assertEqual(set(projection), {"reason", "checkout_root", "expected_parent",
                         "canonical_expected_parent", "executable", "canonical_executable", "alias_paths"})
        self.assertFalse(self.output.exists())


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / ".github/workflows/package-smoke.yml").read_text()

    def test_manual_arm_selection_preserves_default_five_platform_matrix(self):
        line = next(line for line in self.workflow.splitlines() if "matrix: ${{ fromJSON(" in line)
        matrices = [json.loads(value)["include"] for value in re.findall(r"'(\{\"include\".*?\})'", line)]
        self.assertEqual(len(matrices), 2)
        self.assertEqual(matrices[0], [{"name": "linux-aarch64", "platform": "linux", "runner": "ubuntu-24.04-arm"}])
        self.assertEqual([value["name"] for value in matrices[1]],
                         ["macos", "windows-x86_64", "windows-aarch64", "linux-x86_64", "linux-aarch64"])
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.platform == 'linux-aarch64'", line)
        self.assertIn("default: all\n        options:\n          - all\n          - linux-aarch64", self.workflow)

    def test_sdk_compile_only_export_is_separate_after_existing_core_export(self):
        compile_step = self.workflow.split("      - name: Compile and retain the Linux ARM64 controlled SDK test\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("if: matrix.name == 'linux-aarch64'", compile_step)
        self.assertIn("--test dev_tunnel_workflow_lab --no-run", compile_step)
        self.assertIn("--message-format=json-render-diagnostics", compile_step)
        self.assertNotIn("--ignored", compile_step)
        self.assertNotIn("FESTERM_RUN_DEV_TUNNEL_LAB", compile_step)
        self.assertLess(self.workflow.index("Retain existing Linux ARM64 development runtime"),
                        self.workflow.index("Compile and retain the Linux ARM64 controlled SDK test"))
        upload = self.workflow.split("      - name: Retain the Linux ARM64 controlled SDK executable\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("actions/upload-artifact@330a01c490aca151604b8cf639adc76d48f6c5d4", upload)
        self.assertIn("retention-days: 7", upload)
        self.assertIn("development-linux-aarch64-sdk-", upload)

    def test_failed_compile_or_collection_always_retains_build_diagnostics(self):
        step = self.workflow.split("      - name: Retain failed Linux ARM64 SDK build diagnostics\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("if: always() && matrix.name == 'linux-aarch64' && steps.linux_sdk.outcome == 'failure'", step)
        self.assertIn("target/release/linux-arm-sdk-build.jsonl", step)
        self.assertIn("target/release/linux-arm-sdk-path-diagnostic.json", step)
        self.assertIn("retention-days: 7", step)
        self.assertIn("if-no-files-found: error", step)


if __name__ == "__main__":
    unittest.main()
