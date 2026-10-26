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
    def scratch(self):
        directory = tempfile.TemporaryDirectory(dir=fonts.ROOT, prefix=".font-test-")
        self.addCleanup(directory.cleanup)
        return Path(directory.name)

    def catalog(self):
        root = self.scratch()
        for family in fonts.OPTIONAL_FAMILIES:
            directory = root / "assets/fonts" / family
            directory.mkdir(parents=True)
            files = []
            for name in ("Regular.ttf", "Bold.ttf", "Italic.ttf", "BoldItalic.ttf", "LICENSE"):
                data = f"{family}/{name}".encode()
                (directory / name).write_bytes(data)
                files.append({
                    "archive_path": name,
                    "path": f"assets/fonts/{family}/{name}",
                    "sha256": fonts.digest(data),
                    "size_bytes": len(data),
                })
            (directory / "manifest.json").write_text(json.dumps({
                "schema_version": 1,
                "family": family,
                "pinned_release": "v1.0",
                "pinned_version": "1.0",
                "archive_url": "https://example.test/v1.0/fonts.zip",
                "files": files,
            }), encoding="utf-8")
        return root

    def test_stage_exact_repository_assets_hashes_sizes_and_exclusions(self):
        destination = self.scratch() / "downloads"
        report = fonts.stage_downloads(fonts.ROOT, destination)
        expected = []
        for family in fonts.OPTIONAL_FAMILIES:
            manifest = fonts.load_manifest(fonts.ROOT / "assets/fonts" / family / "manifest.json")
            for entry in manifest["files"]:
                path = Path(entry["path"])
                if path.suffix == ".ttf" or path.name.startswith("LICENSE"):
                    suffix = ".ttf" if path.suffix == ".ttf" else ".license.txt"
                    name = f"festerm-font-{entry['sha256']}{suffix}"
                    expected.append(name)
                    data = (destination / name).read_bytes()
                    self.assertEqual(len(data), entry["size_bytes"])
                    self.assertEqual(fonts.digest(data), entry["sha256"])
                    self.assertEqual(data, (fonts.ROOT / entry["path"]).read_bytes())
        self.assertEqual(sorted(path.name for path in destination.iterdir()), sorted(expected))
        self.assertEqual(len(expected), 15)
        self.assertEqual(report["face_count"], 12)
        self.assertEqual(
            report["font_size_bytes"],
            sum(asset["size_bytes"] for asset in report["assets"] if asset["name"].endswith(".ttf")),
        )
        self.assertEqual(
            report["total_size_bytes"], sum(path.stat().st_size for path in destination.iterdir())
        )
        for asset in report["assets"]:
            self.assertNotRegex(asset["path"], "jetbrains|noto|inter")
        self.assertEqual(fonts.stage_downloads(fonts.ROOT, destination), report)

    def test_stage_refuses_tampered_input_before_creating_output(self):
        root = self.catalog()
        (root / "assets/fonts/iosevka-term/Regular.ttf").write_bytes(b"tampered")
        with self.assertRaisesRegex(fonts.FontError, "checksum differs"):
            fonts.stage_downloads(root, root / "downloads")
        self.assertFalse((root / "downloads").exists())

    def test_stage_uses_updated_manifest_catalog_without_pinned_asset_names(self):
        root = self.catalog()
        path = root / "assets/fonts/iosevka-term/manifest.json"
        manifest = fonts.load_manifest(path)
        entry = manifest["files"][0]
        old_name = f"festerm-font-{entry['sha256']}.ttf"
        data = b"new reviewed custom build"
        (root / entry["path"]).write_bytes(data)
        entry["sha256"] = fonts.digest(data)
        entry["size_bytes"] = len(data)
        path.write_text(json.dumps(manifest), encoding="utf-8")
        report = fonts.stage_downloads(root, root / "downloads")
        names = {asset["name"] for asset in report["assets"]}
        self.assertNotIn(old_name, names)
        self.assertIn(f"festerm-font-{fonts.digest(data)}.ttf", names)
        self.assertEqual(report["face_count"], 12)

    def test_stage_requires_sizes_and_validates_exact_sizes(self):
        for size in (None, 0, True, 123456):
            with self.subTest(size=size):
                root = self.catalog()
                path = root / "assets/fonts/iosevka-term/manifest.json"
                manifest = fonts.load_manifest(path)
                manifest["files"][0]["size_bytes"] = size
                path.write_text(json.dumps(manifest), encoding="utf-8")
                with self.assertRaisesRegex(fonts.FontError, "size"):
                    fonts.stage_downloads(root, root / "downloads")
                self.assertFalse((root / "downloads").exists())

    def test_stage_never_overwrites_wrong_bytes_or_partially_populates(self):
        root = self.catalog()
        destination = root / "downloads"
        destination.mkdir()
        asset = fonts.optional_downloads(root)[-1]
        target = destination / asset["name"]
        wrong = b"x" * asset["size_bytes"]
        target.write_bytes(wrong)
        with self.assertRaisesRegex(fonts.FontError, "refusing to overwrite"):
            fonts.stage_downloads(root, destination)
        self.assertEqual(target.read_bytes(), wrong)
        self.assertEqual(list(destination.iterdir()), [target])

    def test_stage_rejects_stale_assets_from_another_catalog(self):
        root = self.catalog()
        destination = root / "downloads"
        fonts.stage_downloads(root, destination)
        (destination / "stale.ttf").write_bytes(b"old version")
        with self.assertRaisesRegex(fonts.FontError, "unexpected files"):
            fonts.stage_downloads(root, destination)

    def test_embed_exclusion_ignores_test_items_and_notices_but_rejects_runtime_fonts(self):
        root = self.scratch()
        path = root / "crates/example/src/fonts.rs"
        path.parent.mkdir(parents=True)
        path.write_text('''
const LICENSE: &str = include_str!("../../../assets/fonts/iosevka-term/LICENSE.md");
#[cfg(test)]
const FIXTURE: &[u8] = include_bytes!("../../../assets/fonts/iosevka-term/Regular.ttf");
#[cfg(test)]
mod tests {
    const FIXTURE: &[u8] = include_bytes!("../../../assets/fonts/julia-mono/Regular.ttf");
    fn example() { let braces = "}"; }
}
// include_bytes!("../../../assets/fonts/maple-mono/Regular.ttf");
''', encoding="utf-8")
        fonts.verify_runtime_embed_exclusion(root)
        with path.open("a", encoding="utf-8") as output:
            output.write('const LIVE: &[u8] = include_bytes!(\n"../../../assets/fonts/maple-mono/Regular.ttf");')
        with self.assertRaisesRegex(fonts.FontError, "optional fonts embedded in runtime"):
            fonts.verify_runtime_embed_exclusion(root)

    def test_normal_verification_checks_embed_exclusion(self):
        with mock.patch.object(sys, "argv", ["manage_bundled_font.py"]), \
             mock.patch.object(fonts, "verify_runtime_embed_exclusion") as exclusion, \
             mock.patch.object(fonts, "verify_runtime_license_embeddings") as licenses, \
             mock.patch("sys.stdout", new_callable=io.StringIO):
            self.assertEqual(fonts.main(), 0)
        exclusion.assert_called_once_with(fonts.ROOT)
        licenses.assert_called_once_with(fonts.ROOT)

    def test_production_embeds_every_complete_runtime_and_optional_font_notice(self):
        fonts.verify_runtime_license_embeddings(fonts.ROOT)
        root = self.catalog()
        source = root / "app/example/src/licenses.rs"
        source.parent.mkdir(parents=True)
        source.write_text(
            '\n'.join(
                'FontLicenseNotice { '
                f'family: "{manifest["family"]}", '
                f'version: "{manifest["pinned_version"]}", '
                'notice: "complete notice", '
                f'text: include_str!("../../../{entry["path"]}"), '
                '}'
                for family in fonts.OPTIONAL_FAMILIES
                for manifest in [
                    fonts.load_manifest(root / "assets/fonts" / family / "manifest.json")
                ]
                for entry in fonts.manifest_notice_entries(manifest)
            ),
            encoding="utf-8",
        )
        fonts.verify_runtime_license_embeddings(root)
        source.write_text(
            source.read_text(encoding="utf-8").replace(
                'include_str!("../../../assets/fonts/maple-mono/LICENSE")',
                '"not embedded"',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(fonts.FontError, "maple-mono"):
            fonts.verify_runtime_license_embeddings(root)

    def test_runtime_embedded_families_require_every_manifest_notice(self):
        root = self.catalog()
        notices = []
        runtime = root / "crates/example/src/fonts.rs"
        runtime.parent.mkdir(parents=True)
        for slug, family, version, notice_names in [
            ("jetbrains-mono", "JetBrains Mono", "2.304", ("OFL.txt", "AUTHORS.txt")),
            ("noto-emoji", "Noto Emoji", "3.002", ("OFL.txt",)),
        ]:
            directory = root / "assets/fonts" / slug
            directory.mkdir(parents=True)
            files = []
            font = directory / "Regular.ttf"
            font.write_bytes(slug.encode())
            files.append({
                "path": f"assets/fonts/{slug}/Regular.ttf",
                "sha256": fonts.digest(font.read_bytes()),
                "size_bytes": font.stat().st_size,
            })
            for name in notice_names:
                path = directory / name
                path.write_text(f"{family} {name}", encoding="utf-8")
                entry = {
                    "path": f"assets/fonts/{slug}/{name}",
                    "sha256": fonts.digest(path.read_bytes()),
                    "size_bytes": path.stat().st_size,
                }
                files.append(entry)
                notices.append(
                    'FontLicenseNotice { '
                    f'family: "{family}", version: "{version}", '
                    f'notice: "{name}", text: include_str!("../../../{entry["path"]}"), '
                    '}'
                )
            (directory / "manifest.json").write_text(json.dumps({
                "schema_version": 1,
                "family": family,
                "pinned_version": version,
                "files": files,
            }), encoding="utf-8")
        optional_notices = []
        for family in fonts.OPTIONAL_FAMILIES:
            manifest = fonts.load_manifest(root / "assets/fonts" / family / "manifest.json")
            optional_notices.extend(
                'FontLicenseNotice { '
                f'family: "{manifest["family"]}", '
                f'version: "{manifest["pinned_version"]}", '
                'notice: "complete notice", '
                f'text: include_str!("../../../{entry["path"]}"), '
                '}'
                for entry in fonts.manifest_notice_entries(manifest)
            )
        runtime.write_text(
            '\n'.join([
                'const JETBRAINS: &[u8] = include_bytes!("../../../assets/fonts/jetbrains-mono/Regular.ttf");',
                'const NOTO: &[u8] = include_bytes!("../../../assets/fonts/noto-emoji/Regular.ttf");',
                *notices,
                *optional_notices,
            ]),
            encoding="utf-8",
        )
        fonts.verify_runtime_license_embeddings(root)
        runtime.write_text(
            runtime.read_text(encoding="utf-8").replace(
                'text: include_str!("../../../assets/fonts/jetbrains-mono/AUTHORS.txt"),',
                'text: "missing",',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(fonts.FontError, "AUTHORS.txt"):
            fonts.verify_runtime_license_embeddings(root)

    def test_runtime_font_notice_versions_match_manifests(self):
        root = self.catalog()
        source = root / "app/example/src/licenses.rs"
        source.parent.mkdir(parents=True)
        entries = []
        for family in fonts.OPTIONAL_FAMILIES:
            manifest = fonts.load_manifest(root / "assets/fonts" / family / "manifest.json")
            entries.extend(
                'FontLicenseNotice { '
                f'family: "{manifest["family"]}", '
                f'version: "{manifest["pinned_version"]}", '
                'notice: "complete notice", '
                f'text: include_str!("../../../{entry["path"]}"), '
                '}'
                for entry in fonts.manifest_notice_entries(manifest)
            )
        source.write_text("\n".join(entries), encoding="utf-8")
        fonts.verify_runtime_license_embeddings(root)
        source.write_text(
            source.read_text(encoding="utf-8").replace(
                'family: "maple-mono", version: "1.0"',
                'family: "maple-mono", version: "9.9"',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(fonts.FontError, "label drift.*9.9.*expected.*1.0"):
            fonts.verify_runtime_license_embeddings(root)

    def test_staging_cli_writes_machine_report_without_network(self):
        root = self.catalog()
        report_path = root / "report.json"
        with mock.patch.object(fonts, "ROOT", root), \
             mock.patch.object(sys, "argv", [
                 "manage_bundled_font.py", "--stage-downloads", str(root / "downloads"),
                 "--downloads-report", str(report_path),
             ]), mock.patch("sys.stdout", new_callable=io.StringIO) as stdout, \
             mock.patch.object(fonts, "download") as download:
            self.assertEqual(fonts.main(), 0)
        self.assertEqual(json.loads(stdout.getvalue()), json.loads(report_path.read_text()))
        download.assert_not_called()

    def test_release_stages_and_publishes_version_specific_assets_separately(self):
        workflow = (fonts.ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        validation = workflow.split("\n  macos:", 1)[0]
        self.assertIn("--stage-downloads font-downloads --downloads-report font-downloads.json", validation)
        self.assertIn("name: optional-font-downloads\n", validation)
        publish = workflow.split("\n  publish:", 1)[1]
        self.assertIn("environment: release", publish)
        self.assertIn("if: needs.validate.outputs.publish == 'true'", publish)
        self.assertIn("name: optional-font-downloads\n", publish)
        self.assertIn("--stage-downloads font-downloads", publish)
        self.assertIn('gh release upload "$TAG" font-downloads/*', publish)
        self.assertLess(
            publish.index('gh release upload "$TAG" font-downloads/*'),
            publish.index('gh release edit "$TAG" --draft=false'),
        )
        self.assertNotIn("--clobber", publish)

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
