import hashlib
import json
import sys
import tempfile
import unittest
from unittest import mock
import urllib.error
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import manage_bundled_font as fonts


class BundledFontTests(unittest.TestCase):
    def test_version_comparison_is_numeric(self):
        self.assertTrue(fonts.update_available("2.304", "2.1000"))
        self.assertFalse(fonts.update_available("2.304", "2.304"))
        self.assertFalse(fonts.update_available("2.304", "2.99"))

    def test_verify_detects_changed_vendored_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            asset = root / "assets/fonts/jetbrains-mono/font.ttf"
            asset.parent.mkdir(parents=True)
            asset.write_bytes(b"official font bytes")
            marker = root / "docs/font.md"
            marker.parent.mkdir()
            marker.write_text("Pinned font 2.304\n", encoding="utf-8")
            manifest = {
                "schema_version": 1,
                "family": "JetBrains Mono NL",
                "upstream_repository": "JetBrains/JetBrainsMono",
                "pinned_release": "v2.304",
                "pinned_version": "2.304",
                "archive_url": "https://example.test/v2.304/JetBrainsMono-2.304.zip",
                "files": [
                    {
                        "archive_path": "font.ttf",
                        "path": "assets/fonts/jetbrains-mono/font.ttf",
                        "sha256": hashlib.sha256(b"official font bytes").hexdigest(),
                    }
                ],
                "version_markers": ["docs/font.md"],
            }
            fonts.verify(root, manifest)
            asset.write_bytes(b"changed")
            with self.assertRaisesRegex(fonts.FontError, "checksum differs"):
                fonts.verify(root, manifest)

    def test_manifest_loader_rejects_unknown_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps({"schema_version": 99}), encoding="utf-8")
            with self.assertRaisesRegex(fonts.FontError, "schema_version"):
                fonts.load_manifest(path)

    @mock.patch("manage_bundled_font.subprocess.run")
    @mock.patch("manage_bundled_font.urllib.request.urlopen")
    def test_request_falls_back_to_verified_curl_without_a_shell(self, urlopen, run):
        urlopen.side_effect = urllib.error.URLError("missing Python trust store")
        run.return_value = mock.Mock(returncode=0, stdout=b"payload", stderr=b"")

        self.assertEqual(fonts.request_bytes("https://example.test/file", 30), b"payload")
        command = run.call_args.args[0]
        self.assertEqual(command[0], "curl")
        self.assertIn("-fLsS", command)
        self.assertNotIn("-k", command)
        self.assertEqual(run.call_args.kwargs.get("shell"), None)


if __name__ == "__main__":
    unittest.main()
