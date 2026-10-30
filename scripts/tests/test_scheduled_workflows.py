from pathlib import Path
import importlib.util
import re
import tempfile
import unittest


WORKFLOWS = Path(__file__).resolve().parents[2] / ".github" / "workflows"
SPEC = importlib.util.spec_from_file_location(
    "select_headless_vulkan",
    Path(__file__).resolve().parents[1] / "select_headless_vulkan.py",
)
headless_vulkan = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(headless_vulkan)


class HeadlessVulkanWorkflowTests(unittest.TestCase):
    def test_linux_validation_disables_only_optional_vulkan_debugging(self):
        ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        quality = ci.split("\n  quality:\n", 1)[1]
        self.assertIn(
            "      - name: Avoid optional Vulkan object naming in headless tests\n"
            "        if: matrix.os == 'ubuntu-latest'\n"
            "        run: |\n"
            '          echo "WGPU_DEBUG=0" >> "$GITHUB_ENV"\n'
            '          echo "WGPU_VALIDATION=1" >> "$GITHUB_ENV"',
            quality,
        )

        release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
        validate = release.split("\n  validate:\n", 1)[1].split("\n  macos:\n", 1)[0]
        self.assertIn(
            '    env:\n      WGPU_DEBUG: "0"\n      WGPU_VALIDATION: "1"',
            validate,
        )
        self.assertIn("      - run: cargo test --workspace", validate)
        select_driver = (
            'run: python3 scripts/select_headless_vulkan.py --github-env "$GITHUB_ENV"'
        )
        self.assertIn(select_driver, quality)
        self.assertIn(select_driver, validate)

    def test_only_the_packaged_software_icd_is_selected_without_replacing_other_environment(self):
        for name in ("lvp_icd.x86_64.json", "lvp_icd.json"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                driver = root / name
                driver.write_text("{}", encoding="utf-8")
                (root / "intel_icd.json").write_text("{}", encoding="utf-8")
                (root / "lvp_icd.directory.json").mkdir()
                environment = root / "github-env"
                environment.write_text("EXISTING=preserved\n", encoding="utf-8")

                selected = headless_vulkan.configure(root, environment)

                self.assertEqual(selected, driver.resolve())
                self.assertEqual(
                    environment.read_text(encoding="utf-8"),
                    f"EXISTING=preserved\nVK_DRIVER_FILES={driver.resolve()}\n",
                )

    def test_missing_or_ambiguous_software_icds_fail_without_changing_environment(self):
        for names in ((), ("lvp_icd.json", "lvp_icd.x86_64.json")):
            with self.subTest(names=names), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for name in names:
                    (root / name).write_text("{}", encoding="utf-8")
                environment = root / "github-env"
                environment.write_text("EXISTING=preserved\n", encoding="utf-8")

                with self.assertRaisesRegex(ValueError, "exactly one Lavapipe"):
                    headless_vulkan.configure(root, environment)

                self.assertEqual(
                    environment.read_text(encoding="utf-8"), "EXISTING=preserved\n"
                )


class ScheduledWorkflowTests(unittest.TestCase):
    def test_fuzz_explicitly_selects_nightly_over_the_repository_toolchain(self):
        workflow = (WORKFLOWS / "fuzz.yml").read_text(encoding="utf-8")
        self.assertIn("cargo +nightly install cargo-fuzz --locked", workflow)
        self.assertIn('cargo +nightly fuzz run "$FUZZ_TARGET"', workflow)
        self.assertNotRegex(workflow, r"\bcargo (?:fuzz|install cargo-fuzz)\b")

    def test_fuzz_duration_is_data_and_cannot_disable_the_time_limit(self):
        workflow = (WORKFLOWS / "fuzz.yml").read_text(encoding="utf-8")
        self.assertIn("FUZZ_DURATION: ${{ inputs.duration || '900' }}", workflow)
        self.assertIn('[[ ! "$FUZZ_DURATION" =~ ^[1-9][0-9]*$ ]]', workflow)
        self.assertIn('"-max_total_time=$FUZZ_DURATION"', workflow)

    def test_native_dependencies_refresh_indexes_before_installing(self):
        workflow = (WORKFLOWS / "native-smoke.yml").read_text(encoding="utf-8")
        self.assertIn("sudo apt-get update", workflow)
        update = workflow.index("sudo apt-get update")
        installs = list(re.finditer(r"sudo apt-get install\b", workflow))
        self.assertTrue(installs)
        for install in installs:
            self.assertLess(update, install.start())
        self.assertIn("sudo apt-get install -y xvfb socat", workflow)

    def test_every_native_gui_step_has_an_external_deadline_and_private_config(self):
        workflow = (WORKFLOWS / "native-smoke.yml").read_text(encoding="utf-8")
        steps = re.findall(
            r"(?ms)^      - name: Run (Windows|Linux|macOS) "
            r"(native-window|native emoji) smoke[^\n]*\n"
            r"(.*?)(?=^      - |\Z)",
            workflow,
        )
        self.assertEqual(len(steps), 6)
        configurations = {}
        for platform, kind, step in steps:
            with self.subTest(platform=platform, kind=kind):
                self.assertIn("timeout-minutes: 2", step)
                config = re.search(
                    r"FESTERM_CONFIG_PATH: (\$\{\{ runner.temp \}\}/[^\n]+)", step
                )
                self.assertIsNotNone(config)
                configurations.setdefault(platform, set()).add(config.group(1))
        self.assertEqual(set(configurations), {"Windows", "Linux", "macOS"})
        self.assertTrue(all(len(paths) == 2 for paths in configurations.values()))

    def test_failure_and_cancellation_preserve_native_smoke_results(self):
        workflow = (WORKFLOWS / "native-smoke.yml").read_text(encoding="utf-8")
        uploads = re.findall(
            r"(?ms)^      - name: Upload failure artifacts\n"
            r"(.*?)(?=^  [a-z]|\Z)",
            workflow,
        )
        self.assertEqual(len(uploads), 3)
        for upload in uploads:
            self.assertIn("if: failure() || cancelled()", upload)
            self.assertIn("native-smoke-window-result.txt", upload)
            self.assertIn("native-emoji-smoke-result.txt", upload)


class IosSpikeWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (WORKFLOWS / "ios-spike.yml").read_text(encoding="utf-8")
        self.steps = re.split(r"(?m)^      - ", self.workflow)[1:]

    def step_running(self, command):
        matches = [
            step for step in self.steps
            if re.search(r"(?m)^(?:        )?run: " + re.escape(command) + r"$", step)
        ]
        self.assertEqual(len(matches), 1, command)
        return matches[0]

    def test_ios_pr_build_checks_remain_unconditional(self):
        job = self.workflow.split("\njobs:\n", 1)[1].split("\n    steps:\n", 1)[0]
        self.assertNotIn("if:", job)
        for command in (
            "cargo test --locked -p festerm-mobile --lib",
            "cargo test --locked -p festerm-mobile --features live-ssh --lib",
            "cargo clippy --locked -p festerm-mobile --features live-ssh --all-targets -- -D warnings",
            'python3 scripts/capture-mobile-ui.py --output "$FESTERM_MOBILE_UI_REVIEW_DIR"',
            "python3 scripts/build-ios-spike.py --check-dependencies",
            "python3 scripts/build-ios-spike.py --check-dependencies --live-ssh",
            "cargo check --locked -p festerm-mobile --target aarch64-apple-ios",
            "cargo check --locked -p festerm-mobile --features live-ssh --target aarch64-apple-ios",
            "cargo check --locked -p festerm-ssh --target aarch64-apple-ios",
            "python3 -m unittest discover -s scripts/tests -p test_ios_ssh_dependencies.py",
            "python3 scripts/build-ios-spike.py",
            "python3 scripts/build-ios-spike.py --live-ssh",
        ):
            with self.subTest(command=command):
                self.assertNotRegex(self.step_running(command), r"(?m)^        if:")
        bundle = next(
            step for step in self.steps if "name: festerm-ios-simulator-spike" in step
        )
        self.assertIn("if: always()", bundle)
        self.assertIn("path: target/ios-spike/fesTermSpike-simulator.tar.gz", bundle)
        self.assertIn("if-no-files-found: error", bundle)
        captures = next(step for step in self.steps if "name: festerm-mobile-headless-ui-review" in step)
        self.assertIn("if: always()", captures)
        self.assertIn("path: ${{ env.FESTERM_MOBILE_UI_REVIEW_DIR }}/", captures)
        self.assertIn(
            "FESTERM_MOBILE_UI_REVIEW_DIR: target/mobile-ui-review/${{ github.run_id }}-${{ github.run_attempt }}",
            self.workflow,
        )
        self.assertIn("if-no-files-found: error", captures)

    def test_ios_native_smoke_runs_only_on_schedule_or_manual_dispatch(self):
        self.assertIn(
            "name: ${{ github.event_name == 'pull_request' && 'simulator-build' || "
            "'experimental-simulator-smoke' }}",
            self.workflow,
        )
        condition = (
            "if: github.event_name == 'schedule' || "
            "github.event_name == 'workflow_dispatch'"
        )
        for command, deadline in (
            ("python3 scripts/smoke-ios-simulator.py --prepare-only", 4),
            ("python3 scripts/smoke-ios-simulator.py --run", 12),
        ):
            with self.subTest(command=command):
                step = self.step_running(command)
                self.assertRegex(step, r"(?m)^        " + re.escape(condition) + r"$")
                self.assertIn(f"timeout-minutes: {deadline}", step)
        self.assertLess(
            self.workflow.index("run: python3 scripts/smoke-ios-simulator.py --prepare-only"),
            self.workflow.index("run: cargo test --locked -p festerm-mobile --lib"),
        )

    def test_ios_scheduled_smoke_preserves_failures_evidence_and_toolchain(self):
        self.assertIn(
            "  schedule:\n    - cron: '17 8 * * *'\n  workflow_dispatch:",
            self.workflow,
        )
        self.assertIn("permissions:\n  contents: read", self.workflow)
        self.assertNotIn("pull_request_target:", self.workflow)
        self.assertNotIn("continue-on-error", self.workflow)
        self.assertNotIn("|| true", self.workflow)
        self.assertIn("runs-on: macos-15", self.workflow)
        self.assertIn("timeout-minutes: 30", self.workflow)
        self.assertIn(
            "DEVELOPER_DIR: /Applications/Xcode_16.4.app/Contents/Developer",
            self.workflow,
        )
        evidence = next(
            step for step in self.steps if "name: festerm-ios-simulator-evidence" in step
        )
        self.assertIn(
            "if: always() && (github.event_name == 'schedule' || "
            "github.event_name == 'workflow_dispatch')",
            evidence,
        )
        self.assertIn("path: target/ios-smoke/", evidence)
        self.assertIn("if-no-files-found: warn", evidence)

    def test_ios_pr_paths_preserve_mobile_and_shared_source_coverage(self):
        triggers = self.workflow.split("\n  schedule:", 1)[0]
        for path in (
            "app/festerm-mobile/**",
            "crates/festerm-core/**",
            "crates/festerm-ios-window/**",
            "crates/festerm-secret-store/**",
            "crates/festerm-session/**",
            "crates/festerm-ssh/**",
            "crates/festerm-ui-egui/**",
            "crates/festerm-markdown/**",
            "crates/festerm-syntax/**",
            "vendor/egui-winit/**",
            "vendor/egui-wgpu/**",
            "vendor/epaint/**",
            "vendor/russh/**",
            "vendor/russh-sftp/**",
            "assets/fonts/**",
            "Cargo.toml",
            "Cargo.lock",
            "scripts/build-ios-spike.py",
            "scripts/smoke-ios-simulator.py",
            "scripts/tests/test_ios_simulator_smoke.py",
            "scripts/tests/test_ios_ssh_dependencies.py",
            ".github/workflows/ios-spike.yml",
        ):
            with self.subTest(path=path):
                self.assertIn(f"      - '{path}'", triggers)


if __name__ == "__main__":
    unittest.main()
