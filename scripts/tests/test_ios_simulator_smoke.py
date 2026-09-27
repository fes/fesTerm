import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import subprocess


SCRIPT = Path(__file__).resolve().parents[1] / "smoke-ios-simulator.py"
SPEC = importlib.util.spec_from_file_location("ios_smoke", SCRIPT)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)

OWNED = "11111111-2222-3333-4444-555555555555"
EXISTING = "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE"
DEVICE = {"family": "iPhone", "model": "iPhone Test", "device_type": "phone",
          "runtime": "com.apple.CoreSimulator.SimRuntime.iOS-18-5", "ios": "18.5"}


class FakeRunner:
    def __init__(self, fail=None, created=OWNED, first_ui=True):
        self.first_ui = first_ui
        self.calls = []
        self.fail = fail
        self.created = created

    def __call__(self, *args, **kwargs):
        self.calls.append(args)
        if self.fail and self.fail(args):
            raise RuntimeError("injected failure")
        if args[0] == "ps":
            return "/private/Simulator/fesTermSpike.app/festerm-mobile\n"
        command = args[2]
        if command == "create":
            return self.created + "\n"
        if command == "launch":
            stderr = next(a.split("=", 1)[1] for a in args if a.startswith("--stderr="))
            Path(stderr).write_text("festerm-mobile: first UI built\n" if self.first_ui else "renderer failed\n")
            return smoke.BUNDLE_ID + ": 1234\n"
        if command == "io":
            Path(args[-1]).write_bytes(b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13)
                                     + b"IHDR" + struct.pack(">II", 1170, 2532))
        return ""


class IosSimulatorSmokeTests(unittest.TestCase):
    def test_ios_smoke_selects_available_matching_phone_and_ipad_runtime(self):
        inventory = {"runtimes": [], "devices": {}}
        for version, available in [("18.5", True), ("26.0", True), ("18.6", True)]:
            runtime = "com.apple.CoreSimulator.SimRuntime.iOS-" + version.replace(".", "-")
            inventory["runtimes"].append({"identifier": runtime, "version": version, "isAvailable": available})
            inventory["devices"][runtime] = [
                {"name": family, "deviceTypeIdentifier": family, "isAvailable": True}
                for family in (["iPhone"] if version == "18.6" else ["iPhone", "iPad"])
            ]
        devices = smoke.choose_devices(inventory, "18.5")
        self.assertEqual([d["family"] for d in devices], ["iPhone", "iPad"])
        self.assertTrue(all(d["ios"] == "18.5" for d in devices))
        inventory["devices"] = {}
        with self.assertRaisesRegex(RuntimeError, "Install an iOS"):
                smoke.choose_devices(inventory, "18.5")

    def test_ios_smoke_keeps_partial_timeout_output(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "commands.log"
            error = subprocess.TimeoutExpired(["xcrun"], 1, output=b"partial diagnostic")
            with patch.object(smoke.subprocess, "run", side_effect=error):
                with self.assertRaises(subprocess.TimeoutExpired):
                    smoke.Runner(log)("xcrun", timeout=1)
            self.assertIn("partial diagnostic", log.read_text())
            self.assertIn("timeout=1s", log.read_text())

    def test_ios_smoke_launch_and_relaunch_only_mutate_the_created_simulator(self):
        run = FakeRunner()
        with tempfile.TemporaryDirectory() as directory:
            result = smoke.exercise_device(run, DEVICE, Path("fixture.app"), Path(directory),
                                           {EXISTING}, pause=lambda _: None)
            self.assertEqual(result["status"], "pass")
            self.assertEqual([c["phase"] for c in result["captures"]], ["launch", "relaunch"])
            self.assertEqual([c["pixels"] for c in result["captures"]], [(1170, 2532)] * 2)
        calls = run.calls
        self.assertFalse(any(EXISTING in call for call in calls))
        self.assertEqual([c[2] for c in calls if c[0] == "xcrun"][-2:], ["shutdown", "delete"])
        self.assertEqual(sum(c[:3] == ("xcrun", "simctl", "launch") for c in calls), 2)
        self.assertEqual(sum(c[0] == "ps" for c in calls), 4)

    def test_ios_smoke_crash_boot_and_screenshot_failures_never_pass_and_clean_up(self):
        for failure in ("ps", "bootstatus", "io"):
            run = FakeRunner(fail=lambda c: c[0] == failure or (len(c) > 2 and c[2] == failure))
            with tempfile.TemporaryDirectory() as directory:
                result = smoke.exercise_device(run, DEVICE, Path("fixture.app"), Path(directory),
                                               {EXISTING}, pause=lambda _: None)
            self.assertEqual(result["status"], "fail")
            self.assertEqual(run.calls[-1], ("xcrun", "simctl", "delete", OWNED))
            self.assertFalse(any(EXISTING in call for call in run.calls))

    def test_ios_smoke_refuses_existing_udid_and_reports_cleanup_failure(self):
        for run in (FakeRunner(created=EXISTING), FakeRunner(fail=lambda c: c[2] == "delete")):
            with tempfile.TemporaryDirectory() as directory:
                result = smoke.exercise_device(run, DEVICE, Path("fixture.app"), Path(directory),
                                               {EXISTING}, pause=lambda _: None)
            self.assertEqual(result["status"], "fail")
            self.assertFalse(any(EXISTING in call for call in run.calls))

    def test_ios_smoke_live_process_without_ui_fails_and_preserves_capture(self):
        run = FakeRunner(first_ui=False)
        with tempfile.TemporaryDirectory() as directory:
            result = smoke.exercise_device(run, DEVICE, Path("fixture.app"), Path(directory),
                                           {EXISTING}, pause=lambda _: None)
            self.assertEqual(result["status"], "fail")
            self.assertFalse(result["captures"][0]["first_ui_built"])
            self.assertTrue((Path(directory) / result["captures"][0]["screenshot"]).exists())
            self.assertIn("first UI", result["error"])
        self.assertEqual(run.calls[-1], ("xcrun", "simctl", "delete", OWNED))

    def test_ios_smoke_rejects_missing_or_invalid_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "capture.png"
            path.write_bytes(b"not a screenshot")
            with self.assertRaises(RuntimeError):
                smoke.png_dimensions(path)


if __name__ == "__main__":
    unittest.main()
