"""Tests for the opt-in, exact-commit shared VM evidence bootstrapper."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
BOOTSTRAP = REPOSITORY_ROOT / "scripts" / "bootstrap-vm-evidence-lab.sh"


def _windows_git_bash() -> str:
    """Locates Git for Windows' real `bash.exe`, bypassing the WSL stub.

    Modern Windows ships its own `C:\\Windows\\System32\\bash.exe`, which
    just launches WSL (and fails with "Windows Subsystem for Linux has no
    installed distributions" if none is installed). Since `System32`
    typically precedes Git's `bin` directory on `PATH`, a bare `"bash"`
    lookup resolves to that stub instead of Git's real bash on GitHub's
    hosted `windows-latest` runners. Derive Git's bash path from `git.exe`
    (guaranteed to be on `PATH`) instead of trusting `PATH` order for
    `bash` itself.
    """
    git_exe = shutil.which("git")
    if git_exe:
        # Git for Windows ships bash.exe at <root>/bin/bash.exe, with
        # git.exe itself living in <root>/cmd or <root>/mingw64/bin.
        candidate = Path(git_exe).resolve().parent.parent / "bin" / "bash.exe"
        if candidate.exists():
            return str(candidate)
    return "bash"


def bootstrap_command(*arguments: str) -> list[str]:
    """Builds the argument list to run the `sh` bootstrap script.

    Windows has no shebang support, so `CreateProcess` cannot launch a `.sh`
    file directly (`WinError 193: %1 is not a valid Win32 application`).
    GitHub's hosted `windows-latest` runners (and most Windows dev machines
    with Git installed) ship Git for Windows' `bash.exe`, so route through
    that there instead of relying on direct execution.
    """
    if sys.platform == "win32":
        return [_windows_git_bash(), str(BOOTSTRAP), *arguments]
    return [str(BOOTSTRAP), *arguments]


def run(*arguments: str, cwd: Path) -> str:
    return subprocess.check_output(arguments, cwd=cwd, text=True).strip()


class BootstrapVmEvidenceLabTests(unittest.TestCase):
    def test_clones_and_restores_the_locked_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            run("git", "init", "--quiet", cwd=source)
            run("git", "config", "user.email", "test@example.invalid", cwd=source)
            run("git", "config", "user.name", "VM Evidence Test", cwd=source)
            (source / "evidence.txt").write_text("locked\n", encoding="utf-8")
            run("git", "add", "evidence.txt", cwd=source)
            run("git", "commit", "--quiet", "-m", "locked", cwd=source)
            locked_commit = run("git", "rev-parse", "HEAD", cwd=source)

            lock = root / "vm-evidence-lab.lock"
            lock.write_text(
                f"repository={source}\ncommit={locked_commit}\n", encoding="utf-8"
            )
            checkout = root / "checkout"
            subprocess.run(
                bootstrap_command("--lock", str(lock), "--path", str(checkout)),
                check=True,
                text=True,
            )
            self.assertEqual(run("git", "rev-parse", "HEAD", cwd=checkout), locked_commit)

            (source / "evidence.txt").write_text("newer\n", encoding="utf-8")
            run("git", "commit", "--quiet", "-am", "newer", cwd=source)
            newer_commit = run("git", "rev-parse", "HEAD", cwd=source)
            run("git", "fetch", "--quiet", "origin", cwd=checkout)
            run("git", "checkout", "--detach", "--quiet", newer_commit, cwd=checkout)

            subprocess.run(
                bootstrap_command("--lock", str(lock), "--path", str(checkout)),
                check=True,
                text=True,
            )
            self.assertEqual(run("git", "rev-parse", "HEAD", cwd=checkout), locked_commit)


if __name__ == "__main__":
    unittest.main()
