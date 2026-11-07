import importlib.util
from pathlib import Path
import json
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "run-mobile-ui-flows.py"
SPEC = importlib.util.spec_from_file_location("mobile_ui_flows", SCRIPT)
flows = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(flows)

OWNED = "11111111-2222-3333-4444-555555555555"
EXISTING = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"
INVENTORY = {
    "runtimes": [{"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-18-5",
                  "version": "18.5", "isAvailable": True}],
    "devices": {"com.apple.CoreSimulator.SimRuntime.iOS-18-5": [
        {"udid": EXISTING, "name": "iPhone Test", "isAvailable": True,
         "deviceTypeIdentifier": "phone"},
        {"udid": "BBBBBBBB-BBBB-CCCC-DDDD-EEEEEEEEEEEE", "name": "iPad Test",
         "isAvailable": True, "deviceTypeIdentifier": "pad"},
    ]},
}


class FakeRunner:
    def __init__(self, qemu="1"):
        self.calls = []
        self.qemu = qemu

    def __call__(self, *args, **kwargs):
        self.calls.append(args)
        if args[:3] == ("xcrun", "simctl", "list"):
            return json.dumps(INVENTORY)
        if "--show-sdk-version" in args:
            return "18.5\n"
        if args[:3] == ("xcrun", "simctl", "create"):
            return OWNED + "\n"
        if "getprop" in args:
            return self.qemu + "\n"
        return ""


class MobileUiFlowsTests(unittest.TestCase):
    def run_main(self, argv, maestro_exit=0, fake=None, maestro=True):
        fake = fake or FakeRunner()
        with tempfile.TemporaryDirectory() as home:
            env = {"PATH": "", "HOME": home}
            if maestro:
                binary = Path(home) / ".maestro/bin/maestro"
                binary.parent.mkdir(parents=True)
                binary.write_text("")
            with patch.object(flows.platform, "system", return_value="Darwin"), \
                    patch.object(flows.subprocess, "run") as maestro_run:
                maestro_run.return_value.returncode = maestro_exit
                code = flows.main([*argv, "--output", str(Path(home) / "out")], env=env,
                                  runner_factory=lambda _log: fake)
        return code, fake.calls, maestro_run.call_args_list

    def test_missing_maestro_reports_a_skip_without_touching_devices(self):
        code, calls, maestro = self.run_main(["--platform", "ios"], maestro=False)
        self.assertEqual(code, flows.SKIPPED)
        self.assertEqual(calls, [])
        self.assertEqual(maestro, [])

    def test_ios_runs_on_a_fresh_simulator_and_deletes_it_even_when_flows_fail(self):
        code, calls, maestro = self.run_main(["--platform", "ios"], maestro_exit=1)
        self.assertEqual(code, 1)
        command = maestro[0].args[0]
        self.assertEqual(command[command.index("--device") + 1], OWNED)
        self.assertIn("APP_ID=org.festerm.mobile-spike", command)
        self.assertIn(("xcrun", "simctl", "delete", OWNED), calls)
        self.assertFalse(any(EXISTING in call for call in calls))

    def test_explicit_ios_device_is_used_but_never_created_or_deleted(self):
        code, calls, _ = self.run_main(["--platform", "ios", "--device", EXISTING])
        self.assertEqual(code, 0)
        self.assertFalse(any(call[2] in {"create", "shutdown", "delete"}
                             for call in calls if call[:2] == ("xcrun", "simctl")))

    def test_android_refuses_devices_that_are_not_emulators(self):
        code, calls, maestro = self.run_main(
            ["--platform", "android", "--device", "phone", "--apk", "app.apk"],
            fake=FakeRunner(qemu="0"))
        self.assertEqual(code, 1)
        self.assertFalse(any("install" in call for call in calls))
        self.assertEqual(maestro, [])

    def test_android_installs_the_apk_and_targets_the_android_package(self):
        code, calls, maestro = self.run_main(
            ["--platform", "android", "--device", "emulator-5554", "--apk", "app.apk"])
        self.assertEqual(code, 0)
        adb = [call[3] for call in calls if call[:3] == ("adb", "-s", "emulator-5554")]
        self.assertLess(adb.index("uninstall"), adb.index("install"), "flows need a fresh app")
        self.assertIn("APP_ID=io.github.fes.festerm", maestro[0].args[0])

    def test_android_without_an_artifact_clears_the_installed_app(self):
        code, calls, _ = self.run_main(["--platform", "android", "--device", "emulator-5554"])
        self.assertEqual(code, 0)
        self.assertIn(("adb", "-s", "emulator-5554", "shell", "pm", "clear", "io.github.fes.festerm"), calls)
        self.assertFalse(any("uninstall" in call for call in calls))


if __name__ == "__main__":
    unittest.main()
