#!/usr/bin/env python3
"""Shaped TCP link for SSH ProxyCommand: one-way delay plus a token-bucket rate per direction.

Usage (ssh_config): ProxyCommand python3 link.py --mbit 50 --delay-ms 20 127.0.0.1 %p

Bytes keep their order and are never dropped; each chunk becomes deliverable
`delay` after it was read and is released no faster than the configured rate.
This models a bandwidth-limited, latency-bearing path for comparisons between
builds, not a faithful network simulation (no loss, jitter or TCP dynamics).
"""
from __future__ import annotations

import argparse
import collections
import os
import selectors
import socket
import sys
import time

CHUNK = 64 * 1024


class Direction:
    def __init__(self, source, sink, rate: float, delay: float):
        self.source, self.sink, self.rate, self.delay = source, sink, rate, delay
        self.queue: collections.deque[tuple[float, bytes]] = collections.deque()
        self.budget_at = time.monotonic()  # time when the link becomes free again
        self.open = True

    def read(self) -> None:
        data = self.source() if callable(self.source) else b""
        if not data:
            self.open = False
            return
        self.queue.append((time.monotonic() + self.delay, data))

    def flush(self) -> float | None:
        """Write what is due; return the next deadline, if any."""
        now = time.monotonic()
        while self.queue:
            due, data = self.queue[0]
            start = max(due, self.budget_at)
            if start > now:
                return start
            self.budget_at = start + len(data) / self.rate
            self.sink(data)
            self.queue.popleft()
            now = time.monotonic()
        return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mbit", type=float, required=True)
    parser.add_argument("--delay-ms", type=float, required=True)
    parser.add_argument("host")
    parser.add_argument("port", type=int)
    args = parser.parse_args()
    rate = args.mbit * 1_000_000 / 8
    delay = args.delay_ms / 1000
    upstream = socket.create_connection((args.host, args.port))
    upstream.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    stdin, stdout = sys.stdin.fileno(), sys.stdout.fileno()
    os.set_blocking(stdin, False)

    def write_all(fd_write, data: bytes) -> None:
        view = memoryview(data)
        while view:
            try:
                view = view[fd_write(view):]
            except BlockingIOError:
                selectors.select.select([], [stdout], [], 1)

    outbound = Direction(lambda: os.read(stdin, CHUNK), lambda data: upstream.sendall(data), rate, delay)
    inbound = Direction(lambda: upstream.recv(CHUNK), lambda data: write_all(lambda v: os.write(stdout, v), data),
                        rate, delay)
    selector = selectors.DefaultSelector()
    selector.register(stdin, selectors.EVENT_READ, outbound)
    selector.register(upstream, selectors.EVENT_READ, inbound)
    while outbound.open or inbound.open or outbound.queue or inbound.queue:
        deadlines = [d for d in (outbound.flush(), inbound.flush()) if d is not None]
        if not (outbound.open or inbound.open) and not deadlines:
            break
        timeout = max(0.0, min(deadlines) - time.monotonic()) if deadlines else None
        for key, _ in selector.select(timeout):
            direction = key.data
            direction.read()
            if not direction.open:
                selector.unregister(key.fileobj)
                if direction is outbound:
                    upstream.shutdown(socket.SHUT_WR)
        if not inbound.open and not inbound.queue:
            break
    return 0


if __name__ == "__main__":
    sys.exit(main())
