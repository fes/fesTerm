import importlib.util
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "prepare_macos_release_config.py"
ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("prepare_macos_release_config", SCRIPT)
release_config = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(release_config)


class PrepareMacosReleaseConfigTests(unittest.TestCase):
    def test_repository_config_retains_plist_and_adds_signing_identity(self):
        source = (ROOT / "packaging/macos.toml").read_text(encoding="utf-8")
        identity = 'Developer ID Application: Example "Team"'

        rendered = release_config.render_release_config(source, identity)
        config = tomllib.loads(rendered)

        self.assertEqual(config["macos"]["signing-identity"], identity)
        self.assertEqual(
            config["macos"]["info-plist-path"],
            "../packaging/macos-info.plist",
        )
        self.assertEqual(rendered.count("[macos]"), 1)

    def test_missing_macos_table_is_rejected(self):
        with self.assertRaisesRegex(
            release_config.ConfigError,
            r"must contain one \[macos\] table",
        ):
            release_config.render_release_config('name = "festerm"\n', "identity")

    def test_checked_in_signing_identity_is_rejected(self):
        source = '[macos]\nsigning-identity = "unexpected"\n'
        with self.assertRaisesRegex(
            release_config.ConfigError,
            "must not contain a signing identity",
        ):
            release_config.render_release_config(source, "identity")

    def test_command_writes_a_parseable_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "macos.toml"
            output = root / "macos-release.toml"
            source.write_text(
                '[macos]\ninfo-plist-path = "Info.plist"\n',
                encoding="utf-8",
            )

            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--input",
                    str(source),
                    "--output",
                    str(output),
                    "--signing-identity",
                    "Developer ID Application: Example",
                ],
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                tomllib.loads(output.read_text(encoding="utf-8"))["macos"][
                    "signing-identity"
                ],
                "Developer ID Application: Example",
            )


if __name__ == "__main__":
    unittest.main()
