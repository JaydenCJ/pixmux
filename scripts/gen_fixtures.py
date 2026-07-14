#!/usr/bin/env python3
"""Regenerate the synthetic wire-format test fixtures in tests/fixtures/.

The fixtures are generated, not captured from live sessions: they reproduce,
byte for byte, the wire format that real kitty
graphics protocol emitters use (kitty +kitten icat, matplotlib kitty
backends): chunked APC transmissions with base64 payloads, interleaved with
ordinary shell/CSI output.

The tmux golden file is produced by an *independent* Python implementation of
tmux passthrough wrapping (ESC doubling inside `ESC Ptmux; ... ESC \\`), so the
Rust implementation is cross-checked against a second implementation rather
than against itself.

Deterministic: running this script twice produces identical files.
Stdlib only; no third-party dependencies.
"""

import base64
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "..", "tests", "fixtures")

ESC = b"\x1b"
ST = ESC + b"\\"


def png_chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def make_png(width: int, height: int) -> bytes:
    """A real, decodable RGB PNG with a deterministic gradient pattern."""
    raw = b""
    for y in range(height):
        row = b"\x00"  # filter type 0
        for x in range(width):
            r = (x * 255) // max(width - 1, 1)
            g = (y * 255) // max(height - 1, 1)
            b = (x * y * 255) // max((width - 1) * (height - 1), 1)
            row += bytes((r, g, b))
        raw += row
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(raw, 9))
        + png_chunk(b"IEND", b"")
    )


def apc(body: bytes) -> bytes:
    return ESC + b"_G" + body + ST


def kitty_transmit_chunked(png: bytes, image_id: int, nchunks: int) -> bytes:
    """First chunk carries all keys + m=1; continuations only m=; last m=0."""
    b64 = base64.standard_b64encode(png)
    chunk = -(-len(b64) // nchunks)  # ceil division: exactly nchunks parts
    parts = [b64[i : i + chunk] for i in range(0, len(b64), chunk)] or [b""]
    assert len(parts) == nchunks, "fixture chunk count drifted"
    out = b""
    for idx, part in enumerate(parts):
        last = idx == len(parts) - 1
        if idx == 0:
            keys = b"a=T,f=100,i=%d,q=2" % image_id
            if len(parts) > 1:
                keys += b",m=1" if not last else b",m=0"
        else:
            keys = b"m=0" if last else b"m=1"
        out += apc(keys + b";" + part)
    return out


def tmux_wrap(seq: bytes) -> bytes:
    """Independent implementation of tmux passthrough wrapping."""
    return ESC + b"Ptmux;" + seq.replace(ESC, ESC + ESC) + ST


def build_icat_stream(png: bytes) -> bytes:
    """Prompt + colored ls output + chunked image + delete + trailing prompt."""
    return (
        b"$ \x1b[1mls\x1b[0m demo.png\r\n"
        b"\x1b[32mdemo.png\x1b[0m\r\n"
        b"$ kitty +kitten icat demo.png\r\n"
        + kitty_transmit_chunked(png, image_id=31337, nchunks=3)
        + b"\r\n$ "
        + apc(b"a=d,d=i,i=31337")
        + b"\r\n$ exit\r\n"
    )


def build_plain_vim_stream() -> bytes:
    """A graphics-free recording: DECSET, OSC title, CSI moves, text."""
    return (
        b"\x1b[?1049h"  # alternate screen
        b"\x1b]0;vim notes.txt\x07"  # OSC window title (BEL-terminated)
        b"\x1b[2J\x1b[H"
        b"\x1b[1;1H~ hello from vim\r\n"
        b"\x1b[7m-- INSERT --\x1b[0m"
        b"\x1b_Zsome-other-apc\x1b\\"  # foreign APC: must pass through
        b"\x1b[?1049l"
        b"bye\r\n"
    )


def translate_tmux(stream: bytes) -> bytes:
    """Wrap every kitty graphics APC in tmux passthrough; leave the rest.

    Simple two-phase scan (not incremental): fixtures are complete streams.
    """
    out = b""
    i = 0
    marker = ESC + b"_G"
    while i < len(stream):
        j = stream.find(marker, i)
        if j < 0:
            out += stream[i:]
            break
        out += stream[i:j]
        k = stream.find(ST, j + len(marker))
        assert k >= 0, "unterminated APC in fixture stream"
        seq = stream[j : k + len(ST)]
        out += tmux_wrap(seq)
        i = k + len(ST)
    return out


def main() -> None:
    os.makedirs(FIXTURES, exist_ok=True)
    png = make_png(16, 16)

    with open(os.path.join(FIXTURES, "sample.png"), "wb") as f:
        f.write(png)

    icat = build_icat_stream(png)
    with open(os.path.join(FIXTURES, "icat_chunked.bin"), "wb") as f:
        f.write(icat)

    with open(os.path.join(FIXTURES, "icat_chunked.tmux.golden"), "wb") as f:
        f.write(translate_tmux(icat))

    with open(os.path.join(FIXTURES, "plain_vim_session.bin"), "wb") as f:
        f.write(build_plain_vim_stream())

    print("fixtures written to", os.path.normpath(FIXTURES))


if __name__ == "__main__":
    main()
