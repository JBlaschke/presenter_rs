#!/usr/bin/env python3
"""Writes plot.png next to this script: a small line chart, drawn pixel by pixel.

Only the Python standard library is used and the output is byte-for-byte deterministic,
so the committed figure can be regenerated with `python3 make-plot.py`. Python is not
needed to build the example deck; this script only documents where the figure came from.
"""

import math
import pathlib
import struct
import zlib

WIDTH, HEIGHT = 640, 400
LEFT, RIGHT, TOP, BOTTOM = 60, 620, 30, 350  # plot area (pixels)
WHITE = (255, 255, 255)
GRID = (229, 231, 235)
AXIS = (55, 65, 81)
LINE = (37, 99, 235)
FILL = (219, 234, 254)
POINT = (234, 88, 12)


def curve(x):
    """A damped oscillation on 0 <= x <= 10, scaled to roughly -1..1."""
    return math.exp(-x / 6.0) * math.cos(1.3 * x)


def to_px(x, y):
    px = LEFT + (RIGHT - LEFT) * x / 10.0
    py = TOP + (BOTTOM - TOP) * (1.0 - (y + 1.0) / 2.0)
    return px, py


def main():
    pixels = [[WHITE] * WIDTH for _ in range(HEIGHT)]

    def put(x, y, color):
        if 0 <= x < WIDTH and 0 <= y < HEIGHT:
            pixels[y][x] = color

    def disc(cx, cy, r, color):
        for y in range(int(cy - r) - 1, int(cy + r) + 2):
            for x in range(int(cx - r) - 1, int(cx + r) + 2):
                if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
                    put(x, y, color)

    # grid: 10 vertical and 8 horizontal divisions
    for i in range(11):
        x = LEFT + (RIGHT - LEFT) * i // 10
        for y in range(TOP, BOTTOM + 1):
            put(x, y, GRID)
    for j in range(9):
        y = TOP + (BOTTOM - TOP) * j // 8
        for x in range(LEFT, RIGHT + 1):
            put(x, y, GRID)

    # area between the curve and y = 0
    _, zero = to_px(0, 0.0)
    for x in range(LEFT, RIGHT + 1):
        _, py = to_px(0, curve(10.0 * (x - LEFT) / (RIGHT - LEFT)))
        lo, hi = sorted((int(round(py)), int(round(zero))))
        for y in range(lo, hi + 1):
            put(x, y, FILL)

    # axes: y = 0 line and the left axis
    for x in range(LEFT, RIGHT + 1):
        for dy in (0, 1):
            put(x, int(round(zero)) + dy, AXIS)
    for y in range(TOP, BOTTOM + 1):
        for dx in (0, 1):
            put(LEFT + dx, y, AXIS)

    # the curve, as overlapping discs along a fine sampling
    steps = 2000
    for i in range(steps + 1):
        x = 10.0 * i / steps
        disc(*to_px(x, curve(x)), 2.2, LINE)

    # measured points
    for k in range(11):
        x = float(k)
        disc(*to_px(x, curve(x)), 5.5, POINT)

    raw = b"".join(b"\x00" + bytes(c for px in row for c in px) for row in pixels)

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", WIDTH, HEIGHT, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    out = pathlib.Path(__file__).with_name("plot.png")
    out.write_bytes(png)
    print(f"wrote {out} ({len(png)} bytes)")


if __name__ == "__main__":
    main()
