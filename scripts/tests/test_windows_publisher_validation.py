"""Offline acquisition/orchestration contracts; never native PKI qualification."""

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PureWindowsPath
import re
import shutil
import stat
import subprocess
import sys
import unittest
from unittest import mock
import urllib.request
import uuid
import zipfile


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "windows_publisher_validation", ROOT / "scripts/run_windows_publisher_validation.py"
)
PKI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PKI)


class FixtureContracts(unittest.TestCase):
    def setUp(self):
        self.root = ROOT / (".pki-contract-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root)

    def archive(self, names=None, mode=stat.S_IFREG | 0o755):
        path = self.root / "fixture.zip"
        with zipfile.ZipFile(path, "w") as archive:
            for name in names or [PKI.MEMBER]:
                entry = zipfile.ZipInfo(name)
                entry.create_system = 3
                entry.external_attr = mode << 16
                archive.writestr(entry, b"synthetic-not-an-executable")
        return path

    def extract(self, path):
        # Only synthetic fixtures replace pins, inside this offline test module.
        with mock.patch.object(PKI, "ZIP_SHA256", PKI.digest(path)), mock.patch.object(
            PKI, "MEMBER_SHA256", hashlib.sha256(b"synthetic-not-an-executable").hexdigest()
        ):
            PKI.extract_member(path, self.root / PKI.MEMBER)

    def test_exact_published_fixture_and_selector_are_fixed(self):
        self.assertEqual(PKI.URL, "https://github.com/fes/fesTerm/releases/download/"
                         "v0.16.0/fesTerm-tunnel-0.16.0-windows-x86_64.zip")
        self.assertEqual(PKI.ZIP_SHA256,
                         "3edfbf5a0725b70a0bc56448d81bb3745e25d745a2cd670978ecc1f33f51e896")
        self.assertEqual(PKI.MEMBER, "festerm-sessiond.exe")
        self.assertEqual(PKI.MEMBER_SHA256,
                         "4cd3e1b12026412717848cef2a872e110c50039bc0a6464ce8bb05a2781a481b")
        self.assertIn("--exact", PKI.TEST_COMMAND)
        self.assertIn("--ignored", PKI.TEST_COMMAND)
        self.assertIn("--lib", PKI.TEST_COMMAND)
        self.assertIn(PKI.TEST, PKI.TEST_COMMAND)

    def test_native_entry_requires_windows_and_both_explicit_opt_ins(self):
        with mock.patch.object(sys, "platform", "darwin"), mock.patch.dict(
            os.environ, {}, clear=True
        ):
            with self.assertRaises(PKI.ValidationFailure):
                PKI.require_opt_in()
        for enabled in ((), ("FESTERM_RUN_OPTIONAL_VALIDATION",),
                        ("FESTERM_RUN_ADR84_PUBLISHER_NATIVE",)):
            with self.subTest(enabled=enabled), mock.patch.object(
                sys, "platform", "win32"
            ), mock.patch.dict(os.environ, dict.fromkeys(enabled, "1"), clear=True):
                with self.assertRaises(PKI.ValidationFailure):
                    PKI.require_opt_in()
        with mock.patch.object(sys, "platform", "win32"), mock.patch.dict(os.environ, {
            "FESTERM_RUN_OPTIONAL_VALIDATION": "1",
            "FESTERM_RUN_ADR84_PUBLISHER_NATIVE": "1",
        }, clear=True):
            PKI.require_opt_in()

    def test_new_checkout_relative_directory_refuses_existing_or_escape(self):
        relative = self.root.relative_to(ROOT) / "new"
        output = PKI.fresh_directory(relative)
        (output / "sentinel").write_bytes(b"preserve")
        with self.assertRaises(FileExistsError):
            PKI.fresh_directory(relative)
        for path in (ROOT, "../escape", ".", ""):
            with self.subTest(path=path), self.assertRaises(PKI.ValidationFailure):
                PKI.fresh_directory(path)
        self.assertEqual((output / "sentinel").read_bytes(), b"preserve")

    def test_cli_missing_opt_in_cannot_download_or_start_commands(self):
        with mock.patch.object(sys, "argv", ["publisher"]), \
                mock.patch.object(sys, "platform", "win32"), \
                mock.patch.dict(os.environ, {}, clear=True), \
                mock.patch.object(PKI, "acquire") as acquire, \
                mock.patch.object(PKI, "run") as run:
            with self.assertRaises(PKI.ValidationFailure):
                PKI.main()
            acquire.assert_not_called()
            run.assert_not_called()

    def test_windows_anchors_are_refused_before_root_join_or_filesystem(self):
        for value in ("/outside", r"\outside", "C:outside", r"C:\outside",
                      r"\\server\share\outside"):
            lexical = PureWindowsPath(value)
            self.assertTrue(lexical.anchor)
            root = mock.MagicMock()
            root.__truediv__.side_effect = AssertionError("guard reached ROOT join")
            with self.subTest(path=value), mock.patch.object(
                PKI, "Path", return_value=lexical
            ), mock.patch.object(PKI, "ROOT", root):
                with self.assertRaises(PKI.ValidationFailure):
                    PKI.fresh_directory(value)
                root.__truediv__.assert_not_called()

    def test_cli_has_no_url_member_hash_or_policy_override(self):
        for flag in ("--url", "--member", "--sha256", "--allow-offline"):
            with self.subTest(flag=flag), mock.patch.object(
                sys, "argv", ["publisher", flag, "not-accepted"]
            ), mock.patch.object(sys, "stderr", io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    PKI.main()
                self.assertEqual(error.exception.code, 2)

    def test_evidence_directory_refuses_symlink_ancestors(self):
        if os.name == "nt":
            # Do not request symlink privileges on hosted Windows.
            with mock.patch.object(Path, "is_symlink", return_value=True):
                with self.assertRaises(PKI.ValidationFailure):
                    PKI.fresh_directory(self.root.relative_to(ROOT) / "linked")
        else:
            link = self.root / "link"
            link.symlink_to(self.root, target_is_directory=True)
            with self.assertRaises(PKI.ValidationFailure):
                PKI.fresh_directory(link.relative_to(ROOT) / "new")

    def test_download_uses_only_fixed_public_request_and_checks_digest(self):
        body = b"synthetic-public-response"
        response = io.BytesIO(body)
        response.status = 200
        response.headers = {"Content-Length": str(len(body))}
        opener = mock.Mock()
        opener.open.return_value = response
        with mock.patch.object(PKI.urllib.request, "build_opener", return_value=opener), \
                mock.patch.object(PKI, "ZIP_SHA256", hashlib.sha256(body).hexdigest()):
            output = self.root / "download.zip"
            PKI.download(output)
        self.assertEqual(opener.open.call_args.args[0].full_url, PKI.URL)
        self.assertEqual(opener.open.call_args.kwargs["timeout"], 15)
        self.assertEqual(output.read_bytes(), body)

    def test_download_refuses_status_declared_length_truncation_and_bad_hash(self):
        cases = ((206, {}, b"x"), (200, {"Content-Length": str(PKI.MAX_ZIP + 1)}, b"x"),
                 (200, {"Content-Length": "5"}, b"x"), (200, {}, b"wrong-hash"))
        for index, (status, headers, body) in enumerate(cases):
            response = io.BytesIO(body)
            response.status, response.headers = status, headers
            opener = mock.Mock()
            opener.open.return_value = response
            with self.subTest(index=index), mock.patch.object(
                PKI.urllib.request, "build_opener", return_value=opener
            ), self.assertRaises(PKI.ValidationFailure):
                PKI.download(self.root / f"bad-{index}.zip")

    def test_redirects_refuse_downgrade_foreign_host_credentials_and_fragments(self):
        handler = PKI.PublicReleaseRedirects()
        request = urllib.request.Request(PKI.URL)
        good = "https://release-assets.githubusercontent.com/public-fixture"
        redirected = handler.redirect_request(request, None, 302, "", {}, good)
        self.assertEqual(redirected.full_url, good)
        for url in ("http://release-assets.githubusercontent.com/a",
                    "https://evil.invalid/a", "https://user@release-assets.githubusercontent.com/a",
                    good + "#fragment", "https://release-assets.githubusercontent.com:444/a"):
            with self.subTest(url=url), self.assertRaises(PKI.ValidationFailure):
                handler.redirect_request(request, None, 302, "", {}, url)
        self.assertEqual(handler.max_redirections, 3)

    def test_copy_refuses_byte_and_deadline_exhaustion(self):
        with self.assertRaises(PKI.ValidationFailure):
            PKI.copy_bounded(io.BytesIO(b"too long"), io.BytesIO(), 2, float("inf"))
        with self.assertRaises(PKI.ValidationFailure):
            PKI.copy_bounded(io.BytesIO(b"x"), io.BytesIO(), 2, 0)
        with mock.patch.object(PKI.time, "monotonic", side_effect=[0, 2]):
            with self.assertRaises(PKI.ValidationFailure):
                PKI.copy_bounded(io.BytesIO(b"x"), io.BytesIO(), 2, 1)
        with self.assertRaises(PKI.ValidationFailure):
            PKI.digest(self.archive(), maximum=1)

    def test_only_exact_member_is_extracted_and_existing_output_is_not_overwritten(self):
        archive = self.archive([PKI.MEMBER, "VERSION"])
        self.extract(archive)
        self.assertFalse((self.root / "VERSION").exists())
        with self.assertRaises(FileExistsError):
            self.extract(archive)
        self.assertEqual((self.root / PKI.MEMBER).read_bytes(), b"synthetic-not-an-executable")

    def test_zip_refuses_archive_hash_and_member_hash_mismatch(self):
        archive = self.archive()
        with self.assertRaises(PKI.ValidationFailure):
            PKI.extract_member(archive, self.root / PKI.MEMBER)
        with mock.patch.object(PKI, "ZIP_SHA256", PKI.digest(archive)):
            with self.assertRaises(PKI.ValidationFailure):
                PKI.extract_member(archive, self.root / PKI.MEMBER)

    def test_zip_refuses_traversal_duplicates_nonfiles_and_missing_member(self):
        cases = [
            ([PKI.MEMBER, "../escape"], stat.S_IFREG),
            ([PKI.MEMBER, "folder\\escape"], stat.S_IFREG),
            ([PKI.MEMBER, "C:escape"], stat.S_IFREG),
            ([PKI.MEMBER, PKI.MEMBER.upper()], stat.S_IFREG),
            ([PKI.MEMBER, "/absolute"], stat.S_IFREG),
            ([PKI.MEMBER, "trailing."], stat.S_IFREG),
            ([PKI.MEMBER], stat.S_IFLNK),
            ([PKI.MEMBER], stat.S_IFDIR),
            (["other.exe"], stat.S_IFREG),
        ]
        for names, mode in cases:
            with self.subTest(names=names, mode=mode), self.assertRaises(PKI.ValidationFailure):
                self.extract(self.archive(names, mode))
            self.assertFalse((self.root / PKI.MEMBER).exists())

    def test_zip_refuses_entry_total_and_member_bounds(self):
        archive = self.archive()
        for name, value in (("MAX_MEMBER", 1), ("MAX_EXPANDED", 1), ("MAX_ZIP", 1)):
            with self.subTest(bound=name), mock.patch.object(PKI, name, value):
                with self.assertRaises(PKI.ValidationFailure):
                    self.extract(archive)

    def test_zip_refuses_nul_filename_and_excess_entries(self):
        archive = self.archive([PKI.MEMBER, "nulNAME"])
        archive.write_bytes(archive.read_bytes().replace(b"nulNAME", b"nul\x00AME"))
        with self.assertRaises(PKI.ValidationFailure):
            self.extract(archive)
        archive = self.archive([PKI.MEMBER] + [f"entry-{index}" for index in range(64)])
        with self.assertRaises(PKI.ValidationFailure):
            self.extract(archive)

    def test_command_preserves_actual_failure_and_timeout_without_native_execution(self):
        for name, outcome in (("failed", subprocess.CompletedProcess(["cargo"], 101)),
                              ("timeout", subprocess.TimeoutExpired(["cargo"], 1))):
            report = {"commands": []}
            with self.subTest(name=name), mock.patch.object(PKI.subprocess, "run") as invoke:
                if isinstance(outcome, Exception):
                    invoke.side_effect = outcome
                else:
                    invoke.return_value = outcome
                with self.assertRaises(PKI.ValidationFailure):
                    PKI.command(self.root, report, name, ["cargo", "test"], 1)
            evidence = report["commands"][0]
            self.assertEqual(evidence["exit_code"], 101 if name == "failed" else None)
            self.assertEqual(evidence["timed_out"], name == "timeout")
            self.assertEqual(evidence["sha256"], PKI.digest(self.root / (name + ".log")))

    def orchestration(self, native_text, code=0):
        output = self.root.relative_to(ROOT) / "run"

        def fake_command(directory, report, name, arguments, timeout, environment=None):
            log = directory / (name + ".log")
            text = ""
            if name == "source-head":
                text = "a" * 40
            if name == "native-pki":
                self.assertEqual(tuple(arguments), PKI.TEST_COMMAND)
                self.assertEqual(environment["CARGO_BUILD_JOBS"], "1")
                self.assertEqual(environment["CARGO_INCREMENTAL"], "0")
                self.assertEqual(environment["FESTERM_ADR84_SIGNED_HOST_FIXTURE"],
                                 str(ROOT / output / "fixture" / PKI.MEMBER))
                text = native_text
            if name == "acquisition":
                self.assertEqual(timeout, 150)
                self.assertIn("--acquire-only", arguments)
                self.assertNotEqual(arguments[0], PKI.MEMBER)
            log.write_text(text)
            report["commands"].append({"argv": list(arguments), "exit_code": code if
                                       name == "native-pki" else 0, "sha256": PKI.digest(log)})
            if name == "native-pki" and code:
                raise PKI.ValidationFailure("synthetic native failure", code)
            return log

        with mock.patch.object(PKI, "command", side_effect=fake_command), \
                mock.patch.object(PKI, "require_digest", side_effect=lambda path, expected: expected), \
                mock.patch.dict(os.environ, {
                    "GITHUB_SHA": "a" * 40,
                    "FESTERM_ADR84_SIGNED_HOST_FIXTURE": "foreign-input-must-be-ignored",
                }):
            result = PKI.run(output)
        return result, json.loads((ROOT / output / "report.json").read_text())

    def test_zero_tests_cannot_be_native_success(self):
        code, report = self.orchestration("test result: ok. 0 passed; 0 failed; 0 ignored;")
        self.assertNotEqual(code, 0)
        self.assertFalse(report["native_pki_fixture_passed"])
        self.assertEqual(report["status"], "failed")

    def test_native_failure_is_not_waived_or_changed_to_success(self):
        code, report = self.orchestration("synthetic policy mismatch", 101)
        self.assertEqual(code, 101)
        self.assertFalse(report["native_pki_fixture_passed"])
        self.assertEqual(report["commands"][-1]["exit_code"], 101)

    def test_only_selected_pki_success_never_qualifies_runtime_or_hash_authentication(self):
        code, report = self.orchestration(
            PKI.TEST + "\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"
            "native_task_runtime_qualification=false; fixture=read-only-publisher"
        )
        self.assertEqual(code, 0)
        self.assertTrue(report["native_pki_fixture_passed"])
        for key in ("fixture_hashes_are_authentication", "native_task_runtime_qualification",
                    "ordinary_user_vm_qualification", "whole_bundle_release_qualification",
                    "daemon_gui_qualification"):
            self.assertFalse(report[key], key)
        self.assertFalse(any(PKI.MEMBER in command["argv"][0] for command in report["commands"]))


@unittest.skipUnless(shutil.which("pwsh") or os.name == "nt",
                     "PowerShell 7 unavailable locally; launcher execution not tested")
class PublisherLauncherContracts(unittest.TestCase):
    def probe(self, exit_code):
        powershell = shutil.which("pwsh")
        self.assertIsNotNone(powershell, "Windows preflight requires PowerShell 7")
        directory = ROOT / (".pki launcher-" + uuid.uuid4().hex)
        directory.mkdir()
        self.addCleanup(shutil.rmtree, directory)
        commands = []
        for name in ("first", "second"):
            parent = directory / name
            parent.mkdir()
            command = parent / ("python.cmd" if os.name == "nt" else "python")
            code = exit_code if name == "first" else 0
            if os.name == "nt":
                command.write_text(
                    f'@echo off\necho {name}>>"%FESTERM_LAUNCHER_MARKER%"\nexit /b {code}\n'
                )
            else:
                command.write_text(
                    f'#!/bin/sh\nprintf "{name}\\n" >> "$FESTERM_LAUNCHER_MARKER"\nexit {code}\n'
                )
                command.chmod(0o700)
            commands.append(command)
        marker = directory / "invocations.txt"
        environment = os.environ.copy()
        environment.update({
            "FESTERM_OPTIONAL_SCRIPT": str(ROOT / "scripts/run-optional-validation.ps1"),
            "FESTERM_FIRST_COMMAND": str(commands[0]),
            "FESTERM_SECOND_COMMAND": str(commands[1]),
            "FESTERM_LAUNCHER_MARKER": str(marker),
            "FESTERM_LAUNCHER_EXIT": str(exit_code),
        })
        result = subprocess.run(
            [powershell, "-NoProfile", "-NonInteractive", "-Command", r"""
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
function Assert($condition, $message) { if (-not $condition) { throw $message } }
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    $env:FESTERM_OPTIONAL_SCRIPT, [ref]$tokens, [ref]$errors)
Assert ($errors.Count -eq 0) 'Aggregate script parses'
foreach ($name in @('Invoke-NativeCommand', 'Invoke-WindowsPublisherValidation')) {
    $definition = $ast.Find({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq $name
    }, $true)
    Assert ($null -ne $definition) 'Actual launcher function exists'
    . ([scriptblock]::Create($definition.Extent.Text))
}
$script:applications = @(
    Microsoft.PowerShell.Core\Get-Command $env:FESTERM_FIRST_COMMAND -CommandType Application
    Microsoft.PowerShell.Core\Get-Command $env:FESTERM_SECOND_COMMAND -CommandType Application
)
Assert ($script:applications.Count -eq 2) 'Two executable matches'
foreach ($application in $script:applications) {
    Assert ($application -is [Management.Automation.ApplicationInfo]) 'Real application metadata'
}
# Mock only discovery, preserving PATH-priority order; execute only owned fixtures.
function Get-Command {
    [CmdletBinding()]
    param([string] $Name, [Management.Automation.CommandTypes] $CommandType)
    Assert ($Name -eq 'python') 'Only Python is resolved'
    Assert ($CommandType -eq [Management.Automation.CommandTypes]::Application) 'Application only'
    $script:applications
}
$rejected = $false
try { Invoke-WindowsPublisherValidation } catch {
    Assert ($_.Exception.Message -like 'Read-only Windows publisher validation failed*') 'Native failure retained'
    $rejected = $true
}
$nativeExit = $LASTEXITCODE
Assert ($nativeExit -eq [int]$env:FESTERM_LAUNCHER_EXIT) 'Actual native exit preserved'
Assert ($rejected -eq ($nativeExit -ne 0)) 'Nonzero is never accepted'
$invocations = @(Get-Content -LiteralPath $env:FESTERM_LAUNCHER_MARKER)
Assert ($invocations.Count -eq 1 -and $invocations[0] -eq 'first') 'Second executable never invoked'
Write-Output 'launcher_fixture_only=true native_pki_tested=false'
exit $nativeExit
"""],
            cwd=ROOT, env=environment, capture_output=True, text=True, timeout=20, check=False,
        )
        self.assertEqual(result.returncode, exit_code, result.stdout + result.stderr)
        self.assertIn("launcher_fixture_only=true native_pki_tested=false", result.stdout)
        self.assertEqual(marker.read_text().splitlines(), ["first"])

    def test_two_application_matches_execute_only_first(self):
        self.probe(0)

    def test_two_application_matches_preserve_first_native_failure_without_fallback(self):
        self.probe(37)


class WorkflowContracts(unittest.TestCase):
    def test_publisher_launcher_selects_one_application_before_source(self):
        script = (ROOT / "scripts/run-optional-validation.ps1").read_text()
        self.assertRegex(
            script,
            r"\$python\s*=\s*\(Get-Command python -CommandType Application -ErrorAction Stop"
            r"\s*\|\s*Select-Object -First 1\)\.Source",
        )
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  windows-native-pki:", 1)[1].split(
            "  windows-sampler-diagnostics:", 1
        )[0]
        self.assertIn('test_windows_publisher_validation.py', job.split("id: publisher", 1)[0])

    def test_dispatch_is_default_false_read_only_bounded_and_only_runs_pki_job(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        setting = workflow.split("      windows_native_pki:", 1)[1].split(
            "      windows_sampler_diagnostics:", 1
        )[0]
        self.assertIn("type: boolean", setting)
        self.assertIn("default: false", setting)
        job = workflow.split("  windows-native-pki:", 1)[1].split(
            "  windows-sampler-diagnostics:", 1
        )[0]
        self.assertIn("if: github.event_name == 'workflow_dispatch' && inputs.windows_native_pki", job)
        self.assertIn("runs-on: windows-latest", job)
        self.assertIn("timeout-minutes: 20", job)
        self.assertIn("contents: read", job)
        self.assertIn("persist-credentials: false", job)
        self.assertIn("-WindowsPublisherOnly", job)
        self.assertIn("if: inputs.windows_sampler_diagnostics", job)
        self.assertIn("if: always() && steps.publisher.outcome", job)
        self.assertIn("include-hidden-files: true", job)
        self.assertIn(".windows-pki-evidence/*.json", job)
        self.assertIn(".windows-pki-evidence/*.log", job)
        self.assertNotIn(".windows-pki-evidence/**", job)
        self.assertNotIn("continue-on-error", job)
        self.assertNotIn("secrets.", job)
        for action in re.findall(r"uses: ([^\s]+)", job):
            self.assertRegex(action, r"@[a-f0-9]{40}$")
        for name in ("checks", "helper-signature-compatibility", "conformance",
                     "windows-security-privileged", "quality"):
            body = workflow.split(f"  {name}:", 1)[1].split("    steps:", 1)[0]
            self.assertIn("if: github.event_name != 'workflow_dispatch' || !inputs.windows_native_pki", body)
        self.assertIn("inputs.windows_sampler_diagnostics && !inputs.windows_native_pki", workflow)

    def test_optional_single_suite_returns_before_unrelated_native_or_gui_work(self):
        script = (ROOT / "scripts/run-optional-validation.ps1").read_text()
        branch = script.split("if ($WindowsPublisherOnly) {", 1)[1].split(
            "if ($MarkdownScrollingOnly) {", 1
        )[0]
        self.assertIn("Invoke-WindowsPublisherValidation", branch)
        self.assertIn("[IO.FileMode]::CreateNew", branch)
        self.assertIn("native_task_runtime_qualification=false", branch)
        self.assertIn("return", branch)
        self.assertNotIn("cargo test", branch)
        self.assertNotIn("Invoke-MarkdownScrollingValidation", branch)
        self.assertIn("run_windows_publisher_validation.py", script)

if __name__ == "__main__":
    unittest.main()
