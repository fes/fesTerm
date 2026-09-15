import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch


spec = importlib.util.spec_from_file_location(
    "running_sessions", Path(__file__).parents[1] / "check_running_sessions.py")
running_sessions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(running_sessions)


class RunningSessionRunnerTests(unittest.TestCase):
    @unittest.skipIf(running_sessions.os.name == "nt", "Unix socket byte budget")
    def test_runtime_ignores_long_checkout_and_tmpdir(self):
        with tempfile.TemporaryDirectory(prefix="fs-fixture-", dir="/tmp") as directory:
            inherited_temp = Path(directory) / ("long-temp-" * 20)
            inherited_temp.mkdir()
            with patch.dict(running_sessions.os.environ, {"TMPDIR": str(inherited_temp)}):
                root = running_sessions.create_runtime_root()
            try:
                endpoint = root / "n/4294967295-0/festerm/sessiond/4294967295-1789499400000.sock"
                self.assertLess(len(running_sessions.os.fsencode(endpoint)), 104)
                self.assertEqual(root.stat().st_mode & 0o777, 0o700)
                self.assertNotIn(str(inherited_temp), str(root))
            finally:
                shutil.rmtree(root)

    def test_long_checkout_namespace_is_cleaned_on_success_and_runner_failure(self):
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                checkout = Path(directory).resolve() / ("long-checkout-" * 12)
                roots = []

                def run(command, *, cwd, env, timeout):
                    root = Path(env["FESTERM_SESSIOND_TEST_RUNTIME_ROOT"]).parent
                    roots.append(root)
                    self.assertEqual(cwd, checkout)
                    self.assertNotIn(str(checkout), str(root))
                    self.assertTrue(root.joinpath("owned-running-session-validation").is_file())
                    self.assertEqual(env["SCREENDIR"], str(root / "screen"))
                    self.assertEqual(env["TMUX_TMPDIR"], str(root / "tmux"))
                    self.assertNotIn("TMUX", env)
                    self.assertNotIn("STY", env)
                    if fail and command[1] == "test":
                        raise subprocess.CalledProcessError(7, command)

                with patch.object(running_sessions, "__file__", str(checkout / "scripts/check_running_sessions.py")), \
                     patch("sys.argv", ["check_running_sessions.py", "--batch", "1", "--cycles", "1"]), \
                     patch.object(running_sessions, "run_checked", side_effect=run), \
                     patch.object(running_sessions.shutil, "which", return_value=None):
                    if fail:
                        with self.assertRaises(subprocess.CalledProcessError):
                            running_sessions.main()
                    else:
                        running_sessions.main()
                self.assertGreaterEqual(len(roots), 2)
                self.assertTrue(all(root == roots[0] for root in roots))
                self.assertFalse(roots[0].exists())

    def test_cleanup_failure_retains_only_the_owned_namespace(self):
        roots = []

        def run(command, *, cwd, env, timeout):
            if command[1] == "test":
                root = Path(env["FESTERM_SESSIOND_TEST_RUNTIME_ROOT"]).parent
                roots.append(root)
                registry = root / "n/123-0/festerm/sessiond/registry.json"
                registry.parent.mkdir(parents=True)
                registry.write_text(json.dumps({"sessions": {"owned-session": {}}}))
                raise subprocess.CalledProcessError(7, command)

        try:
            with patch("sys.argv", ["check_running_sessions.py", "--batch", "1", "--cycles", "1"]), \
                 patch.object(running_sessions, "run_checked", side_effect=run), \
                 patch.object(running_sessions.subprocess, "run",
                              side_effect=subprocess.CalledProcessError(1, ["owned-kill"])) as cleanup:
                with self.assertRaisesRegex(RuntimeError, "Cleanup incomplete; owned namespace retained"):
                    running_sessions.main()
            self.assertTrue(roots[0].exists())
            self.assertEqual(cleanup.call_args.args[0][-2:], ["--name", "owned-session"])
            key = "LOCALAPPDATA" if running_sessions.os.name == "nt" else "XDG_STATE_HOME"
            self.assertEqual(cleanup.call_args.kwargs["env"][key], str(roots[0] / "n/123-0"))
        finally:
            for root in roots:
                shutil.rmtree(root)

    def test_cleanup_selects_exact_live_provider_identifiers_only(self):
        self.assertEqual(running_sessions.owned_targets("tmux", "$3\nmain\n$4\n"), ["$3", "$4"])
        self.assertEqual(running_sessions.owned_targets("screen",
            "There are screens on:\n 42.main (Detached)\n 43.main (Attached)\n 44.dead (Dead ???)\n"),
            ["42.main", "43.main"])

    def test_failure_status_is_not_masked(self):
        process = Mock()
        process.wait.return_value = 7
        process.poll.return_value = 7
        with patch.object(running_sessions.subprocess, "Popen", return_value=process):
            with self.assertRaises(subprocess.CalledProcessError) as error:
                running_sessions.run_checked(["cargo", "test"], cwd=".", env={}, timeout=1)
        self.assertEqual(error.exception.returncode, 7)

    @unittest.skipIf(running_sessions.os.name == "nt", "Unix process-group ownership")
    def test_timeout_terminates_only_the_captured_owned_process_group(self):
        process = Mock(pid=12345)
        process.wait.side_effect = [subprocess.TimeoutExpired("cargo", 1), 0, 0]
        process.poll.return_value = None
        with patch.object(running_sessions.subprocess, "Popen", return_value=process) as spawn:
            with patch.object(running_sessions.os, "killpg") as terminate:
                with self.assertRaises(subprocess.TimeoutExpired):
                    running_sessions.run_checked(["cargo", "test"], cwd=".", env={}, timeout=1)
        self.assertTrue(spawn.call_args.kwargs["start_new_session"])
        terminate.assert_called_once_with(12345, running_sessions.signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
