import os
import pathlib
import subprocess
import unittest

git_bash = pathlib.Path(r"C:\Program Files\Git\bin\bash.exe")
assert git_bash.is_file(), "the explicitly selected Git Bash must exist"
original_popen = subprocess.Popen


class InstalledGitBashPopen(original_popen):
    def __init__(self, args, *positional, **keywords):
        if isinstance(args, (list, tuple)) and args and args[0] == "bash":
            keywords["executable"] = os.fspath(git_bash)
        super().__init__(args, *positional, **keywords)


subprocess.Popen = InstalledGitBashPopen
print(f"Explicit bare-bash CreateProcess executable: {git_bash}", flush=True)
suite = unittest.defaultTestLoader.discover("scripts/tests", pattern="test_*.py")
result = unittest.TextTestRunner(verbosity=1).run(suite)
raise SystemExit(0 if result.wasSuccessful() else 1)
