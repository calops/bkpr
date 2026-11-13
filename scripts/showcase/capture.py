#!/usr/bin/env python3
"""Capture a scripted demo with real native UIs, broker communication and loopback SSH."""
from __future__ import annotations

import argparse
import contextlib
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True

from frame import physicalize
from provider import ScriptedProvider, launch_arguments, native_harness, prepare_profiles, sanitize
from runtime import (
    Changes, OwnedProcesses, digest, enter_namespace, native, private_environment,
    start_display, supervise_namespace, wait_path, write_json,
)


HERE = Path(__file__).resolve().parent
SESSION = "workbench"
REMOTE_SESSION = "forge"
COLUMNS, ROWS = 156, 49
PALETTE = {
    "background": "#1e1e2e", "foreground": "#cdd6f4", "cursor": "#f5e0dc",
    "selection_background": "#45475a", "selection_foreground": "#cdd6f4",
    "color0": "#45475a", "color1": "#f38ba8", "color2": "#a6e3a1", "color3": "#f9e2af",
    "color4": "#89b4fa", "color5": "#f5c2e7", "color6": "#94e2d5", "color7": "#bac2de",
    "color8": "#585b70", "color9": "#f38ba8", "color10": "#a6e3a1", "color11": "#f9e2af",
    "color12": "#89b4fa", "color13": "#f5c2e7", "color14": "#94e2d5", "color15": "#a6adc8",
}


def objects(value):
    if isinstance(value, str):
        with contextlib.suppress(ValueError):
            yield from objects(json.loads(value))
    elif isinstance(value, dict):
        yield value
        for item in value.values():
            yield from objects(item)
    elif isinstance(value, list):
        for item in value:
            yield from objects(item)


def field(value, name):
    found = [item[name] for item in objects(value) if name in item]
    if not found:
        raise RuntimeError(f"Real native tool result has no {name}: {value!r}")
    return found[0]


def ensure_success(value):
    if any(item.get("isError") is True or item.get("ok") is False for item in objects(value)):
        raise RuntimeError(f"Native MCP tool failed: {value!r}")
    return value


def graphics_readiness(output: bytes, device_input: bytes) -> dict | None:
    uploaded, placements = set(), {}
    pending = None
    last_placement = 0
    for match in re.finditer(rb"(?:\x1b\[(\d+);(\d+)H)?\x1b_G([^\x1b]*)\x1b\\", output):
        header = match[3].split(b";", 1)[0]
        fields = dict(part.split(b"=", 1) for part in header.split(b",") if b"=" in part)
        action, identifier = fields.get(b"a"), fields.get(b"i")
        if action == b"t":
            pending = identifier
        if fields.get(b"m") == b"0" and pending:
            uploaded.add(pending)
            pending = None
        if action == b"d" and identifier:
            if fields.get(b"d") == b"I":
                uploaded.discard(identifier)
            placements = {key: value for key, value in placements.items() if key[0] != identifier}
        if action == b"p" and identifier and match[1] and match[2]:
            placements[identifier, fields.get(b"p")] = (int(match[1]), int(match[2]))
            last_placement = match.end()
    replies = re.findall(rb"\x1b_G[^;\x1b]*;([^\x1b]*)\x1b\\", device_input)
    if any(reply != b"OK" for reply in replies):
        raise RuntimeError(f"Real host rejected graphics: {replies!r}")
    active = [position for (identifier, _), position in placements.items() if identifier in uploaded]
    sidebar = [position for position in active if position[1] <= 38]
    panes = [position for position in active if position[1] > 38]
    if not sidebar or not panes or b"OK" not in replies or output.rfind(b"\x1b[?2026l") < last_placement:
        return None
    return {"uploaded_images": len(uploaded), "sidebar_placements": len(sidebar),
            "pane_placements": len(panes), "host_graphics_query_accepted": True,
            "complete_synchronized_frame": True}


class Session:
    def __init__(self, name: str, env: dict[str, str], pair: Path, owned: OwnedProcesses, root: Path):
        self.name, self.env, self.pair, self.owned, self.root = name, env, pair, owned, root
        key = hashlib.sha256(name.encode()).hexdigest()
        self.endpoint = Path(env["XDG_RUNTIME_DIR"]) / "bkpr" / key / "s"
        self.state = Path(env["XDG_STATE_HOME"]) / "bkpr/sessions" / key
        for directory in (
            Path(env["XDG_RUNTIME_DIR"]) / "bkpr", self.endpoint.parent,
            Path(env["XDG_STATE_HOME"]) / "bkpr", self.state.parent, self.state,
        ):
            directory.mkdir(mode=0o700, exist_ok=True)
        self.process = None
        self.family = None

    def start(self):
        argv = [self.pair / "bkprd", "--bkpr", self.pair / "bkpr", "--session", self.name,
                "--socket", self.endpoint, "--state-root", self.state, "--home", self.env["HOME"],
                "--cwd", self.root]
        with (self.owned.evidence / f"daemon-{self.name}.log").open("wb") as log:
            self.process = subprocess.Popen(list(map(str, argv)), env=self.env,
                                            start_new_session=True, stdout=log, stderr=subprocess.STDOUT)
        self.owned.children.append((f"daemon-{self.name}", self.process))
        self.owned.discover()
        log = self.owned.evidence / f"daemon-{self.name}.log"
        with Changes([log, self.endpoint.parent]) as changes:
            def ready():
                if self.process.poll() is not None:
                    raise RuntimeError(f"Owned daemon exited before readiness: {log.read_text()}")
                for line in log.read_text().splitlines():
                    with contextlib.suppress(ValueError):
                        value = json.loads(line)
                        if isinstance(value, dict) and value.get("ready") is True:
                            version = value.get("version")
                            if type(version) is not int or version <= 0:
                                raise RuntimeError("Owned daemon readiness has no valid protocol version")
                            return version if self.endpoint.exists() else None
                return None
            self.family = changes.until(ready, f"Owned daemon {self.name} protocol readiness")
        self.request("session_info")

    def request(self, op: str, **fields):
        result = subprocess.run([str(self.pair / "bkpr"), "request", "--socket", str(self.endpoint),
                                 json.dumps({"version": self.family, "op": op, **fields})],
                                env=self.env, capture_output=True, timeout=35, check=True)
        if len(result.stdout) > 8 * 1024 * 1024:
            raise RuntimeError("Native request exceeded evidence bound")
        value = json.loads(result.stdout)
        if not value.get("ok"):
            raise RuntimeError(f"Native request {op} failed: {value}")
        return value["result"]

    def registry(self):
        return self.request("inspect")["registry"]

    def wait(self, predicate, label: str, timeout: float = 90):
        deadline = time.monotonic() + timeout
        while True:
            snapshot = self.registry()
            if result := predicate(snapshot):
                return result
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(label)
            self.request("wait", after=snapshot["revision"], wait_ms=min(int(remaining * 1000), 30000))

    def stop(self):
        if self.process and self.process.poll() is None:
            self.request("shutdown")
            code = self.process.wait(timeout=30)
            if code:
                raise RuntimeError(f"Owned session {self.name} exited {code}")
            return {"session": self.name, "returncode": code, "normal_shutdown": True}
        return {"session": self.name, "returncode": self.process.poll() if self.process else None,
                "normal_shutdown": False}


def spawn_spec(argv: list, cwd: Path, env: dict[str, str], title: str) -> dict:
    return {"argv": list(map(str, argv)), "cwd": str(cwd), "home": env["HOME"],
            "isolated_environment": False, "title": title,
            "size": {"cols": 68, "rows": 32, "cell_width": 9, "cell_height": 20}}


def repository(owned: OwnedProcesses, root: Path, branch: str) -> None:
    root.mkdir(mode=0o700)
    (root / "src").mkdir()
    (root / "tests").mkdir()
    (root / "src/index.rs").write_text('pub fn search(query: &str) -> Vec<&str> {\n    query.split_whitespace().collect()\n}\n')
    (root / "tests/search.txt").write_text("empty query\nprefix match\nUnicode tokens\n")
    owned.command(["git", "init", "--quiet", "--initial-branch", branch, root])
    owned.command(["git", "-C", root, "add", "."])
    owned.command(["git", "-C", root, "-c", "commit.gpgsign=false", "-c", "user.name=Showcase",
                   "-c", "user.email=showcase@localhost", "commit", "--quiet", "-m", "Fixture search index"])
    (root / "src/index.rs").write_text('pub fn search(query: &str) -> Vec<&str> {\n    query.split_whitespace()\n        .filter(|token| !token.is_empty())\n        .collect()\n}\n')
    (root / "tests/unicode.txt").write_text("café\n東京\n")


def start_ssh(owned: OwnedProcesses, tools: dict, root: Path, remote_env: dict, pair: Path) -> tuple[Path, Path, dict]:
    ssh_root = root / "ssh"
    ssh_root.mkdir(mode=0o700)
    for name in ("host", "client"):
        owned.command([tools["ssh-keygen"], "-q", "-t", "ed25519", "-N", "", "-f", ssh_root / name])
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
StrictModes yes
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
LogLevel VERBOSE
""")
    daemon = owned.start("sshd", [tools["sshd"], "-D", "-e", "-f", config])
    log = owned.evidence / "sshd.log"
    with Changes([log]) as changes:
        def listening():
            if daemon.poll() is not None:
                raise RuntimeError(f"Private sshd exited: {log.read_text()}")
            return "Server listening on" in log.read_text()
        changes.until(listening, "Real private SSH listener")
    public = (ssh_root / "host.pub").read_text().split()
    known = ssh_root / "known_hosts"
    known.write_text(f"[127.0.0.1]:{port} {public[0]} {public[1]}\n")
    known.chmod(0o600)
    client_config = ssh_root / "config"
    client_config.write_text(f"""Host forgebox
    HostName 127.0.0.1
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
    remote_bin.write_text(f"#!{tools['bash']}\nexec {shlex.join([str(tools['env']), '-i', *(f'{key}={value}' for key, value in remote_env.items()), str(pair / 'bkpr')])} \"$@\"\n")
    remote_bin.chmod(0o700)
    return client_config, remote_bin, {"transport": "real OpenSSH loopback", "alias": "forgebox",
                                     "port": port, "host_key_sha256": digest(ssh_root / "host.pub"),
                                     "agent_forwarding": False, "password_authentication": False}


def broker_conversation(provider: ScriptedProvider) -> dict:
    def tool(role, name, arguments=None, occurrence=1):
        provider.tool(role, name, arguments or {})
        return ensure_success(provider.wait_result(role, name, occurrence=occurrence, timeout=90))

    identities = {role: tool(role, "whoami") for role in ("omp", "pi")}
    launches = {role: field(result, "launch_id") for role, result in identities.items()}
    discovery = tool("omp", "discover_agents", {"scope": "workspace"})
    if not any(item.get("launch_id") == launches["pi"] for item in objects(discovery)):
        raise RuntimeError("Genuine native discovery did not find the other managed harness")
    sent = tool("omp", "send_message", {"to": launches["pi"],
                "text": "Please review Unicode token boundaries in the search-index patch: café, 東京, and empty queries. Preserve matching semantics; flag normalization changes before merge.",
                "scope": "workspace", "delivery": "quiet", "idempotency_key": "showcase-search-review"})
    inbox = tool("pi", "read_inbox")
    message = field(sent, "message_id")
    if not any(item.get("message_id", item.get("id")) == message for item in objects(inbox)):
        raise RuntimeError("Pi did not read the exact genuine OMP message")
    acknowledged = tool("pi", "ack_message", {"ids": [message]})
    reply = tool("pi", "send_message", {"to": launches["omp"],
                 "text": "Review request received. I'll cover café, 東京, and whitespace-only queries, then report normalization risks before merge.",
                 "scope": "workspace", "delivery": "quiet", "idempotency_key": "showcase-search-review-reply"})
    reply_id = field(reply, "message_id")
    received = tool("omp", "read_inbox")
    if not any(item.get("message_id", item.get("id")) == reply_id for item in objects(received)):
        raise RuntimeError("OMP did not read the exact genuine Pi reply")
    reply_ack = tool("omp", "ack_message", {"ids": [reply_id]})
    outboxes = {role: tool(role, "inspect_outbox") for role in ("omp", "pi")}
    for role, identifier in (("omp", message), ("pi", reply_id)):
        matching = [item for item in objects(outboxes[role]) if item.get("message_id", item.get("id")) == identifier]
        if not matching or not any(item.get("acknowledged_at") is not None for row in matching for item in objects(row)):
            raise RuntimeError(f"No durable processed acknowledgement in {role} outbox")
    provider.stop(
        "omp",
        "## Handoff complete\n\n"
        "Pi received the search-index review request\n"
        "and sent back the review scope.\n\n"
        "**Requested review**\n"
        "- Unicode token boundaries\n"
        "- café: preserve the accent\n"
        "- 東京: preserve the whole token\n"
        "- Empty and whitespace-only queries\n\n"
        "**Response from Pi**\n"
        "> Review request received.\n"
        "> I'll cover café, 東京, and whitespace-only\n"
        "> queries, then report normalization risks\n"
        "> before merge.\n\n"
        "**Next**\n"
        "Agree on normalization before merging.\n"
        "Keep behavior changes explicit.",
    )
    provider.stop(
        "pi",
        "## Review handoff received\n\n"
        "**Message from OMP**\n"
        "> Please review Unicode token boundaries\n"
        "> in the search-index patch: café, 東京,\n"
        "> and empty queries.\n"
        "> Preserve matching semantics;\n"
        "> flag normalization changes before merge.\n\n"
        "**Review checklist**\n"
        "- Preserve accents and whole Unicode tokens\n"
        "- Include empty and whitespace-only queries\n\n"
        "**Reply sent to OMP**\n"
        "> Review request received.\n"
        "> I'll cover café, 東京, and whitespace-only\n"
        "> queries, then report normalization risks\n"
        "> before merge.\n\n"
        "OMP read and acknowledged the reply.\n\n"
        "**Next**\n"
        "Inspect normalization in src/index.rs.",
    )
    return {"identities": identities, "discovery": discovery, "send": sent, "read": inbox,
            "processed_ack": acknowledged, "reply": reply, "reply_read": received,
            "reply_processed_ack": reply_ack, "outboxes": outboxes}


def capture_inside(args) -> None:
    evidence, pair = args.evidence, args.evidence / "pair"
    if {name for _, name in socket.if_nameindex()} != {"lo"}:
        raise RuntimeError("Showcase requires a loopback-only network namespace")
    tools = {name: native(name) for name in ("Xvfb", "kitty", "xdotool", "import", "sshd", "ssh",
                                              "ssh-keygen", "bash", "env", "script", "git", "fc-match")}
    harnesses = {name: native_harness(name) for name in ("omp", "pi")}
    root = Path(tempfile.mkdtemp(prefix="bs-", dir="/tmp"))
    env = private_environment(root, dict(os.environ))
    bin_dir = root / "bin"
    bin_dir.mkdir(mode=0o700)
    for name, path in harnesses.items():
        (bin_dir / name).symlink_to(path)
    for name in ("bkpr", "bkprd"):
        (bin_dir / name).symlink_to(pair / name)
    paths = [bin_dir, *(Path(part).resolve() for part in env["PATH"].split(":") if part)]
    env["PATH"] = ":".join(dict.fromkeys(str(path) for path in paths if path.is_dir()))
    env.update(SHELL=str(tools["bash"]), OMP_SKIP_SETUP="1", PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1", PI_TELEMETRY="0")
    owned = OwnedProcesses(env, evidence)
    provider = ScriptedProvider(evidence)
    sessions: list[Session] = []
    manifest = {"passed": False, "scenario": "Scripted demo; real native OMP/Pi UIs, real broker tools, real loopback SSH",
                "network": {"interfaces": ["lo"], "host_home_mounted": False, "external_provider_calls": 0},
                "approval_bypass": False, "peer_message_pty_typing": False,
                "pair": {name: {"sha256": digest(pair / name)} for name in ("bkpr", "bkprd")},
                "tools": {}, "layout": {"columns": COLUMNS, "rows": ROWS, "sessions": [SESSION, "forgebox:" + REMOTE_SESSION],
                                          "window": "Agents", "split": "two native harnesses above repository shell"},
                "terminal_palette": PALETTE}
    error = None
    try:
        profile_env = prepare_profiles(Path(env["HOME"]), provider.base_url)
        profile_env["PATH"] = str(bin_dir) + os.pathsep + profile_env["PATH"]
        env.update(profile_env)
        manifest["graphics_environment"] = {key: env[key] for key in (
            "LIBGL_DRIVERS_PATH", "__EGL_VENDOR_LIBRARY_FILENAMES", "__GLX_VENDOR_LIBRARY_NAME",
            "GALLIUM_DRIVER", "LIBGL_ALWAYS_SOFTWARE") if key in env}
        for name, path in {**tools, **harnesses, "bkpr": pair / "bkpr", "bkprd": pair / "bkprd"}.items():
            flag = "-V" if name in ("ssh", "sshd") else "--version"
            if name in ("Xvfb", "xdotool", "import", "fc-match", "env", "script", "ssh-keygen"):
                manifest["tools"][name] = {"path": str(path), "sha256": digest(path)}
                continue
            result = subprocess.run([str(path), flag], env=env, capture_output=True, timeout=15)
            manifest["tools"][name] = {"path": str(path), "sha256": digest(path), "version": (result.stdout + result.stderr).decode(errors="replace").strip()[:1000]}
        manifest["font"] = owned.command([tools["fc-match"], "-f", "%{family}\n%{file}\n", env.get("SHOWCASE_FONT", "JetBrainsMono Nerd Font")]).stdout.decode().splitlines()
        if len(manifest["font"]) < 2 or env.get("SHOWCASE_FONT", "JetBrainsMono Nerd Font").casefold() not in manifest["font"][0].casefold():
            raise RuntimeError("Pinned showcase font unavailable")
        manifest["font_sha256"] = digest(Path(manifest["font"][1]))
        repo = Path(env["HOME"]) / "projects" / "search-index"
        repo.parent.mkdir(mode=0o700)
        repository(owned, repo, "feature/search-index")
        remote_root = root / "remote"
        remote_root.mkdir(mode=0o700)
        remote_env = private_environment(remote_root, env)
        remote_env.update(PATH=env["PATH"], SHELL=str(tools["bash"]))
        remote_repo = remote_root / "build-tools"
        repository(owned, remote_repo, "main")
        local = Session(SESSION, env, pair, owned, repo)
        remote = Session(REMOTE_SESSION, remote_env, pair, owned, remote_repo)
        sessions.extend((local, remote))
        for session in sessions:
            session.start()
        shell = [tools["bash"], "--noprofile", "--norc", "-c",
                 "git --no-pager diff --color=always -- src/index.rs; printf '\\nUnicode fixture: tests/unicode.txt\\n'; cat tests/unicode.txt; exec bash --noprofile --norc -i"]
        local.request("workspace_create", name="search-index", initial=spawn_spec(
            [pair / "bkpr", "run", "omp", *launch_arguments("omp")], repo, env, "OMP · implement"))
        workspace = local.registry()["workspaces"][0]
        window = workspace["windows"][0]
        local.request("window_rename", workspace_id=workspace["id"], window_id=window["id"], expected_revision=window["revision"], name="Agents")
        window = local.registry()["workspaces"][0]["windows"][0]
        omp_pane = local.registry()["panes"][0]["id"]
        local.request("pane_spawn", workspace_id=workspace["id"], window_id=window["id"], expected_revision=window["revision"],
                      panes=[{"reference": "pi", "spec": spawn_spec([pair / "bkpr", "run", "pi", *launch_arguments("pi")], repo, env, "Pi · review")},
                             {"reference": "shell", "spec": spawn_spec(shell, repo, env, "Repository · working tree")}],
                      layout={"kind": "split", "axis": "vertical", "ratio": 700,
                              "first": {"kind": "split", "axis": "horizontal", "ratio": 500,
                                        "first": {"kind": "existing", "pane_id": omp_pane}, "second": {"kind": "new", "reference": "pi"}},
                              "second": {"kind": "new", "reference": "shell"}})
        workspace = local.registry()["workspaces"][0]
        local.request("window_create", workspace_id=workspace["id"], expected_revision=workspace["revision"],
                      name="Repository", spec=spawn_spec(shell, repo, env, "Git · patch review"))
        remote.request("workspace_create", name="build-tools", initial=spawn_spec(
            [tools["bash"], "--noprofile", "--norc", "-c", "uname -sr; git status --short; exec bash --noprofile --norc -i"],
            remote_repo, remote_env, "SSH · build host"))
        remote_workspace = remote.registry()["workspaces"][0]
        remote_window = remote_workspace["windows"][0]
        remote.request("window_rename", workspace_id=remote_workspace["id"], window_id=remote_window["id"], expected_revision=remote_window["revision"], name="Build host")
        ssh_config, remote_bin, ssh_facts = start_ssh(owned, tools, root, remote_env, pair)
        inspect_remote = owned.command([pair / "bkpr", "remote", "inspect", "--remote", "forgebox", "--session", REMOTE_SESSION,
                                        "--ssh-config", ssh_config, "--remote-bkpr-bin", remote_bin])
        ssh_facts["inspection"] = json.loads(inspect_remote.stdout)
        manifest["ssh"] = ssh_facts
        env["DISPLAY"] = start_display(owned, tools)
        terminal_socket = root / "kitty.sock"
        transcript = evidence / "terminal.raw"
        device_input = evidence / "device-input.raw"
        client = [pair / "bkpr", "--bkprd", pair / "bkprd", "--session", SESSION,
                  "--session", f"forgebox:{REMOTE_SESSION}", "--ssh-config", ssh_config,
                  "--remote-bkpr-bin", remote_bin, "--clipboard", "deny"]
        wrapped = [tools["script"], "--quiet", "--return", "--flush", "--echo", "never",
                   "--log-out", transcript, "--log-in", device_input, "--command", shlex.join(map(str, client))]
        options = {**PALETTE, "allow_remote_control": "socket-only", "confirm_os_window_close": "0",
                   "enable_audio_bell": "no", "remember_window_size": "no", "window_padding_width": "0",
                   "initial_window_width": f"{COLUMNS}c", "initial_window_height": f"{ROWS}c",
                   "font_family": env.get("SHOWCASE_FONT", "JetBrainsMono Nerd Font"), "font_size": "12",
                   "cursor_blink_interval": "0", "hide_window_decorations": "yes", "sync_to_monitor": "no"}
        terminal = owned.start("kitty", [tools["kitty"], "--config", "NONE", "--listen-on", "unix:" + str(terminal_socket),
                                        *(item for key, value in options.items() for item in ("-o", f"{key}={value}")),
                                        "--title", "BKPR-SHOWCASE", *wrapped], cwd=repo)
        wait_path(terminal_socket)
        wait_path(transcript)

        def rc(*arguments):
            return owned.command([tools["kitty"], "@", "--to", "unix:" + str(terminal_socket), *arguments]).stdout.decode()

        def text_ready(predicate, label, timeout=90):
            with Changes([transcript]) as changes:
                def observe():
                    if terminal.poll() is not None:
                        raise RuntimeError("Kitty exited before physical readiness")
                    content = rc("get-text", "--extent", "screen")
                    return content if predicate(content) else None
                return changes.until(observe, label, timeout)

        local.wait(lambda snapshot: len([pane for pane in snapshot["panes"] if ((pane.get("harness") or {}).get("managed") or {}).get("conversation")]) == 2,
                   "Both managed native conversations bound")
        manifest["broker"] = broker_conversation(provider)
        required_text = (
            "Handoff complete", "Keep behavior changes explicit.",
            "Review handoff received", "Inspect normalization in src/index.rs.",
            "workbench", "forgebox", "Repository",
            "Unicode fixture: tests/unicode.txt", "café", "東京",
        )
        content = text_ready(lambda text: all(part in text for part in required_text),
                             "Complete native summaries, split layout, sidebar and SSH session")
        with Changes([transcript, device_input]) as changes:
            manifest["rich_graphics"] = changes.until(
                lambda: graphics_readiness(transcript.read_bytes(), device_input.read_bytes()),
                "Real host accepts graphics; complete sidebar icons and pane chrome placements", timeout=60)
        with Changes([transcript]) as changes:
            deadline = time.monotonic() + 15
            while changes.wait(0.4):
                if time.monotonic() >= deadline:
                    raise TimeoutError("Native terminal did not settle after completed conversation")
        content = rc("get-text", "--extent", "screen")
        (evidence / "screen.txt").write_text(content)
        listing = json.loads(rc("ls"))[0]
        grid = listing["tabs"][0]["windows"][0]
        if (grid["columns"], grid["lines"]) != (COLUMNS, ROWS):
            raise RuntimeError(f"Unexpected native terminal grid: {grid['columns']}x{grid['lines']}")
        owned.command([tools["xdotool"], "mousemove", "--window", listing["platform_window_id"], "2350", "1450"])
        raw = evidence / "raw.png"
        owned.command([tools["import"], "-window", listing["platform_window_id"], raw])
        manifest["physicalization"] = physicalize(raw, evidence / "showcase.png")
        manifest["readiness"] = {"complete_native_summaries": True, "real_ssh_inspection": True,
                                 "columns": grid["columns"], "rows": grid["lines"],
                                 "screen_sha256": digest(evidence / "screen.txt"), "captured_ns": time.monotonic_ns()}
        manifest["registry"] = {session.name: session.registry() for session in sessions}
        rc("send-text", "--", "\x02d")
        code = terminal.wait(timeout=30)
        manifest["terminal_exit"] = code
        if code:
            raise RuntimeError(f"Actual terminal client normal detach exited {code}")
        manifest["normal_exits"] = [session.stop() for session in sessions]
        manifest["passed"] = True
    except BaseException as failure:
        error = failure
        manifest["failure"] = {"type": type(failure).__name__, "message": str(failure)[:4000]}
        manifest["failure_registry"] = {}
        if "rc" in locals():
            with contextlib.suppress(Exception):
                (evidence / "failure-screen.txt").write_text(rc("get-text", "--extent", "screen"))
                failed_window = json.loads(rc("ls"))[0]["platform_window_id"]
                owned.command([tools["import"], "-window", failed_window, evidence / "failure.png"])
        for session in sessions:
            with contextlib.suppress(Exception):
                manifest["failure_registry"][session.name] = session.registry()
    finally:
        try:
            provider.close()
        except Exception as provider_error:
            manifest["passed"] = False
            error = error or provider_error
        for session in reversed(sessions):
            with contextlib.suppress(Exception):
                session.stop()
        try:
            manifest["cleanup"] = owned.close()
        except Exception as cleanup_error:
            manifest["passed"] = False
            error = error or cleanup_error
        shutil.rmtree(root)
        write_json(evidence / "manifest.json", sanitize(manifest))
    if error:
        raise error


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bkpr", type=Path, required=True)
    parser.add_argument("--bkprd", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("docs/assets/showcase.png"))
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--namespace-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--inside", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("The reproducible showcase requires the Linux .#showcase shell")
    if args.namespace_root:
        enter_namespace(args.namespace_root, [(HERE, False), (args.evidence, True)],
                        [str(HERE / "capture.py"), "--inside", "--bkpr", str(args.bkpr), "--bkprd", str(args.bkprd),
                         "--output", str(args.output), "--evidence", str(args.evidence)])
    if args.inside:
        capture_inside(args)
        return
    for name in ("bkpr", "bkprd"):
        path = getattr(args, name)
        if not path.is_file():
            parser.error(f"Missing {name}: {path}. Supply an explicit matching prebuilt pair; capture never builds product binaries.")
        setattr(args, name, native(path))
    args.output = args.output.resolve()
    evidence = (args.evidence or Path("target/showcase") / ("run-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{time.monotonic_ns()}")).resolve()
    evidence.mkdir(parents=True, mode=0o700, exist_ok=False)
    pair = evidence / "pair"
    pair.mkdir(mode=0o700)
    for name in ("bkpr", "bkprd"):
        shutil.copyfile(getattr(args, name), pair / name)
        (pair / name).chmod(0o500)
        if digest(pair / name) != digest(getattr(args, name)):
            raise RuntimeError(f"Source {name} changed while freezing the supplied pair")
    arguments = ["--bkpr", str(pair / "bkpr"), "--bkprd", str(pair / "bkprd"),
                 "--output", str(evidence / "showcase.png"), "--evidence", str(evidence)]
    supervise_namespace(evidence, HERE / "capture.py", arguments)
    manifest = json.loads((evidence / "manifest.json").read_text())
    manifest["namespace_cleanup"] = json.loads((evidence / "namespace/cleanup.json").read_text())
    write_json(evidence / "manifest.json", sanitize(manifest))
    if not manifest["passed"]:
        raise RuntimeError("Showcase failed; output was not published")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(evidence / "showcase.png", args.output)
    print(json.dumps({"output": str(args.output), "evidence": str(evidence), "sha256": digest(args.output)}))


if __name__ == "__main__":
    main()
