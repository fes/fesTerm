import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import Mock, patch

PATH = Path(__file__).resolve().parents[1] / "check_dev_tunnel_workflow.py"
spec = importlib.util.spec_from_file_location("probe_workflow", PATH)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class NativeProbeRunnerTests(unittest.TestCase):
    def test_explicit_probe_delegates_with_inherited_tty_without_fixture_or_cargo(self):
        delegate = Mock(return_value=0)
        fake = types.SimpleNamespace(main=delegate)
        with patch.object(runner, "os", types.SimpleNamespace(name="posix")), \
                patch.dict(sys.modules, {"native_terminal_capability_probe": fake}), \
                patch.object(runner, "fixture_record", side_effect=AssertionError("service fixture")), \
                patch.object(runner, "run_bounded", side_effect=AssertionError("Cargo/pipe runner")):
            code = runner.main(["--terminal-capability-probe", "--case", "fixture", "--result", "private/result.json"])
        self.assertEqual(code, 0)
        delegate.assert_called_once_with(["--case", "fixture", "--output", "private/result.json"])

    def test_probe_excludes_service_run_and_requires_case_and_result(self):
        for args in (["--terminal-capability-probe"],
                     ["--terminal-capability-probe", "--case", "fixture"],
                     ["--terminal-capability-probe", "--case", "fixture", "--result", "new.json", "--run"]):
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()), \
                    self.assertRaises(SystemExit) as caught:
                runner.main(args)
            self.assertEqual(caught.exception.code, 2)

    def test_windows_requires_explicit_staged_helper_without_service_fixture(self):
        with patch.object(runner, "os", types.SimpleNamespace(name="nt", environ={})), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                patch.object(runner, "fixture_record", side_effect=AssertionError("service fixture")):
            code = runner.main(["--terminal-capability-probe", "--case", "fixture", "--result", "new.json"])
        self.assertEqual(code, 2)
        self.assertEqual(output.getvalue(), "")

    def test_windows_helper_inherits_console_without_capture_or_shell(self):
        helper = str(PATH.parent / "staged-probe.exe")
        result = types.SimpleNamespace(returncode=1)
        with patch.object(runner, "os", types.SimpleNamespace(name="nt", environ={"FESTERM_TERMINAL_CAPABILITY_HELPER": helper})), \
                patch.object(runner.Path, "is_file", return_value=True), \
                patch.object(runner.subprocess, "run", return_value=result) as run, \
                patch.object(runner, "fixture_record", side_effect=AssertionError("service fixture")):
            code = runner.main(["--terminal-capability-probe", "--case", "fixture", "--result", "new.json"])
        self.assertEqual(code, 1)
        run.assert_called_once_with([helper, "--case", "fixture", "--output", "new.json"], check=False)

    def test_windows_helper_failure_stays_silent_and_relative_helper_is_refused(self):
        for helper in ("relative.exe", str(PATH.parent / "absent.exe")):
            with self.subTest(helper=helper), \
                    patch.object(runner, "os", types.SimpleNamespace(name="nt", environ={"FESTERM_TERMINAL_CAPABILITY_HELPER": helper})), \
                    patch.object(runner.Path, "is_file", return_value=False), \
                    patch.object(runner.subprocess, "run", side_effect=AssertionError("must not launch")):
                self.assertEqual(runner.main(["--terminal-capability-probe", "--case", "fixture", "--result", "new.json"]), 2)

    def test_windows_launch_error_returns_silent_admission_failure(self):
        helper = str(PATH.parent / "staged-probe.exe")
        with patch.object(runner, "os", types.SimpleNamespace(name="nt", environ={"FESTERM_TERMINAL_CAPABILITY_HELPER": helper})), \
                patch.object(runner.Path, "is_file", return_value=True), \
                patch.object(runner.subprocess, "run", side_effect=OSError("private platform text")), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                contextlib.redirect_stderr(io.StringIO()) as errors:
            self.assertEqual(runner.main(["--terminal-capability-probe", "--case", "fixture", "--result", "new.json"]), 2)
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(errors.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
