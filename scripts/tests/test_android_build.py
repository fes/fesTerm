import hashlib
import importlib.util
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


if __name__ == "__main__":
    unittest.main()
