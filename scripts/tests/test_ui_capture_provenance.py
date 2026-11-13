import importlib.util
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location(
    "ui_capture_provenance", Path(__file__).resolve().parents[1] / "ui_capture_provenance.py"
)
provenance = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(provenance)


class CaptureProvenanceTest(unittest.TestCase):
    def test_changed_widget_sources_cannot_receive_success_provenance(self):
        metadata = {"source_tree_sha256": "before", "source_dirty": True}
        with self.assertRaisesRegex(ValueError, "changed during capture"):
            provenance.finish(metadata, {"scenarios": []}, "after")
        result = provenance.finish(metadata, {"scenarios": []}, "before")
        self.assertTrue(result["capture"]["source_dirty"])
        self.assertIn("completed_utc", result["capture"])
