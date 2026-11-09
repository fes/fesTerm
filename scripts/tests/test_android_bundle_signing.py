import hashlib
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "sign-android-bundle.py"
SPEC = importlib.util.spec_from_file_location("android_bundle_signing", SCRIPT)
signing = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signing)
CERT = ":".join(["AB"] * 32)
CERT_OUTPUT = f"SHA256: {CERT}\n"


class BundleSigningTests(unittest.TestCase):
    def test_upload_signing_requires_environment_and_refuses_development_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            development = root / "target/android/development.keystore"
            development.parent.mkdir(parents=True)
            development.write_bytes(b"debug")
            upload = root / "upload.p12"
            upload.write_bytes(b"upload")
            complete = {signing.UPLOAD_KEYSTORE: str(upload), signing.UPLOAD_PASSWORD: "secret",
                        signing.UPLOAD_ALIAS: "festerm-upload"}
            with patch.object(signing, "ROOT", root):
                with patch.dict(signing.os.environ, complete, clear=True):
                    self.assertEqual(signing.upload_signing(), (upload.resolve(), "festerm-upload"))
                for override, message in (
                    ({signing.UPLOAD_PASSWORD: ""}, signing.UPLOAD_PASSWORD),
                    ({signing.UPLOAD_KEYSTORE: str(development)}, "development keystore"),
                    ({signing.UPLOAD_KEYSTORE: str(root / "none")}, "regular file"),
                    ({signing.UPLOAD_ALIAS: "androiddebugkey"}, "non-development"),
                ):
                    with self.subTest(message=message), \
                            patch.dict(signing.os.environ, {**complete, **override}, clear=True), \
                            self.assertRaisesRegex(ValueError, message):
                        signing.upload_signing()

    def test_bundle_from_build_job_is_hash_checked_before_the_key_is_used(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            unsigned = root / "fesTerm-unsigned.aab"
            unsigned.write_bytes(b"aab")
            calls = []
            with self.assertRaisesRegex(ValueError, "does not match"):
                signing.sign(unsigned, root / "fesTerm.aab", "0" * 64, runner=calls.append)
            self.assertEqual(calls, [])

    def test_existing_signed_bundle_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            unsigned = root / "unsigned.aab"
            signed = root / "signed.aab"
            unsigned.write_bytes(b"unsigned")
            signed.write_bytes(b"keep")
            with self.assertRaisesRegex(ValueError, "overwrite"):
                signing.sign(unsigned, signed, hashlib.sha256(b"unsigned").hexdigest())
            self.assertEqual(signed.read_bytes(), b"keep")

    def test_upload_identity_requires_an_adequate_private_rsa_key(self):
        with patch.object(signing, "run", return_value=(
            "Entry type: PrivateKeyEntry\nSubject Public Key Algorithm: 2048-bit RSA key\n" + CERT_OUTPUT
        )):
            self.assertEqual(signing.upload_identity(Path("upload.p12"), "upload", {}), "AB" * 32)
        with patch.object(signing, "run", return_value=(
            "Entry type: PrivateKeyEntry\nSubject Public Key Algorithm: 1024-bit RSA key\n" + CERT_OUTPUT
        )):
            with self.assertRaisesRegex(ValueError, "at least 2048"):
                signing.upload_identity(Path("upload.p12"), "upload", {})

    def test_signature_rejects_unsigned_and_unexpected_certificate_errors(self):
        self_signed = (
            "jar verified, with signer errors.\n\nError:\n"
            "This jar contains entries whose certificate chain is invalid. Reason: self-signed\n"
            "This jar contains entries whose signer certificate is self-signed.\n\nWarning:\n"
        )
        with patch.object(signing, "run", return_value=CERT_OUTPUT), \
                patch.object(signing.subprocess, "run") as verify:
            verify.return_value = subprocess.CompletedProcess([], 4, self_signed, "")
            signing.verify_signed_bundle(Path("candidate.aab"), "AB" * 32, {})
            verify.return_value = subprocess.CompletedProcess([], 16, "unsigned entries", "")
            with self.assertRaisesRegex(ValueError, "verification failed"):
                signing.verify_signed_bundle(Path("candidate.aab"), "AB" * 32, {})
            verify.return_value = subprocess.CompletedProcess(
                [], 4, self_signed.replace("\n\nWarning:", "\nThis certificate has expired.\n\nWarning:"), "",
            )
            with self.assertRaisesRegex(ValueError, "certificate errors"):
                signing.verify_signed_bundle(Path("candidate.aab"), "AB" * 32, {})
            verify.return_value = subprocess.CompletedProcess([], 0, "jar verified", "")
            with self.assertRaisesRegex(ValueError, "does not match"):
                signing.verify_signed_bundle(Path("candidate.aab"), "CD" * 32, {})


if __name__ == "__main__":
    unittest.main()
