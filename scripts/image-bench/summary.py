#!/usr/bin/env python3
"""Print a compact comparison of image-bench result directories."""
import json
import sys
from pathlib import Path

for directory in map(Path, sys.argv[1:]):
    result = json.loads((directory / "result.json").read_text())
    presentation, emitted, processes = result["presentation"], result["emitter"], result["processes"]
    sizes = sorted(emitted["frame_bytes"])
    print(f"== {directory.name}: image {emitted['image']['width']}x{emitted['image']['height']}, "
          f"frame median {sizes[len(sizes) // 2] // 1024} KiB")
    print(f"   bytes MB: app->bkprd {result['bytes']['emitter_to_bkprd'] / 1e6:.1f}, "
          f"client->terminal {result['bytes']['client_to_terminal'] / 1e6:.1f}")
    print(f"   final frame shown {presentation['complete_after_last_send_ms']:.0f} ms after send; "
          f"blank episodes {presentation['blank_episodes']}, ms {presentation['blank_episode_ms'][-6:]}")
    busy = {name: f"{p['cpu_ms']:.0f}ms r{p['rchar'] / 1e6:.1f}MB w{p['wchar'] / 1e6:.1f}MB"
            for name, p in processes.items() if p["cpu_ms"] or p["rchar"] > 1e5}
    print(f"   processes: {busy}")
    timeline = result.get("timeline_blank_fraction_100ms", [])
    blank = sum(value for value in timeline if value is not None)
    print(f"   blank time over run: {blank / max(len(timeline), 1):.0%}")
