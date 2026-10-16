"""Launch the three-file release in a private terminal without Nix or user sessions."""

import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

bundle = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix="b-", dir="/tmp") as home:
    runtime = Path(home) / "r"
    runtime.mkdir()
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("BKPR_", "ZELLIJ", "NIX_", "GH_", "GITHUB_", "SSH_"))
    }
    env.update(
        HOME=home,
        XDG_RUNTIME_DIR=str(runtime),
        XDG_STATE_HOME=str(Path(home) / "state"),
        TERM="xterm-256color",
        PATH="/usr/bin:/bin",
    )
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
    child = subprocess.Popen(
        [str(bundle / "bkpr"), "--session", "portable-smoke"],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        start_new_session=True,
    )
    os.close(slave)
    socket_dir = runtime / "bkpr" / "z"
    output = bytearray()
    try:
        started = False
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.2)[0]:
                try:
                    output.extend(os.read(master, 65536))
                except OSError as error:
                    raise RuntimeError(f"portable terminal closed: {output[-2000:]!r}") from error
            sockets = socket_dir / "contract_version_1"
            if sockets.is_dir() and any(sockets.iterdir()) and child.poll() is None:
                started = True
                print("portable Bkpr started a private Zellij session")
                break
            if child.poll() is not None:
                break
        if not started:
            raise RuntimeError(f"portable Bkpr failed to start: {output[-2000:]!r}")
    finally:
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        try:
            subprocess.run(
                [str(bundle / "zellij"), "kill-all-sessions", "--yes"],
                env={**env, "ZELLIJ_SOCKET_DIR": str(socket_dir)},
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=5,
                check=False,
            )
        finally:
            os.close(master)
