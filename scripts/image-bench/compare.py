#!/usr/bin/env python3
"""Tabulate image-bench runs per configuration across labels.

Usage: compare.py LABEL [LABEL...]   (reads target/image-bench/runs/<label>-<config>-1)
"""
import json
import statistics
import sys
from pathlib import Path

RUNS = Path("target/image-bench/runs")
labels = sys.argv[1:]
configs = sorted({p.name[len(labels[0]) + 1:-2] for p in RUNS.glob(f"{labels[0]}-*-1") if p.is_dir()})


def metrics(path: Path):
    try:
        r = json.loads((path / "result.json").read_text())
    except FileNotFoundError:
        return None
    p, procs = r["presentation"], r["processes"]
    timeline = [v for v in r.get("timeline_blank_fraction_100ms", []) if v is not None]
    episodes = p["blank_episode_ms"]
    return {
        "blank%": round(100 * sum(timeline) / max(len(timeline), 1)),
        "repl ms": round(statistics.median(episodes)) if episodes else 0,
        "final ms": round(p["complete_after_last_send_ms"] or -1),
        "term MB": round(r["bytes"]["client_to_terminal"] / 1e6, 1),
        "link MB": round(sum(v["rchar"] for k, v in procs.items() if k == "ssh") / 1e6, 1),
        "bkprd s": round(procs.get("bkprd", {}).get("cpu_ms", 0) / 1000, 2),
        "client s": round(sum(v["cpu_ms"] for k, v in procs.items() if k == "bkpr") / 1000, 2),
        "kitty s": round(procs.get("kitty", {}).get("cpu_ms", 0) / 1000, 2),
    }


columns = ["blank%", "repl ms", "final ms", "term MB", "link MB", "bkprd s", "client s", "kitty s"]
print(f"{'config':28} {'build':10} " + " ".join(f"{c:>9}" for c in columns))
for config in configs:
    for label in labels:
        m = metrics(RUNS / f"{label}-{config}-1")
        row = " ".join(f"{m[c]:>9}" for c in columns) if m else "   (missing)"
        print(f"{config:28} {label:10} {row}")
