import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "keyboard_routing", Path(__file__).parents[1] / "check_keyboard_routing.py"
)
keyboard_routing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(keyboard_routing)


class KeyboardRoutingRunnerTests(unittest.TestCase):
    def test_native_mode_requires_an_explicit_isolated_desktop(self):
        with patch("sys.argv", ["check_keyboard_routing.py", "--native"]), \
             patch.dict(keyboard_routing.os.environ, {}, clear=True), \
             patch.object(keyboard_routing.subprocess, "run") as run:
            with self.assertRaises(SystemExit) as error:
                keyboard_routing.main()

        self.assertEqual(error.exception.code, 2)
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
