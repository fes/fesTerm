"""Tests for the State of the UI document generator."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from unittest.mock import patch
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


class SurfaceCoverageReportTest(unittest.TestCase):
    def test_reconciled_matrix_explicitly_keeps_every_family_and_state_pending(self) -> None:
        report = builder.surface_matrix_report()
        self.assertEqual(len(report["families"]), 33)
        gui = {edge for family in report["families"] for edge in family["gui"]}
        self.assertEqual(len(gui), 163)
        self.assertEqual(len(report["original_controls"]["warp"]), 4)
        self.assertEqual(len(report["original_controls"]["construction"]), 12)
        self.assertEqual(report["original_controls"]["gallery_baseline"]["scenes"], 48)
        for family in report["families"]:
            for state in family["audited_state_groups"] + family["profile_dimensions"]:
                self.assertIn(state["status"], ("currently_unmeasured", "native_only"))
                self.assertTrue(state["prerequisites"])
            for fixture in family["fixture_states"]:
                self.assertEqual(len(fixture["variants"]), 2)
                for variant in fixture["variants"]:
                    self.assertEqual(variant["construction"], "currently_unmeasured")
                    self.assertEqual(variant["completed_draw_sync_readback"], "currently_unmeasured")
                    self.assertEqual(variant["native_acceptance"], "pending")

    def test_scaffold_catalog_is_unique_and_native_states_have_no_headless_fixtures(self) -> None:
        report = builder.surface_matrix_report()
        identifiers = []
        for family in report["families"]:
            if family["id"].startswith("UI-NATIVE-"):
                self.assertFalse(family["fixture_states"])
                self.assertTrue(all(
                    state["status"] == "native_only"
                    for state in family["audited_state_groups"]
                ))
            for fixture in family["fixture_states"]:
                identifiers.extend(variant["scene"] for variant in fixture["variants"])
        self.assertEqual(len(identifiers), 52)
        self.assertEqual(len(set(identifiers)), 52)
        palette = next(family for family in report["families"] if family["id"] == "UI-PALETTE")
        self.assertEqual(
            palette["gallery_only_states"][0]["scenes"],
            ["palette-command-menu", "palette-command-menu-narrow"],
        )
        self.assertEqual(palette["gallery_only_states"][0]["performance_status"], "currently_unmeasured")

    def test_style_review_catalog_remains_gallery_only_and_unqualified(self) -> None:
        report = builder.surface_matrix_report()
        identity = report["gallery_fixture_identity"]
        self.assertIn("cross-worktree visual matching remains unqualified", identity["default"])
        self.assertIn("source-only", identity["execution_status"])
        self.assertIn("UNFIXED", identity["canonical_display_metadata"])
        self.assertIn("not synthetic display metadata", identity["caption"])
        states = [
            state
            for family in report["families"]
            for state in family["gallery_only_states"]
        ]
        identifiers = [scene for state in states for scene in state["scenes"]]
        self.assertEqual(len(identifiers), 25)
        self.assertEqual(len(set(identifiers)), 25)
        self.assertEqual(sum(scene.endswith("-short") for scene in identifiers), 5)
        for state in states:
            self.assertEqual(state["execution_status"], "not-run-awaiting-exclusive-validation-slot")
            self.assertEqual(state["performance_status"], "currently_unmeasured")
            self.assertEqual(state["native_acceptance"], "pending")
            self.assertIn("exclusive-validation-slot", state["prerequisites"])
        save_as = next(
            state for state in states if "style-save-as-notes-overwrite-short" in state["scenes"]
        )
        self.assertIn("actual task-loaded NOTES.md", save_as["state"])
        self.assertIn("physical-display-identity-publication-review", save_as["prerequisites"])

    def test_exact_completed_reports_never_qualify_native_or_sibling_states(self) -> None:
        root = REPOSITORY_ROOT / "target" / "report-test-virtual-inputs"
        profile_path = root / "profile.json"
        warp_directory = root / "warp"
        virtual = {
            profile_path: {
                "schema": "festerm-interactive-surface-profile-v2",
                "provenance": {"fixture": "synthetic unit-test report, not real measurement"},
                "samples": [{
                    "name": "about-unavailable",
                    "last_shape_count": 1,
                    "last_vertex_count": 4,
                    "fixture_state_verified": True,
                }],
            },
            warp_directory / "about-unavailable" / "status.json": {"status": "complete"},
            warp_directory / "about-unavailable" / "report.json": {
                "schema": "festerm-warp-ui-replay-v2",
                "scene": "about-unavailable",
                "fixture_state_verified": True,
                "steady_completed_draw_readback": {"samples_ms": [1.0]},
                "provenance": {"fixture": "synthetic unit-test report, not real measurement"},
            },
        }
        read_text = Path.read_text
        exists = Path.exists

        def virtual_read(path, **kwargs):
            return json.dumps(virtual[path]) if path in virtual else read_text(path, **kwargs)

        with patch.object(Path, "read_text", virtual_read), patch.object(
            Path, "exists", lambda path: path in virtual or exists(path)
        ):
            report = builder.surface_matrix_report(
                profile_path=profile_path, warp_directory=warp_directory
            )
        about = next(family for family in report["families"] if family["id"] == "UI-ABOUT")
        normal, narrow = about["fixture_states"][0]["variants"]
        self.assertEqual(normal["construction"], "covered")
        self.assertEqual(normal["completed_draw_sync_readback"], "covered")
        self.assertEqual(normal["native_acceptance"], "pending")
        self.assertEqual(narrow["construction"], "currently_unmeasured")
        self.assertTrue(all(
            state["status"] == "currently_unmeasured"
            for state in about["audited_state_groups"]
        ))
        self.assertTrue(all(
            state["status"] == "native_only"
            for family in report["families"] if family["id"].startswith("UI-NATIVE-")
            for state in family["audited_state_groups"]
        ))

        virtual[warp_directory / "about-unavailable" / "status.json"]["status"] = "running"
        with patch.object(Path, "read_text", virtual_read), patch.object(
            Path, "exists", lambda path: path in virtual or exists(path)
        ), self.assertRaises(builder.BuildError):
            builder.surface_matrix_report(profile_path=profile_path, warp_directory=warp_directory)


if __name__ == "__main__":
    unittest.main()
