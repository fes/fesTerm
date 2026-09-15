import importlib.util
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "validate-icons.py"
SPEC = importlib.util.spec_from_file_location("validate_icons", SCRIPT)
icons = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = icons
SPEC.loader.exec_module(icons)


class IconValidationTests(unittest.TestCase):
    def canonical_icons(self):
        result = []
        for path in sorted(icons.SOURCE.glob("*.svg")):
            root = icons.validate(path)
            result.append((path, root, icons.runtime_primitives(root)))
        return result

    def test_edit_closed_path_generates_the_closing_pencil_edge(self):
        root = icons.validate(icons.SOURCE / "edit.svg")
        pencil = icons.runtime_primitives(root)[0]

        self.assertIsInstance(pencil, icons.Polyline)
        self.assertEqual(pencil.points[0], (4.0, 20.0))
        self.assertEqual(pencil.points[-2:], ((8.0, 20.0), (4.0, 20.0)))

    def test_committed_runtime_geometry_matches_canonical_svgs(self):
        generated = icons.build_runtime_geometry(self.canonical_icons())

        self.assertEqual(
            icons.RUNTIME_GEOMETRY.read_text(encoding="utf-8"),
            generated,
        )

    def test_runtime_paths_require_an_initial_moveto(self):
        with self.assertRaisesRegex(ValueError, "begin with a moveto"):
            icons.parse_path("L 1 1 L 2 2")

    def test_optical_stroke_width_is_generated_from_the_source(self):
        generated = icons.build_runtime_geometry(self.canonical_icons())
        self.assertIn("Icon::SshRemote => 1.0,", generated)
        self.assertIn("Icon::RemoteGlobe => 1.0,", generated)
        self.assertIn("Icon::Serial => 1.25,", generated)
        self.assertIn("_ => 1.75,", generated)

    def test_dot_primitives_use_their_source_stroke_width(self):
        root = ET.fromstring("""\
<svg fill="none" stroke="currentColor" stroke-width="1">
  <path d="M2 2h.01"/>
  <line x1="4" y1="4" x2="4.01" y2="4"/>
  <polyline points="6,6 6.01,6"/>
</svg>
""")
        primitives = icons.runtime_primitives(root)
        self.assertEqual(len(primitives), 3)
        for primitive in primitives:
            self.assertIsInstance(primitive, icons.Circle)
            self.assertTrue(primitive.filled)
            self.assertEqual(primitive.radius, 0.5)

    def test_invalid_optical_stroke_width_is_rejected(self):
        for width in ("0", "-1", ".99", "1.76", "nan", "inf"):
            with self.subTest(width=width), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "invalid.svg"
                path.write_text(f"""\
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
     fill="none" stroke="currentColor" stroke-width="{width}"
     stroke-linecap="round"><path d="M1 1H5"/></svg>
""", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "stroke-width"):
                    icons.validate(path)

    def test_serial_review_sheet_preserves_filled_pin_color(self):
        path = icons.SOURCE / "serial.svg"
        root = icons.validate(path)
        sheet = ET.fromstring(icons.build_sheet([(path, root, icons.runtime_primitives(root))]))
        pins = list(sheet.iter(f"{{{icons.SVG_NS}}}circle"))
        self.assertEqual(len(pins), 18)  # Nine pins at each review size.
        for pin in pins:
            self.assertEqual(pin.attrib["fill"], "#e9edf2")
            self.assertEqual(pin.attrib["stroke"], "none")

    def test_nested_svg_geometry_is_rejected_instead_of_omitted(self):
        document = """\
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
     fill="none" stroke="currentColor" stroke-width="1.75"
     stroke-linecap="round">
  <svg><path d="M 1 1 L 2 2"/></svg>
</svg>
"""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "nested.svg"
            path.write_text(document, encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "nested <svg>"):
                icons.validate(path)


if __name__ == "__main__":
    unittest.main()
