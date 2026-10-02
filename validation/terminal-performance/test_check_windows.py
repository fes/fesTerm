from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import check_windows as checker


class NativeComparisonEvidenceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.runs = []
        declared = []
        for sequence, mode in enumerate(checker.MODES, 1):
            declared.append({"Host": "festerm", "Workload": "localized", "Mode": mode, "Sequence": sequence})
            name = f"{sequence:02}-{mode}-festerm-localized"
            folder = self.directory / name
            folder.mkdir()
            started = datetime(2026, 1, 1, tzinfo=timezone.utc) + timedelta(minutes=sequence)
            result = {
                **declared[-1], "Status": "valid", "DesktopActive": True, "SourceSha": "a" * 40,
                "ExecutableSha256": "b" * 64, "ProducerSha256": "c" * 64,
                "StartedUtc": started.isoformat(), "LogicalProcessors": 16,
                "InputChanged": False, "ForegroundChanged": False, "GeometryChanged": False,
                "SampleSeconds": 10, "SampleStartedUnixMs": 5000, "HostCopyRequested": mode != "A",
                "RetainedCompositionRequested": mode == "C", "OverlayControl": False,
                "Window": 123, "Metrics": [100, 100, 100, 100, 96], "Font": "controlled font",
                "CpuPercent": {"A": 4, "B": 3, "C": 2}[mode], "PrivateBytes": 1000,
                "Direct2DFramesPerSecond": 10, "HostCopyFramesPerSecond": 10 if mode != "A" else None,
                "RetainedUiFramesPerSecond": 9 if mode == "C" else None,
                "RetainedUiRebuildsPerSecond": 1 if mode == "C" else None,
                "Producer": {
                    "workload": "localized", "frames": 200, "interval_ms": 100,
                    "geometry": {"columns": 120, "rows": 40}, "bytes": 100,
                    "started_unix_ms": 0, "completed_ms": list(range(100, 20001, 100)),
                },
                "Intervals": [{
                    "DesktopActive": True, "InputChanged": False, "Foreground": 123, "Metrics": [100, 100, 100, 100, 96],
                    "CpuPercent": 1, "PrivateBytes": 1000, "WorkingSetBytes": 500, "Handles": 20, "Threads": 2,
                }],
            }
            self.runs.append(result)
            self.write(folder / "result.json", result)
            self.write(folder / "cleanup.json", {
                "NormalExit": True, "Forced": False, "ExitCode": 0, "RemainingDescendantPids": [],
                "FinishedUtc": (started + timedelta(seconds=30)).isoformat(),
            })
        self.write(self.directory / "manifest.json", {
            "SchemaVersion": 1, "Modes": list(checker.MODES), "DirtyChanges": [],
            "Configuration": "release", "Architecture": "X64", "SourceSha": "a" * 40,
            "FesTermSha256": "b" * 64, "ProducerSha256": "c" * 64, "Runs": declared,
            "LogicalProcessors": 16, "SampleSeconds": 10, "ProducerFrames": 200,
            "OverlayControl": False, "CaptureBoundary": "synthetic capture boundary",
        })
        self.save_runs()

    @staticmethod
    def write(path, value):
        path.write_text(json.dumps(value), encoding="utf-8")

    def save_runs(self):
        self.write(self.directory / "results.json", self.runs)
        for item in self.runs:
            name = f"{item['Sequence']:02}-{item['Mode']}-festerm-localized"
            self.write(self.directory / name / "result.json", item)

    def test_balanced_completed_series_preserves_order_effects(self):
        summary = checker.summarize(self.directory)
        localized = summary["cases"]["localized"]
        self.assertEqual(localized["C_vs_A_percent"], -50)
        self.assertEqual(len(localized["ordered_blocks"]), 4)
        self.assertEqual(localized["C"]["cpu_percent"]["samples"], [2] * 4)

    def test_adverse_results_are_not_hidden_or_rejected(self):
        for item in self.runs:
            if item["Mode"] == "C":
                item["CpuPercent"] = 5
        self.save_runs()
        self.assertEqual(checker.summarize(self.directory)["cases"]["localized"]["C_vs_A_percent"], 25)

    def test_guard_provenance_delivery_geometry_and_path_failures_are_rejected(self):
        mutations = (
            ("Status", "invalid-input-or-window"), ("SourceSha", "d" * 40),
            ("ExecutableSha256", "d" * 64), ("ProducerSha256", "d" * 64),
            ("LogicalProcessors", 8), ("InputChanged", True), ("ForegroundChanged", True),
            ("GeometryChanged", True), ("SampleSeconds", 9), ("HostCopyRequested", False),
            ("DesktopActive", False),
            ("Metrics", [100, 100, 99, 100, 96]), ("Font", "other font"),
            ("HostCopyFramesPerSecond", 0), ("CpuPercent", float("nan")),
        )
        original = dict(self.runs[1])
        for field, value in mutations:
            with self.subTest(field=field):
                self.runs[1] = {**original, field: value}
                self.save_runs()
                with self.assertRaises(ValueError):
                    checker.summarize(self.directory)
        self.runs[1] = original
        self.runs[2]["RetainedUiFramesPerSecond"] = 0
        self.save_runs()
        with self.assertRaisesRegex(ValueError, "retained path"):
            checker.summarize(self.directory)

    def test_disconnected_or_unrecorded_desktop_cannot_pass_interval_guards(self):
        interval = self.runs[0]["Intervals"][0]
        interval["DesktopActive"] = False
        self.save_runs()
        with self.assertRaisesRegex(ValueError, "interval guard"):
            checker.summarize(self.directory)
        interval.pop("DesktopActive")
        self.save_runs()
        with self.assertRaisesRegex(ValueError, "interval guard"):
            checker.summarize(self.directory)

    def test_incomplete_series_and_forced_cleanup_cannot_be_aggregated(self):
        self.write(self.directory / "results.json", self.runs[:-1])
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            checker.summarize(self.directory)
        self.save_runs()
        path = self.directory / "01-A-festerm-localized" / "cleanup.json"
        cleanup = checker.read_json(path)
        for mutation in ({"Forced": True}, {"NormalExit": False}, {"RemainingDescendantPids": [999]}):
            with self.subTest(mutation=mutation):
                self.write(path, {**cleanup, **mutation})
                with self.assertRaisesRegex(ValueError, "shutdown"):
                    checker.summarize(self.directory)

    def test_changed_raw_result_and_preserved_failure_are_rejected(self):
        folder = self.directory / "01-A-festerm-localized"
        self.write(folder / "result.json", {**self.runs[0], "CpuPercent": 100})
        with self.assertRaisesRegex(ValueError, "aggregate"):
            checker.summarize(self.directory)
        self.save_runs()
        self.write(folder / "failure.json", {"Status": "failed"})
        with self.assertRaisesRegex(ValueError, "cannot be pooled"):
            checker.summarize(self.directory)

    def test_recorded_warmup_guard_requires_unchanged_actual_observations(self):
        folder = self.directory / "01-A-festerm-localized"
        guard = {
            "SchemaVersion": 1, "InputTickBefore": 100, "InputTickObserved": 100,
            "ExpectedWindow": 123, "ObservedWindow": 123,
            "ExpectedMetrics": self.runs[0]["Metrics"], "ObservedMetrics": self.runs[0]["Metrics"],
            "InputChanged": False, "ForegroundChanged": False, "GeometryChanged": False,
        }
        self.write(folder / "warmup-guard.json", guard)
        checker.summarize(self.directory)
        for mutation in (
            {"InputChanged": True}, {"ForegroundChanged": True}, {"GeometryChanged": True},
            {"InputTickObserved": 101}, {"ObservedWindow": 456},
            {"ObservedMetrics": [100, 100, 99, 100, 96]}, {"InputTickBefore": -1},
            {"InputTickObserved": True}, {"SchemaVersion": 2},
        ):
            with self.subTest(mutation=mutation):
                self.write(folder / "warmup-guard.json", {**guard, **mutation})
                with self.assertRaisesRegex(ValueError, "warmup guard"):
                    checker.summarize(self.directory)


class NativePaletteProbeTests(unittest.TestCase):
    def test_overlay_query_matches_the_actual_palette_window_name(self):
        directory = Path(__file__).resolve().parent
        driver = (directory / "compare-windows.ps1").read_text(encoding="utf-8")
        palette = (directory.parents[1] / "crates" / "festerm-ui-egui" / "src" / "palette.rs").read_text(
            encoding="utf-8"
        )
        window_names = re.findall(r'Window::new\("([^"]+)"\)', palette)
        queried_names = re.findall(r"NameProperty,\s*'([^']+)'", driver)
        self.assertEqual(window_names, ["Command Palette"])
        self.assertEqual(queried_names, window_names)


@unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 required for pure native-guard predicate")
class NativeWarmupPredicateTests(unittest.TestCase):
    def test_unchanged_mouse_tick_foreground_and_geometry_are_independent(self):
        script = r"""
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $env:FESTERM_COMPARISON_DRIVER, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Comparison driver does not parse.' }
$functions = @($ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Get-NativeWarmupGuard'
}, $true))
if ($functions.Count -ne 1) { throw 'Expected one pure warmup guard function.' }
. ([scriptblock]::Create($functions[0].Extent.Text))
$inputCase = $env:FESTERM_WARMUP_CASE | ConvertFrom-Json
Get-NativeWarmupGuard -InputBefore $inputCase.before -InputObserved $inputCase.observed `
    -ExpectedWindow 123 -ObservedWindow $inputCase.window `
    -ExpectedMetrics @(100,100,100,100,96) -ObservedMetrics $inputCase.metrics |
    ConvertTo-Json -Depth 4
"""
        cases = (
            (100, 100, 123, [100, 100, 100, 100, 96], (False, False, False)),
            (100, 101, 123, [100, 100, 100, 100, 96], (True, False, False)),
            (100, 100, 456, [100, 100, 100, 100, 96], (False, True, False)),
            (100, 100, 123, [100, 100, 99, 100, 96], (False, False, True)),
            (0xFFFFFFFF, 0, 456, [100, 100, 99, 100, 96], (True, True, True)),
        )
        for before, observed, window, metrics, expected in cases:
            with self.subTest(expected=expected):
                environment = os.environ.copy()
                environment["FESTERM_COMPARISON_DRIVER"] = str(Path(__file__).with_name("compare-windows.ps1"))
                environment["FESTERM_WARMUP_CASE"] = json.dumps({
                    "before": before, "observed": observed, "window": window, "metrics": metrics,
                })
                result = subprocess.run(
                    [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-Command", script],
                    env=environment, capture_output=True, text=True, timeout=30, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                guard = json.loads(result.stdout)
                self.assertEqual(
                    tuple(guard[key] for key in ("InputChanged", "ForegroundChanged", "GeometryChanged")),
                    expected,
                )
                self.assertEqual(guard["InputTickBefore"], before)
                self.assertEqual(guard["InputTickObserved"], observed)
                self.assertEqual(guard["ObservedWindow"], window)
                self.assertEqual(guard["ObservedMetrics"], metrics)

    def test_guard_evidence_and_rejection_precede_every_sample(self):
        source = Path(__file__).with_name("compare-windows.ps1").read_text(encoding="utf-8")
        observation = source.index("$warmupGuard = Get-NativeWarmupGuard")
        evidence = source.index('Set-Content -LiteralPath "$directory\\warmup-guard.json"')
        rejection = source.index("if ($warmupGuard.InputChanged -or $warmupGuard.ForegroundChanged")
        sampling = source.index("$beforeFrames =")
        self.assertLess(observation, evidence)
        self.assertLess(evidence, rejection)
        self.assertLess(rejection, sampling)
        self.assertIn("$warmupGuard.GeometryChanged", source[rejection:sampling])
        self.assertIn("stopped before sampling without retry", source[rejection:sampling])


if __name__ == "__main__":
    unittest.main()
