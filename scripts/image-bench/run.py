#!/usr/bin/env python3
"""Measure the pane image pipeline end to end on a real Kitty/Xvfb surface.

One run: an isolated bkprd hosts a pane running `emitter.py`; a real bkpr
client inside an isolated Kitty on Xvfb displays it, either locally or through
a private loopback OpenSSH endpoint (`--topology remote`). The harness records:

* bytes per leg: emitter -> bkprd (emitter count), bkprd -> client (client
  read accounting), client -> terminal (exact `script` transcript),
* CPU time of every owned mux process during the measured window,
* presentation from Xvfb's memory-mapped framebuffer (`-fbdir`): first image,
  completion (green marker) and blank-image samples, which expose flicker.

Nothing touches live sessions: HOME/XDG, sockets, SSH keys and the X server
are private and every process is owned and reaped. Evidence goes to --out.
"""
from __future__ import annotations

import argparse
import json
import mmap
import os
from pathlib import Path
import pwd
import shlex
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "showcase"))
from runtime import Changes, OwnedProcesses, private_environment, wait_path, write_json  # noqa: E402

BACKGROUND = (0x10, 0x10, 0x10)
GREEN = (0, 200, 0)
GRID = (64, 40)
SESSION = "bench"


def tool(name: str) -> Path:
    found = shutil.which(name)
    if not found:
        raise SystemExit(f"missing tool: {name}")
    return Path(found).resolve()


def request(pair: Path, env: dict, endpoint: Path, family: int, op: str, **fields) -> dict:
    result = subprocess.run([str(pair / "bkpr"), "request", "--socket", str(endpoint),
                             json.dumps({"version": family, "op": op, **fields})],
                            env=env, capture_output=True, timeout=35, check=True)
    value = json.loads(result.stdout)
    if not value.get("ok"):
        raise RuntimeError(f"request {op} failed: {value}")
    return value["result"]


def start_host(owned: OwnedProcesses, pair: Path, env: dict, cwd: Path) -> tuple[subprocess.Popen, Path, int]:
    endpoint = Path(env["XDG_RUNTIME_DIR"]) / "bkpr/d"
    state = Path(env["XDG_STATE_HOME"]) / "bkpr"
    endpoint.parent.mkdir(mode=0o700, exist_ok=True)
    state.mkdir(mode=0o700, exist_ok=True)
    log = owned.evidence / "daemon.log"
    process = owned.start("daemon", [pair / "bkprd", "--bkpr", pair / "bkpr", "--socket", endpoint,
                                     "--state-root", state, "--home", env["HOME"], "--cwd", cwd])
    with Changes([log, endpoint.parent]) as changes:
        def ready():
            if process.poll() is not None:
                raise RuntimeError(f"daemon exited: {log.read_text()}")
            for line in log.read_text().splitlines():
                try:
                    value = json.loads(line)
                except ValueError:
                    continue
                if isinstance(value, dict) and value.get("ready") is True and endpoint.exists():
                    return value["version"]
            return None
        family = changes.until(ready, "daemon readiness")
    return process, endpoint, family


def open_session(pair: Path, env: dict, endpoint: Path, family: int, argv: list, cwd: Path) -> None:
    defaults = {"argv": [str(tool("bash")), "--norc", "--noprofile"], "cwd": str(cwd), "home": env["HOME"],
                "isolated_environment": True}
    opened = request(pair, env, endpoint, family, "session_open", name=SESSION, defaults=defaults, initial=None)
    session = Path(opened["socket"])
    spec = {"argv": list(map(str, argv)), "cwd": str(cwd), "home": env["HOME"], "isolated_environment": False,
            "title": "image bench", "size": {"cols": 120, "rows": 40, "cell_width": 9, "cell_height": 20}}
    request(pair, env, session, family, "workspace_create", name="bench", initial=spec)


def start_ssh(owned: OwnedProcesses, root: Path, remote_env: dict, pair: Path,
              link: tuple[float, float] | None) -> tuple[Path, Path]:
    ssh_root = root / "ssh"
    ssh_root.mkdir(mode=0o700)
    for name in ("host", "client"):
        owned.command([tool("ssh-keygen"), "-q", "-t", "ed25519", "-N", "", "-f", ssh_root / name])
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    user = pwd.getpwuid(os.getuid()).pw_name
    config = ssh_root / "sshd_config"
    config.write_text(f"""ListenAddress 127.0.0.1
Port {port}
HostKey {ssh_root / 'host'}
PidFile {ssh_root / 'pid'}
AuthorizedKeysFile {ssh_root / 'client.pub'}
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin no
AllowUsers {user}
AllowAgentForwarding no
AllowTcpForwarding no
X11Forwarding no
UseDNS no
PrintMotd no
PermitUserRC no
# The login shell starts with the fixture's private HOME/XDG, so host shell
# configuration (which may print to stdout) never reaches the remote stream.
SetEnv HOME={remote_env['HOME']} XDG_CONFIG_HOME={remote_env['XDG_CONFIG_HOME']} XDG_DATA_HOME={remote_env['XDG_DATA_HOME']}
LogLevel VERBOSE
""")
    daemon = owned.start("sshd", [tool("sshd"), "-D", "-e", "-f", config])
    log = owned.evidence / "sshd.log"
    with Changes([log]) as changes:
        def listening():
            if daemon.poll() is not None:
                raise RuntimeError(f"sshd exited: {log.read_text()}")
            return "Server listening on" in log.read_text()
        changes.until(listening, "private sshd")
    public = (ssh_root / "host.pub").read_text().split()
    known = ssh_root / "known_hosts"
    known.write_text(f"[127.0.0.1]:{port} {public[0]} {public[1]}\n")
    client_config = ssh_root / "config"
    proxy = ""
    if link:
        mbit, delay_ms = link
        proxy = f"    ProxyCommand {tool('python3')} {HERE / 'link.py'} --mbit {mbit} --delay-ms {delay_ms} %h %p\n"
    client_config.write_text(f"""Host benchhost
{proxy}    HostName 127.0.0.1
    Port {port}
    User {user}
    IdentityFile {ssh_root / 'client'}
    IdentityAgent none
    IdentitiesOnly yes
    BatchMode yes
    StrictHostKeyChecking yes
    UserKnownHostsFile {known}
    GlobalKnownHostsFile /dev/null
    ClearAllForwardings yes
    SendEnv -*
    LogLevel ERROR
""")
    client_config.chmod(0o600)
    remote_bin = ssh_root / "bkpr-remote"
    environment = [f"{key}={value}" for key, value in remote_env.items()]
    # Keep env's own name: resolving it can reach a multi-call coreutils binary.
    env_tool = shutil.which("env")
    # --norc: bash sources ~/.bashrc for SSH-launched scripts, whose output would corrupt the stream.
    remote_bin.write_text(f"#!{tool('bash')} --norc\nexec {shlex.join([env_tool, '-i', *environment, str(pair / 'bkpr')])} \"$@\"\n")
    remote_bin.chmod(0o700)
    return client_config, remote_bin


class Framebuffer:
    """Xvfb -fbdir screen, read through a shared mapping without screenshots."""

    def __init__(self, path: Path):
        self.file = path.open("rb")
        self.map = mmap.mmap(self.file.fileno(), 0, access=mmap.ACCESS_READ)
        fields = struct.unpack(">25I", self.map[:100])
        header, bpp, self.stride, byte_order, ncolors = fields[0], fields[11], fields[12], fields[7], fields[19]
        if bpp != 32 or byte_order != 0:
            raise RuntimeError(f"unexpected XWD layout bpp={bpp} order={byte_order}")
        self.offset = header + ncolors * 12

    def pixel(self, x: int, y: int) -> tuple[int, int, int]:
        at = self.offset + y * self.stride + x * 4
        b, g, r = self.map[at], self.map[at + 1], self.map[at + 2]
        return r, g, b

    def close(self):
        self.map.close()
        self.file.close()


def accounting(owned: OwnedProcesses) -> dict:
    owned.discover()
    tick = os.sysconf("SC_CLK_TCK")
    result = {}
    for pid in list(owned.births):
        try:
            stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            io = dict(line.split(": ") for line in Path(f"/proc/{pid}/io").read_text().splitlines())
            argv = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        except (OSError, ValueError):
            continue
        name = Path(argv[0].decode(errors="replace")).name
        if len(argv) > 1 and name in ("python3", "python3.13", "bash", "env"):
            name = Path(argv[1].decode(errors="replace")).name
        result[str(pid)] = {"name": name, "cpu_ms": (int(stat[11]) + int(stat[12])) * 1000 / tick,
                            "rchar": int(io["rchar"]), "wchar": int(io["wchar"])}
    return result


def delta(before: dict, after: dict) -> dict:
    totals: dict[str, dict] = {}
    for pid, end in after.items():
        start = before.get(pid, {"cpu_ms": 0, "rchar": 0, "wchar": 0})
        entry = totals.setdefault(end["name"], {"cpu_ms": 0.0, "rchar": 0, "wchar": 0, "processes": 0})
        entry["processes"] += 1
        for key in ("cpu_ms", "rchar", "wchar"):
            entry[key] += end[key] - start[key]
    return totals


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pair", type=Path, required=True, help="directory holding matching bkpr and bkprd")
    parser.add_argument("--topology", choices=("local", "remote", "ssh-tty"), default="local",
                        help="remote: bkpr remote mode over SSH; ssh-tty: client on the host, terminal over SSH")
    parser.add_argument("--link", help="shape SSH as MBIT,DELAY_MS (one-way delay per direction)")
    parser.add_argument("--mode", choices=("png", "strips"), default="png")
    parser.add_argument("--frames", type=int, default=60)
    parser.add_argument("--fps", type=float, default=15)
    parser.add_argument("--columns", type=int, default=180)
    parser.add_argument("--rows", type=int, default=52)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    pair = args.pair.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    evidence = args.out.resolve()

    root = Path(tempfile.mkdtemp(prefix="bkib.", dir="/tmp"))
    root.chmod(0o700)
    base = {key: os.environ[key] for key in ("PATH", "LANG", "LOCALE_ARCHIVE", "FONTCONFIG_FILE") if key in os.environ}
    # Never let fixture processes find a live session's harness wrappers.
    base["PATH"] = os.pathsep.join(entry for entry in base["PATH"].split(os.pathsep) if "/bkpr/sessions/" not in entry)
    host_root, client_root = root / "host", root / "client"
    host_root.mkdir()
    client_root.mkdir()
    host_env = private_environment(host_root, base)
    client_env = private_environment(client_root, base) if args.topology != "local" else dict(host_env)
    link = tuple(map(float, args.link.split(","))) if args.link else None
    owned = OwnedProcesses(host_env, evidence)
    result: dict = {"args": {key: str(value) for key, value in vars(args).items()}}
    framebuffer = None
    try:
        fbdir = root / "fb"
        fbdir.mkdir()
        reader, writer = os.pipe()
        try:
            owned.start("xvfb", [tool("Xvfb"), "-displayfd", writer, "-noreset", "-nolisten", "tcp",
                                 "-screen", "0", "2400x1500x24", "-fbdir", fbdir, "-ac"], pass_fds=(writer,))
        finally:
            os.close(writer)
        display = ":" + os.read(reader, 64).decode().strip()
        os.close(reader)
        client_env["DISPLAY"] = display

        trigger, emitter_log = root / "trigger", root / "emitter.json"
        _, endpoint, family = start_host(owned, pair, host_env, host_root)
        emitter = [tool("python3"), HERE / "emitter.py", "--mode", args.mode, "--frames", args.frames,
                   "--fps", args.fps, "--trigger", trigger, "--log", emitter_log]
        open_session(pair, host_env, endpoint, family, emitter, host_root)

        if args.topology == "remote":
            ssh_config, remote_bin = start_ssh(owned, root, host_env, pair, link)
            client = [pair / "bkpr", "--session", f"benchhost:{SESSION}", "--ssh-config", ssh_config,
                      "--remote-bkpr-bin", remote_bin, "--clipboard", "deny"]
        elif args.topology == "ssh-tty":
            ssh_config, remote_bin = start_ssh(owned, root, host_env, pair, link)
            client = [tool("ssh"), "-tt", "-F", ssh_config, "benchhost", remote_bin, "--bkprd", pair / "bkprd",
                      "--session", SESSION, "--clipboard", "deny"]
        else:
            client = [pair / "bkpr", "--bkprd", pair / "bkprd", "--session", SESSION, "--clipboard", "deny"]

        transcript = root / "terminal.raw"
        wrapped = [tool("script"), "--quiet", "--return", "--flush", "--echo", "never",
                   "--log-out", transcript, "--command", shlex.join(map(str, client))]
        terminal_socket = root / "kitty.sock"
        options = {"background": "#101010", "foreground": "#d0d0d0", "allow_remote_control": "socket-only",
                   "confirm_os_window_close": "0", "enable_audio_bell": "no", "remember_window_size": "no",
                   "window_padding_width": "0", "initial_window_width": f"{args.columns}c",
                   "initial_window_height": f"{args.rows}c", "font_size": "11", "cursor_blink_interval": "0",
                   "hide_window_decorations": "yes", "sync_to_monitor": "no"}
        owned.env = client_env
        kitty = owned.start("kitty", [tool("kitty"), "--config", "NONE", "--listen-on", f"unix:{terminal_socket}",
                                      *(item for key, value in options.items() for item in ("-o", f"{key}={value}")),
                                      *wrapped], cwd=client_root)
        wait_path(terminal_socket)
        wait_path(transcript)

        def screen_text() -> str:
            return owned.command([tool("kitty"), "@", "--to", f"unix:{terminal_socket}", "get-text"]).stdout.decode()

        with Changes([transcript]) as changes:
            def attached():
                if kitty.poll() is not None:
                    raise RuntimeError("kitty exited before attach")
                return "waiting for trigger" in screen_text()
            try:
                changes.until(attached, "client attached and pane visible", 60)
            except TimeoutError:
                (evidence / "attach-timeout-screen.txt").write_text(screen_text())
                shutil.copy(transcript, evidence / "attach-timeout-terminal.raw")
                raise

        window = json.loads(owned.command([tool("kitty"), "@", "--to", f"unix:{terminal_socket}", "ls"]).stdout)[0]["platform_window_id"]
        geometry = dict(line.split("=", 1) for line in owned.command(
            [tool("xdotool"), "getwindowgeometry", "--shell", window]).stdout.decode().splitlines() if "=" in line)
        x0, y0, width, height = (int(geometry[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))
        points = [(x0 + (i + 0.5) * width / GRID[0], y0 + (j + 0.5) * height / GRID[1])
                  for j in range(GRID[1]) for i in range(GRID[0])]
        points = [(int(x), int(y)) for x, y in points]
        framebuffer = Framebuffer(fbdir / "Xvfb_screen0")

        before = accounting(owned)
        transcript_before = transcript.stat().st_size
        trigger.touch()
        samples = []
        deadline = time.time() + 30 + args.frames / args.fps * 4
        green_since = None
        while time.time() < deadline:
            now = time.time()
            bg = green = 0
            for index, (x, y) in enumerate(points):
                colour = framebuffer.pixel(x, y)
                if colour == BACKGROUND:
                    bg |= 1 << index
                elif colour == GREEN:
                    green |= 1 << index
            samples.append((now, bg, green))
            if green.bit_count() > len(points) // 4:
                green_since = green_since or now
                if now - green_since > 1.0:
                    break
        after = accounting(owned)
        transcript_after = transcript.stat().st_size

        emitted = json.loads(emitter_log.read_text())
        final_green = samples[-1][2]
        image_points = final_green
        count = image_points.bit_count()
        if count == 0:
            raise RuntimeError("completion marker never presented")
        first_image = next((t for t, bg, _ in samples if t >= emitted["first_sent"] and bg & image_points == 0), None)
        complete = next((t for t, _, green in samples if (green & image_points).bit_count() >= count * 0.95), None)
        measured = [(t, bg) for t, bg, _ in samples if first_image and t >= first_image and (not complete or t < complete)]
        partial = [t for t, bg in measured if bg & image_points]
        full = [t for t, bg in measured if bg & image_points == image_points]
        episodes, previous = [], False
        for t, bg in measured:
            blank = bool(bg & image_points)
            if blank and not previous:
                episodes.append([t, t])
            elif blank:
                episodes[-1][1] = t
            previous = blank
        intervals = [b - a for (a, _), (b, _) in zip(measured, measured[1:])]
        timeline = []
        for t, bg, green in samples:
            slot = int((t - emitted["first_sent"]) * 10)
            if slot < 0:
                continue
            while len(timeline) <= slot:
                timeline.append([0, 0])
            timeline[slot][0] += (bg & image_points).bit_count() / count
            timeline[slot][1] += 1
        result.update({
            "timeline_blank_fraction_100ms": [round(total / n, 2) if n else None for total, n in timeline],
            "emitter": emitted,
            "sampling": {"samples": len(measured), "points": count,
                         "median_interval_ms": sorted(intervals)[len(intervals) // 2] * 1000 if intervals else None},
            "presentation": {
                "first_image_after_first_send_ms": (first_image - emitted["first_sent"]) * 1000 if first_image else None,
                "complete_after_last_send_ms": (complete - emitted["last_sent"]) * 1000 if complete else None,
                "complete_after_first_send_ms": (complete - emitted["first_sent"]) * 1000 if complete else None,
                "blank_samples_any": len(partial), "blank_samples_full": len(full),
                "blank_episodes": len(episodes),
                "blank_episode_ms": sorted(round((b - a) * 1000, 1) for a, b in episodes),
            },
            "bytes": {"emitter_to_bkprd": emitted["bytes"], "client_to_terminal": transcript_after - transcript_before},
            "processes": delta(before, after),
        })
        write_json(evidence / "result.json", result)
        print(json.dumps({k: result[k] for k in ("presentation", "bytes", "processes", "sampling")}, indent=1))
    finally:
        if framebuffer:
            framebuffer.close()
        owned.close()
        shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
