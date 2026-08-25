import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "generate_update_manifest.py"
SPEC = importlib.util.spec_from_file_location("generate_update_manifest", SCRIPT)
manifest = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(manifest)


class UpdateManifestTests(unittest.TestCase):
    def test_manifest_requires_and_embeds_the_signature(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "festerm.AppImage"
            artifact.write_bytes(b"signed artifact")
            Path(f"{artifact}.sig").write_text("signature", encoding="utf-8")

            result = manifest.generate(
                "0.2.0",
                "Release notes",
                [
                    (
                        "linux-x86_64",
                        artifact,
                        "https://github.com/fes/fesTerm/releases/download/v0.2.0/festerm.AppImage",
                        "appimage",
                    )
                ],
                "2026-08-24T00:00:00Z",
            )

            self.assertEqual(result["version"], "v0.2.0")
            self.assertEqual(
                result["platforms"]["linux-x86_64"]["signature"],
                "signature",
            )

    def test_manifest_rejects_missing_signatures(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "festerm-setup.exe"
            artifact.write_bytes(b"unsigned artifact")

            with self.assertRaisesRegex(manifest.ManifestError, "signature is missing"):
                manifest.generate(
                    "0.2.0",
                    "",
                    [
                        (
                            "windows-x86_64",
                            artifact,
                            "https://github.com/fes/fesTerm/releases/download/v0.2.0/festerm-setup.exe",
                            "nsis",
                        )
                    ],
                    "2026-08-24T00:00:00Z",
                )

    def test_manifest_rejects_archived_appimage(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "festerm.AppImage.tar.gz"
            artifact.write_bytes(b"archive")
            Path(f"{artifact}.sig").write_text("signature", encoding="utf-8")

            with self.assertRaisesRegex(
                manifest.ManifestError,
                r"must end with \.AppImage",
            ):
                manifest.generate(
                    "0.2.0",
                    "",
                    [
                        (
                            "linux-x86_64",
                            artifact,
                            "https://github.com/fes/fesTerm/releases/download/v0.2.0/festerm.AppImage.tar.gz",
                            "appimage",
                        )
                    ],
                    "2026-08-24T00:00:00Z",
                )

    def test_manifest_rejects_format_for_wrong_platform(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "festerm-setup.exe"
            artifact.write_bytes(b"installer")
            Path(f"{artifact}.sig").write_text("signature", encoding="utf-8")

            with self.assertRaisesRegex(
                manifest.ManifestError,
                "does not support format nsis",
            ):
                manifest.generate(
                    "0.2.0",
                    "",
                    [
                        (
                            "linux-x86_64",
                            artifact,
                            "https://github.com/fes/fesTerm/releases/download/v0.2.0/festerm-setup.exe",
                            "nsis",
                        )
                    ],
                    "2026-08-24T00:00:00Z",
                )

    def test_manifest_rejects_release_url_for_different_version(self):
        with tempfile.TemporaryDirectory() as directory:
            artifact = Path(directory) / "festerm.AppImage"
            artifact.write_bytes(b"appimage")
            Path(f"{artifact}.sig").write_text("signature", encoding="utf-8")

            with self.assertRaisesRegex(
                manifest.ManifestError,
                "immutable fesTerm v0.2.0 GitHub Release URL",
            ):
                manifest.generate(
                    "0.2.0",
                    "",
                    [
                        (
                            "linux-x86_64",
                            artifact,
                            "https://github.com/fes/fesTerm/releases/download/v0.1.0/festerm.AppImage",
                            "appimage",
                        )
                    ],
                    "2026-08-24T00:00:00Z",
                )


if __name__ == "__main__":
    unittest.main()
