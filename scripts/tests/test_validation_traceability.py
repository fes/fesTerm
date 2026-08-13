import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import check_validation_traceability as trace


class TraceabilityCheckerTests(unittest.TestCase):
    def test_github_anchor_handles_punctuation_and_spacing(self):
        self.assertEqual(
            trace.github_anchor("Closing sessions and quitting"),
            "closing-sessions-and-quitting",
        )
        self.assertEqual(
            trace.github_anchor("Deferred native Markdown viewing"),
            "deferred-native-markdown-viewing",
        )

    def test_edge_parser_accepts_digit_bearing_families(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "graph.md"
            path.write_text("| `A11Y-01` | state |\n| `ROOT-01` | state |\n", encoding="utf-8")
            self.assertEqual(
                trace.markdown_ids(path, trace.EDGE_RE),
                {"A11Y-01", "ROOT-01"},
            )

    def test_validation_impact_ids_do_not_truncate_adr_numbers(self):
        self.assertEqual(
            trace.validation_impact_ids("GUI:PASTE-05, GUI:A11Y-01, ADR-0014"),
            {"PASTE-05", "A11Y-01", "ADR-0014"},
        )

    def test_registry_closes_a_minimal_trace(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs" / "adr").mkdir(parents=True)
            (root / "app").mkdir()
            (root / "validation").mkdir()
            (root / "docs" / "gui-action-graph.md").write_text(
                "| `ROOT-01` | from | action | oracle | return | P |\n",
                encoding="utf-8",
            )
            (root / "docs" / "manual-validation.md").write_text(
                "| AS-01 | workflow | manual | later |\n",
                encoding="utf-8",
            )
            (root / "docs" / "gui-design.md").write_text(
                "# Design\n\n## Root Application States\n",
                encoding="utf-8",
            )
            (root / "docs" / "adr" / "0001-test.md").write_text(
                "# ADR 0001\n\n## Validation impact\n\n- ROOT-01\n",
                encoding="utf-8",
            )
            (root / "app" / "test.rs").write_text(
                "#[test]\nfn root_returns_to_launcher() {}\n",
                encoding="utf-8",
            )
            registry = {
                "schema_version": 1,
                "graph": "docs/gui-action-graph.md",
                "manual_registry": "docs/manual-validation.md",
                "normative_documents": ["docs/gui-design.md"],
                "legacy_adrs_without_validation_impact": [],
                "coverage": [
                    {
                        "id": "root",
                        "edges": ["ROOT-*"],
                        "requirements": ["docs/gui-design.md#root-application-states"],
                        "decisions": ["ADR-0001"],
                        "automated_tests": ["root_returns_to_launcher"],
                        "manual_scenarios": ["AS-01"],
                        "status": "partial",
                        "prerequisites": [],
                    }
                ],
            }
            registry_path = root / "validation" / "traceability.json"
            registry_path.write_text(json.dumps(registry), encoding="utf-8")

            report = trace.validate_registry(root, registry_path)

            self.assertEqual(report["total"], 1)
            self.assertEqual(report["partial"], 1)
            self.assertEqual(report["unclassified"], 0)

    def test_deferred_entry_requires_a_named_prerequisite(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs" / "adr").mkdir(parents=True)
            (root / "validation").mkdir()
            (root / "docs" / "gui-action-graph.md").write_text(
                "| `MD-01` | from | action | oracle | return | deferred |\n",
                encoding="utf-8",
            )
            (root / "docs" / "manual-validation.md").write_text(
                "| CP-06 | workflow | deferred | later |\n",
                encoding="utf-8",
            )
            (root / "docs" / "gui-design.md").write_text(
                "# Design\n\n## Markdown\n",
                encoding="utf-8",
            )
            registry = {
                "schema_version": 1,
                "graph": "docs/gui-action-graph.md",
                "manual_registry": "docs/manual-validation.md",
                "normative_documents": [],
                "legacy_adrs_without_validation_impact": [],
                "coverage": [
                    {
                        "id": "markdown",
                        "edges": ["MD-*"],
                        "requirements": ["docs/gui-design.md#markdown"],
                        "decisions": [],
                        "automated_tests": [],
                        "manual_scenarios": ["CP-06"],
                        "status": "deferred",
                        "prerequisites": [],
                    }
                ],
            }
            registry_path = root / "validation" / "traceability.json"
            registry_path.write_text(json.dumps(registry), encoding="utf-8")

            with self.assertRaisesRegex(trace.TraceabilityError, "named prerequisites"):
                trace.validate_registry(root, registry_path)


if __name__ == "__main__":
    unittest.main()
