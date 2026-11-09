import hashlib
import io
import json
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
POWERSHELL = shutil.which("pwsh") or (shutil.which("powershell.exe") if os.name == "nt" else None)


@unittest.skipIf(os.name == "nt", "Unix installer uses native Unix tools")
class UnixInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="festerm-installer-test-", dir=ROOT)
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
        self.write_tool("id", f"echo {os.getuid()}")
        # The Linux fixture must read real permissions on BSD hosts too.
        stat = self.bin / "stat"
        stat.write_text(
            f"#!{shutil.which('python3') or shutil.which('python')}\n"
            "import os, stat, sys\n"
            "assert sys.argv[1:3] == ['-c', '%u %a']\n"
            "metadata = os.stat(sys.argv[-1])\n"
            "print(metadata.st_uid, format(stat.S_IMODE(metadata.st_mode), 'o'))\n"
        )
        stat.chmod(0o700)
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

    def make_archive(self, extra=None, duplicate=False, link=False, version=VERSION, release=VERSION, management=True):
        helper = (
            "#!/bin/sh\n"
            "if [ \"$1\" = --help ]; then echo '" +
            ("--manage {list" if management else "old helper help") + "'; exit 0; fi\n"
        )
        if management:
            helper += (
                "if [ \"$1\" = --manage ]; then printf 'FIXTURE-MANAGEMENT\\n'; "
                "printf '%s\\n' \"$@\"; exit 0; fi\n"
            )
        helper += "printf 'FIXTURE-SETUP-EXECUTED\\n'; printf '%s\\n' \"$@\"\nwhile [ $# -gt 0 ]; do [ \"$1\" = --state-dir ] && mkdir -p \"$2\" && printf '{\"fixture\":true}\\n' > \"$2/festerm-profile.json\"; shift; done\n"
        files = {
            "VERSION": version + "\n",
            "LICENSE": "fixture",
            "THIRD_PARTY_NOTICES.txt": "fixture",
            "festerm-sessiond": "#!/bin/sh\nexit 0\n",
            "festerm-tunnel-bridge": "#!/bin/sh\nexit 0\n",
            "festerm-tunnel-setup": helper,
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
        self.checksum.write_text(f"{digest}  fesTerm-tunnel-{release}-linux-x86_64.tar.gz\n")

    def run_installer(self, *args, include_version=True):
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
            ["bash", str(self.script), *([VERSION] if include_version else []), *args],
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

    def management_cache(self):
        root = self.home / ".local/share/festerm-tunnels"
        release = root / "releases/123.4.5-linux-x86_64"
        release.mkdir(parents=True)
        for path in (root, root / "releases", release):
            path.chmod(0o700)
        helper = release / "festerm-tunnel-setup"
        helper.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = --help ]; then echo '--manage {list'; exit 0; fi\n"
            "printf 'FIXTURE-MANAGEMENT\\n'; printf '%s\\n' \"$@\"\n"
        )
        helper.chmod(0o700)
        self.write_tool("curl", "echo UNEXPECTED-DOWNLOAD >&2; exit 99")
        return root

    def test_management_forwards_only_explicit_operation_without_setup_or_download(self):
        root = self.management_cache()
        installation = "12345678-1234-4234-8234-123456789abc"
        for args in [
            ("--manage", "list", "--json"),
            ("--manage", "delete", installation, "--yes"),
            ("--manage", "delete-all", "--yes", "--json"),
            ("--manage", "archive", installation, "--yes"),
            ("--manage", "delete", installation, "--yes", "--confirm-legacy-cloud-id", "fixture.usw3"),
        ]:
            with self.subTest(args=args):
                result = self.run_installer(*args, include_version=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("FIXTURE-MANAGEMENT\n--manage\n", result.stdout)
                self.assertNotIn("--managed\n", result.stdout)
                self.assertNotIn("UNEXPECTED-DOWNLOAD", result.stderr)
                self.assertEqual(sorted(path.name for path in root.iterdir()), ["releases"])

    def test_management_inaccessible_ancestor_is_unknown_not_empty_inventory(self):
        self.home.chmod(0)
        try:
            for operation in (("--manage", "list", "--json"),
                              ("--manage", "delete-all", "--yes")):
                result = self.run_installer(*operation, include_version=False)
                self.assert_refused(result, "inventory is unknown")
                self.assertNotIn('"hosts":[]', result.stdout)
        finally:
            self.home.chmod(0o700)

    def test_management_bootstrap_only_stages_verified_tools_and_preserves_old_host_state(self):
        root = self.home / ".local/share/festerm-tunnels"
        state = root / "state"
        state.mkdir(parents=True)
        root.chmod(0o700)
        state.chmod(0o700)
        record = state / "installation.json"
        original = b'{"fixture":"old-host-record-paths-must-not-change"}'
        record.write_bytes(original)
        result = self.run_installer("--manage", "list", "--install-management-tools")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FIXTURE-MANAGEMENT\n--manage\nlist\n", result.stdout)
        self.assertNotIn("FIXTURE-SETUP-EXECUTED", result.stdout)
        self.assertNotIn("--managed\n", result.stdout)
        self.assertEqual(record.read_bytes(), original)
        self.assertEqual(sorted(path.name for path in state.iterdir()), ["installation.json"])
        self.assertTrue((root / "releases" / f"{VERSION}-linux-x86_64").is_dir())

    def test_management_bootstrap_verifies_bytes_and_refuses_old_release_without_setup(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"tampered")
        self.assert_refused(
            self.run_installer("--manage", "list", "--install-management-tools"),
            "SHA-256 mismatch",
        )
        self.make_archive(management=False)
        self.assert_refused(
            self.run_installer("--manage", "list", "--install-management-tools"),
            "lacks management support",
        )
        self.assertFalse((self.home / ".local/share/festerm-tunnels/state").exists())

    def test_management_bootstrap_never_executes_cli_runtime_preflight(self):
        old_digest = hashlib.sha256(self.cli.read_bytes()).hexdigest()
        self.cli.write_text("#!/bin/sh\necho UNEXPECTED-CLI-RUNTIME >&2\nexit 99\n")
        self.script.write_text(self.script.read_text().replace(
            old_digest, hashlib.sha256(self.cli.read_bytes()).hexdigest()
        ))
        result = self.run_installer("--manage", "list", "--install-management-tools")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FIXTURE-MANAGEMENT\n--manage\nlist\n", result.stdout)
        self.assertNotIn("UNEXPECTED-CLI-RUNTIME", result.stdout + result.stderr)
        self.assertNotIn("--managed\n", result.stdout)
        self.assertNotIn("FIXTURE-SETUP-EXECUTED", result.stdout)
        normal = self.run_installer()
        self.assert_refused(normal, "UNEXPECTED-CLI-RUNTIME")

    def test_management_refuses_invalid_combinations_before_any_provisioning(self):
        for args in [
            ("--manage", "delete-all"),
            ("--manage", "delete", "../state", "--yes"),
            ("--manage", "list", "--yes"),
            ("--manage", "list", "--upgrade"),
            ("--manage", "list", "--repair-session"),
            ("--manage", "list", "--install-dependencies"),
            ("--yes",),
            ("--install-management-tools",),
            ("--manage", "list", "--install-management-tools", "--upgrade"),
            ("--manage", "delete-all", "--yes", "--confirm-legacy-cloud-id", "fixture.usw3"),
        ]:
            with self.subTest(args=args):
                result = self.run_installer(*args, include_version=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("FIXTURE-SETUP-EXECUTED", result.stdout)
                self.assertFalse((self.home / ".local/share/festerm-tunnels").exists())

    def test_management_missing_cache_list_is_non_mutating_and_symlink_cache_refuses(self):
        result = self.run_installer("--manage", "list", "--json", include_version=False)
        self.assertEqual(json.loads(result.stdout)["hosts"], [])
        self.assertFalse((self.home / ".local/share/festerm-tunnels").exists())
        root = self.management_cache()
        releases = root / "releases"
        releases.rename(root / "foreign")
        releases.symlink_to(root / "foreign", target_is_directory=True)
        result = self.run_installer("--manage", "delete-all", "--yes", include_version=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symbolic link", result.stderr)
        self.assertNotIn("FIXTURE-MANAGEMENT", result.stdout)

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

    def test_upgrade_installs_next_to_the_previous_release_and_passes_the_flag(self):
        first = self.run_installer("--no-takeover")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertNotIn("--upgrade", first.stdout)
        newer = "123.4.6"
        self.make_archive(version=newer, release=newer)
        upgraded = self.run_installer(newer, "--upgrade", "--no-takeover", include_version=False)
        self.assertEqual(upgraded.returncode, 0, upgraded.stderr)
        self.assertIn("--no-takeover\n--upgrade\n", upgraded.stdout)
        self.assertIn(f"/releases/{newer}-linux-x86_64\n", upgraded.stdout)
        releases = self.home / ".local/share/festerm-tunnels/releases"
        self.assertEqual(sorted(entry.name for entry in releases.iterdir()),
                         [f"{VERSION}-linux-x86_64", f"{newer}-linux-x86_64"])
        self.assert_refused(self.run_installer("--upgrade", "--upgrade-everything"), "Usage:")

    def test_friendly_name_is_forwarded_as_one_managed_setup_argument(self):
        result = self.run_installer("--friendly-name", "Raspberry Pi 5")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--friendly-name\nRaspberry Pi 5\n", result.stdout)

    def test_friendly_name_is_refused_when_upgrading_an_existing_host(self):
        result = self.run_installer("--upgrade", "--friendly-name", "Raspberry Pi 5")
        self.assert_refused(result, "applies only when creating a new host")

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

    def test_root_is_refused_before_downloads_or_dependency_installation(self):
        self.write_tool("id", "echo 0")
        self.write_tool("curl", "echo UNEXPECTED-DOWNLOAD >&2; exit 99")
        for include_version in [True, False]:
            with self.subTest(include_version=include_version):
                result = self.run_installer("--install-dependencies", include_version=include_version)
                self.assert_refused(result, "normal user, not root")
                self.assertNotIn("UNEXPECTED-DOWNLOAD", result.stderr)
                self.assertFalse((self.home / ".local").exists())

    def test_dependency_elevation_runs_only_explicit_package_manager_commands(self):
        self.write_tool("apt-get", "exit 99")
        self.write_tool("apt-cache", "printf 'libicu76\\n'")
        self.write_tool(
            "sudo",
            'printf "%s\\n" "$*" >> "$HOME/sudo-commands"\n'
            '[ "$1" = apt-get ] || exit 99',
        )
        result = self.run_installer("--install-dependencies")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.home / "sudo-commands").read_text().splitlines(),
            ["apt-get update",
             "apt-get install -y ca-certificates curl tar gzip libsecret-1-0 libicu76"],
        )
        self.assertIn("tunnel setup remains unelevated", result.stdout)
        self.assertIn("FIXTURE-SETUP-EXECUTED", result.stdout)

    def test_macos_dependency_flag_refuses_without_elevation_or_installation(self):
        self.write_tool("uname", 'case "$1" in -s) echo Darwin;; -m) echo arm64;; esac')
        self.assert_refused(self.run_installer("--install-dependencies"), "Linux-only")
        self.assertFalse((self.home / ".local").exists())

    def test_denied_dependency_elevation_refuses_before_download_or_setup(self):
        self.write_tool("apt-get", "exit 99")
        self.write_tool("sudo", "echo FIXTURE-SUDO-DENIED >&2; exit 77")
        self.write_tool("curl", "echo UNEXPECTED-DOWNLOAD >&2; exit 99")
        result = self.run_installer("--install-dependencies")
        self.assert_refused(result, "FIXTURE-SUDO-DENIED")
        self.assertNotIn("UNEXPECTED-DOWNLOAD", result.stderr)
        self.assertFalse((self.home / ".local").exists())

    def test_archive_type_inspection_failure_executes_nothing(self):
        tar = shutil.which("tar")
        self.assertIsNotNone(tar)
        self.write_tool(
            "tar",
            f'[ "$1" != -tvzf ] || exit 17\nexec "{tar}" "$@"',
        )
        self.assert_refused(self.run_installer(), "Could not validate helper archive file types")


class InstallerSyntaxTests(unittest.TestCase):
    @unittest.skipUnless(POWERSHELL, "Installer completion requires a native PowerShell engine")
    def test_powershell_setup_and_upgrade_display_and_copy_connection_profile(self):
        source = WINDOWS_INSTALLER.read_text()
        parameters = source[:source.index("\nSet-StrictMode")]
        start = source.index("    $profile = Join-Path $root 'state\\festerm-profile.json'")
        completion = source[start:source.index("\n} finally {", start)]
        profile_json = json.dumps({"fixture": "public-connection"})
        with tempfile.TemporaryDirectory(prefix="festerm-installer-completion-") as directory:
            root = Path(directory)
            (root / "state").mkdir()
            (root / "state" / "festerm-profile.json").write_text(profile_json)
            script = root / "completion.ps1"
            script.write_text(
                parameters + "\nSet-StrictMode -Version Latest\n"
                "$ErrorActionPreference='Stop'\n"
                "$root=$env:FESTERM_TEST_INSTALL_ROOT\n"
                "$script:copiedProfile=$null\n"
                "function Set-Clipboard { param([string]$Value) $script:copiedProfile=$Value }\n" +
                completion +
                "\nif($script:copiedProfile -cne $env:FESTERM_TEST_PROFILE_JSON){"
                "throw 'Connection profile was not copied correctly'}\n",
                encoding="utf-8-sig",
            )
            for arguments in ([], ["-Upgrade"]):
                with self.subTest(arguments=arguments):
                    result = subprocess.run(
                        [POWERSHELL, "-NoProfile", "-NonInteractive", "-File", str(script),
                         *arguments],
                        env={**os.environ, "FESTERM_TEST_INSTALL_ROOT": str(root),
                             "FESTERM_TEST_PROFILE_JSON": profile_json},
                        capture_output=True, text=True, timeout=30,
                    )
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn(profile_json, result.stdout)
                    self.assertIn("Copied to the clipboard.", result.stdout)

    @unittest.skipIf(os.name == "nt", "Bash native syntax check")
    def test_unix_shell_syntax(self):
        subprocess.run(["bash", "-n", str(UNIX_INSTALLER)], check=True)

    @unittest.skipUnless(POWERSHELL, "PowerShell parser requires a native PowerShell engine")
    def test_powershell_syntax(self):
        result = subprocess.run(
            [POWERSHELL, "-NoLogo", "-NoProfile", "-Command",
             "$tokens=$null; $errors=$null; "
             "[Management.Automation.Language.Parser]::ParseFile($env:FESTERM_INSTALLER_PARSE_PATH,"
             "[ref]$tokens,[ref]$errors) > $null; "
             "if($errors.Count){$errors|ForEach-Object{$_.Message};exit 1}",
             ],
            env={**os.environ, "FESTERM_INSTALLER_PARSE_PATH": str(WINDOWS_INSTALLER)},
            text=True, capture_output=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "Native Windows user/ACL tests")
class WindowsInstallerTests(unittest.TestCase):
    def setUp(self):
        identity = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command",
             "$i=[Security.Principal.WindowsIdentity]::GetCurrent(); "
             "$p=New-Object Security.Principal.WindowsPrincipal($i); "
             "($i.IsSystem -or $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"],
            capture_output=True, text=True, check=True, timeout=15,
        )
        if identity.stdout.strip() != "False":
            self.skipTest("Positive native installer checks require an unelevated ordinary user")
        self.temporary = tempfile.TemporaryDirectory(prefix="festerm-windows-installer-test-", dir=ROOT)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.local = self.root / "profile"
        self.local.mkdir()
        self.curl = self.root / "fixture-curl.cmd"
        source = WINDOWS_INSTALLER.read_text()
        substitutions = {
            "$curl = (Get-Command curl.exe -CommandType Application -ErrorAction Stop).Source":
                "$curl = '" + str(self.curl).replace("'", "''") + "'",
            "$local = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)":
                "$local = '" + str(self.local).replace("'", "''") + "'",
        }
        for original, replacement in substitutions.items():
            self.assertEqual(source.count(original), 1)
            source = source.replace(original, replacement)
        self.script = self.root / "installer.ps1"
        self.script.write_text(source, encoding="utf-8-sig")

    def run_installer(self, *args):
        return subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-File", str(self.script),
             "-Version", VERSION, *args],
            capture_output=True, text=True, timeout=30,
        )

    def assert_clean_refusal(self, result, message):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(message, result.stdout + result.stderr)
        self.assertNotIn("Cannot find path", result.stderr)
        self.assertNotIn("Installed private helpers", result.stdout)
        root = self.local / "fesTerm-tunnels"
        self.assertEqual(sorted(path.name for path in root.iterdir()), ["releases"])
        self.assertEqual(list((root / "releases").iterdir()), [])
        acl = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command",
             "$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User; "
             "$result=@(); foreach($path in @($env:FESTERM_TEST_INSTALL_ROOT,"
             "(Join-Path $env:FESTERM_TEST_INSTALL_ROOT 'releases'))){"
             "$acl=Get-Acl -LiteralPath $path; "
             "$foreign=@($acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier])"
             "|Where-Object {$_.AccessControlType -eq 'Allow' -and $_.IdentityReference -ne $sid}); "
             "$result+=($acl.AreAccessRulesProtected -and "
             "$acl.GetOwner([Security.Principal.SecurityIdentifier]) -eq $sid -and $foreign.Count -eq 0)}; "
             "ConvertTo-Json -InputObject $result -Compress"],
            env={**os.environ, "FESTERM_TEST_INSTALL_ROOT": str(root)},
            capture_output=True, text=True, check=True, timeout=15,
        )
        self.assertEqual(json.loads(acl.stdout), [True, True])

    def test_powershell_download_failure_preserves_error_and_cleans_empty_stage(self):
        self.curl.write_text("@echo off\nexit /b 22\n")
        self.assert_clean_refusal(self.run_installer(), "Authenticated HTTPS download failed")

    def test_management_missing_cache_lists_without_setup_download_or_directory_creation(self):
        self.curl.write_text("@echo off\necho UNEXPECTED-DOWNLOAD\nexit /b 99\n")
        result = self.run_installer("-Manage", "list", "-Json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["hosts"], [])
        self.assertFalse((self.local / "fesTerm-tunnels").exists())

    def test_management_confirmation_and_option_conflicts_refuse_before_setup(self):
        for args in [
            ("-Manage", "delete-all"),
            ("-Manage", "delete", "-InstallationId", "../state", "-Yes"),
            ("-Manage", "list", "-Upgrade"),
            ("-Manage", "list", "-RepairSession"),
            ("-Manage", "list", "-Yes"),
            ("-Json",),
            ("-InstallManagementTools",),
            ("-Manage", "list", "-InstallManagementTools", "-Upgrade"),
            ("-Manage", "delete-all", "-Yes", "-ConfirmLegacyCloudId", "fixture.usw3"),
        ]:
            with self.subTest(args=args):
                result = self.run_installer(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.local / "fesTerm-tunnels").exists())

    def test_management_forwards_to_private_verified_helper_without_provisioning(self):
        root = self.local / "fesTerm-tunnels"
        release = root / "releases" / f"{VERSION}-windows-x86_64"
        source = self.script.read_text(encoding="utf-8-sig")
        # A harmless script fixture replaces the signed executable. Keep actual
        # path, owner, ACL and argument checks; only signature execution is faked.
        source = source.replace(
            "'festerm-tunnel-setup.exe'", "'fixture-management.ps1'"
        ).replace(
            "(Get-AuthenticodeSignature -LiteralPath $helper).Status",
            "([pscustomobject]@{Status='Valid'}).Status",
        )
        self.script.write_text(source, encoding="utf-8-sig")
        private_function = source[
            source.index("function New-PrivateDirectory"):
            source.index("function Assert-PrivateDirectory")
        ]
        directories = (root, root / "releases", release)
        commands = "; ".join(
            "New-PrivateDirectory '" + str(path).replace("'", "''") + "' | Out-Null"
            for path in directories
        )
        subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-Command",
             "$ErrorActionPreference='Stop'; " + private_function + "\n" + commands],
            check=True, capture_output=True, text=True, timeout=30,
        )
        (release / "fixture-management.ps1").write_text(
            "if($args.Count -eq 1 -and $args[0] -eq '--help'){"
            "Write-Output '--manage {list';exit 0};"
            "Write-Output 'FIXTURE-MANAGEMENT';"
            "$args | ForEach-Object { Write-Output $_ };exit 0",
            encoding="utf-8-sig",
        )
        self.curl.write_text("@echo off\necho UNEXPECTED-DOWNLOAD\nexit /b 99\n")
        installation = "12345678-1234-4234-8234-123456789abc"
        for args in [
            ("-Manage", "list", "-Json"),
            ("-Manage", "delete", "-InstallationId", installation, "-Yes"),
            ("-Manage", "delete-all", "-Yes", "-Json"),
            ("-Manage", "archive", "-InstallationId", installation, "-Yes"),
            ("-Manage", "delete", "-InstallationId", installation, "-Yes",
             "-ConfirmLegacyCloudId", "fixture.usw3"),
        ]:
            with self.subTest(args=args):
                result = self.run_installer(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("FIXTURE-MANAGEMENT\n--manage\n", result.stdout)
                self.assertNotIn("--managed\n", result.stdout)
                self.assertNotIn("UNEXPECTED-DOWNLOAD", result.stdout + result.stderr)
                self.assertEqual(sorted(path.name for path in root.iterdir()), ["releases"])

    def test_management_bootstrap_download_failure_never_falls_through_to_setup(self):
        self.curl.write_text("@echo off\nexit /b 22\n")
        result = self.run_installer("-Manage", "list", "-InstallManagementTools")
        self.assert_clean_refusal(result, "Authenticated HTTPS download failed")
        self.assertFalse((self.local / "fesTerm-tunnels/state").exists())

    def test_management_bootstrap_verified_stage_dispatch_returns_before_provisioning(self):
        source = WINDOWS_INSTALLER.read_text()
        start = source.index("    if ($Manage) {\n        $helper = Join-Path $cached")
        end = source.index('    Write-Host "Installed private helpers: $cached"', start)
        dispatch = source[start:end].replace(
            "'festerm-tunnel-setup.exe'", "'fixture-management.ps1'"
        )
        preflight_start = source.index("    if (-not $Manage) {\n        & $cli --version")
        preflight_end = source.index("    if (Test-Path -LiteralPath $cached)", preflight_start)
        preflight = source[preflight_start:preflight_end]
        helper = self.root / "fixture-management.ps1"
        cli = self.root / "fixture-unavailable-cli.ps1"
        cli.write_text(
            "Write-Output 'UNEXPECTED-CLI-RUNTIME';exit 99",
            encoding="utf-8-sig",
        )
        script = self.root / "verified-stage-dispatch.ps1"
        script.write_text(
            "$ErrorActionPreference='Stop';$Manage='list';"
            "$cached='" + str(self.root).replace("'", "''") + "';"
            "$cli='" + str(cli).replace("'", "''") + "';"
            "$managementArguments=@('--manage','list','--json');\n" +
            preflight + dispatch + "\nthrow 'UNEXPECTED-PROVISIONING';\n",
            encoding="utf-8-sig",
        )
        for supported in (True, False):
            helper.write_text(
                "if($args[0] -eq '--help'){Write-Output '" +
                ("--manage {list" if supported else "old helper help") +
                "';exit 0};"
                "if($args[0] -ne '--manage'){throw 'UNEXPECTED-SETUP'};"
                "Write-Output 'FIXTURE-MANAGEMENT';exit 0",
                encoding="utf-8-sig",
            )
            result = subprocess.run(
                [POWERSHELL, "-NoProfile", "-NonInteractive", "-File", str(script)],
                capture_output=True, text=True, timeout=30,
            )
            self.assertNotIn("UNEXPECTED-PROVISIONING", result.stdout + result.stderr)
            self.assertNotIn("UNEXPECTED-SETUP", result.stdout + result.stderr)
            self.assertNotIn("UNEXPECTED-CLI-RUNTIME", result.stdout + result.stderr)
            if supported:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("FIXTURE-MANAGEMENT", result.stdout)
            else:
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("lacks management support", result.stderr)
                self.assertNotIn("FIXTURE-MANAGEMENT", result.stdout)
        script.write_text(
            script.read_text(encoding="utf-8-sig").replace("$Manage='list';", "$Manage='';"),
            encoding="utf-8-sig",
        )
        normal = subprocess.run(
            [POWERSHELL, "-NoProfile", "-NonInteractive", "-File", str(script)],
            capture_output=True, text=True, timeout=30,
        )
        self.assertNotEqual(normal.returncode, 0)
        self.assertIn("UNEXPECTED-CLI-RUNTIME", normal.stdout)
        self.assertIn("runtime prerequisites are unavailable", normal.stderr)
        self.assertNotIn("FIXTURE-MANAGEMENT", normal.stdout)
        self.assertNotIn("UNEXPECTED-PROVISIONING", normal.stderr)

    def test_powershell_invalid_checksum_executes_nothing_and_cleans_stage(self):
        self.curl.write_text(
            '@echo off\n:next\nif "%~1"=="" exit /b 22\n'
            'if "%~1"=="-o" goto write\nshift\ngoto next\n'
            ':write\nshift\n>"%~1" echo invalid-checksum\nexit /b 0\n'
        )
        self.assert_clean_refusal(self.run_installer(), "Checksum does not name this exact release")


if __name__ == "__main__":
    unittest.main()
