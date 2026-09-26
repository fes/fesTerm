import unittest

from PIL import Image

from compare import compare


class FramebufferComparisonTests(unittest.TestCase):
    def test_equal_images(self):
        image = Image.new("RGBA", (2, 2), (12, 34, 56, 255))
        self.assertTrue(compare(image, image)["equivalent"])

    def test_rounding_tolerance_is_per_channel(self):
        reference = Image.new("RGBA", (2, 2), (12, 34, 56, 255))
        candidate = Image.new("RGBA", (2, 2), (14, 32, 58, 255))
        self.assertTrue(compare(reference, candidate)["equivalent"])
        candidate.putpixel((0, 0), (15, 34, 56, 255))
        self.assertEqual(compare(reference, candidate)["pixels_over_tolerance"], 1)

    def test_one_missing_glyph_pixel_cannot_hide_in_a_large_background(self):
        reference = Image.new("RGBA", (640, 400), (16, 20, 24, 255))
        candidate = reference.copy()
        reference.putpixel((100, 100), (255, 255, 255, 255))
        self.assertFalse(compare(reference, candidate)["equivalent"])

    def test_alpha_changes_are_not_ignored(self):
        reference = Image.new("RGBA", (2, 2), (12, 34, 56, 255))
        candidate = Image.new("RGBA", (2, 2), (12, 34, 56, 128))
        self.assertEqual(compare(reference, candidate)["pixels_over_tolerance"], 4)

    def test_dimensions_must_match(self):
        with self.assertRaises(ValueError):
            compare(Image.new("RGBA", (1, 1)), Image.new("RGBA", (2, 1)))


if __name__ == "__main__":
    unittest.main()
