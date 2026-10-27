#!/usr/bin/env python3
"""Draw the background of the PowerMac disk image window (FANCY=1 in
scripts/package-dmg.sh): a soft vertical gradient, an arrow from the app's
icon (left) to Applications (right). Pure Python (zlib),
so packaging needs nothing beyond what macOS ships.

    dmg-background.py OUT.png [WIDTH HEIGHT]
"""

import struct
import sys
import zlib


def png(path, w, h, pixels):
    raw = b"".join(b"\x00" + bytes(pixels[y * w * 3:(y + 1) * w * 3]) for y in range(h))

    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


def main():
    out = sys.argv[1]
    w = int(sys.argv[2]) if len(sys.argv) > 2 else 640
    h = int(sys.argv[3]) if len(sys.argv) > 3 else 400
    top, bottom = (0xF7, 0xF8, 0xFB), (0xE6, 0xEB, 0xF4)
    px = bytearray(w * h * 3)
    for y in range(h):
        t = y / (h - 1)
        c = [round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)]
        row = bytes(c) * w
        px[y * w * 3:(y + 1) * w * 3] = row

    def blend(x, y, col, a):
        if 0 <= x < w and 0 <= y < h and a > 0:
            i = (y * w + x) * 3
            for k in range(3):
                px[i + k] = round(px[i + k] * (1 - a) + col[k] * a)

    accent = (0x2B, 0x6C, 0xF0)
    # Arrow shaft between the icons (centers at x=170 and x=470, y=190).
    cy = 190 * h // 400
    x0, x1 = 250 * w // 640, 382 * w // 640
    for x in range(x0, x1):
        for dy in range(-3, 4):
            a = 1.0 if abs(dy) < 3 else 0.5
            blend(x, cy + dy, accent, a)
    # Head.
    for i in range(0, 26):
        half = 16 - i * 16 // 26
        for dy in range(-half, half + 1):
            a = 1.0 if abs(dy) < half else 0.5
            blend(x1 + i, cy + dy, accent, a)
    png(out, w, h, px)


if __name__ == "__main__":
    main()
