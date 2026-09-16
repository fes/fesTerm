"""Tests for the State of the UI document generator."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY_ROOT / "scripts" / "build_ui_state_doc.py"


def _load_module():
    specification = importlib.util.spec_from_file_location("build_ui_state_doc", SCRIPT)
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


builder = _load_module()


# Fixture image bytes. The generator only hashes and links these, so a valid
# PNG is unnecessary and a fixed byte string keeps the test hermetic.
PNG_BYTES = b"\x89PNG\r\n\x1a\nfesterm-ui-state-test-fixture"


class BuildUiStateDocTest(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        self.root = Path(self._temporary.name)
        self.images = self.root / "images"
        self.images.mkdir()
        self.addCleanup(self._temporary.cleanup)

    def _write_image(self, name: str) -> str:
        path = self.images / name
        path.write_bytes(PNG_BYTES)
        return hashlib.sha256(PNG_BYTES).hexdigest()

    def _write_manifest(self, scenarios: list[dict], *, name: str = "manifest.json") -> Path:
        path = self.images / name
        path.write_text(
            json.dumps({"schema_version": 1, "scenarios": scenarios}, indent=2) + "\n",
            encoding="utf-8",
        )
        return path

    def _write_narrative(self, text: str) -> Path:
        path = self.root / "narrative.md"
        path.write_text(text, encoding="utf-8")
        return path

    def _scenario(self, identifier: str, section: str, digest: str) -> dict:
        return {
            "id": identifier,
            "section": section,
            "title": f"Title {identifier}",
            "caption": f"Caption {identifier}.",
            "image": f"{identifier}.png",
            "width": 1240,
            "height": 880,
            "tier": "headless",
            "sha256": digest,
        }

    def test_builds_document_grouped_by_narrative_section_order(self) -> None:
        first = self._write_image("beta.png")
        second = self._write_image("alpha.png")
        manifest = self._write_manifest(
            [
                self._scenario("beta", "settings", first),
                self._scenario("alpha", "new-session", second),
            ]
        )
        narrative = self._write_narrative(
            "Notes to whoever edits this file.\n"
            "\n"
            "<!-- section: new-session title: New Session -->\n"
            "How sessions start.\n"
            "\n"
            "<!-- section: settings title: Settings -->\n"
            "How settings look.\n"
        )
        output = self.root / "state-of-the-ui.md"

        status = builder.main(
            [
                "--manifest",
                str(manifest),
                "--narrative",
                str(narrative),
                "--output",
                str(output),
            ]
        )

        self.assertEqual(status, 0)
        document = output.read_text(encoding="utf-8")
        self.assertIn("# State of the UI", document)
        # Text before the first marker instructs the narrative's editor and
        # must not reach the published document.
        self.assertNotIn("Notes to whoever edits this file.", document)
        # Narrative order wins over manifest order.
        self.assertLess(document.index("## New Session"), document.index("## Settings"))
        self.assertIn("![Title alpha](images/alpha.png)", document)
        self.assertIn("Caption beta.", document)

    def test_allows_a_section_with_prose_but_no_screenshots(self) -> None:
        digest = self._write_image("alpha.png")
        manifest = self._write_manifest(
            [self._scenario("alpha", "new-session", digest)]
        )
        narrative = self._write_narrative(
            "<!-- section: overview title: How to read this -->\n"
            "Orientation for the reader.\n"
            "\n"
            "<!-- section: new-session title: New Session -->\n"
            "How sessions start.\n"
        )
        output = self.root / "state-of-the-ui.md"

        status = builder.main(
            [
                "--manifest",
                str(manifest),
                "--narrative",
                str(narrative),
                "--output",
                str(output),
            ]
        )

        self.assertEqual(status, 0)
        document = output.read_text(encoding="utf-8")
        self.assertIn("## How to read this", document)
        self.assertIn("Orientation for the reader.", document)
        self.assertNotIn("No screenshots", document)

    def test_rejects_a_screenshot_whose_section_has_no_narrative(self) -> None:
        digest = self._write_image("alpha.png")
        manifest = self._write_manifest([self._scenario("alpha", "orphan", digest)])
        narrative = self._write_narrative(
            "<!-- section: new-session title: New Session -->\nText.\n"
        )

        status = builder.main(
            [
                "--manifest",
                str(manifest),
                "--narrative",
                str(narrative),
                "--output",
                str(self.root / "out.md"),
            ]
        )

        self.assertEqual(status, 1)

    def test_rejects_a_manifest_whose_image_digest_is_stale(self) -> None:
        digest = self._write_image("alpha.png")
        manifest = self._write_manifest([self._scenario("alpha", "new-session", digest)])
        (self.images / "alpha.png").write_bytes(PNG_BYTES + b"tampered")
        narrative = self._write_narrative(
            "<!-- section: new-session title: New Session -->\nText.\n"
        )

        status = builder.main(
            [
                "--manifest",
                str(manifest),
                "--narrative",
                str(narrative),
                "--output",
                str(self.root / "out.md"),
            ]
        )

        self.assertEqual(status, 1)

    def test_rejects_a_missing_image(self) -> None:
        manifest = self._write_manifest(
            [self._scenario("ghost", "new-session", "0" * 64)]
        )
        narrative = self._write_narrative(
            "<!-- section: new-session title: New Session -->\nText.\n"
        )

        status = builder.main(
            [
                "--manifest",
                str(manifest),
                "--narrative",
                str(narrative),
                "--output",
                str(self.root / "out.md"),
            ]
        )

        self.assertEqual(status, 1)

    def test_merges_multiple_manifests_and_rejects_duplicate_ids(self) -> None:
        digest = self._write_image("alpha.png")
        vm_digest = self._write_image("native.png")
        headless = self._write_manifest(
            [self._scenario("alpha", "new-session", digest)], name="headless.json"
        )
        native_scenario = self._scenario("native", "native", vm_digest)
        native_scenario["tier"] = "vm"
        native = self._write_manifest([native_scenario], name="native.json")
        narrative = self._write_narrative(
            "<!-- section: new-session title: New Session -->\nText.\n"
            "\n"
            "<!-- section: native title: Native Window -->\nText.\n"
        )
        output = self.root / "out.md"

        status = builder.main(
            [
                "--manifest",
                str(headless),
                "--manifest",
                str(native),
                "--narrative",
                str(narrative),
                "--output",
                str(output),
            ]
        )

        self.assertEqual(status, 0)
        document = output.read_text(encoding="utf-8")
        self.assertIn("## New Session", document)
        self.assertIn("## Native Window", document)

        duplicate = self._write_manifest(
            [self._scenario("alpha", "new-session", digest)], name="duplicate.json"
        )
        status = builder.main(
            [
                "--manifest",
                str(headless),
                "--manifest",
                str(duplicate),
                "--narrative",
                str(narrative),
                "--output",
                str(output),
            ]
        )
        self.assertEqual(status, 1)

    def test_check_mode_detects_drift(self) -> None:
        digest = self._write_image("alpha.png")
        manifest = self._write_manifest([self._scenario("alpha", "new-session", digest)])
        narrative = self._write_narrative(
            "<!-- section: new-session title: New Session -->\nText.\n"
        )
        output = self.root / "out.md"
        arguments = [
            "--manifest",
            str(manifest),
            "--narrative",
            str(narrative),
            "--output",
            str(output),
        ]

        self.assertEqual(builder.main(arguments), 0)
        self.assertEqual(builder.main(arguments + ["--check"]), 0)

        output.write_text("stale\n", encoding="utf-8")
        self.assertEqual(builder.main(arguments + ["--check"]), 1)

    def test_rejects_an_unexpected_schema_version(self) -> None:
        digest = self._write_image("alpha.png")
        path = self.images / "manifest.json"
        path.write_text(
            json.dumps(
                {
                    "schema_version": 99,
                    "scenarios": [self._scenario("alpha", "new-session", digest)],
                }
            ),
            encoding="utf-8",
        )
        narrative = self._write_narrative(
            "<!-- section: new-session title: New Session -->\nText.\n"
        )

        status = builder.main(
            [
                "--manifest",
                str(path),
                "--narrative",
                str(narrative),
                "--output",
                str(self.root / "out.md"),
            ]
        )

        self.assertEqual(status, 1)


if __name__ == "__main__":
    unittest.main()
