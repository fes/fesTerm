"""Check Direct2D probe preconditions without launching a native window."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


class Direct2DFallbackDiagnosticTests(unittest.TestCase):
    def test_automatic_native_evidence_binds_source_before_desktop_access(self):
        root = Path(__file__).resolve().parents[2]
        driver = (root / "validation/terminal-performance/compare-windows.ps1").read_text()
        desktop = driver.index("[FesTermApplicationWindow]::RequireInteractiveDesktop()")
        build = driver.index('& "$root\\scripts\\stage-conpty.ps1" -Configuration Release')
        self.assertLess(driver.index("Assert-NativeCandidateExecutable $FesTerm"), build)
        self.assertLess(driver.index("if ($dirty.Count -gt 0)"), build)
        self.assertLess(build, desktop)
        self.assertLess(driver.index("$builtSource -ne $source"), desktop)
        self.assertIn("SourceAttribution='driver-built-clean-checkout'", driver)
        smoke = (root / "scripts/run-windows-os-input-smoke.ps1").read_text()
        self.assertIn("SourceSha=$(if ($SkipBuild) { $null }", smoke)
        self.assertIn("CompositionPolicy=$(if ($SkipBuild) { 'unverified-executable' }", smoke)
        self.assertIn("'unverified-prebuilt-executable'", smoke)

    def test_production_warp_installation_does_not_read_retired_switches(self):
        source = (Path(__file__).resolve().parents[2] / "app/festerm/src/direct2d.rs").read_text()
        for suffix in ("DIRECT2D", "HOST_COPY", "RETAINED_COMPOSITION"):
            self.assertNotIn("FESTERM_EXPERIMENTAL_" + suffix, source)
        self.assertIn("CompositionSelection::for_adapter(", source)

    def test_native_required_probes_reject_unsupported_frame_transitions(self):
        root = Path(__file__).resolve().parents[2]
        source = (root / "app" / "festerm" / "src" / "direct2d.rs").read_text(
            encoding="utf-8"
        )
        match = re.search(
            r'"(unsupported Direct2D frame;[^"]+)"', source
        )
        self.assertIsNotNone(match)
        message = match.group(1)
        for relative in (
            ("scripts", "check-windows-idle-rendering.ps1"),
            ("validation", "terminal-performance", "compare-windows.ps1"),
        ):
            with self.subTest(script=relative):
                script = root.joinpath(*relative).read_text(encoding="utf-8")
                patterns = [
                    pattern
                    for pattern in re.findall(r"-Pattern '([^']+)'", script)
                    if "Direct2D state poisoned" in pattern
                ]
                self.assertEqual(len(patterns), 1)
                self.assertRegex(message, patterns[0])


@unittest.skipUnless(sys.platform == "win32", "Windows rendering probe")
class Direct2DProbePolicyTests(unittest.TestCase):
    def test_retired_renderer_settings_do_not_change_probe_preconditions(self):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 is required for native probe tests")
        script = (
            Path(__file__).resolve().parents[1] / "check-windows-idle-rendering.ps1"
        )
        with tempfile.TemporaryDirectory() as directory:
            missing_executable = Path(directory) / "missing-rendering-policy-fixture.exe"
            for value in [None, "1", "0", "invalid", " 1"]:
                with self.subTest(value=value):
                    environment = os.environ.copy()
                    environment["FESTERM_RUN_OPTIONAL_VALIDATION"] = "1"
                    environment.pop("FESTERM_EXPERIMENTAL_DIRECT2D", None)
                    if value is not None:
                        environment["FESTERM_EXPERIMENTAL_DIRECT2D"] = value
                    result = subprocess.run(
                        [
                            shell,
                            "-NoProfile",
                            "-NonInteractive",
                            "-File",
                            str(script),
                            "-Executable",
                            str(missing_executable),
                            "-IncludeSustainedOutput",
                            "-RequireDirect2D",
                        ],
                        env=environment,
                        capture_output=True,
                        text=True,
                        timeout=30,
                        check=False,
                    )
                    output = result.stdout + result.stderr
                    self.assertNotEqual(result.returncode, 0, output)
                    self.assertIn(missing_executable.name, output)
                    self.assertNotIn("RequireDirect2D requires FESTERM_EXPERIMENTAL_DIRECT2D", output)


if __name__ == "__main__":
    unittest.main()
