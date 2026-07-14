#!/usr/bin/env python3
"""Generate docs/assets/screenshot.png from real pixmux output.

The figure shows the actual transcoding result: the 16x16 gradient test PNG
(left) next to the image reconstructed from pixmux's real sixel output
(right), both magnified 8x. The right panel is produced by running the
compiled binary (`pixmux cat --target zellij`) and decoding the sixel bytes
it emits — nothing is mocked.

Stdlib only; deterministic given the same pixmux binary output.
"""

import os
import struct
import subprocess
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, ".."))
BIN = os.path.join(ROOT, "target", "release", "pixmux")
SAMPLE = os.path.join(ROOT, "tests", "fixtures", "sample.png")
OUT = os.path.join(ROOT, "docs", "assets", "screenshot.png")

W = H = 16  # sample image dimensions
SCALE = 8


def sample_pixels():
    """Same formula as scripts/gen_fixtures.py make_png(16, 16)."""
    px = {}
    for y in range(H):
        for x in range(W):
            r = (x * 255) // (W - 1)
            g = (y * 255) // (H - 1)
            b = (x * y * 255) // ((W - 1) * (H - 1))
            px[(x, y)] = (r, g, b)
    return px


def decode_sixel(data: bytes):
    """Minimal sixel decoder: palette defs + sixel chars -> pixel dict."""
    text = data.decode("ascii", errors="replace")
    q = text.index("q")
    body = text[q + 1 :]
    if body.startswith('"'):  # raster attributes "P1;P2;W;H
        end = 1
        while end < len(body) and (body[end].isdigit() or body[end] == ";"):
            end += 1
        body = body[end:]
    palette = {}
    pixels = {}
    color = 0
    x = 0
    band = 0
    i = 0
    while i < len(body):
        ch = body[i]
        if ch == "\x1b":
            break
        if ch == "#":
            j = i + 1
            while j < len(body) and body[j].isdigit():
                j += 1
            reg = int(body[i + 1 : j])
            if j < len(body) and body[j] == ";":
                parts = []
                k = j
                while len(parts) < 4 and k < len(body):
                    k += 1
                    num = ""
                    while k < len(body) and body[k].isdigit():
                        num += body[k]
                        k += 1
                    parts.append(int(num))
                    if k >= len(body) or body[k] != ";":
                        break
                mode, r, g, b = parts
                assert mode == 2
                palette[reg] = (r * 255 // 100, g * 255 // 100, b * 255 // 100)
                i = k
            else:
                color = reg
                x = 0 if x == 0 else x
                i = j
            continue
        if ch == "$":
            x = 0
            i += 1
            continue
        if ch == "-":
            band += 1
            x = 0
            i += 1
            continue
        if ch == "!":
            j = i + 1
            while body[j].isdigit():
                j += 1
            count = int(body[i + 1 : j])
            bits = ord(body[j]) - 0x3F
            for _ in range(count):
                for row in range(6):
                    if bits & (1 << row):
                        pixels[(x, band * 6 + row)] = palette[color]
                x += 1
            i = j + 1
            continue
        code = ord(ch)
        if 0x3F <= code <= 0x7E:
            bits = code - 0x3F
            for row in range(6):
                if bits & (1 << row):
                    pixels[(x, band * 6 + row)] = palette[color]
            x += 1
        i += 1
    return pixels


# 3x5 pixel font for the two labels (uppercase subset).
FONT = {
    "K": ["101", "101", "110", "101", "101"],
    "I": ["111", "010", "010", "010", "111"],
    "T": ["111", "010", "010", "010", "010"],
    "Y": ["101", "101", "010", "010", "010"],
    "P": ["110", "101", "110", "100", "100"],
    "N": ["101", "111", "111", "111", "101"],
    "G": ["111", "100", "101", "101", "111"],
    "Z": ["111", "001", "010", "100", "111"],
    "E": ["111", "100", "110", "100", "111"],
    "L": ["100", "100", "100", "100", "111"],
    "J": ["011", "001", "001", "101", "111"],
    "S": ["111", "100", "111", "001", "111"],
    "X": ["101", "101", "010", "101", "101"],
    "U": ["101", "101", "101", "101", "111"],
    "M": ["101", "111", "111", "101", "101"],
    " ": ["000", "000", "000", "000", "000"],
    ">": ["100", "010", "001", "010", "100"],
}


def draw_text(canvas, cw, text, ox, oy, scale, rgb):
    for idx, chch in enumerate(text):
        glyph = FONT[chch]
        for gy, row in enumerate(glyph):
            for gx, bit in enumerate(row):
                if bit == "1":
                    for sy in range(scale):
                        for sx in range(scale):
                            px = ox + (idx * 4 + gx) * scale + sx
                            py = oy + gy * scale + sy
                            canvas[py * cw + px] = rgb


def write_png(path, cw, chh, canvas):
    raw = b""
    for y in range(chh):
        row = b"\x00"
        for x in range(cw):
            row += bytes(canvas[y * cw + x])
        raw += row

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", cw, chh, 8, 2, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b"")
        )


def main():
    if not os.path.exists(BIN):
        sys.exit("build first: cargo build --release")
    sixel = subprocess.run(
        [BIN, "cat", "--target", "zellij", SAMPLE],
        capture_output=True,
        check=True,
    ).stdout
    right = decode_sixel(sixel)
    left = sample_pixels()

    margin, gap, panel = 24, 56, W * SCALE
    label_h = 5 * 3 + 10
    cw = margin + panel + gap + panel + margin
    chh = margin + label_h + panel + margin
    bg = (24, 26, 32)
    canvas = [bg] * (cw * chh)

    def blit(pixels, ox, oy):
        for (x, y), rgb in pixels.items():
            if 0 <= x < W and 0 <= y < H:
                for sy in range(SCALE):
                    for sx in range(SCALE):
                        canvas[(oy + y * SCALE + sy) * cw + ox + x * SCALE + sx] = rgb

    text_col = (210, 214, 220)
    draw_text(canvas, cw, "KITTY PNG", margin, margin, 3, text_col)
    draw_text(canvas, cw, "ZELLIJ SIXEL", margin + panel + gap, margin, 3, text_col)
    blit(left, margin, margin + label_h)
    blit(right, margin + panel + gap, margin + label_h)
    # Arrow between panels.
    ay = margin + label_h + panel // 2
    ax = margin + panel + gap // 2 - 6
    draw_text(canvas, cw, ">", ax, ay - 7, 5, (120, 190, 120))

    write_png(OUT, cw, chh, canvas)
    print(f"wrote {os.path.relpath(OUT, ROOT)} ({cw}x{chh})")


if __name__ == "__main__":
    main()
