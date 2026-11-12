"""Runner contracts only; these tests do not qualify Windows native execution."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def fixture_case_paths(fixture, name):
    return fixture / "applications" / name, fixture / "harnesses" / f"invoke-{name}.ps1"


class WindowsEvidenceEnvironmentTests(unittest.TestCase):
    def test_adapter_checks_and_uses_the_selected_msvc_driver(self):
        source = (ROOT / "scripts/vm-evidence-adapter/windows.ps1").read_text()
        self.assertIn(
            "$ccName = if (Test-Path -LiteralPath (Join-Path $llvmBinPath "
            "'clang-cl.exe')) { 'clang-cl' } else { 'clang' }", source)
        self.assertEqual(source.count('Join-Path $llvmBinPath "$ccName.exe"'), 3)
        self.assertNotIn("Join-Path $llvmBinPath 'clang.exe'", source)
        self.assertEqual(source.count("set CC=$ccName &&"), 3)

    def test_optional_validation_selects_the_same_msvc_driver(self):
        source = (ROOT / "scripts/run-optional-validation.ps1").read_text()
        self.assertIn(
            "$ccName = if (Test-Path -LiteralPath (Join-Path $LlvmBinPath "
            "'clang-cl.exe')) { 'clang-cl' } else { 'clang' }", source)
        self.assertIn('set `"CC=$ccName`"', source)
        self.assertIn("'if errorlevel 1 exit /b %errorlevel%'", source)

    def test_os_input_adapter_keeps_build_tools_and_native_exit_checks(self):
        source = (ROOT / "scripts/vm-evidence-adapter/windows.ps1").read_text()
        body = source.split("{ $_ -in @('os-input-smoke', 'keyboard-routing-native') }", 1)[1]
        body = body.split("'optional-validation' {", 1)[0]
        self.assertIn('call `"$vcvarsallPath`" arm64 >nul &&', body)
        self.assertIn('set `"PATH=$llvmBinPath;%PATH%`" && set CC=$ccName &&', body)
        self.assertIn('powershell.exe -NoProfile -ExecutionPolicy Bypass -File', body)
        self.assertIn('Invoke-NativeCommand { cmd.exe /d /c $osInputCommand }', body)
        self.assertIn("if ($LASTEXITCODE -ne 0)", body)
        self.assertIn("Require-PassStatus $resultPath", body)

    def test_os_input_stderr_scope_does_not_mask_cargo_failure(self):
        source = (ROOT / "scripts/run-windows-os-input-smoke.ps1").read_text()
        self.assertIn(
            "$previousErrorAction = $ErrorActionPreference\n"
            "    $ErrorActionPreference = 'Continue'\n"
            "    $global:LASTEXITCODE = $null\n"
            "    try { cargo @buildArguments } finally { $ErrorActionPreference = $previousErrorAction }\n"
            "    if ($null -eq $global:LASTEXITCODE) {\n"
            "        throw 'Cargo did not start; refusing a stale OS-input executable.'\n"
            "    }\n"
            "    if ($global:LASTEXITCODE -ne 0) { exit $global:LASTEXITCODE }", source)
        self.assertIn("'build','--locked','--workspace'", source)
        self.assertIn("OS-input source changed during build.", source)
        self.assertIn("OS-input checkout changed during build.", source)

    def test_missing_cargo_cannot_reuse_git_success_or_reach_stale_executable(self):
        source = (ROOT / "scripts/run-windows-os-input-smoke.ps1").read_text()
        self.assertNotRegex(source, r"(?m)^\s*\$LASTEXITCODE\s*=")
        build = source.index("$global:LASTEXITCODE = $null")
        call = source.index("try { cargo @buildArguments }", build)
        missing = source.index("if ($null -eq $global:LASTEXITCODE)", call)
        nonzero = source.index(
            "if ($global:LASTEXITCODE -ne 0) { exit $global:LASTEXITCODE }", missing)
        identity = source.index("$builtSource = & git", nonzero)
        executable = source.index("$executableHash = (Get-FileHash", identity)
        self.assertLess(build, call)
        self.assertLess(call, missing)
        self.assertLess(missing, nonzero)
        self.assertLess(nonzero, identity)
        self.assertLess(identity, executable)

    def test_fixture_harness_cannot_be_discovered_as_an_application_suffix(self):
        fixture = Path("owned-fixture")
        for name in ("missing.exe", "unstartable.exe", "success.cmd", "failure.cmd"):
            with self.subTest(application=name):
                application, script = fixture_case_paths(fixture, name)
                self.assertEqual(application.parent, fixture / "applications")
                self.assertEqual(script.parent, fixture / "harnesses")
                self.assertNotEqual(script, Path(str(application) + ".ps1"))
                self.assertNotEqual(script.name, name + ".ps1")

    @unittest.skipUnless(
        os.name == "nt" and (shutil.which("powershell.exe") or shutil.which("pwsh")),
        "Windows PowerShell required for bounded synthetic process-start contract")
    def test_windows_cargo_scope_rejects_missing_and_unstartable_applications(self):
        shell = shutil.which("powershell.exe") or shutil.which("pwsh")
        source = (ROOT / "scripts/run-windows-os-input-smoke.ps1").read_text()
        start = source.index("$previousErrorAction = $ErrorActionPreference")
        end = source.index("$builtSource = & git", start)
        scope = source[start:end]
        scratch = ROOT / "target" / "windows-evidence-contracts"
        scratch.mkdir(mode=0o700, parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            fixture = Path(directory)
            (fixture / "applications").mkdir()
            (fixture / "harnesses").mkdir()
            for name, body, expected in (
                ("missing.exe", None, None),
                ("unstartable.exe", "not a Windows executable\n", None),
                ("success.cmd", "@echo off\nexit /b 0\n", 0),
                ("failure.cmd", "@echo off\nexit /b 7\n", 7),
            ):
                with self.subTest(application=name):
                    application, script = fixture_case_paths(fixture, name)
                    if body is not None:
                        application.write_text(body)
                    else:
                        self.assertFalse(any(application.parent.glob(name + "*")))
                    quoted_application = str(application).replace("'", "''")
                    script.write_text(
                        "$ErrorActionPreference = 'Stop'\n"
                        "$global:LASTEXITCODE = 0\n"
                        f"Set-Alias -Name cargo -Value '{quoted_application}'\n"
                        "$buildArguments = @()\n"
                        f"[Console]::WriteLine('CARGO-CONTRACT-INVOKE:{name}')\n"
                        "try {\n" + scope +
                        "\nexit 0\n} catch {\n"
                        "if ($ErrorActionPreference -ne 'Stop') { exit 91 }\n"
                        "Write-Output 'CARGO-START-REFUSED'\nexit 19\n}\n",
                        encoding="utf-8-sig")
                    try:
                        result = subprocess.run(
                            [shell, "-NoProfile", "-NonInteractive", "-File", str(script)],
                            stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=15)
                    except subprocess.TimeoutExpired as error:
                        self.fail(
                            f"{name}: PowerShell fixture timed out after 15s; "
                            f"stdout={repr(error.stdout)[-2048:]} "
                            f"stderr={repr(error.stderr)[-2048:]}")
                    self.assertEqual(
                        result.stdout.count(f"CARGO-CONTRACT-INVOKE:{name}"), 1,
                        result.stdout + result.stderr)
                    if expected is None:
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertNotEqual(result.returncode, 91, result.stdout + result.stderr)
                    else:
                        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
