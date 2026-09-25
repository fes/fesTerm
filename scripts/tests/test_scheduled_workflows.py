from pathlib import Path
import re
import unittest


WORKFLOWS = Path(__file__).resolve().parents[2] / ".github" / "workflows"


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


if __name__ == "__main__":
    unittest.main()
