import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
UNIX_INSTALLER = ROOT / "scripts/install-dev-tunnel.sh"
WINDOWS_INSTALLER = ROOT / "scripts/install-dev-tunnel.ps1"
VERSION = "123.4.5"


@unittest.skipIf(os.name == "nt", "Unix installer uses native Unix tools")
class UnixInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="festerm-installer-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.archive = self.root / "bundle.tar.gz"
        self.checksum = self.root / "bundle.sha256"
        self.cli = self.root / "devtunnel"
        self.cli.write_text("#!/bin/sh\nprintf 'fixture CLI version\\n'\n")
        cli_digest = hashlib.sha256(self.cli.read_bytes()).hexdigest()
        # Substitute only the test CLI's digest; all production verification
        # logic still runs against real SHA-256, never a mocked hash tool.
        source = UNIX_INSTALLER.read_text().replace(
            "723d6aebdc226064aef321258fb17c8a2770c9cc2bd05004c0fb83e72b302c3f",
            cli_digest,
        )
        self.script = self.root / "installer.sh"
        self.script.write_text(source)
        self.write_tool("uname", 'case "$1" in -s) echo Linux;; -m) echo x86_64;; esac')
        self.write_tool("id", "echo 1000")
        self.write_tool("getconf", "echo 'glibc 2.39'")
        self.write_tool("sudo", "echo UNEXPECTED-SUDO >&2; exit 99")
        curl = self.bin / "curl"
        curl.write_text(
            f"#!{shutil.which('python3') or shutil.which('python')}\n"
            "import os, shutil, sys\n"
            "args = sys.argv[1:]\n"
            "assert '--proto' in args and '--proto-redir' in args\n"
            "assert '--insecure' not in args\n"
            "url = next(a for a in args if a.startswith('https://'))\n"
            "target = args[args.index('-o') + 1]\n"
            "name = 'CHECKSUM' if url.endswith('.sha256') else "
            "('CLI' if url.startswith('https://aka.ms/') else 'ARCHIVE')\n"
            "shutil.copyfile(os.environ['FIXTURE_' + name], target)\n"
        )
        curl.chmod(0o700)
        self.make_archive()

    def write_tool(self, name, body):
        tool = self.bin / name
        tool.write_text("#!/bin/sh\nset -eu\n" + body + "\n")
        tool.chmod(0o700)

    def make_archive(self, extra=None, duplicate=False, link=False, version=VERSION):
        files = {
            "VERSION": version + "\n",
            "LICENSE": "fixture",
            "THIRD_PARTY_NOTICES.txt": "fixture",
            "festerm-sessiond": "#!/bin/sh\nexit 0\n",
            "festerm-tunnel-bridge": "#!/bin/sh\nexit 0\n",
            "festerm-tunnel-setup": "#!/bin/sh\nprintf 'FIXTURE-SETUP-EXECUTED\\n'; printf '%s\\n' \"$@\"\nwhile [ $# -gt 0 ]; do [ \"$1\" = --state-dir ] && mkdir -p \"$2\" && printf '{\"fixture\":true}\\n' > \"$2/festerm-profile.json\"; shift; done\n",
        }
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, value in files.items():
                entry = tarfile.TarInfo(name)
                data = value.encode()
                entry.size = len(data)
                entry.mode = 0o700
                archive.addfile(entry, io.BytesIO(data))
            if extra:
                entry = tarfile.TarInfo(extra)
                entry.size = 0
                archive.addfile(entry, io.BytesIO())
            if duplicate:
                archive.addfile(tarfile.TarInfo("VERSION"), io.BytesIO())
        if link:
            with tarfile.open(self.archive, "w:gz") as archive:
                for name, value in files.items():
                    entry = tarfile.TarInfo(name)
                    if name == "festerm-tunnel-setup":
                        entry.type = tarfile.SYMTYPE
                        entry.linkname = "/bin/sh"
                        archive.addfile(entry)
                    else:
                        data = value.encode()
                        entry.size = len(data)
                        entry.mode = 0o700
                        archive.addfile(entry, io.BytesIO(data))
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.checksum.write_text(f"{digest}  fesTerm-tunnel-{VERSION}-linux-x86_64.tar.gz\n")

    def run_installer(self, *args):
        environment = os.environ.copy()
        environment.update(
            HOME=str(self.home),
            TMPDIR=str(self.root),
            PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
            FIXTURE_CHECKSUM=str(self.checksum),
            FIXTURE_ARCHIVE=str(self.archive),
            FIXTURE_CLI=str(self.cli),
        )
        return subprocess.run(
            ["bash", str(self.script), VERSION, *args],
            env=environment,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def assert_refused(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)
        self.assertNotIn("FIXTURE-SETUP-EXECUTED", result.stdout)
        self.assertNotIn("UNEXPECTED-SUDO", result.stderr)

    def test_verified_bundle_executes_only_its_setup_with_private_paths(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FIXTURE-SETUP-EXECUTED", result.stdout)
        self.assertIn("--devtunnel\n", result.stdout)
        self.assertIn("--state-dir\n", result.stdout)
        root = self.home / ".local/share/festerm-tunnels"
        self.assertEqual(root.stat().st_mode & 0o777, 0o700)
        installation = root / "releases"
        self.assertEqual(installation.stat().st_mode & 0o777, 0o700)

    def test_bad_hash_executes_nothing(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"tampered")
        self.assert_refused(self.run_installer(), "SHA-256 mismatch")

    def test_repeat_reuses_same_release_and_managed_state_path(self):
        first = self.run_installer()
        second = self.run_installer()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.stdout, second.stdout)
        self.assertIn("--managed\n", first.stdout)
        self.assertIn(str((self.home / ".local/share/festerm-tunnels/state").resolve()), first.stdout)
        releases = self.home / ".local/share/festerm-tunnels/releases"
        self.assertEqual([entry.name for entry in releases.iterdir()],
                         [f"{VERSION}-linux-x86_64"])

    def test_repeat_refuses_modified_cache_without_overwriting_it(self):
        self.assertEqual(self.run_installer().returncode, 0)
        helper = self.home / f".local/share/festerm-tunnels/releases/{VERSION}-linux-x86_64/festerm-sessiond"
        helper.write_text("changed-live-helper")
        self.assert_refused(self.run_installer(), "refusing to overwrite live helpers")
        self.assertEqual(helper.read_text(), "changed-live-helper")

    def test_repeat_reuses_verified_cli_when_upstream_download_changes(self):
        first = self.run_installer()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.cli.write_text("#!/bin/sh\necho UNVERIFIED-CLI-EXECUTED\n")
        repeated = self.run_installer()
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        self.assertEqual(first.stdout, repeated.stdout)

    def test_foreign_cache_lock_is_not_removed(self):
        self.assertEqual(self.run_installer().returncode, 0)
        lock = self.home / f".local/share/festerm-tunnels/releases/{VERSION}-linux-x86_64.lock"
        lock.mkdir()
        self.assert_refused(self.run_installer(), "owns this release lock")
        self.assertTrue(lock.is_dir())

    def test_parent_traversal_executes_nothing(self):
        self.make_archive(extra="../escape")
        self.assert_refused(self.run_installer(), "unexpected, duplicated or missing")
        self.assertFalse((self.root / "escape").exists())

    def test_duplicate_entries_execute_nothing(self):
        self.make_archive(duplicate=True)
        self.assert_refused(self.run_installer(), "unexpected, duplicated or missing")

    def test_symlink_entry_executes_nothing(self):
        self.make_archive(link=True)
        self.assert_refused(self.run_installer(), "non-regular file")

    def test_bundle_version_must_match_requested_release(self):
        self.make_archive(version="999.9.9")
        self.assert_refused(self.run_installer(), "Bundle version mismatch")

    def test_changed_official_cli_pin_executes_nothing(self):
        self.cli.write_text("#!/bin/sh\necho UNVERIFIED-CLI-EXECUTED\n")
        result = self.run_installer()
        self.assert_refused(result, "CLI hash changed")
        self.assertNotIn("UNVERIFIED-CLI-EXECUTED", result.stdout)

    def test_checksum_must_name_exact_release_asset(self):
        self.checksum.write_text(self.checksum.read_text().replace(VERSION, "999.9.9"))
        self.assert_refused(self.run_installer(), "Checksum does not name this exact release")

    def test_existing_symlink_install_root_refuses(self):
        parent = self.home / ".local/share"
        parent.mkdir(parents=True)
        (parent / "festerm-tunnels").symlink_to(self.root, target_is_directory=True)
        self.assert_refused(self.run_installer(), "must not be a symbolic link")


class InstallerSyntaxTests(unittest.TestCase):
    @unittest.skipIf(os.name == "nt", "Bash native syntax check")
    def test_unix_shell_syntax(self):
        subprocess.run(["bash", "-n", str(UNIX_INSTALLER)], check=True)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell parser requires pwsh")
    def test_powershell_syntax(self):
        result = subprocess.run(
            ["pwsh", "-NoLogo", "-NoProfile", "-Command",
             "$tokens=$null; $errors=$null; "
             "[Management.Automation.Language.Parser]::ParseFile($env:FESTERM_INSTALLER_PARSE_PATH,"
             "[ref]$tokens,[ref]$errors) > $null; "
             "if($errors.Count){$errors|ForEach-Object{$_.Message};exit 1}",
             ],
            env={**os.environ, "FESTERM_INSTALLER_PARSE_PATH": str(WINDOWS_INSTALLER)},
            text=True, capture_output=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
