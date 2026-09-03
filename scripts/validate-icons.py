#!/usr/bin/env python3
"""Validate fesTerm icon sources and regenerate the review sheet."""

from __future__ import annotations

import argparse
import copy
from dataclasses import dataclass
import math
from pathlib import Path
import re
import sys
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets" / "icons" / "source"
SHEET = ROOT / "assets" / "icons" / "icon-sheet.svg"
RUNTIME_GEOMETRY = ROOT / "crates" / "festerm-ui-egui" / "src" / "icon_geometry.rs"
SVG_NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", SVG_NS)

EXPECTED = {
    "activate", "app-mark", "auth-required", "back", "clear", "close", "command-palette",
    "copy", "diagnostics", "disconnect", "error", "host-key-verification",
    "edit", "keyboard-shortcuts", "local-terminal", "maximize", "minimize",
    "external-link", "markdown-document", "new-session", "outline", "overflow", "paste",
    "profile", "reconnect", "rendered-view", "restore", "search", "secret-storage",
    "serial", "session-inspector", "settings", "source-view", "ssh-remote",
    "theme-appearance", "typography-font", "warning", "workspace",
}
ALLOWED_ELEMENTS = {"svg", "path", "rect", "circle", "line", "polyline"}
FORBIDDEN_ATTRIBUTES = {"class", "id", "style", "transform"}
NUMBER = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"
PATH_TOKEN = re.compile(rf"[A-Za-z]|{NUMBER}")
SUPPORTED_PATH_COMMANDS = set("MmLlHhVvCcAaZz")
ARC_STEP = math.pi / 8
BEZIER_STEPS = 8
STROKE_WIDTH = 1.75
DOT_SEGMENT_LENGTH = 0.02


@dataclass(frozen=True)
class Polyline:
    points: tuple[tuple[float, float], ...]


@dataclass(frozen=True)
class Rectangle:
    x: float
    y: float
    width: float
    height: float
    radius: float


@dataclass(frozen=True)
class Circle:
    x: float
    y: float
    radius: float
    filled: bool


Primitive = Polyline | Rectangle | Circle


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def same_point(
    left: tuple[float, float],
    right: tuple[float, float],
) -> bool:
    return math.isclose(left[0], right[0], abs_tol=1e-9) and math.isclose(
        left[1], right[1], abs_tol=1e-9
    )


def number(element: ET.Element, attribute: str, default: str | None = None) -> float:
    value = element.attrib.get(attribute, default)
    if value is None or re.fullmatch(NUMBER, value) is None:
        raise ValueError(f"<{local_name(element.tag)}> {attribute} must be a number")
    return float(value)


def path_tokens(data: str) -> list[str]:
    tokens: list[str] = []
    end = 0
    for match in PATH_TOKEN.finditer(data):
        if data[end:match.start()].strip(" ,"):
            raise ValueError(f"unsupported path data near {data[end:match.start()]!r}")
        token = match.group()
        if token.isalpha() and token not in SUPPORTED_PATH_COMMANDS:
            raise ValueError(f"unsupported path command {token!r}")
        tokens.append(token)
        end = match.end()
    if data[end:].strip(" ,"):
        raise ValueError(f"unsupported path data near {data[end:]!r}")
    return tokens


def arc_points(
    start: tuple[float, float],
    radius_x: float,
    radius_y: float,
    rotation: float,
    large_arc: bool,
    sweep: bool,
    end: tuple[float, float],
) -> list[tuple[float, float]]:
    if same_point(start, end):
        return []
    radius_x, radius_y = abs(radius_x), abs(radius_y)
    if radius_x == 0 or radius_y == 0:
        return [end]

    angle = math.radians(rotation % 360)
    cos_angle, sin_angle = math.cos(angle), math.sin(angle)
    half_x = (start[0] - end[0]) / 2
    half_y = (start[1] - end[1]) / 2
    transformed_x = cos_angle * half_x + sin_angle * half_y
    transformed_y = -sin_angle * half_x + cos_angle * half_y

    scale = (
        transformed_x * transformed_x / (radius_x * radius_x)
        + transformed_y * transformed_y / (radius_y * radius_y)
    )
    if scale > 1:
        scale = math.sqrt(scale)
        radius_x *= scale
        radius_y *= scale

    numerator = max(
        0.0,
        radius_x * radius_x * radius_y * radius_y
        - radius_x * radius_x * transformed_y * transformed_y
        - radius_y * radius_y * transformed_x * transformed_x,
    )
    denominator = (
        radius_x * radius_x * transformed_y * transformed_y
        + radius_y * radius_y * transformed_x * transformed_x
    )
    coefficient = 0.0
    if denominator:
        coefficient = math.sqrt(numerator / denominator)
        if large_arc == sweep:
            coefficient = -coefficient

    center_x_prime = coefficient * radius_x * transformed_y / radius_y
    center_y_prime = -coefficient * radius_y * transformed_x / radius_x
    center_x = (
        cos_angle * center_x_prime
        - sin_angle * center_y_prime
        + (start[0] + end[0]) / 2
    )
    center_y = (
        sin_angle * center_x_prime
        + cos_angle * center_y_prime
        + (start[1] + end[1]) / 2
    )

    start_vector = (
        (transformed_x - center_x_prime) / radius_x,
        (transformed_y - center_y_prime) / radius_y,
    )
    end_vector = (
        (-transformed_x - center_x_prime) / radius_x,
        (-transformed_y - center_y_prime) / radius_y,
    )
    start_angle = math.atan2(start_vector[1], start_vector[0])
    delta = math.atan2(
        start_vector[0] * end_vector[1] - start_vector[1] * end_vector[0],
        start_vector[0] * end_vector[0] + start_vector[1] * end_vector[1],
    )
    if sweep and delta < 0:
        delta += 2 * math.pi
    elif not sweep and delta > 0:
        delta -= 2 * math.pi

    segment_count = max(1, math.ceil(abs(delta) / ARC_STEP))
    points = []
    for index in range(1, segment_count):
        point_angle = start_angle + delta * index / segment_count
        points.append(
            (
                center_x
                + cos_angle * radius_x * math.cos(point_angle)
                - sin_angle * radius_y * math.sin(point_angle),
                center_y
                + sin_angle * radius_x * math.cos(point_angle)
                + cos_angle * radius_y * math.sin(point_angle),
            )
        )
    points.append(end)
    return points


def cubic_points(
    start: tuple[float, float],
    control_1: tuple[float, float],
    control_2: tuple[float, float],
    end: tuple[float, float],
) -> list[tuple[float, float]]:
    points = []
    for index in range(1, BEZIER_STEPS):
        t = index / BEZIER_STEPS
        inverse = 1 - t
        points.append(
            (
                inverse**3 * start[0]
                + 3 * inverse * inverse * t * control_1[0]
                + 3 * inverse * t * t * control_2[0]
                + t**3 * end[0],
                inverse**3 * start[1]
                + 3 * inverse * inverse * t * control_1[1]
                + 3 * inverse * t * t * control_2[1]
                + t**3 * end[1],
            )
        )
    points.append(end)
    return points


def parse_path(data: str) -> list[Polyline]:
    tokens = path_tokens(data)
    if not tokens or tokens[0] not in {"M", "m"}:
        raise ValueError("path data must begin with a moveto command")
    index = 0
    command: str | None = None
    current = (0.0, 0.0)
    start = (0.0, 0.0)
    points: list[tuple[float, float]] = []
    subpaths: list[Polyline] = []

    def finish_subpath() -> None:
        nonlocal points
        if points:
            if len(points) < 2:
                raise ValueError("path subpath must draw at least one segment")
            subpaths.append(Polyline(tuple(points)))
            points = []

    def read_values(count: int) -> list[float]:
        nonlocal index
        if index + count > len(tokens) or any(
            tokens[offset].isalpha() for offset in range(index, index + count)
        ):
            raise ValueError(f"path command {command!r} is missing parameters")
        values = [float(value) for value in tokens[index:index + count]]
        index += count
        return values

    while index < len(tokens):
        if tokens[index].isalpha():
            command = tokens[index]
            index += 1
            if command in "Zz":
                if not points:
                    raise ValueError("close-path command has no active subpath")
                if not same_point(points[-1], start):
                    points.append(start)
                current = start
                command = None
                continue
        if command is None:
            raise ValueError("path data must start with a command")

        relative = command.islower()
        operation = command.upper()
        if operation == "M":
            x, y = read_values(2)
            if relative:
                x += current[0]
                y += current[1]
            finish_subpath()
            current = (x, y)
            start = current
            points = [current]
            command = "l" if relative else "L"
        elif operation == "L":
            x, y = read_values(2)
            if relative:
                x += current[0]
                y += current[1]
            current = (x, y)
            points.append(current)
        elif operation == "H":
            x = read_values(1)[0]
            if relative:
                x += current[0]
            current = (x, current[1])
            points.append(current)
        elif operation == "V":
            y = read_values(1)[0]
            if relative:
                y += current[1]
            current = (current[0], y)
            points.append(current)
        elif operation == "C":
            control_1_x, control_1_y, control_2_x, control_2_y, x, y = read_values(6)
            if relative:
                control_1_x += current[0]
                control_1_y += current[1]
                control_2_x += current[0]
                control_2_y += current[1]
                x += current[0]
                y += current[1]
            destination = (x, y)
            points.extend(
                cubic_points(
                    current,
                    (control_1_x, control_1_y),
                    (control_2_x, control_2_y),
                    destination,
                )
            )
            current = destination
        elif operation == "A":
            radius_x, radius_y, rotation, large_arc, sweep, x, y = read_values(7)
            if large_arc not in {0.0, 1.0} or sweep not in {0.0, 1.0}:
                raise ValueError("arc flags must be 0 or 1")
            if relative:
                x += current[0]
                y += current[1]
            destination = (x, y)
            points.extend(
                arc_points(
                    current,
                    radius_x,
                    radius_y,
                    rotation,
                    bool(large_arc),
                    bool(sweep),
                    destination,
                )
            )
            current = destination
        else:
            raise ValueError(f"unsupported path command {command!r}")

    finish_subpath()
    return subpaths


def parse_points(value: str) -> tuple[tuple[float, float], ...]:
    tokens = path_tokens(value)
    if any(token.isalpha() for token in tokens) or len(tokens) < 4 or len(tokens) % 2:
        raise ValueError("polyline points must contain at least two coordinate pairs")
    values = [float(token) for token in tokens]
    return tuple(zip(values[::2], values[1::2]))


def runtime_polyline(polyline: Polyline) -> Primitive:
    if len(polyline.points) == 2:
        start, end = polyline.points
        if math.dist(start, end) <= DOT_SEGMENT_LENGTH:
            return Circle(
                (start[0] + end[0]) / 2,
                (start[1] + end[1]) / 2,
                STROKE_WIDTH / 2,
                True,
            )
    return polyline


def runtime_primitives(root: ET.Element) -> tuple[Primitive, ...]:
    primitives: list[Primitive] = []
    root_fill = root.attrib.get("fill", "none")
    root_stroke = root.attrib.get("stroke", "none")
    for element in root:
        name = local_name(element.tag)
        fill = element.attrib.get("fill", root_fill)
        stroke = element.attrib.get("stroke", root_stroke)
        for attribute in ("stroke-width", "stroke-linecap", "stroke-linejoin"):
            if (
                attribute in element.attrib
                and element.attrib[attribute] != root.attrib.get(attribute)
            ):
                raise ValueError(
                    f"<{name}> cannot override root {attribute} in the runtime renderer"
                )
        if fill == "currentColor":
            if name != "circle" or stroke != "none":
                raise ValueError(
                    "runtime fill is supported only for circles without a stroke"
                )
        elif fill != "none" or stroke != "currentColor":
            raise ValueError(
                f"<{name}> must inherit the canonical currentColor stroke"
            )
        if name == "path":
            if "d" not in element.attrib:
                raise ValueError("<path> requires d")
            primitives.extend(
                runtime_polyline(polyline)
                for polyline in parse_path(element.attrib["d"])
            )
        elif name == "line":
            primitives.append(
                runtime_polyline(
                    Polyline(
                        (
                            (number(element, "x1"), number(element, "y1")),
                            (number(element, "x2"), number(element, "y2")),
                        )
                    )
                )
            )
        elif name == "polyline":
            if "points" not in element.attrib:
                raise ValueError("<polyline> requires points")
            primitives.append(
                runtime_polyline(Polyline(parse_points(element.attrib["points"])))
            )
        elif name == "rect":
            radius = number(element, "rx", "0")
            radius_y = number(element, "ry", str(radius))
            if radius != radius_y:
                raise ValueError("runtime rectangles require equal rx and ry")
            primitives.append(
                Rectangle(
                    number(element, "x"),
                    number(element, "y"),
                    number(element, "width"),
                    number(element, "height"),
                    radius,
                )
            )
        elif name == "circle":
            primitives.append(
                Circle(
                    number(element, "cx"),
                    number(element, "cy"),
                    number(element, "r"),
                    fill == "currentColor",
                )
            )
    return tuple(primitives)


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
        if element is not root and name == "svg":
            raise ValueError("nested <svg> elements are unsupported")
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


def build_sheet(icons: list[tuple[Path, ET.Element, tuple[Primitive, ...]]]) -> str:
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
    for index, (path, source_root, _) in enumerate(icons):
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


def rust_number(value: float) -> str:
    if abs(value) < 0.00005:
        value = 0.0
    rendered = f"{value:.4f}".rstrip("0").rstrip(".")
    if "." not in rendered:
        rendered += ".0"
    return rendered


def build_runtime_geometry(
    icons: list[tuple[Path, ET.Element, tuple[Primitive, ...]]],
) -> str:
    lines = [
        "// @generated by scripts/validate-icons.py from assets/icons/source/*.svg.",
        "// Do not edit by hand.",
        "",
        "#[rustfmt::skip]",
        "fn icon_geometry(icon: Icon) -> &'static [Primitive] {",
        "    match icon {",
    ]
    for path, _, primitives in icons:
        variant = "".join(part.capitalize() for part in path.stem.split("-"))
        lines.append(f"        Icon::{variant} => &[")
        for primitive in primitives:
            if isinstance(primitive, Polyline):
                lines.append("            Primitive::Polyline(&[")
                for x, y in primitive.points:
                    lines.append(
                        f"                ({rust_number(x)}, {rust_number(y)}),"
                    )
                lines.append("            ]),")
            elif isinstance(primitive, Rectangle):
                lines.append(
                    "            Primitive::Rectangle { "
                    f"x: {rust_number(primitive.x)}, "
                    f"y: {rust_number(primitive.y)}, "
                    f"width: {rust_number(primitive.width)}, "
                    f"height: {rust_number(primitive.height)}, "
                    f"radius: {rust_number(primitive.radius)} "
                    "},"
                )
            else:
                kind = "FilledCircle" if primitive.filled else "Circle"
                lines.append(
                    f"            Primitive::{kind} {{ "
                    f"x: {rust_number(primitive.x)}, "
                    f"y: {rust_number(primitive.y)}, "
                    f"radius: {rust_number(primitive.radius)} "
                    "},"
                )
        lines.append("        ],")
    lines.extend(["    }", "}", ""])
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify the committed sheet instead of rewriting it")
    args = parser.parse_args()

    paths = sorted(SOURCE.glob("*.svg"))
    actual = {path.stem for path in paths}
    errors: list[str] = []
    if actual != EXPECTED:
        errors.append(f"icon inventory mismatch; missing={sorted(EXPECTED - actual)}, extra={sorted(actual - EXPECTED)}")

    icons: list[tuple[Path, ET.Element, tuple[Primitive, ...]]] = []
    for path in paths:
        try:
            root = validate(path)
            icons.append((path, root, runtime_primitives(root)))
        except ValueError as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1

    sheet = build_sheet(icons)
    runtime_geometry = build_runtime_geometry(icons)
    if args.check:
        stale = []
        for path, generated in ((SHEET, sheet), (RUNTIME_GEOMETRY, runtime_geometry)):
            if not path.exists() or path.read_text(encoding="utf-8") != generated:
                stale.append(str(path.relative_to(ROOT)))
        if stale:
            print(
                f"{', '.join(stale)} stale; run scripts/validate-icons.py",
                file=sys.stderr,
            )
            return 1
    else:
        SHEET.write_text(sheet, encoding="utf-8")
        RUNTIME_GEOMETRY.write_text(runtime_geometry, encoding="utf-8")

    print(
        f"validated {len(icons)} icons; generated "
        f"{SHEET.relative_to(ROOT)} and {RUNTIME_GEOMETRY.relative_to(ROOT)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
