#!/usr/bin/env python3
"""Per-frame delay from emitter send to the client's terminal upload, on one clock.

Usage: latency.py RUN_DIR   (needs --keep-transcript)
Matches full-pane uploads (s=WIDTH,v=HEIGHT) in the transcript to emitter frames
in order, timing each upload by when the transcript file grew past it.
"""
import bisect
import json
import re
import sys
from pathlib import Path

run = Path(sys.argv[1])
result = json.loads((run / "result.json").read_text())
emitted = result["emitter"]
width, height = emitted["image"]["width"], emitted["image"]["height"]
sizes = json.loads((run / "transcript-sizes.json").read_text())
times, ends = [t for t, _ in sizes], [size for _, size in sizes]
data = (run / "terminal.raw").read_bytes()


def written_at(offset: int) -> float | None:
    index = bisect.bisect_right(ends, offset)
    return times[index] if index < len(times) else None


pattern = re.compile(rb"\x1b_Ga=t,f=\d+(?:,o=z)?,s=%d,v=%d," % (width, height))
uploads = [written_at(match.start()) for match in pattern.finditer(data)]
sent = emitted["sent_at"]
print(f"{len(sent)} frames sent, {len(uploads)} full-pane uploads")
for index, upload in enumerate(uploads):
    # Each upload carries the newest frame sent before it.
    frame = bisect.bisect_right(sent, upload) - 1 if upload else None
    delay = (upload - sent[frame]) * 1000 if upload and frame is not None and frame >= 0 else None
    print(f"upload {index}: frame {frame}, {delay:.0f} ms after that frame was sent" if delay is not None
          else f"upload {index}: untimed")
