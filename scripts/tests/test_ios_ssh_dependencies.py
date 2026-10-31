"""Portable checks of the iOS graph gate, without a Rust or Apple toolchain."""

import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ios_ssh_build", ROOT / "scripts/build-ios-spike.py")
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


class IosSshDependencyTests(unittest.TestCase):
    def test_only_live_bundle_declares_local_network_use_for_selected_ssh_hosts(self):
        self.assertNotIn("NSLocalNetworkUsageDescription", build.bundle_info())
        self.assertIn("SSH hosts you choose", build.bundle_info(True)["NSLocalNetworkUsageDescription"])

    def test_live_feature_graph_is_checked_for_both_mobile_targets(self):
        with patch.object(build.sys, "argv", ["build", "--check-dependencies", "--live-ssh"]), \
                patch.object(build, "check_dependencies") as check:
            self.assertEqual(build.main(), 0)
        self.assertEqual([call.args for call in check.call_args_list], [
            ("aarch64-apple-ios",), ("aarch64-apple-ios", "festerm-ssh"), ("aarch64-apple-ios", "festerm-mobile-profiles"), ("aarch64-apple-ios", "festerm-sftp"),
            ("aarch64-apple-ios", "festerm-mobile", True),
            ("aarch64-apple-ios-sim",), ("aarch64-apple-ios-sim", "festerm-ssh"), ("aarch64-apple-ios-sim", "festerm-mobile-profiles"), ("aarch64-apple-ios-sim", "festerm-sftp"),
            ("aarch64-apple-ios-sim", "festerm-mobile", True),
        ])

    def test_live_feature_is_forwarded_to_cargo_graph_inspection(self):
        with patch.object(build, "run", return_value=subprocess.CompletedProcess([], 0, "festerm-mobile v0.11.1")) as run:
            build.check_dependencies("aarch64-apple-ios", live_ssh=True)
        self.assertIn("--features", run.call_args.args)
        self.assertIn("live-ssh", run.call_args.args)

    def test_real_ssh_graph_uses_target_qualified_normal_and_build_dependencies(self):
        output = "festerm-ssh v0.11.1\nfesterm-secret-store v0.11.1\nring v0.17.14\n"
        with patch.object(build, "run", return_value=subprocess.CompletedProcess([], 0, output)) as run:
            build.check_dependencies("aarch64-apple-ios", "festerm-ssh")
        args = run.call_args.args
        self.assertEqual(args[:6], ("cargo", "tree", "--locked", "-p", "festerm-ssh", "--target"))
        self.assertIn("aarch64-apple-ios", args)
        self.assertIn("normal,build", args)

    def test_ios_refuses_desktop_secret_services_and_session_backends(self):
        for package in ("festerm-mobile", "festerm-ssh"):
            for dependency in (
                "windows-native-keyring-store",
                "zbus-secret-service-keyring-store", "festerm-pty", "festerm-sessiond",
                "festerm-serial", "cargo-packager-updater", "festerm",
            ):
                with self.subTest(package=package, dependency=dependency):
                    output = f"{package} v0.11.1\n{dependency} v1.0.0\n"
                    with patch.object(build, "run", return_value=subprocess.CompletedProcess([], 0, output)):
                        with self.assertRaisesRegex(RuntimeError, "dependency boundary violated"):
                            build.check_dependencies("aarch64-apple-ios", package)

    def test_ios_allows_only_the_protected_keychain_store_feature(self):
        for package in ("festerm-mobile", "festerm-ssh"):
            with self.subTest(package=package):
                allowed = f"{package} v0.11.1\napple-native-keyring-store v1.0.2 protected,security-framework\n"
                with patch.object(build, "run", return_value=subprocess.CompletedProcess([], 0, allowed)):
                    build.check_dependencies("aarch64-apple-ios", package)
        for features in ("keychain", "keychain,protected", ""):
            with self.subTest(features=features):
                output = f"festerm-ssh v0.11.1\napple-native-keyring-store v1.0.2 {features}\n"
                with patch.object(build, "run", return_value=subprocess.CompletedProcess([], 0, output)):
                    with self.assertRaisesRegex(RuntimeError, "dependency boundary violated"):
                        build.check_dependencies("aarch64-apple-ios", "festerm-ssh")

    def test_dependency_cli_checks_both_real_roots_on_device_and_simulator(self):
        with patch.object(build.sys, "argv", ["build", "--check-dependencies"]), \
                patch.object(build, "check_dependencies") as check, \
                patch.object(build, "run") as run:
            self.assertEqual(build.main(), 0)
        self.assertEqual([call.args for call in check.call_args_list], [
            ("aarch64-apple-ios",), ("aarch64-apple-ios", "festerm-ssh"), ("aarch64-apple-ios", "festerm-mobile-profiles"), ("aarch64-apple-ios", "festerm-sftp"),
            ("aarch64-apple-ios-sim",), ("aarch64-apple-ios-sim", "festerm-ssh"), ("aarch64-apple-ios-sim", "festerm-mobile-profiles"), ("aarch64-apple-ios-sim", "festerm-sftp"),
        ])
        run.assert_not_called()

    def test_failed_cargo_tree_is_never_treated_as_an_empty_allowed_graph(self):
        failure = subprocess.CalledProcessError(1, ["cargo", "tree"])
        with patch.object(build, "run", side_effect=failure):
            with self.assertRaises(subprocess.CalledProcessError):
                build.check_dependencies("aarch64-apple-ios", "festerm-ssh")


if __name__ == "__main__":
    unittest.main()
