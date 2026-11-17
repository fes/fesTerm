"""Portable TestFlight helper checks; no Apple credentials, Xcode or network."""
import importlib.util
import plistlib
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "build-ios-testflight.py"
SPEC = importlib.util.spec_from_file_location("ios_testflight", SCRIPT)
tf = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tf)
ISSUER = "12345678-1234-1234-1234-123456789abc"


class TestFlightTests(unittest.TestCase):
    def test_distribution_build_includes_host_oriented_clients(self):
        self.assertEqual(tf.FEATURES, "host-clients")

    def test_workspace_version_is_semver(self):
        self.assertRegex(tf.workspace_version(), r"^\d+\.\d+\.\d+$")

    def test_build_numbers_are_monotonic_integers(self):
        self.assertLess(int(tf.default_build_number(0)), int(tf.default_build_number(86400)))
        self.assertEqual(tf.validate_build_number("2611011945"), "2611011945")
        for bad in ("0", "01", "1.2", "", "abc", "1" * 19):
            with self.assertRaises(ValueError):
                tf.validate_build_number(bad)

    def test_info_plist_meets_store_requirements(self):
        info = tf.testflight_info("0.13.0", "7")
        tf.validate_info(info, "0.13.0")
        self.assertEqual(info["CFBundleIdentifier"], "com.feslabs.fesTerm")
        self.assertIs(info["ITSAppUsesNonExemptEncryption"], False)
        with self.assertRaises(ValueError):
            tf.validate_info(info, "0.14.0")
        info["UIDeviceFamily"] = [1]
        with self.assertRaises(ValueError):
            tf.validate_info(info, "0.13.0")

    def test_entitlements_reject_development_or_foreign_groups(self):
        good = {"application-identifier": "F884J4784J.com.feslabs.fesTerm",
                "com.apple.developer.team-identifier": "F884J4784J",
                "keychain-access-groups": ["F884J4784J.com.feslabs.fesTerm"]}
        tf.verify_entitlements(good)
        for change in ({"get-task-allow": True}, {"keychain-access-groups": ["OTHER00000.x"]},
                       {"application-identifier": "F884J4784J.other"}):
            with self.assertRaises(ValueError):
                tf.verify_entitlements({**good, **change})

    def test_credentials_come_from_arguments_or_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / "AuthKey_ABCDE12345.p8"
            key.write_text("x")
            env = {"APP_STORE_CONNECT_KEY_PATH": str(key), "APP_STORE_CONNECT_KEY_ID": "ABCDE12345",
                   "APPLE_API_ISSUER": ISSUER}
            self.assertEqual(tf.api_credentials(None, None, None, env), (key, "ABCDE12345", ISSUER))
            for broken in ({}, {**env, "APPLE_API_ISSUER": "nope"}, {**env, "APP_STORE_CONNECT_KEY_ID": "ZZZZZ99999"}):
                with self.assertRaises(ValueError):
                    tf.api_credentials(None, None, None, broken)

    def test_export_is_manual_distribution_without_debug_signing(self):
        options = tf.export_options()
        self.assertEqual(options["method"], "app-store-connect")
        self.assertEqual(options["signingStyle"], "manual")
        self.assertEqual(options["provisioningProfiles"], {"com.feslabs.fesTerm": "fesTerm App Store"})

    def test_distribution_identity_requires_matching_team(self):
        listing = ('  1) 340D629B6BC27BA626F0097045FF2A88B004D83E "Apple Development: A (X)"\n'
                   '  2) 199ED63BF21520901884550CCA8DFFD062DFB3E0 "Apple Distribution: A (F884J4784J)"\n')
        self.assertEqual(tf.distribution_identity(listing), "199ED63BF21520901884550CCA8DFFD062DFB3E0")
        self.assertIsNone(tf.distribution_identity(listing, "OTHER00000"))

    def test_info_plist_adopts_uiscene_with_a_delegate_defined_in_rust(self):
        # iOS 27 terminates apps without a UIScene lifecycle at launch.
        manifest = tf.testflight_info("0.13.0", "7")["UIApplicationSceneManifest"]
        self.assertIs(manifest["UIApplicationSupportsMultipleScenes"], False)
        configs = manifest["UISceneConfigurations"]["UIWindowSceneSessionRoleApplication"]
        name = configs[0]["UISceneDelegateClassName"]
        source = (SCRIPT.parents[1] / "crates/festerm-ios-window/src/ios.rs").read_text()
        self.assertIn(f'#[name = "{name}"]', source)
        self.assertIn("scene:willConnectToSession:options:", source)
        runtime = (SCRIPT.parents[1] / "app/festerm-mobile/src/runtime.rs").read_text()
        self.assertIn("register_scene_delegate()", runtime)
        main = (SCRIPT.parents[1] / "app/festerm-mobile/src/main.rs").read_text()
        self.assertIn("festerm_mobile::run_native()", main)

    def test_icon_source_is_marketing_size(self):
        tf.check_icon()

    def test_exported_ipa_requires_the_exact_reviewed_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            notice = root / "THIRD_PARTY_NOTICES.txt"
            notice.write_bytes(b"reviewed notices")
            ipa = root / "app.ipa"
            entitlements = {
                "application-identifier": "F884J4784J.com.feslabs.fesTerm",
                "com.apple.developer.team-identifier": "F884J4784J",
            }
            info = tf.testflight_info("0.16.0", "123")
            with mock.patch.object(tf, "NOTICES", notice), \
                    mock.patch.object(tf, "OUTPUT", root / "output"), \
                    mock.patch.object(tf, "run", return_value=SimpleNamespace(
                        stdout=plistlib.dumps(entitlements))):
                for content in (None, b"outdated", b"reviewed notices"):
                    with self.subTest(content=content), zipfile.ZipFile(ipa, "w") as archive:
                        archive.writestr("Payload/fesTerm.app/Info.plist", plistlib.dumps(info))
                        if content is not None:
                            archive.writestr("Payload/fesTerm.app/THIRD_PARTY_NOTICES.txt", content)
                    if content == b"reviewed notices":
                        tf.check_ipa(ipa, tf.TEAM_ID, "0.16.0", "123")
                    else:
                        with self.assertRaisesRegex(ValueError, "third-party notices"):
                            tf.check_ipa(ipa, tf.TEAM_ID, "0.16.0", "123")

    def test_upload_does_not_send_an_ipa_that_fails_verification(self):
        ipa = Path("/tmp/0.16.0-123/export/fesTerm.ipa")
        with mock.patch.object(tf.platform, "system", return_value="Darwin"), \
                mock.patch.object(tf, "workspace_version", return_value="0.16.0"), \
                mock.patch.object(tf, "api_credentials", return_value=(Path("/tmp/key"), "KEY", ISSUER)), \
                mock.patch.object(tf, "check_icon"), mock.patch.object(tf, "run"), \
                mock.patch.object(tf, "latest_ipa", return_value=ipa), \
                mock.patch.object(tf, "check_ipa", side_effect=ValueError("missing notices")), \
                mock.patch.object(tf, "upload") as upload:
            with self.assertRaisesRegex(ValueError, "missing notices"):
                tf.main(["--upload", "--build-number", "123"])
            upload.assert_not_called()


if __name__ == "__main__":
    unittest.main()
