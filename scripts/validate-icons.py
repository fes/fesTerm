#!/usr/bin/env python3
"""Validate fesTerm icon sources and regenerate the review sheet."""

from __future__ import annotations

import argparse
import copy
from pathlib import Path
import sys
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "icons" / "source"
SHEET = ROOT / "assets" / "icons" / "icon-sheet.svg"
SVG_NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG_NS)

EXPECTED = {
    "app-mark", "auth-required", "clear", "close", "command-palette",
    "copy", "diagnostics", "disconnect", "error", "host-key-verification",
    "keyboard-shortcuts", "local-terminal", "maximize", "minimize",
    "new-session", "overflow", "paste", "profile", "reconnect", "restore",
    "search", "secret-storage", "serial", "session-inspector", "settings", "ssh-remote",
    "theme-appearance", "typography-font", "warning", "workspace",
}
ALLOWED_ELEMENTS = {"svg", "path", "rect", "circle", "line", "polyline"}
FORBIDDEN_ATTRIBUTES = {"class", "id", "style", "transform"}


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def validate(path: Path) -> ET.Element:
    try:
        root = ET.parse(path).getroot()
    except ET.ParseError as error:
        raise ValueError(f"invalid XML: {error}") from error

    if local_name(root.tag) != "svg":
        raise ValueError("root element must be svg")
    if root.attrib.get("viewBox") != "0 0 24 24":
        raise ValueError("viewBox must be '0 0 24 24'")

    for element in root.iter():
        name = local_name(element.tag)
        if name not in ALLOWED_ELEMENTS:
            raise ValueError(f"unsupported <{name}> element")
        forbidden = FORBIDDEN_ATTRIBUTES.intersection(element.attrib)
        if forbidden:
            raise ValueError(f"forbidden attributes: {', '.join(sorted(forbidden))}")
        for attr, value in element.attrib.items():
            if attr in {"fill", "stroke"} and value not in {"none", "currentColor"}:
                raise ValueError(f"{attr} must be none or currentColor, got {value!r}")

    if path.stem == "overflow":
        if root.attrib.get("fill") != "currentColor":
            raise ValueError("overflow is the only filled icon and must use currentColor")
    else:
        expected = {
            "fill": "none",
            "stroke": "currentColor",
            "stroke-width": "1.75",
            "stroke-linecap": "round",
        }
        for attr, value in expected.items():
            if root.attrib.get(attr) != value:
                raise ValueError(f"root {attr} must be {value!r}")
    return root


def build_sheet(icons: list[tuple[Path, ET.Element]]) -> str:
    width, height = 960, 650
    svg = ET.Element(
        f"{{{SVG_NS}}}svg",
        {"viewBox": f"0 0 {width} {height}", "width": str(width), "height": str(height)},
    )
    ET.SubElement(svg, f"{{{SVG_NS}}}rect", {"width": str(width), "height": str(height), "fill": "#111318"})
    title = ET.SubElement(svg, f"{{{SVG_NS}}}text", {
        "x": "32", "y": "42", "fill": "#f1f3f5", "font-family": "system-ui, sans-serif",
        "font-size": "22", "font-weight": "600",
    })
    title.text = "fesTerm first-party icon set"
    subtitle = ET.SubElement(svg, f"{{{SVG_NS}}}text", {
        "x": "32", "y": "66", "fill": "#969da8", "font-family": "system-ui, sans-serif",
        "font-size": "12",
    })
    subtitle.text = "24 px source grid · review at 20 px and 16 px · semantic color supplied by UI code"

    columns, cell_width, cell_height = 5, 184, 92
    for index, (path, source_root) in enumerate(icons):
        column, row = index % columns, index // columns
        x, y = 32 + column * cell_width, 100 + row * cell_height
        ET.SubElement(svg, f"{{{SVG_NS}}}rect", {
            "x": str(x), "y": str(y), "width": "168", "height": "76", "rx": "8",
            "fill": "#1c2028", "stroke": "#303744",
        })
        for icon_x, icon_y, scale in ((x + 10, y + 13, 20 / 24), (x + 36, y + 15, 16 / 24)):
            group = ET.SubElement(
                svg,
                f"{{{SVG_NS}}}g",
                {"transform": f"translate({icon_x} {icon_y}) scale({scale:.6f})"},
            )
            for child in source_root:
                copied = copy.deepcopy(child)
                copied.attrib.setdefault("fill", source_root.attrib.get("fill", "none"))
                if source_root.attrib.get("stroke"):
                    copied.attrib.setdefault("stroke", "#e9edf2")
                    copied.attrib.setdefault("stroke-width", source_root.attrib["stroke-width"])
                    copied.attrib.setdefault("stroke-linecap", source_root.attrib.get("stroke-linecap", "round"))
                    copied.attrib.setdefault("stroke-linejoin", source_root.attrib.get("stroke-linejoin", "round"))
                elif copied.attrib.get("fill") == "currentColor":
                    copied.set("fill", "#e9edf2")
                group.append(copied)
        label = ET.SubElement(svg, f"{{{SVG_NS}}}text", {
            "x": str(x + 60), "y": str(y + 28), "fill": "#e9edf2",
            "font-family": "ui-monospace, monospace", "font-size": "9",
        })
        label.text = path.stem
        size = ET.SubElement(svg, f"{{{SVG_NS}}}text", {
            "x": str(x + 60), "y": str(y + 49), "fill": "#8f98a6",
            "font-family": "system-ui, sans-serif", "font-size": "10",
        })
        size.text = "20 px  ·  16 px"

    ET.indent(svg, space="  ")
    return ET.tostring(svg, encoding="unicode") + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify the committed sheet instead of rewriting it")
    args = parser.parse_args()

    paths = sorted(SOURCE.glob("*.svg"))
    actual = {path.stem for path in paths}
    errors: list[str] = []
    if actual != EXPECTED:
        errors.append(f"icon inventory mismatch; missing={sorted(EXPECTED - actual)}, extra={sorted(actual - EXPECTED)}")

    icons: list[tuple[Path, ET.Element]] = []
    for path in paths:
        try:
            icons.append((path, validate(path)))
        except ValueError as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1

    generated = build_sheet(icons)
    if args.check:
        if not SHEET.exists() or SHEET.read_text() != generated:
            print(f"{SHEET.relative_to(ROOT)} is stale; run scripts/validate-icons.py", file=sys.stderr)
            return 1
    else:
        SHEET.write_text(generated)

    print(f"validated {len(icons)} icons; review sheet: {SHEET.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
