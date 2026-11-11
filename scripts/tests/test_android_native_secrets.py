"""Host-only Android namespace tests; no SDK, Keystore or device claim."""

import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid


ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.name == "posix", "Unix native-vault filesystem predicates")
class AndroidNativeSecrets(unittest.TestCase):
    def test_namespace_avoids_selinux_denied_publication(self):
        source = (ROOT / "app/festerm-android/java/io/github/fes/festerm/SecretNamespace.java").read_text(
            encoding="utf-8"
        )
        self.assertNotRegex(source, r"\bFiles\s*\.\s*(?:createLink|move)\s*\(")
        self.assertNotIn("ATOMIC_MOVE", source)

    def test_owner_scope_and_reserved_namespace(self):
        java_home = os.environ.get("JAVA_HOME")
        java = Path(java_home) / "bin/java" if java_home else shutil.which("java")
        javac = Path(java_home) / "bin/javac" if java_home else shutil.which("javac")
        if not java or not javac:
            self.skipTest("JDK unavailable; native namespace tests unexecuted")
        work = ROOT / "target/android-native-secret-tests" / str(uuid.uuid4())
        work.mkdir(parents=True)
        env = os.environ | {"TMPDIR": str(work), "TMP": str(work), "TEMP": str(work)}
        version = subprocess.run(
            [java, "-XX:-UsePerfData", f"-Djava.io.tmpdir={work}", "-version"],
            env=env, capture_output=True, text=True
        )
        if version.returncode:
            self.skipTest("JDK runtime unavailable; native namespace tests unexecuted")
        classes = work / "classes"
        fixtures = work / "fixtures"
        classes.mkdir()
        fixtures.mkdir(mode=0o700)
        source = ROOT / "app/festerm-android/java/io/github/fes/festerm"
        test = ROOT / "app/festerm-android/tests/host/io/github/fes/festerm/SecretNamespaceTest.java"
        commands = [
            [
                javac, "-J-Xmx128m", "-J-XX:-UsePerfData",
                f"-J-Djava.io.tmpdir={work}", "--release", "11",
                "-d", classes, source / "SecretNamespace.java",
                source / "NativeStorageChecks.java", test,
            ],
            [
                java, "-Xmx128m", "-XX:-UsePerfData",
                f"-Djava.io.tmpdir={work}", "-cp", classes,
                "io.github.fes.festerm.SecretNamespaceTest", fixtures,
            ],
        ]
        for number, command in enumerate(commands):
            completed = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
            (work / f"command-{number}.log").write_text(
                completed.stdout + completed.stderr, encoding="utf-8"
            )
            (work / f"command-{number}.exit").write_text(
                str(completed.returncode) + "\n", encoding="ascii"
            )
            self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        print(completed.stdout, end="")


if __name__ == "__main__":
    unittest.main()
