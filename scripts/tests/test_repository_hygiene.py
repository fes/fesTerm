from pathlib import Path, PurePosixPath
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]


class RepositoryHygieneTests(unittest.TestCase):
    def test_vendored_cargo_targets_are_ignored(self):
        for package in ("epaint", "egui-wgpu", "egui-winit"):
            with self.subTest(package=package):
                path = Path("vendor") / package / "target" / "ignore-probe"
                result = subprocess.run(
                    ["git", "-C", str(ROOT), "check-ignore", "--no-index", "-q", str(path)],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr or str(path))

    def test_no_cargo_build_outputs_are_tracked(self):
        result = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-z"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=True,
        )
        outputs = [
            path
            for path in result.stdout.split("\0")
            if path and "target" in PurePosixPath(path).parts
        ]
        self.assertFalse(
            outputs,
            f"{len(outputs)} tracked Cargo build outputs; first paths: {outputs[:10]}",
        )


if __name__ == "__main__":
    unittest.main()
