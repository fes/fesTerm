#!/usr/bin/env python3
"""Build the Windows ICO container from fesTerm's canonical PNG icons."""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "assets/app-icon"
SIZES = (16, 32, 64, 128, 256)
OUTPUT = ICON_DIR / "festerm.ico"
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


class IconError(Exception):
    pass


def png_for_size(size: int) -> bytes:
    path = ICON_DIR / f"app-icon-{size}.png"
    data = path.read_bytes()
    if not data.startswith(PNG_SIGNATURE) or len(data) < 24:
        raise IconError(f"invalid PNG icon: {path.relative_to(ROOT)}")
    width, height = struct.unpack(">II", data[16:24])
    if (width, height) != (size, size):
        raise IconError(
            f"{path.relative_to(ROOT)} is {width}x{height}, expected {size}x{size}"
        )
    return data


def build_ico() -> bytes:
    images = [(size, png_for_size(size)) for size in SIZES]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = []
    payload = []
    for size, image in images:
        dimension = 0 if size == 256 else size
        entries.append(
            struct.pack(
                "<BBBBHHII",
                dimension,
                dimension,
                0,
                0,
                1,
                32,
                len(image),
                offset,
            )
        )
        payload.append(image)
        offset += len(image)
    return header + b"".join(entries) + b"".join(payload)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail unless the checked-in ICO matches the canonical PNGs",
    )
    args = parser.parse_args()
    try:
        expected = build_ico()
        if args.check:
            if not OUTPUT.is_file() or OUTPUT.read_bytes() != expected:
                raise IconError(
                    "assets/app-icon/festerm.ico is stale; "
                    "run scripts/generate_windows_icon.py"
                )
            print("Windows icon: PASS")
        else:
            OUTPUT.write_bytes(expected)
            print(f"Windows icon: wrote {OUTPUT.relative_to(ROOT)}")
    except (IconError, OSError) as error:
        print(f"Windows icon: FAIL\n{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
