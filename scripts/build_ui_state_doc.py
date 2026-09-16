#!/usr/bin/env python3
"""Assembles the State of the UI document from captured gallery manifests.

The screenshots are produced by `scripts/capture-ui-state.sh` (headless egui
harness) and, for surfaces that only exist in a real desktop session, by the
macOS VM evidence lab. Both write the same manifest schema, so this script
merges any number of them into one document.

Narrative prose is kept in a separate, human-edited source file so that
regenerating the document after a UI change never destroys written analysis.
Only the structure and the screenshots are generated.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from collections import Counter
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFESTS = [REPOSITORY_ROOT / "docs" / "images" / "ui-state" / "manifest.json"]
DEFAULT_NARRATIVE = REPOSITORY_ROOT / "docs" / "ui-state-narrative.md"
DEFAULT_TITLE = "State of the UI"
DEFAULT_OUTPUT = REPOSITORY_ROOT / "docs" / "state-of-the-ui.md"

SCHEMA_VERSION = 1
SECTION_MARKER = re.compile(
    r"^<!--\s*section:\s*(?P<id>[a-z0-9-]+)\s+title:\s*(?P<title>.+?)\s*-->\s*$"
)
GENERATED_BANNER = (
    "<!-- GENERATED FILE - do not edit directly.\n"
    "     Screenshots:      scripts/capture-ui-state.sh\n"
    "     Narrative source: docs/ui-state-narrative.md\n"
    "     Rebuild:          python3 scripts/build_ui_state_doc.py -->"
)

REQUIRED_FIELDS = (
    "id",
    "section",
    "title",
    "caption",
    "image",
    "width",
    "height",
    "tier",
    "sha256",
)


class BuildError(Exception):
    """A manifest, narrative or image problem that must stop the build."""


def load_manifest(path: Path) -> list[dict]:
    """Reads one capture manifest and validates it against the contract."""
    if not path.is_file():
        raise BuildError(f"manifest not found: {path}")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise BuildError(f"{path} is not valid JSON: {error}") from error

    version = document.get("schema_version")
    if version != SCHEMA_VERSION:
        raise BuildError(
            f"{path} declares schema_version {version!r}, expected {SCHEMA_VERSION}"
        )

    scenarios = document.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise BuildError(f"{path} contains no scenarios")

    for scenario in scenarios:
        missing = [field for field in REQUIRED_FIELDS if field not in scenario]
        if missing:
            raise BuildError(
                f"{path}: scenario {scenario.get('id', '<unnamed>')!r} "
                f"is missing {', '.join(missing)}"
            )
        scenario["_manifest_directory"] = path.parent
    return scenarios


def verify_images(scenarios: list[dict], *, verify_digest: bool) -> None:
    """Confirms every referenced image exists and, optionally, still matches.

    A stale digest means the manifest and the PNGs came from different runs,
    which would silently publish screenshots of an older UI.
    """
    for scenario in scenarios:
        image_path = scenario["_manifest_directory"] / scenario["image"]
        if not image_path.is_file():
            raise BuildError(
                f"scenario {scenario['id']!r} references missing image {image_path}"
            )
        if not verify_digest:
            continue
        digest = hashlib.sha256(image_path.read_bytes()).hexdigest()
        if digest != scenario["sha256"]:
            raise BuildError(
                f"scenario {scenario['id']!r} image {image_path} has digest "
                f"{digest}, but the manifest records {scenario['sha256']}. "
                "Re-run the capture script so images and manifest agree."
            )


def parse_narrative(path: Path) -> tuple[str, list[tuple[str, str, str]]]:
    """Splits the narrative source into editor notes and ordered sections.

    Section order in this file is the order used in the document, so an
    author controls the reading sequence without touching the capture code.

    Text before the first section marker is addressed to whoever edits the
    narrative, not to the reader of the generated document, so it is parsed
    but never emitted. Putting maintenance instructions at the top of the
    source file is the obvious thing to do, and it should not leak into the
    published page.
    """
    if not path.is_file():
        raise BuildError(f"narrative source not found: {path}")

    editor_notes: list[str] = []
    sections: list[tuple[str, str, list[str]]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        marker = SECTION_MARKER.match(line)
        if marker:
            sections.append((marker["id"], marker["title"], []))
            continue
        if sections:
            sections[-1][2].append(line)
        else:
            editor_notes.append(line)

    if not sections:
        raise BuildError(
            f"{path} defines no sections; expected at least one "
            "'<!-- section: <id> title: <Title> -->' marker"
        )

    seen: set[str] = set()
    for section_id, _, _ in sections:
        if section_id in seen:
            raise BuildError(f"{path} defines section {section_id!r} more than once")
        seen.add(section_id)

    return (
        "\n".join(editor_notes).strip(),
        [(id_, title, "\n".join(body).strip()) for id_, title, body in sections],
    )


def relative_image_path(scenario: dict, output_path: Path) -> str:
    """Builds a link that resolves from the document, not the shell's cwd."""
    image_path = (scenario["_manifest_directory"] / scenario["image"]).resolve()
    try:
        relative = image_path.relative_to(output_path.parent.resolve())
    except ValueError:
        relative = Path(
            os.path.relpath(image_path, output_path.parent.resolve())
        )
    return relative.as_posix()


def render(
    scenarios: list[dict],
    sections: list[tuple[str, str, str]],
    output_path: Path,
    *,
    title: str = DEFAULT_TITLE,
) -> str:
    """Renders the document, grouping scenarios under their narrative section."""
    by_section: dict[str, list[dict]] = {}
    for scenario in scenarios:
        by_section.setdefault(scenario["section"], []).append(scenario)
    for group in by_section.values():
        group.sort(key=lambda scenario: scenario["id"])

    lines = [GENERATED_BANNER, "", f"# {title}", ""]

    documented: set[str] = set()
    for section_id, section_title, body in sections:
        documented.add(section_id)
        lines.append(f"## {section_title}")
        lines.append("")
        if body:
            lines.extend([body, ""])
        # A section with prose but no screenshots is legitimate: the opening
        # section of the document explains how to read it and illustrates
        # nothing. Only the reverse -- captures with no prose -- is an error.
        for scenario in by_section.get(section_id, []):
            lines.append(f"### {scenario['title']}")
            lines.append("")
            lines.append(scenario["caption"].strip())
            lines.append("")
            link = relative_image_path(scenario, output_path)
            lines.append(f"![{scenario['title']}]({link})")
            lines.append("")

    orphans = sorted(set(by_section) - documented)
    if orphans:
        raise BuildError(
            "these sections have screenshots but no narrative entry in "
            f"{DEFAULT_NARRATIVE.name}: {', '.join(orphans)}. Add a "
            "'<!-- section: <id> title: <Title> -->' marker for each."
        )

    return "\n".join(lines).rstrip() + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest",
        action="append",
        type=Path,
        help="capture manifest to include; repeatable. Defaults to the "
        "headless gallery manifest.",
    )
    parser.add_argument("--narrative", type=Path, default=DEFAULT_NARRATIVE)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed document matches what would be generated "
        "instead of writing it; exits non-zero on drift.",
    )
    parser.add_argument(
        "--skip-digest-check",
        action="store_true",
        help="skip verifying that image bytes match the manifest digests.",
    )
    arguments = parser.parse_args(argv)

    manifests = arguments.manifest or DEFAULT_MANIFESTS
    try:
        scenarios: list[dict] = []
        for manifest in manifests:
            scenarios.extend(load_manifest(manifest))

        counts = Counter(scenario["id"] for scenario in scenarios)
        duplicates = sorted(id_ for id_, count in counts.items() if count > 1)
        if duplicates:
            raise BuildError(
                f"scenario ids appear in more than one manifest: {', '.join(duplicates)}"
            )

        verify_images(scenarios, verify_digest=not arguments.skip_digest_check)
        _editor_notes, sections = parse_narrative(arguments.narrative)
        document = render(scenarios, sections, arguments.output)
    except BuildError as error:
        print(f"build_ui_state_doc: {error}", file=sys.stderr)
        return 1

    if arguments.check:
        if not arguments.output.is_file():
            print(
                f"build_ui_state_doc: {arguments.output} does not exist",
                file=sys.stderr,
            )
            return 1
        current = arguments.output.read_text(encoding="utf-8")
        if current != document:
            print(
                f"build_ui_state_doc: {arguments.output} is out of date; "
                "re-run python3 scripts/build_ui_state_doc.py",
                file=sys.stderr,
            )
            return 1
        print(f"build_ui_state_doc: {arguments.output} is up to date")
        return 0

    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(document, encoding="utf-8")
    print(
        f"build_ui_state_doc: wrote {arguments.output} "
        f"({len(scenarios)} screenshots across {len(sections)} sections)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
