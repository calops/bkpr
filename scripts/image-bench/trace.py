#!/usr/bin/env python3
"""Timeline of Kitty graphics commands the client sent, from a kept transcript.

Usage: trace.py RUN_DIR   (needs --keep-transcript)
Prints each non-continuation graphics command with its time offset, plus the
sync-update brackets around it, so replacement ordering can be inspected.
"""
import re
import sys
from pathlib import Path

run = Path(sys.argv[1])
data = (run / "terminal.raw").read_bytes()
# script's log-out starts with a header line; timing entries are "delay bytes".
header = data.index(b"\n") + 1 if data.startswith(b"Script started") else 0
times, offset, clock = [], header, 0.0
for line in (run / "terminal.timing").read_text().splitlines():
    parts = line.split()
    if len(parts) != 2:
        continue
    clock += float(parts[0])
    times.append((offset, clock))
    offset += int(parts[1])


def at(position: int) -> float:
    low, high = 0, len(times) - 1
    while low < high:
        mid = (low + high + 1) // 2
        if times[mid][0] <= position:
            low = mid
        else:
            high = mid - 1
    return times[low][1]


events = []
for match in re.finditer(rb"\x1b_G([^;\x1b]*)(?:;[^\x1b]*)?\x1b\\|\x1b\[\?2026([hl])", data):
    if match.group(2):
        events.append((at(match.start()), "SYNC " + ("begin" if match.group(2) == b"h" else "end")))
        continue
    keys = match.group(1).decode()
    if keys.startswith("m="):
        continue
    events.append((at(match.start()), keys))
start = events[0][0] if events else 0
last = None
for time, label in events:
    if label == last and label.startswith("SYNC"):
        continue
    print(f"{(time - start) * 1000:9.1f} ms  {label}")
    last = label
