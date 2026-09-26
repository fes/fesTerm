"""Check Direct2D probe preconditions without launching a native window."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(sys.platform == "win32", "Windows rendering probe")
class Direct2DProbePolicyTests(unittest.TestCase):
    def test_default_and_explicit_selection_reach_executable_validation(self):
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
                    if value in (None, "1"):
                        self.assertIn(missing_executable.name, output)
                        self.assertNotIn(
                            "RequireDirect2D requires FESTERM_EXPERIMENTAL_DIRECT2D",
                            output,
                        )
                    else:
                        self.assertIn(
                            "RequireDirect2D requires FESTERM_EXPERIMENTAL_DIRECT2D",
                            output,
                        )


if __name__ == "__main__":
    unittest.main()
