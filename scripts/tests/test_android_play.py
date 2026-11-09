import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "build-android-play.py"
SPEC = importlib.util.spec_from_file_location("android_play", SCRIPT)
play = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(play)
CERT = ":".join(["AB"] * 32)
CERT_OUTPUT = f"SHA256: {CERT}\n"


class AndroidPlayTests(unittest.TestCase):
    def test_release_manifest_rejects_wrong_identity_sdk_and_debuggable(self):
        xml = (play.android.APP / "AndroidManifest.xml").read_text()
        root = ET.fromstring(xml)
        version = root.get(play.ANDROID + "versionName")
        code = int(root.get(play.ANDROID + "versionCode"))
        self.assertEqual(play.manifest_details(xml, version), (version, code))
        invalid = (
            xml.replace(play.PACKAGE, "io.github.fes.other"),
            xml.replace(f'android:targetSdkVersion="{play.android.API}"', 'android:targetSdkVersion="1"'),
            xml.replace(f'android:versionCode="{code}"', 'android:versionCode="0"'),
            xml.replace("<application ", '<application android:debuggable="true" '),
        )
        for manifest in invalid:
            with self.subTest(manifest=manifest[:100]), self.assertRaises(ValueError):
                play.manifest_details(manifest, version)
        with self.assertRaises(ValueError):
            play.manifest_details(xml, "99.0.0")

    def test_bundletool_cache_is_pinned_and_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "app/festerm-android").mkdir(parents=True)
            tools = root / "target/android-tools"
            tools.mkdir(parents=True)
            jar = tools / "bundletool-all-1.2.3.jar"
            jar.write_bytes(b"verified")
            pin = {
                "version": "1.2.3",
                "url": "https://github.com/google/bundletool/releases/download/1.2.3/bundletool-all-1.2.3.jar",
                "sha256": hashlib.sha256(b"verified").hexdigest(),
            }
            manifest = root / "app/festerm-android/dependencies.json"
            manifest.write_text(json.dumps({"bundletool": pin}))
            self.assertEqual(play.bundletool(root), (jar, "1.2.3"))
            jar.unlink()
            transient = play.urllib.error.HTTPError(pin["url"], 504, "timeout", {}, None)
            with patch.object(play.urllib.request, "urlopen", side_effect=[transient, io.BytesIO(b"verified")]), \
                    patch.object(play.time, "sleep") as sleep:
                self.assertEqual(play.bundletool(root), (jar, "1.2.3"))
                sleep.assert_called_once_with(5)
            jar.write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "checksum"):
                play.bundletool(root)
            pin["url"] = "https://example.invalid/tool.jar"
            manifest.write_text(json.dumps({"bundletool": pin}))
            with self.assertRaisesRegex(ValueError, "URL"):
                play.bundletool(root)

    def test_signature_rejects_unsigned_and_unexpected_certificate_errors(self):
        self_signed = (
            "jar verified, with signer errors.\n\nError:\n"
            "This jar contains entries whose certificate chain is invalid. Reason: self-signed\n"
            "This jar contains entries whose signer certificate is self-signed.\n\nWarning:\n"
        )
        with patch.object(play, "run", return_value=CERT_OUTPUT), \
                patch.object(play.subprocess, "run") as verify:
            verify.return_value = subprocess.CompletedProcess([], 4, self_signed, "")
            play.verify_signed_bundle(Path("candidate.aab"), "AB" * 32, {})
            verify.return_value = subprocess.CompletedProcess([], 16, "unsigned entries", "")
            with self.assertRaisesRegex(ValueError, "verification failed"):
                play.verify_signed_bundle(Path("candidate.aab"), "AB" * 32, {})
            verify.return_value = subprocess.CompletedProcess(
                [], 4, self_signed.replace("\n\nWarning:", "\nThis certificate has expired.\n\nWarning:"), "",
            )
            with self.assertRaisesRegex(ValueError, "certificate errors"):
                play.verify_signed_bundle(Path("candidate.aab"), "AB" * 32, {})
            verify.return_value = subprocess.CompletedProcess([], 0, "jar verified", "")
            with self.assertRaisesRegex(ValueError, "does not match"):
                play.verify_signed_bundle(Path("candidate.aab"), "CD" * 32, {})

    def test_upload_identity_rejects_development_or_wrong_key(self):
        with tempfile.TemporaryDirectory() as directory:
            keystore = Path(directory) / "upload.p12"
            keystore.write_bytes(b"fixture")
            env = {"ANDROID_UPLOAD_KEYSTORE_PASSWORD": "fixture-only"}
            with patch.object(play, "run", return_value=(
                "Entry type: PrivateKeyEntry\nSubject Public Key Algorithm: 2048-bit RSA key\n" + CERT_OUTPUT
            )):
                self.assertEqual(play.upload_identity(keystore, "upload", env), "AB" * 32)
                with self.assertRaisesRegex(ValueError, "non-development"):
                    play.upload_identity(keystore, "androiddebugkey", env)
            with patch.object(play, "run", return_value=(
                "Entry type: PrivateKeyEntry\nSubject Public Key Algorithm: 1024-bit RSA key\n" + CERT_OUTPUT
            )):
                with self.assertRaisesRegex(ValueError, "at least 2048"):
                    play.upload_identity(keystore, "upload", env)

    def test_native_libraries_require_matching_abi_and_16k_segments(self):
        header = "Machine: AArch64\n"
        aligned = "  LOAD 0x0000 0x0000 0x0000 0x1000 0x1000 R E 0x4000\n"
        with patch.object(play, "run", side_effect=[header, aligned]):
            play.verify_native_library(Path("arm.so"), "arm64-v8a", Path("/sdk"), {})
        with patch.object(play, "run", side_effect=[header, aligned.replace("0x4000", "0x1000")]):
            with self.assertRaisesRegex(ValueError, "16-KiB"):
                play.verify_native_library(Path("arm.so"), "arm64-v8a", Path("/sdk"), {})
        with patch.object(play, "run", return_value=header):
            with self.assertRaisesRegex(ValueError, "architecture"):
                play.verify_native_library(Path("arm.so"), "x86_64", Path("/sdk"), {})

    def test_bundle_keeps_reviewed_notices_and_exact_native_libraries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path, data in (
                ("app/festerm-android/NOTICE.txt", b"host notice"),
                ("vendor/egui-winit/LICENSE-APACHE", b"apache"),
                ("packaging/THIRD_PARTY_NOTICES.txt", b"reviewed"),
                ("classes.dex", b"dex"),
                ("arm.so", b"arm"),
                ("intel.so", b"intel"),
            ):
                destination = root / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(data)
            resources = root / "resources.zip"
            with zipfile.ZipFile(resources, "w") as archive:
                archive.writestr("AndroidManifest.xml", b"proto manifest")
                archive.writestr("resources.pb", b"proto resources")
            libraries = {"arm64-v8a": root / "arm.so", "x86_64": root / "intel.so"}
            module = root / "module.zip"
            play.make_module(module, resources, [root / "classes.dex"], libraries, root)
            bundle = root / "bundle.aab"
            with zipfile.ZipFile(module) as source, zipfile.ZipFile(bundle, "w") as target:
                for name in source.namelist():
                    target.writestr("base/" + name, source.read(name))
            play.verify_bundle_contents(bundle, libraries, root)
            (root / "packaging/THIRD_PARTY_NOTICES.txt").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "reviewed notices"):
                play.verify_bundle_contents(bundle, libraries, root)
            (root / "packaging/THIRD_PARTY_NOTICES.txt").write_bytes(b"reviewed")
            with zipfile.ZipFile(bundle, "a") as archive:
                archive.writestr("base/lib/other/unknown.so", b"unexpected")
            with self.assertRaisesRegex(ValueError, "unexpected native"):
                play.verify_bundle_contents(bundle, libraries, root)


if __name__ == "__main__":
    unittest.main()
