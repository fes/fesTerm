import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).resolve().parents[1] / "run-development.py"
SPEC = importlib.util.spec_from_file_location("development", SCRIPT)
launcher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(launcher)


class DevelopmentLaunchTests(unittest.TestCase):
    def test_environment_overrides_inherited_runtime_and_removes_release_keys(self):
        inherited = {
            "PATH": "/tools",
            "DISPLAY": ":1",
            "FESTERM_CONFIG_PATH": "/installed/config",
            "FESTERM_SESSIOND_RUNTIME_ROOT": "/installed/sessiond",
            "XDG_STATE_HOME": "/installed/state",
            "LOCALAPPDATA": "installed-local",
            "APPLE_ID": "account",
            "CARGO_PACKAGER_SIGN_PRIVATE_KEY": "secret",
            "FESTERM_UPDATE_PUBLIC_KEY": "key",
        }
        root = Path("/development")
        for system, key in (("Linux", "XDG_STATE_HOME"), ("Windows", "LOCALAPPDATA")):
            environment = launcher.development_environment(root, inherited, system)
            self.assertEqual(environment["PATH"], "/tools")
            self.assertEqual(environment["DISPLAY"], ":1")
            self.assertEqual(environment["FESTERM_INSTALLATION_KIND"], "development")
            self.assertEqual(environment["FESTERM_CONFIG_PATH"],
                             str(root / "target/festerm-dev/config.toml"))
            self.assertEqual(environment["FESTERM_SESSIOND_RUNTIME_ROOT"],
                             str(root / "target/festerm-dev/sessiond"))
            self.assertEqual(environment[key], str(root / "target/festerm-dev/state"))
            for secret in ("APPLE_ID", "CARGO_PACKAGER_SIGN_PRIVATE_KEY",
                           "FESTERM_UPDATE_PUBLIC_KEY"):
                self.assertNotIn(secret, environment)

    def test_macos_uses_its_branded_launcher(self):
        with self.assertRaisesRegex(launcher.LaunchError, "run-macos-development"):
            launcher.development_environment(Path("/development"), {}, "Darwin")

    def test_state_is_private_reusable_and_refuses_symlinked_config(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            launcher.prepare_state(root)
            state = root / "target/festerm-dev"
            self.assertTrue(state.is_dir())
            if os.name != "nt":
                self.assertEqual(state.stat().st_mode & 0o777, 0o700)
            launcher.prepare_state(root)
            try:
                (state / "config.toml").symlink_to(root / "installed-config")
            except OSError:
                self.skipTest("symbolic links are unavailable")
            with self.assertRaisesRegex(launcher.LaunchError, "symbolic link"):
                launcher.prepare_state(root)

    @unittest.skipIf(os.name == "nt", "Unix mode bits")
    def test_existing_nonprivate_state_is_refused_without_chmod(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            launcher.prepare_state(root)
            state = root / "target/festerm-dev"
            state.chmod(0o755)
            with self.assertRaisesRegex(launcher.LaunchError, "private"):
                launcher.prepare_state(root)
            self.assertEqual(state.stat().st_mode & 0o777, 0o755)

    def test_builds_helpers_before_forwarding_arguments_and_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(launcher, "ROOT", root), \
                    patch.object(launcher.platform, "system", return_value="Linux"), \
                    patch.dict(os.environ, {"FESTERM_SESSIOND_RUNTIME_ROOT": "/installed"}), \
                    patch.object(launcher.subprocess, "run",
                                 side_effect=[Mock(), Mock(), Mock()]) as run, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(launcher.main(["--", "--example", "two words"]), 0)
            self.assertEqual(run.call_count, 3)
            self.assertEqual(run.call_args_list[0].args[0],
                             ["cargo", "build", "--locked", "-p", "festerm-sessiond"])
            self.assertEqual(run.call_args.args[0][-2:], ["--example", "two words"])
            self.assertTrue(run.call_args.kwargs["check"])
            self.assertEqual(run.call_args.kwargs["env"]["FESTERM_SESSIOND_RUNTIME_ROOT"],
                             str(root / "target/festerm-dev/sessiond"))

    def test_build_failure_never_starts_the_frontend(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(launcher, "ROOT", root), \
                    patch.object(launcher.platform, "system", return_value="Linux"), \
                    patch.object(launcher.subprocess, "run",
                                 side_effect=subprocess.CalledProcessError(1, "cargo")) as run, \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()) as stderr:
                self.assertEqual(launcher.main([]), 1)
            run.assert_called_once()
            self.assertIn("Development launch failed", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
