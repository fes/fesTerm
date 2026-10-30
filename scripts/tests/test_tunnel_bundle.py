import copy
import hashlib
import importlib.util
import io
import shutil
import stat
import subprocess
import sys
import tarfile
import unittest
import uuid
import zipfile
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/build_tunnel_bundle.py"
SPEC = importlib.util.spec_from_file_location("build_tunnel_bundle", SCRIPT)
bundle = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(bundle)
VERSION = "1.2.3"


class TunnelBundleTests(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / "target" / f"test-tunnel-bundle-{uuid.uuid4().hex}"
        self.root.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.root)
        (self.root / "Cargo.toml").write_text(
            f'[workspace.package]\nversion = "{VERSION}"\nlicense = "MIT"\n',
            encoding="utf-8",
        )
        self.binaries = self.root / "bin"
        self.binaries.mkdir()
        for platform in bundle.PLATFORMS:
            for name in bundle.entry_names(platform)[:3]:
                (self.binaries / name).write_bytes(f"mock binary {name}".encode())
        dependency = self.root / "dependency"
        dependency.mkdir()
        (dependency / "LICENSE-MIT").write_text("actual upstream license\n", encoding="utf-8")
        (dependency / "native").mkdir()
        (dependency / "native/NOTICE.txt").write_text("native attribution\n", encoding="utf-8")
        self.metadata = {
            "packages": [
                {
                    "id": name, "name": name, "version": VERSION, "license": "MIT",
                    "manifest_path": str(self.root / "crates" / name / "Cargo.toml"),
                }
                for name in bundle.BINARIES[:2]
            ] + [{
                "id": "dependency", "name": "dependency", "version": "2.0.0",
                "license": "MIT", "license_file": None,
                "manifest_path": str(dependency / "Cargo.toml"),
            }],
            "resolve": {"nodes": [
                {
                    "id": name, "deps": [{
                        "pkg": "dependency", "dep_kinds": [{"kind": None}],
                    }],
                } for name in bundle.BINARIES[:2]
            ] + [{"id": "dependency", "deps": []}]},
        }

    def build(self, platform="linux", arch="x86_64", output="dist", **kwargs):
        return bundle.build_bundle(
            self.root, self.binaries, self.root / output,
            kwargs.pop("version", VERSION), platform, arch,
            metadata=kwargs.pop("metadata", self.metadata), **kwargs,
        )

    def contents(self, archive, platform):
        if platform == "windows":
            with zipfile.ZipFile(archive) as source:
                return {
                    entry.filename: (source.read(entry), entry.external_attr >> 16)
                    for entry in source.infolist()
                }
        with tarfile.open(archive, "r:gz") as source:
            return {
                entry.name: (source.extractfile(entry).read(), stat.S_IFREG | entry.mode)
                for entry in source.getmembers()
            }

    def test_six_native_payloads_have_exact_names_versions_modes_and_checksums(self):
        for platform in bundle.PLATFORMS:
            for arch in bundle.ARCHITECTURES:
                with self.subTest(platform=platform, arch=arch):
                    archive = self.build(platform, arch)
                    extension = "zip" if platform == "windows" else "tar.gz"
                    self.assertEqual(
                        archive.name, f"fesTerm-tunnel-{VERSION}-{platform}-{arch}.{extension}",
                    )
                    contents = self.contents(archive, platform)
                    self.assertEqual(set(contents), set(bundle.entry_names(platform)))
                    self.assertEqual(contents["VERSION"][0], b"1.2.3\n")
                    for name, (content, mode) in contents.items():
                        self.assertTrue(content)
                        self.assertTrue(stat.S_ISREG(mode))
                        self.assertEqual(
                            stat.S_IMODE(mode),
                            0o755 if name in bundle.entry_names(platform)[:3] else 0o644,
                        )
                    checksum = archive.with_name(archive.name + ".sha256")
                    self.assertEqual(
                        checksum.read_text(encoding="ascii"),
                        f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n",
                    )
                    bundle.verify_bundle(archive, VERSION, platform)

    def test_archives_are_deterministic_despite_source_modes_mtimes_and_location(self):
        for platform in bundle.PLATFORMS:
            with self.subTest(platform=platform):
                first = self.build(platform, output=f"first-{platform}")
                for path in self.binaries.iterdir():
                    path.chmod(0o600)
                    path.touch()
                second = self.build(platform, output=f"second-{platform}")
                self.assertEqual(first.read_bytes(), second.read_bytes())

    def test_extra_build_outputs_are_excluded(self):
        (self.binaries / "festerm").write_bytes(b"GUI must not ship")
        (self.binaries / "conpty.dll").write_bytes(b"sidecar must not ship")
        (self.binaries / "unexpected-dir").mkdir()
        archive = self.build()
        self.assertEqual(set(self.contents(archive, "linux")), set(bundle.entry_names("linux")))

    def test_missing_empty_or_symlinked_binaries_fail_before_output(self):
        for kind in ("missing", "empty", "symlink"):
            with self.subTest(kind=kind):
                path = self.binaries / "festerm-tunnel-setup"
                path.unlink(missing_ok=True)
                if kind == "empty":
                    path.write_bytes(b"")
                elif kind == "symlink":
                    path.symlink_to(self.binaries / "festerm-sessiond")
                with self.assertRaisesRegex(ValueError, "helper binary"):
                    self.build()
                self.assertFalse((self.root / "dist").exists())

    def test_invalid_or_mismatched_versions_fail_closed(self):
        for version in ("", "v1.2.3", "01.2.3", "1.2", "1.2.3\n", "1.2.4",
                        "../1.2.3", "1.2.3-rc.1", "1.2.3+build"):
            with self.subTest(version=version):
                with self.assertRaises(ValueError):
                    self.build(version=version)
                self.assertFalse((self.root / "dist").exists())

    def test_package_metadata_must_match_both_helper_packages(self):
        for change in ("missing", "version"):
            metadata = copy.deepcopy(self.metadata)
            if change == "missing":
                metadata["packages"].pop(0)
            else:
                metadata["packages"][0]["version"] = "9.0.0"
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "package metadata"):
                self.build(metadata=metadata)

    def test_notices_follow_helper_closure_and_retain_nested_notices(self):
        metadata = copy.deepcopy(self.metadata)
        metadata["resolve"]["nodes"][0]["deps"].append({
            "pkg": "unused-dev-dependency", "dep_kinds": [{"kind": "dev"}],
        })
        metadata["packages"].append({
            "id": "unrelated-gui", "name": "GUI_DO_NOT_INCLUDE", "version": "9.0.0",
        })
        text = bundle.third_party_notices(metadata, VERSION, self.root).decode()
        self.assertIn("actual upstream license", text)
        self.assertIn("native attribution", text)
        self.assertEqual(text.count("=== dependency 2.0.0 ==="), 1)
        self.assertNotIn("GUI_DO_NOT_INCLUDE", text)

    def test_missing_and_empty_dependency_licenses_fail_closed(self):
        license_file = self.root / "dependency/LICENSE-MIT"
        notice = self.root / "dependency/native/NOTICE.txt"
        notice.unlink()
        license_file.unlink()
        with self.assertRaisesRegex(ValueError, "missing dependency license"):
            self.build()
        license_file.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "empty dependency license"):
            self.build()

    def test_explicit_apache_declarations_are_pinned_not_generic_fallbacks(self):
        shutil.rmtree(self.root / "dependency")
        apache = self.root / "vendor/russh/LICENSE-APACHE"
        apache.parent.mkdir(parents=True)
        apache.write_text("Apache License\nVersion 2.0\nActual retained terms\n", encoding="utf-8")
        for (name, version), attribution in bundle.APACHE_DECLARATIONS.items():
            with self.subTest(name=name):
                metadata = copy.deepcopy(self.metadata)
                metadata["packages"][-1].update({
                    "name": name, "version": version, "license": "Apache-2.0",
                })
                text = bundle.third_party_notices(metadata, VERSION, self.root).decode()
                self.assertIn(attribution, text)
                self.assertIn("Actual retained terms", text)
                metadata["packages"][-1]["version"] = "999.0.0"
                with self.assertRaisesRegex(ValueError, "missing dependency license"):
                    bundle.third_party_notices(metadata, VERSION, self.root)

    def test_dependency_license_path_traversal_and_symlinks_are_rejected(self):
        for traversal in (True, False):
            with self.subTest(traversal=traversal):
                metadata = copy.deepcopy(self.metadata)
                if traversal:
                    metadata["packages"][-1]["license_file"] = "../Cargo.toml"
                else:
                    link = self.root / "dependency/NOTICE-unsafe"
                    link.symlink_to(self.root / "Cargo.toml")
                with self.assertRaisesRegex(ValueError, "unsafe dependency license"):
                    self.build(metadata=metadata)

    def test_existing_outputs_are_not_overwritten(self):
        archive = self.build()
        original = archive.read_bytes()
        with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
            self.build()
        self.assertEqual(archive.read_bytes(), original)

    def test_staged_notices_are_retained_without_regenerating_source_metadata(self):
        archive = self.build(metadata=None, notices=b"native build retained notices\n")
        self.assertEqual(
            self.contents(archive, "linux")["THIRD_PARTY_NOTICES.txt"][0],
            b"native build retained notices\n",
        )
        with self.assertRaisesRegex(ValueError, "empty dependency notices"):
            self.build(metadata=None, notices=b"", output="empty-notices")

    def test_failed_validation_removes_archive_and_never_emits_checksum(self):
        with mock.patch.object(bundle, "verify_bundle", side_effect=ValueError("test rejection")):
            with self.assertRaisesRegex(ValueError, "test rejection"):
                self.build()
        self.assertEqual(list((self.root / "dist").iterdir()), [])

    def malformed_archive(self, platform, change):
        names = bundle.entry_names(platform)
        contents = {name: b"content" for name in names}
        contents["VERSION"] = f"{VERSION}\n".encode()
        if change in ("../escape", "/absolute", "extra"):
            contents[change] = b"unexpected"
        elif change == "missing":
            del contents[names[0]]
        elif change == "version":
            contents["VERSION"] = b"9.9.9\n"
        path = self.root / f"malformed-{platform}-{change.replace('/', '_')}"
        if platform == "windows":
            with zipfile.ZipFile(path, "w") as archive:
                for name, content in contents.items():
                    info = zipfile.ZipInfo(name)
                    info.create_system = 3
                    mode = 0o755 if name in names[:3] else 0o644
                    info.external_attr = (stat.S_IFREG | mode) << 16
                    if change == "symlink" and name == names[0]:
                        info.external_attr = (stat.S_IFLNK | 0o777) << 16
                    if change == "mode" and name == names[0]:
                        info.external_attr = (stat.S_IFREG | 0o777) << 16
                    if change == "directory" and name == names[0]:
                        info.external_attr = (stat.S_IFDIR | 0o755) << 16
                    archive.writestr(info, content)
        else:
            with tarfile.open(path, "w:gz") as archive:
                for name, content in contents.items():
                    info = tarfile.TarInfo(name)
                    info.mode = 0o755 if name in names[:3] else 0o644
                    info.size = len(content)
                    if change == "symlink" and name == names[0]:
                        info.type = tarfile.SYMTYPE
                        info.linkname = names[1]
                    if change == "mode" and name == names[0]:
                        info.mode = 0o777
                    if change == "directory" and name == names[0]:
                        info.type = tarfile.DIRTYPE
                    archive.addfile(info, io.BytesIO(content))
        return path

    def test_archive_validator_rejects_traversal_extras_missing_wrong_version_links_and_modes(self):
        for platform in bundle.PLATFORMS:
            for change in (
                "../escape", "/absolute", "extra", "missing", "version",
                "symlink", "directory", "mode",
            ):
                with self.subTest(platform=platform, change=change):
                    archive = self.malformed_archive(platform, change)
                    with self.assertRaises(ValueError):
                        bundle.verify_bundle(archive, VERSION, platform)

    def test_duplicate_archive_entries_are_rejected(self):
        archive = self.build("windows")
        with zipfile.ZipFile(archive, "a") as output:
            with self.assertWarns(UserWarning):
                output.writestr("VERSION", f"{VERSION}\n".encode())
        with self.assertRaisesRegex(ValueError, "exactly"):
            bundle.verify_bundle(archive, VERSION, "windows")
        archive = self.build()
        members = []
        with tarfile.open(archive, "r:gz") as source:
            for entry in source.getmembers():
                members.append((entry, source.extractfile(entry).read()))
        with tarfile.open(archive, "w:gz") as output:
            for entry, content in members + [members[0]]:
                output.addfile(entry, io.BytesIO(content))
        with self.assertRaisesRegex(ValueError, "exactly"):
            bundle.verify_bundle(archive, VERSION, "linux")

    def test_cli_invalid_version_exits_unsuccessfully_without_output(self):
        notices = self.root / "notices.txt"
        notices.write_text("retained notices", encoding="utf-8")
        output = self.root / "command-output"
        result = subprocess.run(
            [
                sys.executable, str(SCRIPT), "--version", "../invalid",
                "--platform", "linux", "--arch", "x86_64",
                "--binary-dir", str(self.binaries), "--output-dir", str(output),
                "--notices", str(notices),
            ],
            capture_output=True, text=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tunnel bundle failed", result.stderr)
        self.assertFalse(output.exists())

    def test_cli_builds_native_format_with_retained_notices(self):
        notices = self.root / "notices.txt"
        notices.write_text("native build notices\n", encoding="utf-8")
        version = bundle.tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))[
            "workspace"
        ]["package"]["version"]
        for platform in ("linux", "windows"):
            with self.subTest(platform=platform):
                output = self.root / f"command-{platform}"
                result = subprocess.run(
                    [
                        sys.executable, str(SCRIPT), "--version", version,
                        "--platform", platform, "--arch", "aarch64",
                        "--binary-dir", str(self.binaries), "--output-dir", str(output),
                        "--notices", str(notices),
                    ],
                    capture_output=True, text=True, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                extension = "zip" if platform == "windows" else "tar.gz"
                archive = output / f"fesTerm-tunnel-{version}-{platform}-aarch64.{extension}"
                bundle.verify_bundle(archive, version, platform)
                self.assertTrue(archive.with_name(archive.name + ".sha256").is_file())


if __name__ == "__main__":
    unittest.main()
