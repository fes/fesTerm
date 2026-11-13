import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "capture_mobile_ui", ROOT / "scripts/capture-mobile-ui.py"
)
CAPTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAPTURE)


class MobileCaptureCatalogTests(unittest.TestCase):
    def setUp(self):
        parent = ROOT / "target/mobile-ui-catalog-tests"
        parent.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=parent)
        self.addCleanup(self.directory.cleanup)
        self.output = Path(self.directory.name)
        self.image = self.output / "phone-profiles.png"
        self.image.write_bytes(b"synthetic-image-hash-test")
        self.manifest = {
            "source_commit": "synthetic-source",
            "source_files_sha256": {"synthetic.rs": "test-input-hash"},
            "source_fingerprint_sha256": CAPTURE.source_fingerprint({"synthetic.rs": "test-input-hash"}),
            "images": [{
                "image": self.image.name,
                "state": "profiles",
                "logical_size": [390, 844],
                "sha256": hashlib.sha256(self.image.read_bytes()).hexdigest(),
            }],
        }

    def write_manifest(self):
        (self.output / "manifest.json").write_text(json.dumps(self.manifest))

    def test_every_image_has_a_source_route_and_native_limitations(self):
        self.write_manifest()
        index = self.output / "index.md"
        CAPTURE.write_catalog(self.output, index)
        text = index.read_text()
        self.assertIn("**1 PNGs**", text)
        self.assertIn("phone-profiles.png", text)
        self.assertIn("Profiles tab", text)
        self.assertIn("shell.rs", text)
        self.assertIn("not native iOS or Android captures", text)
        self.assertIn("| Entry | Exit |", text)
        self.assertIn("| open a profile, New, Sessions or Settings. |", text)

    def test_changed_source_map_refuses_attribution(self):
        self.manifest["source_files_sha256"]["synthetic.rs"] = "changed"
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "Source fingerprint mismatch"):
            CAPTURE.write_catalog(self.output, self.output / "index.md")

    def test_surface_without_explicit_exit_refuses_index(self):
        self.write_manifest()
        with patch.object(CAPTURE, "SURFACES", [
            ("profiles", "shell.rs", "Profiles tab only; no exit documented.", "MOB-01"),
        ]):
            with self.assertRaisesRegex(ValueError, "Missing explicit entry/exit"):
                CAPTURE.write_catalog(self.output, self.output / "index.md")

    def test_wide_contents_equivalence_is_explicit_not_a_second_modal(self):
        self.write_manifest()
        documents = self.output / "documents"
        documents.mkdir()
        manifest = dict(self.manifest, images=[])
        for state in ["preview", "contents"]:
            image = documents / f"tablet-{state}.png"
            image.write_bytes(b"identical-wide-preview-presentation")
            manifest["images"].append({
                "image": image.name, "state": state, "logical_size": [1194, 834],
                "sha256": hashlib.sha256(image.read_bytes()).hexdigest(),
            })
        (documents / "manifest.json").write_text(json.dumps(manifest))
        index = self.output / "index.md"
        CAPTURE.write_catalog(self.output, index)
        text = index.read_text()
        self.assertIn("3 PNG files contain 2 distinct byte presentations", text)
        self.assertIn("1 duplicate groups", text)
        self.assertIn("clears a requested Contents sheet", text)
        self.assertEqual(text.count("EQ-01"), 3)

    def test_unexplained_duplicate_cannot_hide_a_mislabeled_surface(self):
        other = self.output / "phone-profiles-copy.png"
        other.write_bytes(self.image.read_bytes())
        self.manifest["images"].append(dict(
            self.manifest["images"][0], image=other.name,
        ))
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "Unclassified identical presentations"):
            CAPTURE.write_catalog(self.output, self.output / "index.md")

    def test_unknown_surface_is_not_silently_omitted(self):
        self.manifest["images"][0]["state"] = "uncatalogued"
        self.write_manifest()
        with self.assertRaises(KeyError):
            CAPTURE.write_catalog(self.output, self.output / "index.md")

    def test_missing_or_changed_capture_refuses_index(self):
        self.write_manifest()
        self.image.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            CAPTURE.write_catalog(self.output, self.output / "index.md")


if __name__ == "__main__":
    unittest.main()
