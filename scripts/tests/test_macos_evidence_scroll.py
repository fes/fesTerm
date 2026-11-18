"""No native event is permitted by these refusal and wrapper-routing checks."""
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(platform.system() == 'Darwin', 'requires the macOS SDK')
class ExistingScroll(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.base = Path(cls.temp.name)
        cls.driver = cls.base / 'driver'
        subprocess.run(['xcrun', 'swiftc', '-warnings-as-errors', str(ROOT / 'scripts/macos-os-input-driver.swift'), '-o', str(cls.driver)], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def invoke(self, *tail):
        result = self.base / 'must-not-be-written'
        result.unlink(missing_ok=True)
        process = subprocess.run([str(self.driver), str(os.getpid()), 'scroll-existing', str(result), *tail], capture_output=True, text=True, timeout=10)
        self.assertEqual(process.returncode, 2, process.stdout + process.stderr)
        self.assertIn('events_posted=0', process.stderr)
        self.assertFalse(result.exists())
        return process.stderr

    def test_invalid_arguments_post_nothing(self):
        for tail in [('1', '2', '0', 'birth'), ('1', '2', '13', 'birth'), ('1', '2', '-13', 'birth'), ('nan', '2', '1', 'birth'), ('1', 'inf', '1', 'birth'), ('1', '2', '1')]:
            with self.subTest(tail=tail):
                self.assertIn('invalid-arguments', self.invoke(*tail))

    def test_non_festerm_or_missing_trust_posts_nothing_without_prompt(self):
        outcome = self.invoke('1', '2', '1', 'birth')
        self.assertTrue('reason=access-missing' in outcome or 'reason=not-festerm' in outcome, outcome)

    def test_existing_wrapper_never_builds_cargo_or_launches_gui(self):
        with tempfile.TemporaryDirectory() as name:
            home = Path(name)
            tools = home / 'tools'; tools.mkdir()
            commands = {
                'ps': '#!/bin/sh\nprintf "Tue Nov 17 22:23:53 2026\\n"\n',
                'launchctl': '#!/bin/sh\nexit 0\n',
                'cargo': '#!/bin/sh\necho forbidden-cargo >&2; exit 99\n',
            }
            for key, body in commands.items():
                path = tools / key; path.write_text(body); path.chmod(0o700)
            driver = home / '.local/share/festerm-vm-evidence-relay/FesTermEvidenceDriver.app/Contents/MacOS/festerm-macos-os-input-driver'
            driver.parent.mkdir(parents=True)
            driver.write_text('#!/bin/sh\n[ "$#" -eq 7 ] && [ "$1" = 123 ] && [ "$2" = scroll-existing ] && [ "$4" = 20 ] && [ "$5" = 30 ] && [ "$6" = -2 ] && [ "$7" = "Tue Nov 17 22:23:53 2026" ] || exit 98\nprintf "status=sent\\nevents_posted=2\\n" > "$3"\n')
            driver.chmod(0o700)
            # Keep the stub newer than source; avoid any compiler/signing effects.
            os.utime(driver, (2000000000, 2000000000))
            result = home / 'sent.txt'
            environment = dict(os.environ, HOME=str(home), PATH=str(tools) + ':/usr/bin:/bin')
            process = subprocess.run(['sh', str(ROOT / 'scripts/run-macos-os-input-smoke.sh'), '--scroll-existing', '123', '20', '30', '-2', str(result)], env=environment, capture_output=True, text=True, timeout=10)
            self.assertEqual(process.returncode, 0, process.stdout + process.stderr)
            self.assertEqual(result.read_text(), 'status=sent\nevents_posted=2\n')
            self.assertFalse(Path(str(result) + '.isolation').exists())


if __name__ == '__main__':
    unittest.main()
