import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "build-android.py"
SPEC = importlib.util.spec_from_file_location("android_build", SCRIPT)
android = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(android)


class AndroidBuildTests(unittest.TestCase):
    def test_host_clients_graph_is_explicit_and_preserves_other_modes(self):
        for offline, hosts, expected in [
            (False, False, []),
            (True, False, ["--no-default-features"]),
            (False, True, ["--no-default-features", "--features", "host-clients"]),
        ]:
            with self.subTest(offline=offline, hosts=hosts), \
                    patch.object(Path, "is_file", return_value=True), \
                    patch.object(android.shutil, "which", return_value="existing-tool"), \
                    patch.object(android, "run", return_value=(
                        "festerm-mobile v1\nfesterm-core v1\nfesterm-ui-egui v1\n")) as run:
                *_, features = android.target_environment(
                    "/synthetic-sdk", "arm64-v8a", offline=offline, host_clients=hosts)
                self.assertEqual(features, expected)
                self.assertEqual(list(run.call_args.args)[-len(expected):] if expected else [], expected)
        with patch.object(Path, "is_file", return_value=True), \
                patch.object(android.shutil, "which", return_value="existing-tool"), \
                patch.object(android, "run", return_value=(
                    "festerm-mobile v1\nfesterm-core v1\nfesterm-ui-egui v1\n")):
            *_, features = android.target_environment(
                "/synthetic-sdk", "arm64-v8a", host_clients=True, recording_validation=True)
        self.assertEqual(features, ["--no-default-features", "--features", "host-clients",
                                    "--features", "native-recording-validation"])

    def test_offline_host_clients_conflict_fails_before_build_or_sdk_access(self):
        with patch.object(android, "build") as build, \
                patch("sys.stderr", new_callable=io.StringIO), self.assertRaises(SystemExit) as error:
            android.main(["--offline", "--host-clients"])
        self.assertEqual(error.exception.code, 2)
        build.assert_not_called()
        with patch.object(Path, "is_file") as sdk, self.assertRaisesRegex(ValueError, "offline"):
            android.target_environment("/not-inspected", "arm64-v8a", offline=True, host_clients=True)
        sdk.assert_not_called()

    def test_packaged_modes_record_features_and_do_not_replace_other_mode_apks(self):
        for offline, hosts, output_mode, prototype, selected, defaults in [
            (False, False, "live", "live-ssh", ["live-ssh"], True),
            (True, False, "offline", "offline-fixture", [], False),
            (False, True, "host-clients", "host-clients", ["host-clients"], False),
        ]:
            with self.subTest(mode=prototype), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for name in ["app/NOTICE.txt", "vendor/egui-winit/LICENSE-APACHE",
                             "packaging/THIRD_PARTY_NOTICES.txt", "target/android/development.keystore",
                             "library.so"]:
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b"fixture")
                untouched = root / "target/android/arm64-v8a" / ("live" if hosts else "host-clients") / "fesTerm.apk"
                untouched.parent.mkdir(parents=True)
                untouched.write_bytes(b"retain-other-mode")
                def compile_app(work, *_args, **_kwargs):
                    unsigned = work / "unsigned.apk"
                    with zipfile.ZipFile(unsigned, "w"):
                        pass
                    dex = work / "classes.dex"
                    dex.write_bytes(b"dex")
                    return unsigned, [dex]
                def run(*args, **_kwargs):
                    if args[0] == "git":
                        return "a" * 40 if args[1] == "rev-parse" else ""
                    if str(args[0]).endswith("zipalign"):
                        shutil.copy2(args[-2], args[-1])
                    if str(args[0]).endswith("apksigner") and args[1] == "sign":
                        shutil.copy2(args[-1], args[args.index("--out") + 1])
                flags = (["--no-default-features", "--features", "host-clients"] if hosts
                         else ["--no-default-features"] if offline else [])
                args = android.argparse.Namespace(recording_validation=False, release=False,
                    sdk="/synthetic-sdk", abi="arm64-v8a", offline=offline, host_clients=hosts,
                    check_dependencies=False, install=False, serial=None)
                with patch.object(android, "ROOT", root), patch.object(android, "APP", root / "app"), \
                        patch.object(android, "target_environment", return_value=(
                            root / "tools", root / "android.jar", "aarch64-linux-android", {}, flags)) as target, \
                        patch.object(android, "native_dependencies", return_value=[]), \
                        patch.object(android, "build_library", return_value=root / "library.so") as library, \
                        patch.object(android, "compile_app", side_effect=compile_app), \
                        patch.object(android, "run", side_effect=run), patch("sys.stdout", new_callable=io.StringIO):
                    android.build(args)
                self.assertEqual(target.call_args.args[-1], hosts)
                self.assertEqual(library.call_args.args[2], flags)
                output = root / "target/android/arm64-v8a" / output_mode
                receipt = json.loads((output / "build-receipt.json").read_text())
                self.assertEqual(receipt["prototype_mode"], prototype)
                self.assertEqual(receipt["cargo_features"], selected)
                self.assertEqual(receipt["cargo_default_features"], defaults)
                self.assertEqual(receipt["live_ssh"], not offline)
                self.assertEqual(receipt["sha256"], hashlib.sha256((output / "fesTerm.apk").read_bytes()).hexdigest())
                self.assertEqual(untouched.read_bytes(), b"retain-other-mode")

    def test_native_index_hook_selects_only_native_production_storage(self):
        root = SCRIPT.parents[1]
        source = (root / "crates/festerm-tunnel-bridge/src/host_client/"
                  "originals_native_index_validation.rs").read_text()
        native = source.split("fn run_native()", 1)[1].split("async fn capture_uncertain", 1)[0]
        self.assertIn("NativeAppPrivateRoot::current()", native)
        self.assertIn("ReceiptIndex::in_native_app_sandbox", native)
        self.assertIn("index.prepare(reviewed)", native)
        self.assertEqual(native.count("OriginalStore::recover_in_native_app_sandbox"), 2)
        self.assertIn("validation_has_native_root()", native)
        self.assertIn("root.same_root(&fresh)", native)
        for forbidden in ("MemorySecretStore", "in_app_sandbox(", "recover_with", "connect("):
            self.assertNotIn(forbidden, native)
        entry = (root / "crates/festerm-tunnel-bridge/src/host_client/originals.rs").read_text()
        self.assertIn('#[cfg(all(target_os = "android", debug_assertions))]\n'
                      '    pub fn native_index_forget()', entry)

    def test_native_index_case_requires_actual_scalar_hook_success(self):
        root = SCRIPT.parents[1]
        source = (root / "app/festerm-android/tests/java/io/github/fes/festerm/tests/"
                  "NativeIndexForgetSmoke.java").read_text()
        self.assertIn('hook.getMethod("nativeIndexForget").invoke(null)', source)
        self.assertIn("if (!Integer.valueOf(0).equals(status)) throw new AssertionError();", source)
        self.assertIn('domain.startsWith("u:r:untrusted_app:', source)
        self.assertIn("FAIL: actual native index reopen and fixture-only Forget", source)
        self.assertIn('android:name=".NativeIndexForgetSmoke"',
                      (root / "app/festerm-android/tests/AndroidManifest.xml").read_text())
        manifest = (root / "app/festerm-android/Cargo.toml").read_text()
        dependency = next(line for line in manifest.splitlines()
                          if line.startswith("festerm-tunnel-bridge ="))
        self.assertIn('optional = true, default-features = false, features = ["testing"]', dependency)

    def test_validation_packaging_overrides_disabled_rust_debug_assertions(self):
        with patch.object(Path, "is_file", return_value=True), \
                patch.object(android.shutil, "which", return_value="existing-tool"), \
                patch.object(android, "run",
                             return_value="festerm-mobile v1\nfesterm-core v1\nfesterm-ui-egui v1\n"), \
                patch.dict(os.environ, {"CARGO_PROFILE_DEV_DEBUG_ASSERTIONS": "false",
                                        "RUSTFLAGS": "-C debug-assertions=no"}):
            *_, env, features = android.target_environment(
                "/synthetic-existing-sdk", "arm64-v8a", offline=True, recording_validation=True)
        self.assertEqual(env["CARGO_PROFILE_DEV_DEBUG_ASSERTIONS"], "true")
        self.assertTrue(env["RUSTFLAGS"].endswith("-C debug-assertions=yes"))
        self.assertIn("native-recording-validation", features)

    def test_validation_packaging_refuses_missing_linked_hooks_and_release(self):
        with patch.object(android, "run", return_value="") as run:
            with self.assertRaisesRegex(ValueError, "Missing linked"):
                android.check_native_validation_exports(Path("fixture.so"), {"ANDROID_NDK_HOME": "/owned/ndk"})
            with self.assertRaisesRegex(ValueError, "release APKs"):
                android.build_library("aarch64-linux-android", {}, [], True, True)
            self.assertEqual(run.call_count, 1)
        symbols = "\n".join("0000 T " + name for name in android.NATIVE_VALIDATION_SYMBOLS)
        with patch.object(android, "run", return_value=symbols):
            android.check_native_validation_exports(Path("fixture.so"), {"ANDROID_NDK_HOME": "/owned/ndk"})
        with patch.object(android, "target_environment") as target:
            with self.assertRaisesRegex(ValueError, "release APKs"):
                android.build(android.argparse.Namespace(recording_validation=True, release=True))
            target.assert_not_called()

    def test_native_instrumentation_build_only_never_queries_or_installs_adb(self):
        source = (SCRIPT.parent / "smoke-android.py").read_text()
        self.assertIn('if not args.build_only and build.run(', source)
        self.assertIn('if args.build_only:\n        print(apk)\n        return\n'
                      '    build.run(*adb, "install", "-r", apk)', source)

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
