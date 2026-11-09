import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "build-android.py"
SPEC = importlib.util.spec_from_file_location("android_build", SCRIPT)
android = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(android)


class AndroidBuildTests(unittest.TestCase):
    def test_shared_graph_refuses_desktop_services(self):
        shared = "festerm-mobile v1\nfesterm-core v1\nfesterm-ui-egui v1\n"
        android.check_dependency_tree(shared + "festerm-ssh v1\n")
        for forbidden in android.FORBIDDEN:
            with self.subTest(forbidden=forbidden), self.assertRaises(ValueError):
                android.check_dependency_tree(shared + forbidden + " v1\n")
        with self.assertRaises(ValueError):
            android.check_dependency_tree("festerm-core v1")

    def test_cached_jvm_dependency_is_checked_before_use(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cache = root / "target/android-dependencies"
            cache.mkdir(parents=True)
            artifact = cache / "verifier.aar"
            artifact.write_bytes(b"expected")
            manifest = {
                "rustls_android_version": "0.2.0",
                "artifacts": [{"name": artifact.name,
                              "url": "https://example.invalid/never-downloaded",
                              "sha256": hashlib.sha256(b"expected").hexdigest()}],
            }
            (root / "dependencies.json").write_text(json.dumps(manifest))
            (root / "Cargo.lock").write_text(
                '[[package]]\nname="rustls-platform-verifier-android"\nversion="0.2.0"\n')
            with patch.object(android, "ROOT", root), patch.object(android, "APP", root), \
                    patch.object(android.urllib.request, "urlopen") as download:
                self.assertEqual(android.native_dependencies(), [artifact])
                artifact.write_bytes(b"tampered")
                with self.assertRaisesRegex(ValueError, "checksum"):
                    android.native_dependencies()
                (root / "Cargo.lock").write_text(
                    '[[package]]\nname="rustls-platform-verifier-android"\nversion="0.3.0"\n')
                with self.assertRaisesRegex(ValueError, "Cargo.lock"):
                    android.native_dependencies()
                download.assert_not_called()

    def arguments(self, **overrides):
        values = dict(release=True, bundle=True, version_code=1001, abi=None, offline=False,
                      recording_validation=False, install=False)
        values.update(overrides)
        return android.argparse.Namespace(**values)

    def test_bundle_arguments_require_release_version_code_and_all_abis(self):
        android.validate_arguments(self.arguments())
        android.validate_arguments(self.arguments(bundle=False, version_code=None, abi="x86_64"))
        for overrides, message in (
            ({"release": False}, "requires --release"),
            ({"version_code": None}, "--version-code"),
            ({"version_code": 0}, "between"),
            ({"version_code": android.MAX_VERSION_CODE + 1}, "between"),
            ({"abi": "arm64-v8a"}, "--abi"),
            ({"offline": True}, "--offline"),
            ({"install": True}, "--install"),
            ({"recording_validation": True}, "Recording validation"),
        ):
            with self.subTest(overrides=overrides), self.assertRaisesRegex(ValueError, message):
                android.validate_arguments(self.arguments(**overrides))

    def test_release_version_must_match_workspace(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.16.0"\n')
            manifest = root / "AndroidManifest.xml"
            with patch.object(android, "ROOT", root), patch.object(android, "APP", root):
                manifest.write_text('<manifest android:versionName="0.16.0" />')
                self.assertEqual(android.check_release_version(), "0.16.0")
                manifest.write_text('<manifest android:versionName="0.15.0" />')
                with self.assertRaisesRegex(ValueError, "does not match"):
                    android.check_release_version()

    def test_base_module_uses_bundletool_layout_and_rejects_unknown_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "vendor/egui-winit").mkdir(parents=True)
            (root / "vendor/egui-winit/LICENSE-APACHE").write_text("license")
            (root / "NOTICE.txt").write_text("notice")
            dex = root / "dex"
            dex.mkdir()
            (dex / "classes.dex").write_bytes(b"dex")
            libraries = []
            for abi in android.TARGETS:
                library = root / f"{abi}.so"
                library.write_bytes(abi.encode())
                libraries.append((abi, library))
            proto = root / "proto.apk"
            with android.zipfile.ZipFile(proto, "w") as archive:
                archive.writestr("AndroidManifest.xml", "manifest")
                archive.writestr("resources.pb", "table")
                archive.writestr("res/xml/config.xml", "xml")
            base = root / "base.zip"
            with patch.object(android, "ROOT", root), patch.object(android, "APP", root):
                android.write_base_module(proto, base, dex, libraries)
                with android.zipfile.ZipFile(base) as module:
                    self.assertEqual(sorted(module.namelist()), sorted([
                        "manifest/AndroidManifest.xml", "resources.pb", "res/xml/config.xml",
                        "dex/classes.dex", "lib/arm64-v8a/libfesterm_android.so",
                        "lib/x86_64/libfesterm_android.so", "assets/NOTICE.txt",
                        "assets/licenses/Apache-2.0.txt"]))
                    self.assertEqual(module.read("lib/x86_64/libfesterm_android.so"), b"x86_64")
                with android.zipfile.ZipFile(proto, "a") as archive:
                    archive.writestr("classes.dex", "unexpected")
                with self.assertRaisesRegex(ValueError, "Unexpected aapt2 proto entry"):
                    android.write_base_module(proto, base, dex, libraries)


    def test_pinned_download_retries_transient_server_errors_only(self):
        def failure(code):
            return android.urllib.error.HTTPError("https://x", code, "err", {}, None)
        ok = io.BytesIO(b"jar")
        with patch.object(android.time, "sleep") as sleep, \
                patch.object(android.urllib.request, "urlopen", side_effect=[failure(504), ok]):
            self.assertEqual(android.download("https://x", 10), b"jar")
            sleep.assert_called_once()
        with patch.object(android.time, "sleep"), \
                patch.object(android.urllib.request, "urlopen", side_effect=[failure(404)]) as opener, \
                self.assertRaises(android.urllib.error.HTTPError):
            android.download("https://x", 10)
        self.assertEqual(opener.call_count, 1)

if __name__ == "__main__":
    unittest.main()
