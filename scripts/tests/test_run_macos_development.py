import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).resolve().parents[1] / "run-macos-development.py"
SPEC = importlib.util.spec_from_file_location("macos_development", SCRIPT)
launcher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(launcher)


class MacOSDevelopmentTests(unittest.TestCase):
    def test_config_preserves_canonical_payload_but_separates_identity_and_signing(self):
        source = tomllib.loads((launcher.ROOT / "packaging/macos.toml").read_text())
        rendered = launcher.render_config(source, Path("/build with spaces"), Path("/private/out"))
        config = tomllib.loads(rendered)
        self.assertEqual(config["version"], source["version"])
        self.assertEqual(config["identifier"], launcher.IDENTIFIER)
        self.assertNotEqual(config["identifier"], source["identifier"])
        self.assertEqual(config["product-name"], launcher.PRODUCT_NAME)
        self.assertEqual(config["formats"], ["app"])
        self.assertEqual(config["macos"]["signing-identity"], "-")
        self.assertEqual(len(config["binaries"]), 4)
        self.assertEqual(
            [Path(binary["path"]).name for binary in config["binaries"]],
            [binary["path"] for binary in source["binaries"]],
        )
        self.assertTrue(all(Path(icon).is_file() for icon in config["icons"]))
        self.assertTrue(Path(config["macos"]["info-plist-path"]).is_file())
        self.assertNotIn("/Applications", rendered)

    def test_manifest_binary_must_not_escape_sibling_directory(self):
        source = tomllib.loads((launcher.ROOT / "packaging/macos.toml").read_text())
        source["binaries"][0]["path"] = "../festerm"
        with self.assertRaises(launcher.LaunchError):
            launcher.render_config(source, Path("/build"), Path("/private/out"))

    def test_toml_paths_and_metadata_round_trip_non_bmp_unicode(self):
        source = tomllib.loads((launcher.ROOT / "packaging/macos.toml").read_text())
        source["authors"] = ["developer \U0001f680"]
        binaries = Path("/build \U0001f680")
        output = Path("/private/\U0001f30e")
        config = tomllib.loads(launcher.render_config(source, binaries, output))
        self.assertEqual(config["authors"], source["authors"])
        self.assertEqual(config["out-dir"], str(output))
        self.assertEqual(config["binaries"][0]["path"], str(binaries / "festerm"))

    def test_build_environment_disables_updater_and_removes_release_credentials(self):
        inherited = {
            "PATH": "/tools", "HOME": "/user", "FESTERM_INSTALLATION_KIND": "app",
            "FESTERM_UPDATE_PUBLIC_KEY": "public", "APPLE_ID": "account",
            "APPLE_API_KEY_PATH": "/private/key",
            "CARGO_PACKAGER_SIGN_PRIVATE_KEY": "private",
            "CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD": "password",
        }
        with patch.dict(os.environ, inherited, clear=True):
            environment = launcher.build_environment()
        self.assertEqual(environment, {
            "PATH": "/tools", "HOME": "/user",
            "FESTERM_INSTALLATION_KIND": "development",
        })

    def test_application_environment_is_private_and_does_not_inherit_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.dict(os.environ, {
                "SSH_AUTH_SOCK": "/real/agent", "AWS_SECRET_ACCESS_KEY": "secret",
                "DYLD_INSERT_LIBRARIES": "/inject", "SHELL": "/custom/shell",
                "FESTERM_CONFIG_PATH": "/real/config",
            }):
                environment = launcher.application_environment(root)
            self.assertEqual(environment["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin")
            for key in ("SSH_AUTH_SOCK", "AWS_SECRET_ACCESS_KEY", "DYLD_INSERT_LIBRARIES",
                        "SHELL", "FESTERM_INSTALLATION_KIND"):
                self.assertNotIn(key, environment)
            self.assertEqual(environment["HOME"], str(root / "home"))
            self.assertEqual(environment["ZDOTDIR"], environment["HOME"])
            self.assertEqual(environment["FESTERM_CONFIG_PATH"],
                             str(root / "config/festerm.toml"))
            self.assertEqual(environment["FESTERM_SESSIOND_RUNTIME_ROOT"],
                             str(root / "sessiond"))
            self.assertEqual(Path(environment["FESTERM_SESSIOND_RUNTIME_ROOT"]).name,
                             "sessiond")
            for name in ("home", "config", "data", "cache", "state", "runtime", "tmp",
                         "sessiond"):
                self.assertTrue((root / name).is_dir())
                if os.name != "nt":
                    self.assertEqual((root / name).stat().st_mode & 0o777, 0o700)

    def test_existing_output_is_refused_without_touching_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sentinel = root / "sentinel"
            sentinel.write_text("unchanged")
            with patch.object(launcher.platform, "system", return_value="Darwin"), \
                    patch.object(launcher, "validate_root"), \
                    patch.object(sys, "argv", [str(SCRIPT), "--output", str(root)]), \
                    patch.object(launcher, "package") as package, \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(launcher.main(), 1)
            package.assert_not_called()
            self.assertEqual(list(root.iterdir()), [sentinel])
            self.assertEqual(sentinel.read_text(), "unchanged")

    def test_build_failure_is_recorded_without_starting_an_application(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve() / "new"
            with patch.object(launcher.platform, "system", return_value="Darwin"), \
                    patch.object(launcher, "validate_root"), \
                    patch.object(sys, "argv", [str(SCRIPT), "--output", str(root)]), \
                    patch.object(launcher.subprocess, "run",
                                 return_value=Mock(stdout="revision")), \
                    patch.object(launcher, "package",
                                 side_effect=launcher.LaunchError("build failed")), \
                    patch.object(launcher.subprocess, "Popen") as popen, \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(launcher.main(), 1)
            popen.assert_not_called()
            state = json.loads((root / "launch.json").read_text())
            self.assertEqual(state["status"], "failed")
            self.assertFalse(state["shutdown_verified"])
            self.assertEqual(state["error"], "build failed")

    def test_runtime_root_reserves_space_for_native_socket_endpoint_in_bytes(self):
        suffix = Path("sessiond/2147483647-18446744073709551615.sock")
        allowed = Path("/" + "a" * (103 - len(os.fsencode(suffix)) - 2))
        launcher.validate_root(allowed)
        with self.assertRaisesRegex(launcher.LaunchError, "too long"):
            launcher.validate_root(Path(str(allowed) + "a"))
        with self.assertRaisesRegex(launcher.LaunchError, "too long"):
            launcher.validate_root(Path("/" + "\N{LATIN SMALL LETTER E WITH ACUTE}" * 40))

    @unittest.skipUnless(platform.system() == "Darwin", "macOS native storage policy")
    def test_native_root_refuses_writable_ancestors_including_sticky_tmp(self):
        with self.assertRaisesRegex(launcher.LaunchError, "Untrusted output ancestor"):
            launcher.validate_root(Path("/private/tmp/fs-dev-test"))
        with tempfile.TemporaryDirectory(dir=Path.home()) as temporary:
            parent = Path(temporary).resolve()
            launcher.validate_root(parent / "new")
            try:
                parent.chmod(0o777)
                with self.assertRaisesRegex(launcher.LaunchError, "Untrusted output ancestor"):
                    launcher.validate_root(parent / "new")
            finally:
                parent.chmod(0o700)

    def test_packager_version_is_checked_before_any_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.object(launcher.subprocess, "run",
                              return_value=Mock(stdout="cargo-packager 9.0.0")) as run, \
                    self.assertRaisesRegex(launcher.LaunchError, "Expected"):
                launcher.package(Path(temporary), {})
            self.assertEqual(run.call_count, 1)

    def test_packaging_uses_actual_cargo_artifacts_and_requires_every_helper(self):
        names = ["festerm", "festerm-sessiond", "festerm-tunnel-setup",
                 "festerm-tunnel-bridge"]

        def artifacts(selected):
            return "\n".join(json.dumps({
                "reason": "compiler-artifact", "target": {"name": name},
                "executable": f"/custom-target/debug/{name}",
            }) for name in selected)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            responses = [
                Mock(stdout=launcher.PACKAGER_VERSION),
                Mock(stdout=artifacts(names[:2])),
                Mock(stdout=artifacts(names[2:])),
                subprocess.CalledProcessError(1, "packager"),
            ]
            with patch.object(launcher.subprocess, "run", side_effect=responses) as run, \
                    self.assertRaises(subprocess.CalledProcessError):
                launcher.package(root, {})
            generated = tomllib.loads((root / "macos-development.toml").read_text())
            self.assertEqual([binary["path"] for binary in generated["binaries"]],
                             [str(Path("/custom-target/debug") / name) for name in names])
            for call in run.call_args_list[1:3]:
                self.assertIn("--locked", call.args[0])
                self.assertIn("--message-format=json-render-diagnostics", call.args[0])
            responses[2] = Mock(stdout=artifacts(names[2:3]))
            with patch.object(launcher.subprocess, "run", side_effect=responses), \
                    self.assertRaisesRegex(launcher.LaunchError, "every canonical"):
                launcher.package(root, {})

    def test_native_control_passes_paths_as_arguments_and_guards_all_identity_fields(self):
        executable = Path("/private/space ' quote/fesTerm Development.app/Contents/MacOS/festerm")
        with patch.object(launcher.subprocess, "run",
                          return_value=Mock(stdout='{"quitRequested": true}')) as run:
            self.assertEqual(launcher.native_application(123, executable, "quit"),
                             {"quitRequested": True})
        command = run.call_args.args[0]
        self.assertEqual(command[-5:], [
            "123", str(executable), launcher.IDENTIFIER,
            str(executable.parents[2]), "quit",
        ])
        self.assertNotIn("shell", run.call_args.kwargs)
        self.assertEqual(run.call_args.kwargs["timeout"], 5)
        script = run.call_args.kwargs["input"]
        for field in ("app.executableURL.path", "app.bundleIdentifier", "app.bundleURL.path"):
            self.assertLess(script.index(field), script.index("app.terminate"))

    def test_readiness_requires_branded_regular_window(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        ready = {"regular": True, "windows": [1], "genericIcon": False,
                 "name": launcher.PRODUCT_NAME}
        with patch.object(launcher, "native_application", side_effect=[
            None, {**ready, "genericIcon": True}, {**ready, "windows": []},
            {**ready, "regular": False}, {**ready, "name": "festerm"}, ready,
        ]) as inspect, patch.object(launcher.time, "sleep"):
            self.assertEqual(launcher.wait_ready(process, Path("/owned/app")), ready)
            self.assertEqual(inspect.call_count, 6)

    def test_readiness_fails_on_early_exit_even_when_exit_code_is_zero(self):
        process = Mock(returncode=0)
        process.poll.return_value = 0
        with self.assertRaisesRegex(launcher.LaunchError, "before readiness: 0"):
            launcher.wait_ready(process, Path("/owned/app"))

    def test_readiness_has_a_deadline(self):
        process = Mock()
        process.poll.return_value = None
        with patch.object(launcher.time, "monotonic",
                          side_effect=[0, launcher.STARTUP_TIMEOUT + 1]), \
                self.assertRaisesRegex(launcher.LaunchError, "timed out"):
            launcher.wait_ready(process, Path("/owned/app"))

    def test_native_quit_refusal_is_not_success(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        for result in (None, {"quitRequested": False}):
            with patch.object(launcher, "native_application", return_value=result), \
                    self.assertRaisesRegex(launcher.LaunchError, "not accepted"):
                launcher.request_quit(process, Path("/owned/app"))

    def test_helpers_are_matched_only_by_exact_owned_bundle_executable(self):
        bundle = Path("/private/ours/fesTerm Development.app")
        owned = bundle / "Contents/MacOS/festerm-sessiond"
        output = (
            "10 /Applications/fesTerm.app/Contents/MacOS/festerm-sessiond\n"
            f"11 {owned}\n"
            f"12 {owned}-impostor\n"
            "13 /private/other/fesTerm Development.app/Contents/MacOS/festerm-sessiond\n"
        )
        with patch.object(launcher.subprocess, "run", return_value=Mock(stdout=output)):
            self.assertEqual(launcher.remaining_helpers(bundle, ["festerm-sessiond"]), [11])

    def run_supervisor(self, *, code=0, check=True, wait_error=None, helpers=None,
                       ready_error=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = root / "bundle/fesTerm Development.app"
            process = Mock(pid=123)
            process.poll.return_value = None
            process.wait.return_value = code
            if wait_error:
                process.wait.side_effect = wait_error
            with patch.object(launcher.subprocess, "Popen", return_value=process) as popen, \
                    patch.object(launcher, "wait_ready", side_effect=ready_error,
                                 return_value={"pid": 123}), \
                    patch.object(launcher, "request_quit") as quit_app, \
                    patch.object(launcher, "remaining_helpers", return_value=helpers or []), \
                    patch.object(launcher.time, "monotonic", side_effect=range(100)), \
                    patch.object(launcher.time, "sleep"), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()):
                error = None
                try:
                    launcher.supervise(root, bundle, ["festerm-sessiond"], check,
                                       {"revision": "test", "dirty": True})
                except (launcher.LaunchError, subprocess.TimeoutExpired) as caught:
                    error = caught
            state = json.loads((root / "launch.json").read_text())
            self.assertTrue((root / "app.log").is_file())
            process.terminate.assert_not_called()
            process.kill.assert_not_called()
            self.assertTrue(popen.call_args.kwargs["start_new_session"])
            self.assertEqual(popen.call_args.args[0],
                             [str(bundle / "Contents/MacOS/festerm")])
            return state, error, process, quit_app

    def test_native_check_requires_zero_exit_and_no_owned_helpers(self):
        state, error, process, quit_app = self.run_supervisor()
        self.assertIsNone(error)
        self.assertEqual(state["status"], "exited")
        self.assertEqual(state["exit_code"], 0)
        self.assertTrue(state["shutdown_verified"])
        self.assertEqual(state["remaining_helpers"], [])
        self.assertEqual(state["revision"], "test")
        quit_app.assert_called_once()
        process.wait.assert_called_once_with(timeout=launcher.QUIT_TIMEOUT)

    def test_interactive_handoff_does_not_quit_until_requested(self):
        state, error, process, quit_app = self.run_supervisor(check=False)
        self.assertIsNone(error)
        self.assertTrue(state["shutdown_verified"])
        quit_app.assert_not_called()
        process.wait.assert_called_once_with(timeout=None)

    def test_interrupt_during_readiness_requests_bounded_normal_quit(self):
        state, error, process, quit_app = self.run_supervisor(
            check=False, ready_error=KeyboardInterrupt())
        self.assertIsNone(error)
        self.assertTrue(state["shutdown_verified"])
        quit_app.assert_called_once()
        process.wait.assert_called_once_with(timeout=launcher.QUIT_TIMEOUT)

    def test_interrupted_native_check_is_failure_even_if_graceful_shutdown_succeeds(self):
        state, error, process, quit_app = self.run_supervisor(ready_error=KeyboardInterrupt())
        self.assertIsInstance(error, launcher.LaunchError)
        self.assertEqual(state["status"], "failed")
        self.assertIn("interrupted", state["error"])
        self.assertTrue(state["interrupted"])
        self.assertTrue(state["shutdown_verified"])
        self.assertEqual(state["exit_code"], 0)
        quit_app.assert_called_once()
        process.wait.assert_called_once_with(timeout=launcher.QUIT_TIMEOUT)

    def test_nonzero_exit_is_a_retained_failure(self):
        state, error, _, _ = self.run_supervisor(code=7)
        self.assertIsInstance(error, launcher.LaunchError)
        self.assertEqual(state["status"], "failed")
        self.assertFalse(state["shutdown_verified"])
        self.assertIn("7", state["error"])

    def test_quit_timeout_reports_live_owned_frontend_without_force_killing(self):
        state, error, _, _ = self.run_supervisor(
            wait_error=subprocess.TimeoutExpired("owned", launcher.QUIT_TIMEOUT))
        self.assertIsInstance(error, subprocess.TimeoutExpired)
        self.assertTrue(state["frontend_running"])
        self.assertFalse(state["shutdown_verified"])
        self.assertEqual(state["pid"], 123)

    def test_owned_persistent_helpers_prevent_a_shutdown_success_claim(self):
        state, error, _, _ = self.run_supervisor(helpers=[456])
        self.assertIsInstance(error, launcher.LaunchError)
        self.assertEqual(state["remaining_helpers"], [456])
        self.assertFalse(state["shutdown_verified"])


if __name__ == "__main__":
    unittest.main()
