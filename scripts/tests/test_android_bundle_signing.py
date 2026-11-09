import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "sign-android-bundle.py"
SPEC = importlib.util.spec_from_file_location("android_bundle_signing", SCRIPT)
signing = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(signing)


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
                    ({signing.UPLOAD_KEYSTORE: str(root / "none")}, "does not exist"),
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


if __name__ == "__main__":
    unittest.main()
