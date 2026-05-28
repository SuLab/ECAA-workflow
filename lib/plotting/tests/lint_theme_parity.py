"""
Theme parity linter for generated renderer figures.

Phase 7 of the flexible plotting upgrade plan — checks that all colors
appearing in a rendered PNG are present in the theme.json Wong/Glasbey
palette.

Usage:
    python lib/plotting/tests/lint_theme_parity.py \
        --png <path/to/figure.png> \
        --theme <path/to/theme.json> \
        --output-json

Exit 0 when all colors are on-palette; exit 1 with a JSON line on stdout
describing the off-palette colors.

Design notes:
- Tolerance: colors within 15 RGB units (L∞ distance) of a palette
  color are considered on-palette. Matplotlib's anti-aliasing and JPEG-
  like quantization effects can shift a palette color by a few counts.
- Background color (the matplotlib default `axes.facecolor` = #FFFFFF
  and figure background = #FFFFFF) is excluded from the check since it
  is not part of the data visualization palette.
- Alpha channel is ignored; only R/G/B are checked.
- Only pixels that appear ≥ MIN_PIXEL_COUNT times are tested (avoids
  false positives from anti-aliasing fringe pixels at color boundaries).
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
import zlib
from pathlib import Path
from typing import Any

# Minimum pixel count for a color to be considered "used" by the figure.
MIN_PIXEL_COUNT: int = 10

# L∞ tolerance for palette matching (anti-aliasing headroom).
PALETTE_TOLERANCE: int = 15

# Colors excluded from the parity check (matplotlib defaults).
EXCLUDED_COLORS: set[tuple[int, int, int]] = {
    (255, 255, 255),  # White background
    (0, 0, 0),        # Black (axis lines, labels)
}


def load_theme_palette(theme_path: str) -> list[tuple[int, int, int]]:
    """Load the Wong/Glasbey palette from theme.json."""
    with open(theme_path) as f:
        theme: dict[str, Any] = json.load(f)

    palette_hex: list[str] = theme.get("palette", [])
    if not palette_hex:
        # Fallback: look for a "colors" key or a "wong_glasbey" key.
        palette_hex = theme.get("colors", theme.get("wong_glasbey", []))

    result: list[tuple[int, int, int]] = []
    for hex_color in palette_hex:
        h = hex_color.lstrip("#")
        if len(h) == 6:
            r, g, b = int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)
            result.append((r, g, b))
    return result


def read_png_pixels(png_path: str) -> list[tuple[int, int, int]]:
    """
    Parse a PNG file and return all unique (R, G, B) tuples that appear
    at least MIN_PIXEL_COUNT times.

    Uses only stdlib (struct + zlib) to avoid requiring Pillow.
    Supports 8-bit RGB and RGBA PNGs only. Other color types raise
    ValueError.
    """
    data = Path(png_path).read_bytes()

    # Validate PNG signature.
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"not a PNG file: {png_path}")

    # Parse chunks.
    pos = 8
    width = height = 0
    bit_depth = color_type = 0
    idat_chunks: list[bytes] = []

    while pos < len(data):
        if pos + 8 > len(data):
            break
        chunk_len = struct.unpack(">I", data[pos: pos + 4])[0]
        chunk_tag = data[pos + 4: pos + 8]
        chunk_data = data[pos + 8: pos + 8 + chunk_len]
        pos += 12 + chunk_len  # skip CRC too

        if chunk_tag == b"IHDR":
            width, height = struct.unpack(">II", chunk_data[:8])
            bit_depth = chunk_data[8]
            color_type = chunk_data[9]
        elif chunk_tag == b"IDAT":
            idat_chunks.append(chunk_data)
        elif chunk_tag == b"IEND":
            break

    if bit_depth != 8:
        raise ValueError(f"unsupported bit depth {bit_depth} in {png_path}")
    if color_type not in (2, 6):
        # 2 = RGB, 6 = RGBA. Indexed (3) not supported.
        raise ValueError(f"unsupported PNG color type {color_type} in {png_path}")

    raw = zlib.decompress(b"".join(idat_chunks))
    bytes_per_pixel = 3 if color_type == 2 else 4
    row_bytes = 1 + width * bytes_per_pixel  # +1 for filter byte

    pixel_counts: dict[tuple[int, int, int], int] = {}
    for y in range(height):
        row_start = y * row_bytes
        # Skip the filter byte (index row_start).
        for x in range(width):
            offset = row_start + 1 + x * bytes_per_pixel
            r, g, b = raw[offset], raw[offset + 1], raw[offset + 2]
            key = (r, g, b)
            pixel_counts[key] = pixel_counts.get(key, 0) + 1

    # Return colors that appear at least MIN_PIXEL_COUNT times.
    return [color for color, count in pixel_counts.items() if count >= MIN_PIXEL_COUNT]


def is_on_palette(
    color: tuple[int, int, int],
    palette: list[tuple[int, int, int]],
    tolerance: int = PALETTE_TOLERANCE,
) -> bool:
    """True when `color` is within L∞ `tolerance` of any palette entry."""
    r, g, b = color
    for pr, pg, pb in palette:
        if max(abs(r - pr), abs(g - pg), abs(b - pb)) <= tolerance:
            return True
    return False


def rgb_to_hex(r: int, g: int, b: int) -> str:
    return f"#{r:02x}{g:02x}{b:02x}"


def main() -> int:
    parser = argparse.ArgumentParser(description="Lint PNG theme parity")
    parser.add_argument("--png", required=True, help="Path to the PNG file")
    parser.add_argument("--theme", required=True, help="Path to theme.json")
    parser.add_argument(
        "--output-json",
        action="store_true",
        help="Emit JSON result to stdout (required by harness)",
    )
    parser.add_argument(
        "--tolerance",
        type=int,
        default=PALETTE_TOLERANCE,
        help=f"L∞ palette match tolerance (default {PALETTE_TOLERANCE})",
    )
    args = parser.parse_args()

    try:
        palette = load_theme_palette(args.theme)
    except Exception as exc:
        msg = {"error": f"failed to load theme: {exc}"}
        print(json.dumps(msg), flush=True)
        return 1

    if not palette:
        msg = {"error": "theme.json palette is empty; cannot check parity"}
        print(json.dumps(msg), flush=True)
        return 1

    try:
        pixels = read_png_pixels(args.png)
    except Exception as exc:
        msg = {"error": f"failed to read PNG: {exc}"}
        print(json.dumps(msg), flush=True)
        return 1

    off_palette: list[str] = []
    for color in pixels:
        if color in EXCLUDED_COLORS:
            continue
        if not is_on_palette(color, palette, args.tolerance):
            off_palette.append(rgb_to_hex(*color))

    if not off_palette:
        if args.output_json:
            print(json.dumps({"status": "pass", "off_palette_colors": []}), flush=True)
        return 0
    else:
        result = {
            "status": "fail",
            "off_palette_colors": sorted(set(off_palette)),
            "palette_size": len(palette),
            "tolerance": args.tolerance,
        }
        if args.output_json:
            print(json.dumps(result), flush=True)
        else:
            print(f"Theme parity check failed: {len(off_palette)} off-palette color(s)")
            for c in sorted(set(off_palette)):
                print(f"  {c}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
