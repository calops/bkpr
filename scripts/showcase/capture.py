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
from provider import FIXTURE_API_KEY, PreflightProxy, ScriptedProvider, launch_arguments, native_harness, prepare_profiles, sanitize
from runtime import (
    Changes, OwnedProcesses, digest, enter_namespace, native, private_environment, process_identity,
    preflight_certificates, start_display, supervise_namespace, wait_path, write_json,
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
FIXTURE_BROKER_TOOLS = {
    "whoami": "Whoami Tool", "discover_agents": "Discover Agents Tool",
    "send_message": "Send Message Tool", "read_inbox": "Read Inbox Tool",
    "ack_message": "Ack Message Tool", "inspect_outbox": "Inspect Outbox Tool",
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


def is_fixture_key_prompt(text: str, configured_key: str) -> bool:
    return (configured_key == FIXTURE_API_KEY
            and "Detected a custom API key in your environment" in text
            and "Do you want to use this API key?" in text
            and bool(re.search(rf"ANTHROPIC_API_KEY:\s*sk-ant-\.\.\.{re.escape(FIXTURE_API_KEY[-20:])}(?:\s|$)", text)))


def is_security_notice(text: str) -> bool:
    notice = """Security notes:
1. Claude can make mistakes.
You're responsible for Claude's actions and should always review them, especially when running code.
2. Due to prompt injection risks, only use it with code you trust
Learn more: https://code.claude.com/docs/en/security
Press Enter to continue…
"""
    normalized = " ".join(text.split())
    prefix, marker, body = normalized.partition("Security notes:")
    banner = not prefix or re.fullmatch(r"Welcome to Claude Code v\d+\.\d+\.\d+\s*[\W_]*", prefix)
    return bool(banner and marker and marker + body == " ".join(notice.split()))


def is_fixture_project_prompt(text: str, repo: Path, home: Path) -> bool:
    try:
        canonical_home, canonical_repo = home.resolve(strict=True), repo.resolve(strict=True)
        root = canonical_home.parent
        if (canonical_home != home.absolute() or canonical_repo != repo.absolute()
                or canonical_home.name != "h" or root.parent != Path("/tmp") or not root.name.startswith("bs-")
                or canonical_repo != canonical_home / "projects/search-index"):
            return False
        for directory in (root, canonical_home, canonical_home / "projects", canonical_repo):
            metadata = directory.stat()
            if not directory.is_dir() or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
                return False
    except OSError:
        return False
    expected = f"""Accessing workspace: {canonical_repo}
Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first.
Claude Code'll be able to read, edit, and execute files here.
Security guide
No, exit
Yes, I trust this folder
Enter to confirm · Esc to cancel
"""
    normalized = " ".join(text.split())
    prefix, marker, body = normalized.partition("Accessing workspace:")
    return bool(marker and not prefix.strip("─ ") and normalized.count("❯") == 1
                and marker + body.replace("❯ ", "") == " ".join(expected.split()))


def is_fixture_broker_dialog(text: str, name: str) -> bool:
    label = FIXTURE_BROKER_TOOLS.get(name)
    if label is None:
        return False
    _, marker, dialog = " ".join(text.split()).rpartition("Tool use ")
    return bool(marker and re.fullmatch(
        rf"bkpr — {re.escape(label)}: \(MCP\) (?:.+ )?About the bkpr — {re.escape(label)}: .+ "
        r"Do you want to proceed\? ❯ 1\. Yes 2\. No Esc to cancel · Tab to amend", dialog))


def native_process(pane: dict, expected_executable: Path) -> dict:
    observed = pane.get("harness") or {}
    pid = observed.get("pid")
    process = process_identity(pid) if pid else None
    executable = Path(f"/proc/{pid}/exe").resolve(strict=True)
    boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    if (pane.get("status") != "running" or not process
            or observed.get("birth") != f"linux:{boot}:{process['birth']}"
            or executable != expected_executable):
        raise RuntimeError("Managed pane does not belong to the verified live native executable")
    return {"process": process, "executable": str(executable)}


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


def client_command(pair: Path, frontend: Path | None, ssh_config: Path, remote_bin: Path, local_socket: Path) -> list:
    if frontend:
        return [frontend, "--session", SESSION, "--session", f"forgebox:{REMOTE_SESSION}",
                "--remote-helper", remote_bin, "--ssh-config", ssh_config, "--socket", local_socket]
    return [pair / "bkpr", "--bkprd", pair / "bkprd", "--session", SESSION,
            "--session", f"forgebox:{REMOTE_SESSION}", "--ssh-config", ssh_config,
            "--remote-bkpr-bin", remote_bin, "--clipboard", "deny"]


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


def managed_bindings(snapshot: dict, roles=("claude", "pi", "codex")) -> dict:
    bindings = {}
    sources = {"claude": {"claude_hook"}, "pi": {"pi_extension"},
               "codex": {"codex_start", "codex_resume", "codex_fork"}}
    for pane in snapshot["panes"]:
        harness = pane.get("harness") or {}
        role = harness.get("kind")
        if role not in roles:
            continue
        managed = harness.get("managed") or {}
        conversation = managed.get("conversation") or {}
        if role in sources and managed.get("error"):
            raise RuntimeError(f"Native {role} capture failed: {managed['error']}")
        if role not in sources or not managed.get("launch_id") or not conversation.get("id"):
            continue
        if conversation.get("source") not in sources[role]:
            raise RuntimeError(f"Native {role} binding has an unexpected capture source")
        if role in bindings:
            raise RuntimeError(f"Duplicate managed native {role}")
        bindings[role] = {"pane_id": pane["id"], **managed}
    return bindings if set(bindings) == set(roles) else {}


def broker_conversation(provider: ScriptedProvider, on_tool=None) -> dict:
    def tool(role, name, arguments=None, occurrence=1):
        if on_tool and role == "claude" and name not in FIXTURE_BROKER_TOOLS:
            raise RuntimeError("Unknown Bkpr tools cannot be dispatched under fixture approval authority")
        provider.tool(role, name, arguments or {})
        if on_tool:
            on_tool(role, name, arguments or {}, occurrence)
        return ensure_success(provider.wait_result(role, name, occurrence=occurrence, timeout=90))

    identities = {role: tool(role, "whoami") for role in ("claude", "pi")}
    launches = {role: field(result, "launch_id") for role, result in identities.items()}
    discovery = tool("claude", "discover_agents", {"scope": "workspace"})
    if not any(item.get("launch_id") == launches["pi"] for item in objects(discovery)):
        raise RuntimeError("Genuine native discovery did not find the other managed harness")
    sent = tool("claude", "send_message", {
        "to": launches["pi"],
        "text": "Please review Unicode token boundaries in the search-index patch: café, 東京, and empty queries. Preserve matching semantics; flag normalization changes before merge.",
        "scope": "workspace", "delivery": "quiet", "idempotency_key": "showcase-search-review"})
    inbox = tool("pi", "read_inbox")
    message = field(sent, "message_id")
    if not any(item.get("message_id", item.get("id")) == message for item in objects(inbox)):
        raise RuntimeError("Pi did not read the exact genuine Claude message")
    acknowledged = tool("pi", "ack_message", {"ids": [message]})
    reply = tool("pi", "send_message", {
        "to": launches["claude"],
        "text": "Review request received. I'll cover café, 東京, and whitespace-only queries, then report normalization risks before merge.",
        "scope": "workspace", "delivery": "quiet", "idempotency_key": "showcase-search-review-reply"})
    reply_id = field(reply, "message_id")
    received = tool("claude", "read_inbox")
    if not any(item.get("message_id", item.get("id")) == reply_id for item in objects(received)):
        raise RuntimeError("Claude did not read the exact genuine Pi reply")
    reply_ack = tool("claude", "ack_message", {"ids": [reply_id]})
    outboxes = {role: tool(role, "inspect_outbox") for role in ("claude", "pi")}
    for role, identifier in (("claude", message), ("pi", reply_id)):
        matching = [item for item in objects(outboxes[role]) if item.get("message_id", item.get("id")) == identifier]
        if not matching or not any(item.get("acknowledged_at") is not None for row in matching for item in objects(row)):
            raise RuntimeError(f"No durable processed acknowledgement in {role} outbox")
    provider.stop("claude", """## Handoff complete

Pi received the search-index review request
and sent back the review scope.

**Requested review**
- Unicode token boundaries
- café: preserve the accent
- 東京: preserve the whole token
- Empty and whitespace-only queries

**Response from Pi**
> Review request received.
> I'll cover café, 東京, and whitespace-only
> queries, then report normalization risks
> before merge.

**Next**
Agree on normalization before merging.
Keep behavior changes explicit.""")
    provider.stop("pi", """## Review handoff received

**Message from Claude**
> Please review Unicode token boundaries
> in the search-index patch: café, 東京,
> and empty queries.
> Preserve matching semantics;
> flag normalization changes before merge.

**Review checklist**
- Preserve accents and whole Unicode tokens
- Include empty and whitespace-only queries

**Reply sent to Claude**
> Review request received.
> I'll cover café, 東京, and whitespace-only
> queries, then report normalization risks
> before merge.

Claude read and acknowledged the reply.

**Next**
Inspect normalization in src/index.rs.""")
    return {"identities": identities, "discovery": discovery, "send": sent, "read": inbox,
            "processed_ack": acknowledged, "reply": reply, "reply_read": received,
            "reply_processed_ack": reply_ack, "outboxes": outboxes}



def capture_inside(args) -> None:
    evidence, pair = args.evidence, args.evidence / "pair"
    if {name for _, name in socket.if_nameindex()} != {"lo"}:
        raise RuntimeError("Showcase requires a loopback-only network namespace")
    tools = {name: native(name) for name in ("Xvfb", "kitty", "xdotool", "import", "sshd", "ssh",
                                              "ssh-keygen", "bash", "env", "script", "git", "fc-match", "openssl")}
    harnesses = {name: native_harness(name) for name in ("claude", "pi", "codex")}
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
    env.update(SHELL=str(tools["bash"]), PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1", PI_TELEMETRY="0")
    owned = OwnedProcesses(env, evidence)
    provider = ScriptedProvider(evidence)
    preflight = None
    sessions: list[Session] = []
    manifest = {"passed": False, "scenario": "Scripted demo; real native Claude/Pi UIs, managed Codex window, real broker tools, real loopback SSH",
                "network": {"interfaces": ["lo"], "host_home_mounted": False, "external_provider_calls": 0},
                "approval_bypass": False, "peer_message_pty_typing": False,
                "consent_policy": {"dummy_key": args.dummy_key_consent, "security_notice": args.security_notice_consent,
                                   "fixture_project_trust": args.fixture_project_trust,
                                   "other_project_trust": "deny", "legal": "deny", "mcp": args.fixture_broker_tools},
                "pair": {name: {"sha256": digest(pair / name)} for name in ("bkpr", "bkprd")},
                "frontend": {"path": str(args.frontend or pair / "bkpr"),
                             "sha256": digest(args.frontend or pair / "bkpr"),
                             "overridden": args.frontend is not None},
                "controller": {name: digest(HERE / name) for name in ("capture.py", "provider.py", "runtime.py", "frame.py")},
                "tools": {}, "layout": {"columns": COLUMNS, "rows": ROWS, "sessions": [SESSION, "forgebox:" + REMOTE_SESSION],
                                          "window": "Agents", "split": "two native harnesses above repository shell"},
                "terminal_palette": PALETTE}
    error = None
    try:
        ca, certificate, key = preflight_certificates(owned, tools["openssl"], root)
        preflight = PreflightProxy(evidence, certificate, key)
        manifest["preflight"] = {"proxy": preflight.url, "private_ca_sha256": digest(ca),
                                 "scope": "Claude child only; exact HTTPS hello paths; no upstream connections"}
        profile_env = prepare_profiles(Path(env["HOME"]), provider.base_url)
        profile_env["PATH"] = str(bin_dir) + os.pathsep + profile_env["PATH"]
        env.update(profile_env)
        manifest["graphics_environment"] = {key: env[key] for key in (
            "LIBGL_DRIVERS_PATH", "__EGL_VENDOR_LIBRARY_FILENAMES", "__GLX_VENDOR_LIBRARY_NAME",
            "GALLIUM_DRIVER", "LIBGL_ALWAYS_SOFTWARE") if key in env}
        for name, path in {**tools, **harnesses, "bkpr": pair / "bkpr", "bkprd": pair / "bkprd"}.items():
            flag = "-V" if name in ("ssh", "sshd") else "--version"
            if name in ("Xvfb", "xdotool", "import", "fc-match", "env", "script", "ssh-keygen", "openssl"):
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
            shell, repo, env, "Repository · working tree"))
        workspace = local.registry()["workspaces"][0]
        window = workspace["windows"][0]
        local.request("window_rename", workspace_id=workspace["id"], window_id=window["id"], expected_revision=window["revision"], name="Agents")
        window = local.registry()["workspaces"][0]["windows"][0]
        shell_pane = local.registry()["panes"][0]["id"]
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
        workspace = local.registry()["workspaces"][0]
        local.request("window_create", workspace_id=workspace["id"], expected_revision=workspace["revision"],
                      name="Codex", spec=spawn_spec(
                          [pair / "bkpr", "run", "codex", *launch_arguments("codex")], repo, env, "Codex · patch review"))
        inspect_remote = owned.command([pair / "bkpr", "remote", "inspect", "--remote", "forgebox", "--session", REMOTE_SESSION,
                                        "--ssh-config", ssh_config, "--remote-bkpr-bin", remote_bin])
        ssh_facts["inspection"] = json.loads(inspect_remote.stdout)
        manifest["ssh"] = ssh_facts
        env["DISPLAY"] = start_display(owned, tools)
        terminal_socket = root / "kitty.sock"
        transcript = evidence / "terminal.raw"
        device_input = evidence / "device-input.raw"
        client = client_command(pair, args.frontend, ssh_config, remote_bin, local.endpoint)
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

        def click_cell(column, row):
            identifier = json.loads(rc("ls"))[0]["platform_window_id"]
            geometry = owned.command([tools["xdotool"], "getwindowgeometry", "--shell", identifier]).stdout.decode()
            dimensions = dict(line.split("=", 1) for line in geometry.splitlines() if "=" in line)
            x = int(column * int(dimensions["WIDTH"]) / COLUMNS)
            y = int(row * int(dimensions["HEIGHT"]) / ROWS)
            owned.command([tools["xdotool"], "mousemove", "--window", identifier, x, y, "click", "1"])

        def select_window(name, predicate):
            content = text_ready(
                lambda text: any("─╴" in line[:35] and line[:35].rstrip().endswith(name)
                                 for line in text.splitlines()),
                f"Native sidebar window {name}")
            row, line = next((row, line) for row, line in enumerate(content.splitlines())
                             if "─╴" in line[:35] and line[:35].rstrip().endswith(name))
            click_cell(line.index(name) + 1.5, row + 0.5)
            return text_ready(predicate, f"Actual native {name} window content")

        def retain_surface(basename, content):
            (evidence / f"{basename}.txt").write_text(content)
            identifier = json.loads(rc("ls"))[0]["platform_window_id"]
            owned.command([tools["xdotool"], "mousemove", "--window", identifier, "2350", "1450"])
            image = evidence / f"{basename}.png"
            owned.command([tools["import"], "-window", identifier, image])
            return {"screen_sha256": digest(evidence / f"{basename}.txt"),
                    "image_sha256": digest(image), "captured_ns": time.monotonic_ns()}

        def repositories_ready():
            for session in sessions:
                session.wait(lambda snapshot: all((pane.get("project") or {}).get("state") == "ready"
                                                   for pane in snapshot["panes"] if pane["status"] == "running"),
                             "Actual local and remote repository facts ready for stable native sidebar")
            text_ready(lambda text: "Git loading" not in text and "feature/search-index" in text and "main" in text,
                       "Complete stable sidebar repository labels")

        text_ready(lambda text: "Unicode fixture: tests/unicode.txt" in text,
                   "Real repository shell on attached terminal before native agent startup")
        repositories_ready()
        codex_content = select_window(
            "Codex", lambda text: "OpenAI Codex" in text and "Stand by for patch review" in text)
        manifest["codex_surface"] = retain_surface("native-codex", codex_content)
        codex_binding = local.wait(lambda snapshot: managed_bindings(snapshot, ("codex",)),
                                   "Actual managed Codex native conversation binding")["codex"]
        codex_pane = next(pane for pane in local.registry()["panes"] if pane["id"] == codex_binding["pane_id"])
        verified = native_process(codex_pane, harnesses["codex"])
        manifest["codex"] = {"binding": codex_binding, **verified}
        provider.stop("codex", """## Ready for patch review

Standing by in the Codex window.
The Unicode review handoff is assigned to Claude and Pi.""")
        codex_content = text_ready(lambda text: "Standing by in the Codex window." in text,
                                   "Complete genuine Codex native response")
        manifest["codex_ready_surface"] = retain_surface("native-codex-ready", codex_content)
        select_window("Agents", lambda text: "Unicode fixture: tests/unicode.txt" in text)

        workspace = local.registry()["workspaces"][0]
        window = next(item for item in workspace["windows"] if item["name"] == "Agents")
        local.request("pane_spawn", workspace_id=workspace["id"], window_id=window["id"], expected_revision=window["revision"],
                      panes=[{"reference": "claude", "spec": spawn_spec(
                                  [tools["env"], f"HTTPS_PROXY={preflight.url}", "NO_PROXY=127.0.0.1,localhost",
                                   f"NODE_EXTRA_CA_CERTS={ca}", pair / "bkpr", "run", "claude", *launch_arguments("claude")],
                                  repo, env, "Claude · implement")},
                             {"reference": "pi", "spec": spawn_spec(
                                  [pair / "bkpr", "run", "pi", *launch_arguments("pi")], repo, env, "Pi · review")}],
                      layout={"kind": "split", "axis": "vertical", "ratio": 700,
                              "first": {"kind": "split", "axis": "horizontal", "ratio": 500,
                                        "first": {"kind": "new", "reference": "claude"},
                                        "second": {"kind": "new", "reference": "pi"}},
                              "second": {"kind": "existing", "pane_id": shell_pane}})

        visual_phrases = ("Choose the text style", "Choose the theme")
        consent_phrases = ("Do you want to use this API key", "Is this a project you created",
                           "Do you trust this folder", "Security notes", "Terms of Service",
                           "Select login method", "Sign in")
        def claude_startup_ready(text):
            return (any(phrase in text for phrase in (*visual_phrases, *consent_phrases,
                                                      "Unable to connect to Anthropic services"))
                    or ("Claude Code" in text and "Coordinate a Unicode token-boundary review" in text))

        claude_content = text_ready(claude_startup_ready, "Complete first native Claude startup screen")
        repositories_ready()
        manifest["claude_startup_surface"] = retain_surface(
            "native-claude-startup", rc("get-text", "--extent", "screen"))
        consent_surface = "native-claude-startup"
        select_window("Claude · implement", claude_startup_ready)
        if any(phrase in claude_content for phrase in visual_phrases):
            if not re.search(r"❯\s*✔?\s*Dark mode\s", claude_content):
                raise RuntimeError("Native Claude visual chooser does not have Dark mode selected")
            rc("send-text", "--", "\r")
            manifest["native_visual_choices"] = [{"harness": "claude", "choice": "Dark mode",
                                                  "input": "one Enter after actual focused native selection"}]
            claude_content = text_ready(
                lambda text: not any(phrase in text for phrase in visual_phrases) and claude_startup_ready(text),
                "Native Claude screen after authorized visual theme selection")
            manifest["claude_after_theme_surface"] = retain_surface("native-claude-after-theme", claude_content)
            consent_surface = "native-claude-after-theme"
        if args.dummy_key_consent == "allow" and "Do you want to use this API key?" in claude_content:
            if (not is_fixture_key_prompt(claude_content, profile_env["ANTHROPIC_API_KEY"])
                    or not re.search(r"❯\s+No \(recommended\)", claude_content)):
                raise RuntimeError("Native API-key prompt does not match the exact isolated fixture key and expected selection")
            rc("send-text", "--", "\x1b[A")
            claude_content = text_ready(
                lambda text: is_fixture_key_prompt(text, profile_env["ANTHROPIC_API_KEY"])
                and bool(re.search(r"❯\s+Yes(?:\s|$)", text)),
                "Native Yes selection for the explicitly authorized fixture-only API key")
            manifest["dummy_key_selected_surface"] = retain_surface("native-claude-dummy-key-selected", claude_content)
            rc("send-text", "--", "\r")
            manifest["native_consent_actions"] = [{"harness": "claude", "kind": "fixture_dummy_key", "decision": "Yes",
                                                   "authority": "--dummy-key-consent=allow", "profile_state_seeded": False}]
            claude_content = text_ready(
                lambda text: "Do you want to use this API key?" not in text and claude_startup_ready(text),
                "Exact native Claude screen after authorized fixture-key confirmation")
            manifest["claude_after_dummy_key_surface"] = retain_surface("native-claude-after-dummy-key", claude_content)
            consent_surface = "native-claude-after-dummy-key"
        if args.security_notice_consent == "allow" and "Security notes:" in claude_content:
            claude_pane = local.wait(
                lambda snapshot: next((pane for pane in snapshot["panes"]
                                       if (pane.get("harness") or {}).get("kind") == "claude"), None),
                "Actual native Claude pane for exact security-notice verification")
            notice = local.request("control_pane_read", pane_id=claude_pane["id"],
                                   query={"text": True, "max_bytes": 16384})
            if not notice["complete"] or not is_security_notice(notice["text"]):
                raise RuntimeError("Native security acknowledgment does not match the exact authorized informational notice")
            rc("send-text", "--", "\r")
            manifest.setdefault("native_consent_actions", []).append({
                "harness": "claude", "kind": "informational_security_notice", "decision": "Enter to continue",
                "authority": "--security-notice-consent=allow", "profile_state_seeded": False})
            claude_content = text_ready(
                lambda text: "Security notes:" not in text and claude_startup_ready(text),
                "Exact native Claude screen after authorized informational security acknowledgment")
            manifest["claude_after_security_notice_surface"] = retain_surface("native-claude-after-security-notice", claude_content)
            consent_surface = "native-claude-after-security-notice"
        if args.fixture_project_trust == "allow" and "Is this a project you created" in claude_content:
            claude_pane = local.wait(
                lambda snapshot: next((pane for pane in snapshot["panes"]
                                       if (pane.get("harness") or {}).get("kind") == "claude"), None),
                "Actual native Claude pane for fixture-only project trust")
            trust = local.request("control_pane_read", pane_id=claude_pane["id"],
                                  query={"text": True, "max_bytes": 16384})
            if (not trust["complete"] or not is_fixture_project_prompt(trust["text"], repo, Path(env["HOME"]))
                    or not re.search(r"❯\s+No, exit(?:\s|$)", trust["text"])):
                raise RuntimeError("Native project-trust prompt does not match the exact owned private fixture repository")
            rc("send-text", "--", "\x1b[B")
            claude_content = text_ready(
                lambda text: bool(re.search(r"❯\s+Yes, I trust this folder(?:\s|$)", text)),
                "Native Yes selection for the explicitly authorized private fixture repository")
            selected = local.request("control_pane_read", pane_id=claude_pane["id"],
                                     query={"text": True, "max_bytes": 16384})
            if (not selected["complete"] or not is_fixture_project_prompt(selected["text"], repo, Path(env["HOME"]))
                    or not re.search(r"❯\s+Yes, I trust this folder(?:\s|$)", selected["text"])):
                raise RuntimeError("Native selected project trust changed before explicit confirmation")
            manifest["fixture_project_selected_surface"] = retain_surface("native-claude-fixture-project-selected", claude_content)
            rc("send-text", "--", "\r")
            manifest.setdefault("native_consent_actions", []).append({
                "harness": "claude", "kind": "fixture_project_trust", "decision": "Yes, I trust this folder",
                "authority": "--fixture-project-trust=allow", "canonical_repository": str(repo.resolve()),
                "profile_state_seeded": False})
            claude_content = text_ready(
                lambda text: "Is this a project you created" not in text and claude_startup_ready(text),
                "Exact native Claude screen after authorized private fixture trust")
            manifest["claude_after_fixture_trust_surface"] = retain_surface("native-claude-after-fixture-trust", claude_content)
            consent_surface = "native-claude-after-fixture-trust"
        if any(phrase in claude_content for phrase in consent_phrases):
            manifest["native_consent"] = {"harness": "claude", "screen": f"{consent_surface}.txt",
                                          "image": f"{consent_surface}.png", "answered": False}
            raise RuntimeError("Genuine native Claude consent awaits explicit user authorization")
        if "Unable to connect to Anthropic services" in claude_content:
            raise RuntimeError("Genuine native Claude connectivity preflight failed")


        manifest["bindings"] = local.wait(managed_bindings, "All three genuine managed native conversations bound")
        expected_claude = manifest["bindings"]["claude"]
        initial_claude = next(pane for pane in local.registry()["panes"] if pane["id"] == expected_claude["pane_id"])
        manifest["claude_native_process"] = native_process(initial_claude, harnesses["claude"])

        def approve_fixture_tool(role, name, arguments, occurrence):
            if role != "claude" or args.fixture_broker_tools != "allow":
                return
            if name not in FIXTURE_BROKER_TOOLS:
                raise RuntimeError("Unknown Bkpr tools cannot receive fixture approval")
            approval = None
            with Changes([transcript, evidence]) as changes:
                def observe():
                    nonlocal approval
                    try:
                        provider.wait_result(role, name, occurrence=occurrence, timeout=0)
                    except TimeoutError:
                        pass
                    else:
                        if approval:
                            approval["native_result_observed"] = any(
                                row["tool_call_id"] == approval["tool_call_id"] for row in provider.receipts)
                            if not approval["native_result_observed"]:
                                raise RuntimeError("Native approval has no exact following tool result")
                        return True
                    pending = provider.pending_call(role)
                    if not pending or pending["name"] == "native_mcp_startup":
                        return False
                    if pending["name"] != name or pending["arguments"] != arguments:
                        raise RuntimeError("Native pending call does not match the exact planned fixture operation")
                    if approval:
                        return False
                    pane = next((pane for pane in local.registry()["panes"]
                                 if pane["id"] == expected_claude["pane_id"]), None)
                    if pane is None:
                        raise RuntimeError("Native Claude approval pane disappeared")
                    harness = pane.get("harness") or {}
                    managed = harness.get("managed") or {}
                    verified = native_process(pane, harnesses["claude"])
                    initial = manifest["claude_native_process"]
                    if (harness.get("kind") != "claude" or managed.get("error")
                            or managed.get("launch_id") != expected_claude["launch_id"]
                            or managed.get("conversation") != expected_claude["conversation"]
                            or pane.get("workspace_id") != workspace["id"] or pane.get("window_id") != window["id"]
                            or Path(harness["cwd"]).resolve() != repo.resolve()
                            or any(verified["process"][key] != initial["process"][key] for key in ("pid", "birth"))):
                        raise RuntimeError("Native MCP approval identity or fixture conversation changed")
                    view = local.request("control_pane_read", pane_id=pane["id"],
                                         query={"text": True, "max_bytes": 16384})
                    text = view["text"]
                    if not view["complete"] or "Do you want to proceed?" not in text or "Esc to cancel · Tab to amend" not in text:
                        return False
                    if not is_fixture_broker_dialog(text, name):
                        raise RuntimeError("Unknown native MCP approval dialog; no input submitted")
                    if provider.pending_call(role) != pending:
                        return False
                    approvals = manifest.setdefault("native_broker_approvals", [])
                    surface = retain_surface(f"native-claude-broker-{len(approvals) + 1:02}-{name}",
                                             rc("get-text", "--extent", "screen"))
                    approval = {**pending, "authority": "--fixture-broker-tools=allow", "decision": "Yes (once)",
                                "pane_id": pane["id"], "launch_id": managed["launch_id"],
                                "conversation": managed["conversation"], "native_process": verified,
                                "surface": surface, "input_submission": "unknown", "native_result_observed": False}
                    approvals.append(approval)
                    rc("send-text", "--", "\r")
                    approval["input_submission"] = "accepted_by_host"
                    return False
                changes.until(observe, f"Exact native {role}/{name} permission and following result", timeout=90)

        manifest["broker"] = broker_conversation(provider, approve_fixture_tool)
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
                for pane in manifest["failure_registry"][session.name]["panes"]:
                    if pane.get("harness"):
                        read = session.request("control_pane_read", pane_id=pane["id"],
                                               query={"text": True, "max_bytes": 16384})
                        write_json(evidence / f"failure-pane-{pane['id']}.json", sanitize(read))
    finally:
        try:
            provider.close()
        except Exception as provider_error:
            manifest["passed"] = False
            error = error or provider_error
        if preflight:
            try:
                preflight.close()
            except Exception as preflight_error:
                manifest["passed"] = False
                error = error or preflight_error
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
    parser.add_argument("--frontend", type=Path, help="Optional native run_sessions frontend consumer; frozen pair still owns all backend and harness operations")
    parser.add_argument("--dummy-key-consent", choices=("deny", "allow"), default="deny",
                        help="Explicitly authorize native Yes only for this isolated fixture's exact dummy API key")
    parser.add_argument("--security-notice-consent", choices=("deny", "allow"), default="deny",
                        help="Explicitly authorize Enter only on the exact native informational security notice")
    parser.add_argument("--fixture-project-trust", choices=("deny", "allow"), default="deny",
                        help="Explicitly authorize native trust only for the canonical owned private generated showcase repository")
    parser.add_argument("--fixture-broker-tools", choices=("deny", "allow"), default="deny",
                        help="Authorize normal one-call Yes only for the six known Bkpr tools in this isolated fixture conversation")
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
                         "--output", str(args.output), "--evidence", str(args.evidence),
                         "--dummy-key-consent", args.dummy_key_consent,
                         "--security-notice-consent", args.security_notice_consent,
                         "--fixture-project-trust", args.fixture_project_trust,
                         "--fixture-broker-tools", args.fixture_broker_tools,
                         *(["--frontend", str(args.frontend)] if args.frontend else [])])
    if args.inside:
        capture_inside(args)
        return
    artifacts = ("bkpr", "bkprd", *(("frontend",) if args.frontend else ()))
    for name in artifacts:
        path = getattr(args, name)
        if not path.is_file():
            parser.error(f"Missing {name}: {path}. Supply an explicit matching prebuilt pair; capture never builds product binaries.")
        setattr(args, name, native(path))
    args.output = args.output.resolve()
    evidence = (args.evidence or Path("target/showcase") / ("run-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{time.monotonic_ns()}")).resolve()
    evidence.mkdir(parents=True, mode=0o700, exist_ok=False)
    pair = evidence / "pair"
    pair.mkdir(mode=0o700)
    for name in artifacts:
        shutil.copyfile(getattr(args, name), pair / name)
        (pair / name).chmod(0o500)
        if digest(pair / name) != digest(getattr(args, name)):
            raise RuntimeError(f"Source {name} changed while freezing the supplied pair")
    arguments = ["--bkpr", str(pair / "bkpr"), "--bkprd", str(pair / "bkprd"),
                 "--output", str(evidence / "showcase.png"), "--evidence", str(evidence),
                 "--dummy-key-consent", args.dummy_key_consent,
                 "--security-notice-consent", args.security_notice_consent,
                 "--fixture-project-trust", args.fixture_project_trust,
                 "--fixture-broker-tools", args.fixture_broker_tools]
    if args.frontend:
        arguments.extend(("--frontend", str(pair / "frontend")))
    controller = evidence / "controller"
    controller.mkdir(mode=0o700)
    for name in ("capture.py", "provider.py", "runtime.py", "frame.py"):
        shutil.copyfile(HERE / name, controller / name)
        (controller / name).chmod(0o400)
    supervise_namespace(evidence, controller / "capture.py", arguments)
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
