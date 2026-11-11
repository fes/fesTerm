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
    def test_native_root_factory_hook_is_debug_only_and_scalar(self):
        root = SCRIPT.parents[1]
        entry = (root / "app/festerm-android/src/lib.rs").read_text()
        self.assertIn('feature = "native-recording-validation",\n    debug_assertions', entry)
        source = (root / "app/festerm-android/src/recording_validation.rs").read_text()
        body = source.split("RecordingNativeTest_applicationPrivateRoot(", 1)[1].split(
            '#[unsafe(no_mangle)]', 1
        )[0]
        self.assertEqual(body.count("NativeAppPrivateRoot::current()"), 2)
        self.assertIn("first.same_root(&second)", body)
        self.assertIn("open_host_approval_directory(true)", body)
        self.assertIn("open_host_approval_directory(false)", body)
        self.assertNotIn("JString", body)
        self.assertNotIn("eprintln!", body)
        self.assertIn("Ok(Err(())) => 1", body)
        self.assertIn("Err(_) => 2", body)

    def test_native_root_case_cannot_pass_a_missing_or_refused_hook(self):
        source = (SCRIPT.parents[1] / "app/festerm-android/tests/java/io/github/fes/"
                  "festerm/tests/RootFactorySmoke.java").read_text()
        self.assertIn('hook.getMethod("applicationPrivateRoot").invoke(null)', source)
        self.assertIn("if (!Integer.valueOf(0).equals(status)) throw new AssertionError();", source)
        self.assertIn("FAIL: actual Rust current-app root factory", source)
        self.assertIn('domain.startsWith("u:r:untrusted_app:', source)

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
