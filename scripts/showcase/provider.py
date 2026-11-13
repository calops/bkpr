"""Loopback-only scripted model; native harnesses execute every Bkpr MCP call.

The capture controller must supply a fresh HOME, a credential-free environment,
fixture working directories, and a loopback-only network namespace. OMP has no
master offline switch: its catalog discovery still needs that network boundary.
"""
from __future__ import annotations

from collections import deque
from copy import deepcopy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shutil
import threading
from typing import Any
from urllib.parse import urlsplit

_ROLES = ("omp", "pi")
_PROVIDER = "showcase"
_PROMPT = "Coordinate a Unicode token-boundary review of the search-index patch through Bkpr, then summarize the handoff."
_MAX_BODY = 2 * 1024 * 1024
_SENSITIVE = re.compile(r"token|authorization|api[_-]?key|secret|password|credential", re.I)


def sanitize(value: Any) -> Any:
    """Redact credentials recursively, including JSON embedded in native tool text."""
    if isinstance(value, dict):
        return {key: "[REDACTED]" if _SENSITIVE.search(key) else sanitize(item)
                for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    if isinstance(value, str):
        try:
            return json.dumps(sanitize(json.loads(value)), ensure_ascii=False)
        except ValueError:
            # Native text may contain JSON inside a codemode marker or Markdown.
            decoder = json.JSONDecoder()
            parts, offset = [], 0
            for match in re.finditer(r"[\[{]", value):
                if match.start() < offset:
                    continue
                try:
                    parsed, length = decoder.raw_decode(value[match.start():])
                except ValueError:
                    continue
                parts.extend((value[offset:match.start()], json.dumps(sanitize(parsed), ensure_ascii=False)))
                offset = match.start() + length
            parts.append(value[offset:])
            text = "".join(parts)
            text = re.sub(r"(?i)Bearer\s+[^\s\"'`,;]+", "Bearer [REDACTED]", text)
            return re.sub(r"(?i)((?:BKPR_CAPTURE_TOKEN|api[_-]?key|password|secret)\s*[=:]\s*)[^\s\"'`,;]+",
                          r"\1[REDACTED]", text)
    return value


def _json_file(path: Path, value: Any) -> None:
    with path.open("x", encoding="utf-8") as stream:
        os.chmod(path, 0o600)
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


def _text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(part.get("text", "") for part in content if isinstance(part, dict))
    raise ValueError("Native tool result has neither text nor content blocks")


def _decode_result(value: Any) -> Any:
    if isinstance(value, str):
        try:
            return _decode_result(json.loads(value))
        except ValueError:
            return value
    if isinstance(value, dict):
        if value.get("isError"):
            return value
        if "structuredContent" in value:
            return _decode_result(value["structuredContent"])
        if isinstance(value.get("content"), list):
            return _decode_result(_text(value["content"]))
    if (isinstance(value, list) and value
            and all(isinstance(part, dict) and part.get("type") == "text" for part in value)):
        return _decode_result(_text(value))
    return value


def _role(name: str) -> None:
    if name not in _ROLES:
        raise ValueError(f"Unknown showcase harness: {name!r}")


def native_harness(name: str) -> Path:
    """Resolve a native ELF, never execute a profile/credential wrapper.

    Only the inspected Nix makeWrapper assignment grammar is accepted. Its final
    static exec target is returned; no shell contents are executed or sourced.
    """
    _role(name)
    found = shutil.which(name)
    if found is None:
        raise FileNotFoundError(f"Native {name} missing; enter nix develop .#showcase")
    path = Path(found).resolve(strict=True)
    with path.open("rb") as stream:
        prefix = stream.read(4)
    if prefix == b"\x7fELF":
        return path
    if not path.is_relative_to("/nix/store") or path.stat().st_size > 16384:
        raise ValueError(f"Refusing non-native {name} profile wrapper: {path}")
    lines = path.read_text(encoding="utf-8").splitlines()
    if not lines or not re.fullmatch(r"#! ?/nix/store/[\w.+-]+/bin/bash -e", lines[0]):
        raise ValueError(f"Unrecognized Nix wrapper interpreter: {path}")
    assignment = re.compile(
        r"(?:PATH|LD_LIBRARY_PATH)=\$\{(?:PATH|LD_LIBRARY_PATH):\+':'\$(?:PATH|LD_LIBRARY_PATH)':'\}"
        r"|(?:PATH|LD_LIBRARY_PATH)=\$\{(?:PATH|LD_LIBRARY_PATH)/':''/nix/store/[\w.+/-]+'':'/':'\}"
        r"|(?:PATH|LD_LIBRARY_PATH)='/nix/store/[\w.+/-]+'\$(?:PATH|LD_LIBRARY_PATH)"
        r"|(?:PATH|LD_LIBRARY_PATH)=\$\{(?:PATH|LD_LIBRARY_PATH)[#%]':'\}"
        r"|export (?:PATH|LD_LIBRARY_PATH)"
        r"|export (?:PI_SKIP_VERSION_CHECK='1'|PI_TELEMETRY='0')"
        r"|export PI_PACKAGE_DIR='/nix/store/[\w.+/-]+'"
    )
    if any(not assignment.fullmatch(line) for line in lines[1:-1]):
        raise ValueError(f"Refusing non-static Nix wrapper: {path}")
    target = re.fullmatch(r'exec "(/nix/store/[\w.+/-]+)"\s+"\$@"\s*', lines[-1])
    if target is None:
        raise ValueError(f"Nix wrapper has no static native exec target: {path}")
    native = Path(target[1]).resolve(strict=True)
    with native.open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            raise ValueError(f"Nix wrapper target is not native ELF: {native}")
    if not os.access(native, os.X_OK):
        raise ValueError(f"Native harness is not executable: {native}")
    return native


def launch_arguments(name: str) -> list[str]:
    """Arguments after ``bkpr run HARNESS``; initial prompt stays native CLI input."""
    _role(name)
    arguments = ["--model", f"{_PROVIDER}/{name}", "--thinking", "off"]
    if name == "omp":
        arguments.extend(("--no-title", "--no-lsp"))
    return [*arguments, "--", _PROMPT]


def prepare_profiles(home: Path, base_url: str) -> dict[str, str]:
    """Create private native configs and return additions to an isolated child env.

    No shared PI_CODING_AGENT_DIR/PI_PACKAGE_DIR override: the two harnesses must
    retain their distinct native config roots and executable-relative assets.
    """
    endpoint = urlsplit(base_url)
    if (endpoint.scheme != "http" or endpoint.hostname != "127.0.0.1"
            or endpoint.port is None or endpoint.path != "/v1"
            or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment):
        raise ValueError("Showcase provider must be http://127.0.0.1:PORT/v1")
    home = home.resolve(strict=True)
    if home.stat().st_uid != os.getuid() or home.stat().st_mode & 0o077:
        raise ValueError("Showcase HOME must be an owned private directory")
    bin_dir = home / ".showcase-bin"
    bin_dir.mkdir(mode=0o700)
    for name in _ROLES:
        (bin_dir / name).symlink_to(native_harness(name))
        agent = home / f".{name}" / "agent"
        agent.parent.mkdir(mode=0o700)
        agent.mkdir(mode=0o700)
        model = {"id": name, "name": f"Scripted {name.upper()}", "reasoning": False,
                 "input": ["text"], "contextWindow": 128000, "maxTokens": 4096,
                 "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}
        provider = {"api": "openai-completions", "baseUrl": base_url,
                    "apiKey": "offline-not-a-credential", "models": [model]}
        if name == "omp":
            provider["models"].append({**model, "id": "omp-helper", "name": "Scripted OMP utility"})
        _json_file(agent / ("models.yml" if name == "omp" else "models.json"),
                   {"providers": {_PROVIDER: provider}})
        if name == "omp":
            settings = {
                "enabledProviders": [_PROVIDER], "enabledModels": [f"{_PROVIDER}/omp", f"{_PROVIDER}/omp-helper"],
                "modelRoles": {role: f"{_PROVIDER}/omp"
                               for role in ("default", "smol", "slow", "plan", "tiny", "memory")},
                "disabledProviders": ["ollama", "llama.cpp", "lm-studio", "apple", "local", "web"],
                "startup": {"checkUpdate": False, "setupWizard": False, "changelogMode": "hidden"},
                "update": {"auto": False}, "marketplace": {"autoUpdate": "off"},
                "providers": {"cacheWarming": "off"}, "spelling": {"autocomplete": "off"},
                "mcp": {"startupTimeoutMs": 0},
                "telemetry": {"otlpExportEnabled": False},
                "compaction": {"enabled": False}, "retry": {"enabled": False, "modelFallback": False},
                "display": {"hideToolActivity": True},
            }
            settings["modelRoles"].update(smol=f"{_PROVIDER}/omp-helper", tiny=f"{_PROVIDER}/omp-helper")
            _json_file(agent / "config.yml", settings)
        else:
            _json_file(agent / "settings.json", {
                "defaultProvider": _PROVIDER, "defaultModel": "pi", "theme": "dark",
                "compaction": {"enabled": False}, "cacheWarming": "off",
                "retry": {"enabled": False}, "enableInstallTelemetry": False,
                "defaultTools": ["+codemode"],
            })
            _json_file(agent / "mcp.json", {"mcpServers": {}, "autoEnableCodemode": True})
    return {"HOME": str(home), "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', os.defpath)}",
            "OMP_SKIP_SETUP": "1", "OMP_PROFILE": "", "PI_CONFIG_DIR": ".omp",
            "PI_OFFLINE": "1", "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0",
            "PI_NO_TITLE": "1", "OTEL_SDK_DISABLED": "true"}


class ScriptedProvider:
    """Serve scripted SSE turns and observe real results in subsequent requests."""

    def __init__(self, evidence: Path):
        self._evidence = evidence
        evidence.mkdir(mode=0o700, parents=True, exist_ok=True)
        self._condition = threading.Condition()
        self._actions: dict[str, deque] = {role: deque() for role in _ROLES}
        self._calls: dict[str, dict[str, str]] = {role: {} for role in _ROLES}
        self._counts = dict.fromkeys(_ROLES, 0)
        self._helper_count = 0
        self._receipts: list[dict[str, Any]] = []
        self._seen: set[tuple[str, str]] = set()
        self._errors: list[str] = []
        self._closed = False
        provider = self

        class Handler(BaseHTTPRequestHandler):
            timeout = 15

            def log_message(self, *_: Any) -> None:
                pass

            def do_POST(self) -> None:
                try:
                    self.connection.settimeout(15)
                    if self.path != "/v1/chat/completions":
                        raise ValueError("Unexpected provider endpoint")
                    length = int(self.headers.get("Content-Length", "0"))
                    if not 0 < length <= _MAX_BODY:
                        raise ValueError("Provider request exceeds body bound")
                    raw = self.rfile.read(length)
                    if len(raw) != length:
                        raise ValueError("Truncated provider request")
                    body = json.loads(raw)
                    role = body["model"]
                    if role == "omp-helper":
                        if body.get("tools"):
                            raise ValueError("Native OMP utility model unexpectedly requested tools")
                        with provider._condition:
                            provider._helper_count += 1
                            index = provider._helper_count
                            if index > 16:
                                raise ValueError("Native OMP utility request count exceeds bound")
                            _json_file(evidence / f"omp-helper-request-{index:02}.json", sanitize(body))
                        delta = {"role": "assistant", "content": "Use for native Bkpr control and authenticated peer messages."}
                        reason = "stop"
                    else:
                        _role(role)
                        with provider._condition:
                            provider._counts[role] += 1
                            index = provider._counts[role]
                            if index > 80:
                                raise ValueError("Provider request count exceeds bound")
                            provider._observe(role, body)
                            _json_file(evidence / f"{role}-request-{index:02}.json", sanitize(body))
                            provider._condition.notify_all()
                            provider._condition.wait_for(
                                lambda: provider._closed or provider._errors or provider._actions[role])
                            if provider._closed or provider._errors:
                                self.send_error(503, "Scripted provider closed")
                                return
                            action = provider._actions[role].popleft()
                            delta, reason = provider._response(role, index, body, action)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    for next_delta, finish in ((delta, None), ({}, reason)):
                        chunk = {"id": f"showcase_{role}_{index}", "object": "chat.completion.chunk",
                                 "created": 0, "model": role,
                                 "choices": [{"index": 0, "delta": next_delta, "finish_reason": finish}]}
                        self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                    self.wfile.write(b"data: [DONE]\n\n")
                    self.wfile.flush()
                except Exception as error:
                    with provider._condition:
                        if not provider._closed:
                            provider._errors.append(str(sanitize(str(error))))
                        provider._condition.notify_all()
                    try:
                        self.send_error(500, "Scripted provider rejected request")
                    except OSError:
                        pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = False
        self.base_url = f"http://127.0.0.1:{self._server.server_port}/v1"
        self._thread = threading.Thread(target=self._server.serve_forever, name="showcase-provider", daemon=True)
        self._thread.start()

    @property
    def receipts(self) -> list[dict[str, Any]]:
        """Sanitized native result receipts, deduplicated by emitted tool-call ID."""
        with self._condition:
            return sanitize(deepcopy(self._receipts))

    def tool(self, role: str, name: str, arguments: dict[str, Any] | None = None) -> None:
        """Queue a Bkpr MCP operation, translated through the native tool inventory."""
        if not re.fullmatch(r"[a-z][a-z0-9_]*", name):
            raise ValueError("Invalid Bkpr MCP tool name")
        self._enqueue(role, {"kind": "tool", "name": name, "arguments": deepcopy(arguments or {})})

    def stop(self, role: str, text: str) -> None:
        """Queue a final assistant response; this never fabricates a tool result."""
        self._enqueue(role, {"kind": "stop", "text": text})

    def _enqueue(self, role: str, action: dict[str, Any]) -> None:
        _role(role)
        with self._condition:
            if self._closed:
                raise RuntimeError("Scripted provider is closed")
            self._actions[role].append(action)
            self._condition.notify_all()

    def _response(self, role: str, index: int, body: dict, action: dict) -> tuple[dict, str]:
        if action["kind"] == "stop":
            return {"role": "assistant", "content": action["text"]}, "stop"
        name, arguments = action["name"], action["arguments"]
        inventory = {tool["function"]["name"] for tool in body.get("tools", [])}
        if role == "pi":
            tool = "codemode"
            code = (f"const r = await tools.mcp__bkpr__{name}({json.dumps(arguments)});\n"
                    f'text("BKPR_RESULT:" + JSON.stringify({{name:{json.dumps(name)},result:r.structuredContent ?? r}}));')
            parameters = {"code": code}
        else:
            tool = "write"
            route = f"xd://mcp__bkpr_{name}"
            advertised = re.findall(r"xd://[A-Za-z0-9_]+", json.dumps(body.get("messages", [])))
            if route not in advertised:
                raise ValueError(f"Native OMP did not advertise {route}")
            parameters = {"i": "Coordinating showcase work", "path": route, "content": json.dumps(arguments)}
        if tool not in inventory:
            raise ValueError(f"Native {role} did not offer required {tool} tool")
        call_id = f"showcase_{role}_{index}"
        self._calls[role][call_id] = name
        return {"role": "assistant", "tool_calls": [{"index": 0, "id": call_id, "type": "function",
                "function": {"name": tool, "arguments": json.dumps(parameters)}}]}, "tool_calls"

    def _observe(self, role: str, body: dict) -> None:
        for message in body.get("messages", []):
            if message.get("role") != "tool":
                continue
            call_id = message.get("tool_call_id")
            name = self._calls[role].get(call_id)
            if name is None or (role, call_id) in self._seen:
                continue
            content = _text(message.get("content"))
            if role == "pi":
                marker = "BKPR_RESULT:"
                if marker not in content:
                    raise ValueError(f"Native Pi tool {name} returned no MCP result marker")
                wrapped, _ = json.JSONDecoder().raw_decode(content.split(marker, 1)[1])
                if wrapped["name"] != name:
                    raise ValueError("Native Pi result marker does not match emitted tool")
                result = _decode_result(wrapped["result"])
            else:
                result = _decode_result(content)
            self._seen.add((role, call_id))
            occurrence = 1 + sum(row["role"] == role and row["name"] == name for row in self._receipts)
            self._receipts.append({"role": role, "name": name, "occurrence": occurrence,
                                   "tool_call_id": call_id, "result": result,
                                   "observed_in_request": self._counts[role]})

    def wait_result(self, role: str, name: str, occurrence: int = 1, timeout: float = 60) -> Any:
        """Wait for a genuine result in native model context; return its decoded payload."""
        _role(role)
        if occurrence < 1 or timeout < 0:
            raise ValueError("Occurrence must be positive and timeout nonnegative")
        with self._condition:
            def ready() -> bool:
                return (self._closed or bool(self._errors) or any(
                    row["role"] == role and row["name"] == name and row["occurrence"] == occurrence
                    for row in self._receipts))
            if not self._condition.wait_for(ready, timeout):
                raise TimeoutError(f"No native {role}/{name} result #{occurrence} within {timeout}s")
            for row in self._receipts:
                if row["role"] == role and row["name"] == name and row["occurrence"] == occurrence:
                    return deepcopy(row["result"])
            if self._errors:
                raise RuntimeError("Scripted provider failed: " + "; ".join(self._errors))
            raise RuntimeError("Scripted provider closed before the native tool result")

    def close(self) -> None:
        """Wake blocked handlers/waiters, stop listening, and join owned threads."""
        with self._condition:
            if self._closed:
                return
            self._closed = True
            self._condition.notify_all()
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()
        _json_file(self._evidence / "provider-receipts.json", self.receipts)
        _json_file(self._evidence / "provider-summary.json", {
            "requests": self._counts, "utility_requests": self._helper_count, "errors": sanitize(self._errors),
            "closed": True, "external_model_calls": 0,
            "disclosure": "Scripted loopback model; tools executed by genuine native harnesses.",
        })
