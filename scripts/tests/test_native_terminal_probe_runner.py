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

    def test_windows_remains_an_explicit_gate_without_importing_posix_collector(self):
        with patch.object(runner, "os", types.SimpleNamespace(name="nt")), \
                contextlib.redirect_stdout(io.StringIO()) as output, \
                patch.object(runner, "fixture_record", side_effect=AssertionError("service fixture")):
            code = runner.main(["--terminal-capability-probe", "--case", "fixture", "--result", "new.json"])
        self.assertEqual(code, 2)
        self.assertEqual(output.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
