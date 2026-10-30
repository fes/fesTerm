"""Portable admission and packaging regressions; no Apple credentials required."""
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "build-ios-device.py"
SPEC = importlib.util.spec_from_file_location("ios_device_build", SCRIPT)
device = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(device)
CERT = b"synthetic certificate bytes"
IDENTITY = hashlib.sha1(CERT).hexdigest().upper()
TEAM = "TESTTEAM01"
BUNDLE = "org.example.festerm-prototype"
UDID = "synthetic-device-udid"
NOW = datetime(2026, 10, 30, tzinfo=timezone.utc)


def profile():
    return {"ExpirationDate": datetime(2099, 1, 1), "Platform": ["iOS"],
            "TeamIdentifier": [TEAM], "ApplicationIdentifierPrefix": ["OLDPREFIX1"],
            "DeveloperCertificates": [CERT], "ProvisionedDevices": [UDID],
            "Entitlements": {"application-identifier": f"OLDPREFIX1.{BUNDLE}",
                             "com.apple.developer.team-identifier": TEAM,
                             "get-task-allow": True,
                             "keychain-access-groups": ["must-not-be-inherited"],
                             "com.apple.security.application-groups": ["must-not-be-inherited"]}}


class DeviceBuildTests(unittest.TestCase):
    def admit(self, payload):
        return device.development_entitlements(payload, BUNDLE, TEAM, IDENTITY, UDID, NOW)

    def test_explicit_profile_accepts_legacy_prefix_without_inheriting_capabilities(self):
        self.assertEqual(self.admit(profile()), {
            "application-identifier": f"OLDPREFIX1.{BUNDLE}",
            "com.apple.developer.team-identifier": TEAM, "get-task-allow": True})

    def test_profile_refuses_expiry_distribution_wrong_team_device_certificate_and_app(self):
        for field, value in [("ExpirationDate", datetime(2020, 1, 1)),
                             ("ExpirationDate", "invalid"), ("Platform", ["tvOS"]),
                             ("TeamIdentifier", ["OTHERTEAM1"]), ("ProvisionedDevices", []),
                             ("ProvisionsAllDevices", True), ("DeveloperCertificates", [b"other"]),
                             ("ApplicationIdentifierPrefix", [])]:
            payload = profile()
            payload[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.admit(payload)
        for field, value in [("get-task-allow", False), ("get-task-allow", 1),
                             ("application-identifier", "OLDPREFIX1.org.example.*"),
                             ("application-identifier", "OLDPREFIX1.org.example.other"),
                             ("com.apple.developer.team-identifier", "OTHERTEAM1")]:
            payload = profile()
            payload["Entitlements"][field] = value
            with self.subTest(entitlement=field), self.assertRaises(ValueError):
                self.admit(payload)
        with self.assertRaises(ValueError):
            self.admit([])

    def test_exact_expiry_and_wildcard_or_shell_like_identifiers_are_refused(self):
        payload = profile()
        payload["ExpirationDate"] = NOW
        with self.assertRaises(ValueError):
            self.admit(payload)
        for bundle, team, identity in [("org.example.*", TEAM, IDENTITY),
                                       ("org.example;echo", TEAM, IDENTITY),
                                       (BUNDLE, "wrong", IDENTITY), (BUNDLE, TEAM, "-")]:
            with self.assertRaises(ValueError):
                device.development_entitlements(profile(), bundle, team, identity, UDID, NOW)

    def test_identity_must_be_a_valid_local_private_key_identity(self):
        for output, accepted in [(f'  1) {IDENTITY} "Apple Development: Synthetic"\n', True),
                                 ('0 valid identities found\n', False)]:
            with patch.object(device, "run", return_value=subprocess.CompletedProcess([], 0, output)):
                if accepted:
                    device.require_local_identity(IDENTITY.lower())
                else:
                    with self.assertRaises(ValueError):
                        device.require_local_identity(IDENTITY)

    def exercise(self, root, preflight=False, fail_signing=False):
        local_profile = root / "fixture.mobileprovision"
        local_profile.write_bytes(b"synthetic signed profile")
        calls = []

        def run(*args, **kwargs):
            calls.append((args, kwargs))
            if args[:3] == ("security", "cms", "-D"):
                self.assertEqual(kwargs["input"], b"synthetic signed profile")
                return subprocess.CompletedProcess(args, 0, plistlib.dumps(profile()))
            if args[:2] == ("security", "find-identity"):
                return subprocess.CompletedProcess(args, 0, f' 1) {IDENTITY} "Synthetic"')
            if args[:2] == ("cargo", "build"):
                binary = root / "target" / device.TARGET / "debug" / "festerm-mobile"
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b"synthetic executable")
                binary.chmod(0o755)
                self.assertEqual(kwargs["env"]["SDKROOT"], "/synthetic/iphoneos.sdk")
            if args[0] == "codesign" and fail_signing:
                raise subprocess.CalledProcessError(1, args)
            stdout = "/synthetic/iphoneos.sdk" if "--show-sdk-path" in args else "fixture"
            if args[:2] == ("git", "status"):
                stdout = ""
            return subprocess.CompletedProcess(args, 0, stdout)

        arguments = ["--profile", str(local_profile), "--identity", IDENTITY, "--team", TEAM,
                     "--bundle-id", BUNDLE, "--device", UDID]
        if preflight:
            arguments.append("--preflight-only")
        with patch.object(device, "ROOT", root), \
                patch.object(device.platform, "system", return_value="Darwin"), \
                patch.object(device, "run", side_effect=run), \
                patch.object(device.spike, "check_dependencies") as graph:
            if fail_signing:
                with self.assertRaises(subprocess.CalledProcessError):
                    device.main(arguments)
            else:
                self.assertEqual(device.main(arguments), 0)
            graph.assert_called_once_with("aarch64-apple-ios")
        return calls

    def test_preflight_never_builds_signs_or_creates_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = self.exercise(root, preflight=True)
            self.assertFalse((root / "target").exists())
            self.assertFalse(any(args[0] in ("cargo", "rustup", "codesign") for args, _ in calls))

    def test_device_bundle_is_distinct_private_and_receipt_cannot_claim_native_acceptance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            calls = self.exercise(root)
            output = next((root / "target/ios-device").iterdir())
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            bundle = output / "fesTermSpike.app"
            info = plistlib.loads((bundle / "Info.plist").read_bytes())
            self.assertEqual(info["CFBundleSupportedPlatforms"], ["iPhoneOS"])
            self.assertEqual(info["UIDeviceFamily"], [1, 2])
            self.assertEqual(info["CFBundleIdentifier"], BUNDLE)
            self.assertEqual((bundle / "embedded.mobileprovision").read_bytes(), b"synthetic signed profile")
            self.assertTrue((bundle / "festerm-mobile").stat().st_mode & 0o111)
            receipt_text = (output / "build-receipt.json").read_text()
            receipt = json.loads(receipt_text)
            self.assertEqual(receipt["status"], "signed")
            self.assertEqual(receipt["native_validation"], "not-run")
            self.assertFalse(receipt["source_dirty"])
            self.assertEqual(receipt["executable_sha256"], hashlib.sha256(b"synthetic executable").hexdigest())
            for private in (UDID, IDENTITY, TEAM, "synthetic signed profile"):
                self.assertNotIn(private, receipt_text)
            self.assertEqual([args[:2] for args, _ in calls if args[0] == "codesign"],
                             [("codesign", "--force"), ("codesign", "--verify")])
            self.assertFalse(any("simctl" in args or "devicectl" in args for args, _ in calls))
            self.assertEqual(device.spike.bundle_info()["CFBundleSupportedPlatforms"], ["iPhoneSimulator"])

    def test_failed_signing_never_leaves_a_signed_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.exercise(root, fail_signing=True)
            receipt = json.loads(next((root / "target/ios-device").glob("*/build-receipt.json")).read_text())
            self.assertEqual(receipt["status"], "signing-pending")
            self.assertEqual(receipt["native_validation"], "not-run")

    def test_all_device_subprocesses_have_a_deadline(self):
        with patch.object(device.subprocess, "run") as process:
            device.run("fixture")
            self.assertEqual(process.call_args.kwargs["timeout"], 120)
            self.assertTrue(process.call_args.kwargs["check"])
            device.run("fixture", timeout=1800)
            self.assertEqual(process.call_args.kwargs["timeout"], 1800)


if __name__ == "__main__":
    unittest.main()
