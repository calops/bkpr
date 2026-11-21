#!/usr/bin/env python3
"""Smoke the installed macOS package on a fresh runner, as the runner user."""

import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid


BIN = Path("/usr/local/bin")
RUNTIME = Path("/usr/local/lib/bkpr")
CLI = BIN / "bkpr"
TERMINFO = RUNTIME / "share/terminfo"


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(argv, *, env, cwd):
    result = subprocess.run(
        argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, capture_output=True,
        text=True, timeout=15,
    )
    check(result.returncode == 0, f"{argv}: exit {result.returncode}: {result.stderr}")
    return result.stdout.strip()


def controlling_terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def smoke(root):
    env = {"PATH": "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin", "TERM": "xterm-256color",
           "SHELL": "/bin/bash", "LC_ALL": "C"}
    for name, leaf in (("HOME", "home"), ("TMPDIR", "tmp"), ("XDG_RUNTIME_DIR", "r"),
                       ("XDG_STATE_HOME", "state"), ("XDG_CONFIG_HOME", "config"),
                       ("XDG_CACHE_HOME", "cache"), ("XDG_DATA_HOME", "data")):
        path = root / leaf
        path.mkdir(mode=0o700)
        env[name] = str(path)

    def command(*argv):
        return run(argv, env=env, cwd=env["HOME"])

    versions = []
    for name in ("bkpr", "bkprd"):
        link = BIN / name
        executable = RUNTIME / "bin" / name
        check(link.is_symlink() and link.readlink() == Path(f"../lib/bkpr/bin/{name}"),
              f"{link} is not the expected relative symlink")
        check(executable.is_file() and not executable.is_symlink() and os.access(executable, os.X_OK),
              f"{executable} is not a real executable")
        program, _, version = command(link, "--version").partition(" ")
        check(program == name and version, f"Unexpected version output for {link}: {program} {version}")
        versions.append(version)
    check(versions[0] == versions[1], f"Installed version mismatch: {versions}")
    check(any((TERMINFO / entry).is_file() for entry in ("b/bkpr", "62/bkpr")),
          f"No compiled bkpr entry in {TERMINFO}")

    socket = Path(env["XDG_RUNTIME_DIR"]) / "bkpr/d"
    daemon_prefixes = tuple(f"{path} --socket {socket} " for path in
                            (BIN / "bkprd", RUNTIME / "bin/bkprd"))
    nonce = uuid.uuid4().hex[:16]
    shell = (
        "printf 'READY-%s\\n' \"$1\"; "
        "read -r -t 90 probe && [ \"$probe\" = probe ] || exit 1; "
        "printf 'RESULT-%s=%s;\\n' \"$1\" \"$TERMINFO_DIRS\"; "
        "read -r -t 90 unused"
    )

    def owned_processes():
        daemons, panes = [], []
        for line in command("/bin/ps", "-axww", "-o", "pid=", "-o", "command=").splitlines():
            fields = line.split(None, 1)
            if len(fields) != 2:
                continue
            pid, argv = fields
            if argv.startswith(daemon_prefixes):
                daemons.append(int(pid))
            if argv.startswith("/bin/bash --noprofile --norc -c ") and argv.endswith(f" bkpr-smoke {nonce}"):
                panes.append(int(pid))
        return daemons, panes

    def wait_for_shutdown():
        deadline = time.monotonic() + 5
        while True:
            daemons, panes = owned_processes()
            if not daemons and not panes and not socket.exists():
                return
            check(time.monotonic() < deadline,
                  f"Shutdown left daemon PIDs {daemons}, pane PIDs {panes}, socket present={socket.exists()}")
            time.sleep(0.05)

    output = bytearray()

    def expect(master, client, marker):
        deadline = time.monotonic() + 25
        while marker not in output:
            check(client.poll() is None, f"Terminal client exited: {client.returncode}")
            remaining = deadline - time.monotonic()
            check(remaining > 0 and select.select([master], [], [], remaining)[0],
                  f"Timed out waiting for {marker!r} in terminal output")
            try:
                chunk = os.read(master, 65536)
            except BlockingIOError:
                continue
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                chunk = b""
            check(chunk, f"Terminal closed before rendering {marker!r}")
            output.extend(chunk)
            del output[:-262144]

    check(not socket.exists(), f"Unexpected pre-existing socket: {socket}")
    master, slave = pty.openpty()
    client = None
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 140, 1260, 648))
        os.set_blocking(master, False)
        client = subprocess.Popen(
            [CLI, "--home", env["HOME"], "--clipboard", "deny", "--", "/bin/bash",
             "--noprofile", "--norc", "-c", shell, "bkpr-smoke", nonce],
            env=env, cwd=env["HOME"], stdin=slave, stdout=slave, stderr=slave,
            preexec_fn=controlling_terminal,
        )
        os.close(slave)
        slave = None
        expect(master, client, f"READY-{nonce}".encode())
        os.write(master, b"probe\r")
        expect(master, client, f"RESULT-{nonce}=:{TERMINFO};".encode())
        info = json.loads(command(CLI, "daemon", "info", "--socket", socket))["daemon"]
        daemons, panes = owned_processes()
        check(info["daemon_id"] and daemons == [info["pid"]] and len(panes) == 1,
              f"{socket} did not identify the installed daemon and its fixture: {info}, {daemons}, {panes}")
    except Exception:
        print(f"Terminal output tail: {bytes(output[-4096:])!r}", file=sys.stderr)
        log = Path(env["XDG_STATE_HOME"]) / "bkpr/bkprd.log"
        if log.is_file():
            with log.open("rb") as stream:
                stream.seek(max(0, log.stat().st_size - 4096))
                print(f"Daemon log tail: {stream.read(4096)!r}", file=sys.stderr)
        raise
    finally:
        if slave is not None:
            os.close(slave)
        os.close(master)
        if client is not None:
            if client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait(timeout=3)
            try:
                if socket.is_socket():
                    command(CLI, "daemon", "stop", "--socket", socket)
                wait_for_shutdown()
            except Exception:
                # No shutdown replay: only signal processes bearing this private fixture's argv.
                for pid in sum(owned_processes(), []):
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                raise

    print(f"Installed bkpr and bkprd {versions[0]}: PTY input/output, pane terminfo, daemon startup and shutdown OK")


if __name__ == "__main__":
    # /tmp is a symlink on macOS; the session endpoint also appends a long name hash.
    with tempfile.TemporaryDirectory(prefix="b", dir="/private/tmp") as directory:
        smoke(Path(directory))
