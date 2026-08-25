import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "check_packaging.py"
SPEC = importlib.util.spec_from_file_location("check_packaging", SCRIPT)
packaging = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(packaging)


class PackagingMetadataTests(unittest.TestCase):
    def test_repository_packaging_metadata_is_consistent(self):
        packaging.verify()

    def test_all_native_platforms_have_explicit_formats(self):
        self.assertEqual(
            set(packaging.EXPECTED_FORMATS),
            {"macos", "windows", "linux"},
        )
        self.assertNotIn("all", {
            package_format
            for formats in packaging.EXPECTED_FORMATS.values()
            for package_format in formats
        })


if __name__ == "__main__":
    unittest.main()

