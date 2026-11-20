#!/usr/bin/env python3
"""Deterministic Kitty-graphics workload for the image pipeline benchmark.

Runs inside a benchmark pane. Frames are generated before the trigger so
generation cost never overlaps measurement. Modes mirror kittybrowser:
`png` replaces one full-pane PNG per frame (kb's whole-frame path, image ID
reuse included); `strips` sends one full PNG, then replaces a zlib-compressed
raw RGB strip per frame (kb's changed-region path). The last frame is always
a solid green full-pane PNG, which the sampler uses as the completion marker.
"""
from __future__ import annotations

import argparse
import base64
import fcntl
import json
import os
from pathlib import Path
import random
import struct
import sys
import termios
import time
import zlib

CHUNK = 4096
GREEN = (0, 200, 0)


def png(width: int, height: int, raw_rows: list[bytes]) -> bytes:
    """Encode 8-bit RGB rows as a PNG using filter type 0, like a naive encoder."""
    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    data = zlib.compress(b"".join(b"\0" + row for row in raw_rows), 6)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", data) + chunk(b"IEND", b"")


def row_pool(width: int, rng: random.Random, count: int = 97) -> list[bytes]:
    """Rows of random colour runs: web-page-like compressibility, never the terminal background."""
    pool = []
    for _ in range(count):
        row = bytearray()
        while len(row) < width * 3:
            colour = bytes(rng.randrange(40, 256) for _ in range(3))
            row += colour * rng.randrange(1, 5)
        pool.append(bytes(row[: width * 3]))
    return pool


def frame_rows(width: int, height: int, frame: int) -> list[bytes]:
    """Freshly generated rows per frame, so no codec window finds long-range repeats."""
    return row_pool(width, random.Random(frame * 1_000_003 + height), height)


def command(keys: str, payload: bytes) -> bytes:
    """One Kitty graphics command, base64 chunked as the protocol requires for direct transmission."""
    encoded = base64.standard_b64encode(payload)
    parts = [encoded[i : i + CHUNK] for i in range(0, len(encoded), CHUNK)] or [b""]
    out = bytearray()
    for index, part in enumerate(parts):
        more = int(index + 1 < len(parts))
        prefix = f"{keys},m={more}" if index == 0 else f"m={more}"
        out += b"\x1b_G" + prefix.encode() + b";" + part + b"\x1b\\"
    return bytes(out)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("png", "strips"), required=True)
    parser.add_argument("--frames", type=int, default=60)
    parser.add_argument("--fps", type=float, default=15)
    parser.add_argument("--trigger", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()

    out = sys.stdout.buffer
    out.write(b"\x1b[?25l\x1b[2J\x1b[Hwaiting for trigger\r\n")
    out.flush()
    while not args.trigger.exists():
        time.sleep(0.05)

    rows, cols, xpix, ypix = struct.unpack("HHHH", fcntl.ioctl(1, termios.TIOCGWINSZ, b"\0" * 8))
    if not xpix or not ypix:
        raise SystemExit("pane reports no pixel size")
    cell_w, cell_h = xpix // cols, ypix // rows
    img_cols, img_rows = cols, rows - 1
    width, height = img_cols * cell_w, img_rows * cell_h
    green = png(width, height, [bytes(GREEN) * width] * height)

    place_full = f"a=T,f=100,i=1,p=1,q=2,C=1,c={img_cols},r={img_rows}"
    frames: list[bytes] = []
    if args.mode == "png":
        for k in range(args.frames):
            frames.append(b"\x1b[2;1H" + command(place_full, png(width, height, frame_rows(width, height, k))))
    else:
        frames.append(b"\x1b[2;1H" + command(place_full, png(width, height, frame_rows(width, height, 0))))
        strip_rows = max(1, img_rows // 6)
        strip_h = strip_rows * cell_h
        for k in range(1, args.frames):
            top = (k * strip_rows) % (img_rows - strip_rows + 1)
            raw = b"".join(frame_rows(width, strip_h, k))
            keys = f"a=T,f=24,s={width},v={strip_h},o=z,i=2,p=1,q=2,C=1,c={img_cols},r={strip_rows}"
            frames.append(f"\x1b[{top + 2};1H".encode() + command(keys, zlib.compress(raw, 6)))
    final = b"\x1b_Ga=d,d=I,i=2,q=2\x1b\\" + b"\x1b[2;1H" + command(place_full, green)

    out.write(b"\x1b[2J\x1b[H")
    out.flush()
    interval = 1.0 / args.fps
    started = time.time()
    sent = 0
    write_ms = []
    for index, frame in enumerate([*frames, final]):
        target = started + index * interval
        if (delay := target - time.time()) > 0:
            time.sleep(delay)
        before = time.time()
        out.write(frame)
        out.flush()
        write_ms.append((time.time() - before) * 1000)
        sent += len(frame)
    finished = time.time()
    args.log.write_text(json.dumps({
        "mode": args.mode, "frames": args.frames + 1, "fps": args.fps,
        "pane": {"cols": cols, "rows": rows, "xpix": xpix, "ypix": ypix},
        "image": {"width": width, "height": height},
        "first_sent": started, "last_sent": finished, "bytes": sent,
        "frame_bytes": [len(frame) for frame in frames],
        "write_ms": write_ms,
    }))
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    sys.exit(main())
