#!/usr/bin/env python3
"""Validate fesTerm's requirement -> scenario -> evidence traceability graph."""

from __future__ import annotations

import argparse
import fnmatch
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

EDGE_RE = re.compile(r"^\| `([A-Z][A-Z0-9]*-[0-9]{2})` \|")
MANUAL_RE = re.compile(r"^\| ([A-Z][A-Z0-9]*-[0-9]{2}) \|")
TEST_ATTRIBUTE_RE = re.compile(r"#\[(?:[A-Za-z0-9_]+::)*test(?:\([^]]*\))?\]")
TEST_FUNCTION_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*\(")
HEADING_RE = re.compile(r"^#{1,6}\s+(.+?)\s*$")
ADR_FILE_RE = re.compile(r"^([0-9]{4})-[a-z0-9-]+\.md$")
VALID_STATUSES = {"automated", "manual", "usability", "partial", "deferred"}


class TraceabilityError(Exception):
    """One or more traceability invariants failed."""


def github_anchor(heading: str) -> str:
    text = re.sub(r"<[^>]+>", "", heading.strip().lower())
    text = re.sub(r"[^\w\- ]", "", text, flags=re.UNICODE)
    return re.sub(r"-+", "-", text.replace(" ", "-")).strip("-")


def markdown_ids(path: Path, pattern: re.Pattern[str]) -> set[str]:
    return {
        match.group(1)
        for line in path.read_text(encoding="utf-8").splitlines()
        if (match := pattern.match(line))
    }


def markdown_anchors(path: Path) -> set[str]:
    anchors: set[str] = set()
    counts: Counter[str] = Counter()
    for line in path.read_text(encoding="utf-8").splitlines():
        match = HEADING_RE.match(line)
        if not match:
            continue
        base = github_anchor(match.group(1))
        suffix = counts[base]
        counts[base] += 1
        anchors.add(base if suffix == 0 else f"{base}-{suffix}")
    return anchors


def rust_test_functions(root: Path) -> set[str]:
    tests: set[str] = set()
    for base in (root / "app", root / "crates", root / "tests"):
        if not base.exists():
            continue
        for path in base.rglob("*.rs"):
            pending_test = False
            for line in path.read_text(encoding="utf-8").splitlines():
                if TEST_ATTRIBUTE_RE.search(line):
                    pending_test = True
                    continue
                function = TEST_FUNCTION_RE.match(line)
                if function:
                    if pending_test:
                        tests.add(function.group(1))
                    pending_test = False
                elif pending_test and line.strip() and not line.lstrip().startswith(("#[", "//")):
                    pending_test = False
    return tests


def adr_ids(root: Path) -> tuple[set[str], dict[str, Path]]:
    identifiers: set[str] = set()
    paths: dict[str, Path] = {}
    for path in (root / "docs" / "adr").glob("*.md"):
        match = ADR_FILE_RE.match(path.name)
        if match:
            identifier = f"ADR-{match.group(1)}"
            identifiers.add(identifier)
            paths[identifier] = path
    return identifiers, paths


def expand_patterns(patterns: list[str], edges: set[str]) -> set[str]:
    return {
        edge
        for pattern in patterns
        for edge in edges
        if fnmatch.fnmatchcase(edge, pattern)
    }


def validate_requirement(root: Path, reference: str, errors: list[str]) -> None:
    path_text, separator, anchor = reference.partition("#")
    path = root / path_text
    if not path.is_file():
        errors.append(f"requirement source does not exist: {reference}")
        return
    if separator and anchor not in markdown_anchors(path):
        errors.append(f"requirement anchor does not exist: {reference}")


def changed_files(root: Path, base: str) -> set[str]:
    command = ["git", "diff", "--name-only", f"{base}...HEAD"]
    result = subprocess.run(command, cwd=root, check=False, text=True, capture_output=True)
    if result.returncode != 0:
        raise TraceabilityError(result.stderr.strip() or "git diff failed")
    return {line.strip() for line in result.stdout.splitlines() if line.strip()}


def validation_impact_trailers(root: Path, base: str) -> list[str]:
    result = subprocess.run(
        ["git", "log", "--format=%B%x00", f"{base}..HEAD"],
        cwd=root,
        check=False,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        raise TraceabilityError(result.stderr.strip() or "git log failed")
    return [
        line.split(":", 1)[1].strip()
        for line in result.stdout.replace("\x00", "\n").splitlines()
        if line.lower().startswith("validation-impact:")
    ]


def validation_impact_ids(trailer: str) -> set[str]:
    edge_ids = re.findall(r"\bGUI:([A-Z][A-Z0-9]*-[0-9]{2})\b", trailer)
    adr_references = re.findall(r"\bADR-[0-9]{4}\b", trailer)
    return set(edge_ids) | set(adr_references)


def validate_changed_impact(
    root: Path,
    base: str,
    registry: dict[str, object],
    graph_edges: set[str],
    adr_identifiers: set[str],
    errors: list[str],
) -> None:
    changed = changed_files(root, base)
    if not changed:
        return
    trailers = validation_impact_trailers(root, base)
    if not trailers:
        errors.append(
            "changed commits need a 'Validation-Impact:' trailer naming graph/ADR IDs "
            "or 'none - <reason>'"
        )
        return
    for trailer in trailers:
        if trailer.lower().startswith("none"):
            if len(trailer.partition("-")[2].strip()) < 8:
                errors.append("Validation-Impact: none requires a meaningful reason after '-'")
            continue
        references = validation_impact_ids(trailer)
        unknown = references - graph_edges - adr_identifiers
        if unknown:
            errors.append(f"Validation-Impact trailer has unknown IDs: {sorted(unknown)}")
        if not references:
            errors.append(
                "Validation-Impact trailer must name GUI:<edge>/ADR-<number> IDs or use none"
            )

    normative = set(registry.get("normative_documents", []))
    normative_changed = {
        path
        for path in changed
        if path in normative or path.startswith("docs/adr/")
    }
    trace_changed = bool(
        changed
        & {
            str(registry["graph"]),
            str(registry["manual_registry"]),
            "validation/traceability.json",
        }
    )
    no_impact = any(trailer.lower().startswith("none") for trailer in trailers)
    if normative_changed and not trace_changed and not no_impact:
        errors.append(
            "normative documents changed without the action graph/trace registry/manual "
            "registry changing or an explicit no-impact reason"
        )


def validate_registry(root: Path, registry_path: Path, base: str | None = None) -> dict[str, int]:
    registry = json.loads(registry_path.read_text(encoding="utf-8"))
    errors: list[str] = []
    if registry.get("schema_version") != 1:
        errors.append("schema_version must be 1")

    graph_path = root / str(registry.get("graph", ""))
    manual_path = root / str(registry.get("manual_registry", ""))
    if not graph_path.is_file():
        errors.append(f"graph does not exist: {graph_path}")
    if not manual_path.is_file():
        errors.append(f"manual registry does not exist: {manual_path}")
    if errors:
        raise TraceabilityError("\n".join(errors))

    graph_edges = markdown_ids(graph_path, EDGE_RE)
    manual_scenarios = markdown_ids(manual_path, MANUAL_RE)
    tests = rust_test_functions(root)
    decisions, decision_paths = adr_ids(root)
    legacy = set(registry.get("legacy_adrs_without_validation_impact", []))
    unknown_legacy = legacy - decisions
    if unknown_legacy:
        errors.append(f"legacy ADR allowlist has unknown IDs: {sorted(unknown_legacy)}")

    assignments: dict[str, str] = {}
    status_counts: Counter[str] = Counter()
    for index, entry in enumerate(registry.get("coverage", []), start=1):
        label = str(entry.get("id", f"coverage[{index}]"))
        patterns = entry.get("edges", [])
        if not isinstance(patterns, list) or not patterns:
            errors.append(f"{label}: edges must be a non-empty list")
            continue
        matched = expand_patterns([str(pattern) for pattern in patterns], graph_edges)
        if not matched:
            errors.append(f"{label}: edge patterns match nothing: {patterns}")
        for edge in matched:
            if edge in assignments:
                errors.append(f"{edge}: assigned by both {assignments[edge]} and {label}")
            assignments[edge] = label

        status = str(entry.get("status", ""))
        if status not in VALID_STATUSES:
            errors.append(f"{label}: invalid status {status!r}")
        else:
            status_counts[status] += len(matched)
        requirements = entry.get("requirements", [])
        if not isinstance(requirements, list) or not requirements:
            errors.append(f"{label}: at least one requirement source is required")
        else:
            for requirement in requirements:
                validate_requirement(root, str(requirement), errors)

        automated = set(map(str, entry.get("automated_tests", [])))
        manual = set(map(str, entry.get("manual_scenarios", [])))
        entry_decisions = set(map(str, entry.get("decisions", [])))
        prerequisites = entry.get("prerequisites", [])
        missing_tests = automated - tests
        missing_manual = manual - manual_scenarios
        missing_decisions = entry_decisions - decisions
        if missing_tests:
            errors.append(f"{label}: unknown automated tests: {sorted(missing_tests)}")
        if missing_manual:
            errors.append(f"{label}: unknown manual scenarios: {sorted(missing_manual)}")
        if missing_decisions:
            errors.append(f"{label}: unknown ADRs: {sorted(missing_decisions)}")
        if status == "automated" and not automated:
            errors.append(f"{label}: automated status requires automated_tests")
        if status in {"manual", "usability"} and not manual:
            errors.append(f"{label}: {status} status requires manual_scenarios")
        if status == "partial" and not (automated and (manual or prerequisites)):
            errors.append(
                f"{label}: partial status requires automated_tests and manual_scenarios/prerequisites"
            )
        if status == "deferred" and not prerequisites:
            errors.append(f"{label}: deferred status requires named prerequisites")

    unclassified = graph_edges - assignments.keys()
    if unclassified:
        errors.append(f"unclassified graph edges: {sorted(unclassified)}")

    changed_adrs: set[str] = set()
    if base:
        for path in changed_files(root, base):
            match = ADR_FILE_RE.match(Path(path).name) if path.startswith("docs/adr/") else None
            if match:
                changed_adrs.add(f"ADR-{match.group(1)}")
    for decision, path in decision_paths.items():
        has_section = "## Validation impact" in path.read_text(encoding="utf-8")
        if not has_section and (decision not in legacy or decision in changed_adrs):
            errors.append(f"{decision}: accepted or changed ADR lacks '## Validation impact'")

    if base:
        validate_changed_impact(root, base, registry, graph_edges, decisions, errors)

    if errors:
        raise TraceabilityError("\n".join(f"- {error}" for error in errors))
    return {
        "total": len(graph_edges),
        **{status: status_counts[status] for status in sorted(VALID_STATUSES)},
        "unclassified": 0,
        "broken_references": 0,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--registry", type=Path)
    parser.add_argument("--base", help="optional merge-base SHA for changed-impact enforcement")
    args = parser.parse_args()
    root = args.root.resolve()
    registry = args.registry or root / "validation" / "traceability.json"
    try:
        report = validate_registry(root, registry, args.base)
    except (OSError, json.JSONDecodeError, TraceabilityError) as error:
        print(f"validation traceability: FAILED\n{error}", file=sys.stderr)
        return 1
    print("validation traceability: PASS")
    print(f"  GUI edges:         {report['total']}")
    for status in ("automated", "partial", "manual", "usability", "deferred"):
        print(f"  {status.capitalize():<17}{report[status]}")
    print(f"  {'Unclassified:':<19}{report['unclassified']}")
    print(f"  {'Broken references:':<19}{report['broken_references']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
