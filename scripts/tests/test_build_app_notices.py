import copy
import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "build_app_notices.py"
SPEC = importlib.util.spec_from_file_location("build_app_notices", SCRIPT)
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)


class AppNoticesTests(unittest.TestCase):
    def test_retains_primary_and_nested_dependency_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root / "crate"
            (crate / "native").mkdir(parents=True)
            (crate / "LICENSE-MIT").write_text("upstream copyright and terms\n")
            (crate / "native/NOTICE.txt").write_text("native attribution\n")
            package = {
                "name": "example", "version": "1.0", "license": "MIT",
                "manifest_path": str(crate / "Cargo.toml"),
            }
            documents = notices.package_documents(package, {}, {}, root)
            self.assertEqual(
                dict(documents),
                {"LICENSE-MIT": b"upstream copyright and terms\n",
                 "native/NOTICE.txt": b"native attribution\n"},
            )

    def test_omitted_primary_license_requires_a_reviewed_exact_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root / "crate"
            crate.mkdir()
            (crate / "NOTICE").write_text("native notice alone is not a license\n")
            fallback = root / "fallback.txt"
            fallback.write_text("original copyright and permission\n")
            source = {"name": "upstream", "path": "fallback.txt",
                      "git-blob": notices.git_blob(fallback.read_bytes())}
            sources = {"upstream": source}
            overrides = {"example@1.0": {"licenses": ["MIT"], "sources": ["upstream"]}}
            package = {"name": "example", "version": "1.0", "license": "MIT",
                       "manifest_path": str(crate / "Cargo.toml")}
            docs = notices.package_documents(package, sources, overrides, root)
            self.assertIn(b"original copyright and permission\n", dict(docs).values())
            for change in ({"version": "2.0"}, {"license": "GPL-3.0"}):
                with self.subTest(change=change), self.assertRaisesRegex(ValueError, "missing reviewed"):
                    notices.package_documents({**package, **change}, sources, overrides, root)
            fallback.write_text("altered")
            with self.assertRaisesRegex(ValueError, "blob mismatch"):
                notices.package_documents(package, sources, overrides, root)

    def test_index_rejects_missing_changed_or_corrupt_release_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "assets/fonts/example").mkdir(parents=True)
            (root / "vendor/example").mkdir(parents=True)
            font_manifest = root / "assets/fonts/example/manifest.json"
            font_manifest.write_text("{}")
            vendor_license = root / "vendor/example/LICENSE"
            vendor_license.write_text("license")
            report = b"reviewed license notice"
            lock = {("dependency", "1.0"): {"source": "registry", "checksum": "abc"}}
            index = {
                "schema": 1,
                "report_sha256": notices.digest(report),
                "inputs": {
                    "assets/fonts/example/manifest.json": notices.digest(font_manifest.read_bytes()),
                    "vendor/example/LICENSE": notices.digest(vendor_license.read_bytes()),
                },
                "packages": {"dependency@1.0": lock[("dependency", "1.0")]},
                "vendor_notices": {"vendor/example": ["vendor/example/LICENSE"]},
            }
            tree = {"target": {("dependency", "1.0"), ("workspace", "1.0")}}
            workspace = {("workspace", "1.0")}
            notices.check_index(index, report, tree, workspace, lock, root)
            with self.assertRaisesRegex(ValueError, "corrupt"):
                notices.check_index(index, b"tampered", tree, workspace, lock, root)
            with self.assertRaisesRegex(ValueError, "missing or stale"):
                notices.check_index(index, report, {"target": {("new", "2.0")}}, workspace, lock, root)
            changed_lock = copy.deepcopy(lock)
            changed_lock[("dependency", "1.0")]["checksum"] = "changed"
            with self.assertRaisesRegex(ValueError, "missing or stale"):
                notices.check_index(index, report, tree, workspace, changed_lock, root)
            (root / "vendor/example/NOTICE").write_text("new notice")
            with self.assertRaisesRegex(ValueError, "vendored notice files changed"):
                notices.check_index(index, report, tree, workspace, lock, root)
            (root / "vendor/example/NOTICE").unlink()
            font_manifest.write_text('{"changed": true}')
            with self.assertRaisesRegex(ValueError, "notice input changed"):
                notices.check_index(index, report, tree, workspace, lock, root)

    def test_tree_parser_handles_duplicates_and_rejects_unexpected_output(self):
        tree = "a v1.0.0\nb v0.2.0 (proc-macro)\na v1.0.0 (*)\n"
        self.assertEqual(notices.tree_packages(tree), {("a", "1.0.0"), ("b", "0.2.0")})
        with self.assertRaisesRegex(ValueError, "unrecognized"):
            notices.tree_packages("unexpected cargo diagnostic\n")


if __name__ == "__main__":
    unittest.main()
