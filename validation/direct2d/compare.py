"""Compare actual framebuffer readbacks, not PrintWindow captures."""

import argparse
import json
from pathlib import Path

from PIL import Image, ImageChops


def compare(reference, candidate, tolerance=2):
    if reference.size != candidate.size:
        raise ValueError("Framebuffer dimensions differ")
    difference = ImageChops.difference(reference.convert("RGBA"), candidate.convert("RGBA"))
    mask = Image.new("L", reference.size)
    maximum = 0
    for band in difference.split():
        maximum = max(maximum, band.getextrema()[1])
        mask = ImageChops.lighter(mask, band.point(lambda value: 255 if value > tolerance else 0))
    changed = mask.histogram()[255]
    return {
        "maximum_channel_difference": maximum,
        "pixels_over_tolerance": changed,
        "total_pixels": reference.width * reference.height,
        "tolerance": tolerance,
        "equivalent": changed == 0,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    results = []
    for case in ("sparse", "dense", "scrolling", "colored", "unicode"):
        reference = Image.open(args.directory / f"{case}-wgpu.png").convert("RGBA")
        raw = (args.directory / f"{case}-d2d.bgra").read_bytes()
        if len(raw) != reference.width * reference.height * 4:
            raise ValueError(f"{case}: invalid Direct2D readback size")
        candidate = Image.frombytes("RGBA", reference.size, raw, "raw", "BGRA")
        candidate.save(args.directory / f"{case}-d2d.png")
        result = {"case": case, **compare(reference, candidate)}
        results.append(result)
        print(json.dumps(result))
    (args.directory / "pixel-comparison.json").write_text(
        json.dumps(results, indent=2) + "\n", encoding="utf-8"
    )
    return 0 if all(result["equivalent"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
