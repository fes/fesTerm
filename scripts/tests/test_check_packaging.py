import importlib.util
import shutil
import subprocess
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check_packaging.py"
SPEC = importlib.util.spec_from_file_location("check_packaging", SCRIPT)
packaging = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(packaging)
BASH = shutil.which("bash")


class PackagingMetadataTests(unittest.TestCase):
    def test_native_renderer_changes_trigger_package_smoke(self):
        workflow = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        triggers = workflow.split("\npermissions:", 1)[0]
        self.assertRegex(
            triggers,
            r"(?m)^      - 'crates/festerm-windows-direct2d/\*\*'$",
        )

    @unittest.skipUnless(BASH, "Linux package smoke requires bash")
    def test_linux_appimage_smoke_has_valid_shell_and_heredoc_syntax(self):
        workflow = (packaging.ROOT / ".github/workflows/package-smoke.yml").read_text(
            encoding="utf-8"
        )
        step = workflow.split("      - name: Build unsigned Linux AppImage\n", 1)[1]
        step = step.split("\n      - name:", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        assert BASH is not None
        result = subprocess.run(
            [BASH, "-n"],
            input=script,
            text=True,
            capture_output=True,
            check=False,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("here-document", result.stderr)

    def test_repository_packaging_metadata_is_consistent(self):
        packaging.verify()

    def test_all_native_platforms_have_explicit_formats(self):
        self.assertEqual(
            set(packaging.EXPECTED_FORMATS),
            {"macos", "windows", "linux"},
        )
        self.assertNotIn("all", {
            package_format
            for formats in packaging.EXPECTED_FORMATS.values()
            for package_format in formats
        })

    def test_windows_helper_name_is_tied_to_the_workspace_release(self):
        version = packaging.workspace_version()
        windows = packaging.load_toml(packaging.CONFIGS["windows"])
        self.assertEqual(windows["binaries"], [{"path": "festerm", "main": True}])
        self.assertIn(
            {
                "src": f"../target/release/festerm-sessiond-{version}.exe",
                "target": f"festerm-sessiond-{version}.exe",
            },
            windows["resources"],
        )
        self.assertNotIn(
            "festerm-sessiond.exe",
            {resource["target"] for resource in windows["resources"]},
        )

    def test_markdown_open_with_packaging_is_alternative_only(self):
        macos = packaging.load_toml(packaging.CONFIGS["macos"])
        windows = packaging.load_toml(packaging.CONFIGS["windows"])
        linux = packaging.load_toml(packaging.CONFIGS["linux"])

        self.assertEqual(
            macos["macos"]["info-plist-path"],
            "../packaging/macos-info.plist",
        )
        self.assertNotIn("file-associations", macos)

        self.assertEqual(
            windows["nsis"]["template"],
            "../packaging/windows-open-with.nsi",
        )
        self.assertNotIn("file-associations", windows)

        self.assertEqual(
            linux["file-associations"],
            [packaging.EXPECTED_MARKDOWN_ASSOCIATION],
        )
        self.assertEqual(
            linux["deb"]["desktop-template"],
            "../packaging/linux-desktop-entry.desktop",
        )


if __name__ == "__main__":
    unittest.main()
