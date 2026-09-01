import hashlib
import io
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

    def test_verify_accepts_commit_pinned_direct_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            asset = root / "assets/fonts/noto-emoji/font.ttf"
            asset.parent.mkdir(parents=True)
            asset.write_bytes(b"font bytes")
            commit = "a" * 40
            manifest = {
                "schema_version": 1,
                "source_kind": "direct",
                "family": "Noto Emoji",
                "upstream_repository": "google/fonts",
                "pinned_version": "3.002",
                "files": [
                    {
                        "source_url": f"https://example.test/{commit}/font.ttf",
                        "source_commit": commit,
                        "path": "assets/fonts/noto-emoji/font.ttf",
                        "sha256": hashlib.sha256(b"font bytes").hexdigest(),
                    }
                ],
            }
            fonts.verify(root, manifest)

    @mock.patch("manage_bundled_font.request_json")
    def test_latest_release_is_generic_across_font_repositories(self, request_json):
        request_json.return_value = {
            "tag_name": "v34.8.2",
            "html_url": "https://github.com/example/font/releases/tag/v34.8.2",
            "assets": [],
        }

        release = fonts.latest_release(
            {"upstream_repository": "example/font"}
        )

        self.assertEqual(release["version"], "34.8.2")
        self.assertEqual(release["tag"], "v34.8.2")
        request_json.assert_called_once_with(
            "https://api.github.com/repos/example/font/releases/latest"
        )

    def test_jetbrains_updater_selects_its_versioned_archive(self):
        release = {
            "tag": "v2.305",
            "version": "2.305",
            "assets": [
                {
                    "name": "JetBrainsMono-2.305.zip",
                    "browser_download_url": "https://example.test/JetBrainsMono-2.305.zip",
                }
            ],
        }

        self.assertEqual(
            fonts.jetbrains_archive_url(release),
            "https://example.test/JetBrainsMono-2.305.zip",
        )

    @mock.patch("manage_bundled_font.latest_release")
    def test_non_jetbrains_automatic_update_fails_before_network_check(
        self, latest_release
    ):
        manifest = (
            Path(__file__).resolve().parents[2]
            / "assets/fonts/iosevka-term/manifest.json"
        )
        with mock.patch.object(
            sys,
            "argv",
            [
                "manage_bundled_font.py",
                "--manifest",
                str(manifest),
                "--update-latest",
            ],
        ), mock.patch("sys.stderr", new_callable=io.StringIO) as stderr:
            self.assertEqual(fonts.main(), 1)
            self.assertIn("supports JetBrains Mono only", stderr.getvalue())
        latest_release.assert_not_called()

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
