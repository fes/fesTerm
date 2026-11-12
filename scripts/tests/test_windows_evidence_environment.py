"""Source contracts only; these tests do not qualify Windows native execution."""

from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


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
            "    try { cargo @buildArguments } finally { $ErrorActionPreference = $previousErrorAction }\n"
            "    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }", source)
        self.assertIn("'build','--locked','--workspace'", source)
        self.assertIn("OS-input source changed during build.", source)
        self.assertIn("OS-input checkout changed during build.", source)


if __name__ == "__main__":
    unittest.main()
