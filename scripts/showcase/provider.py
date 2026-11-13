"""Loopback scripted APIs; genuine native harnesses execute every Bkpr MCP call.

The controller supplies a private HOME and a loopback-only network namespace.
No native trust, onboarding or tool approval state is created here.
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
import ssl
import threading
from typing import Any
from urllib.parse import urlsplit

_ROLES = ("claude", "pi", "codex")
_PROVIDER = "showcase"
_PROMPT = "Coordinate a Unicode token-boundary review of the search-index patch through Bkpr, then summarize the handoff."
_MAX_BODY = 2 * 1024 * 1024
_SENSITIVE = re.compile(r"token|authorization|api[_-]?key|secret|password|credential", re.I)
FIXTURE_API_KEY = "offline-not-a-credential"


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


def _claude_mcp_result(content: str) -> Any:
    text = content.lstrip()
    value, offset = json.JSONDecoder().raw_decode(text)
    trailer = text[offset:].strip()
    if trailer and not re.fullmatch(
            r"<system-reminder>\s*<total_tokens>\d+ tokens left</total_tokens>\s*</system-reminder>", trailer):
        raise ValueError("Native Claude MCP result has an unknown non-JSON trailer")
    return _decode_result(value)


def _role(name: str) -> None:
    if name not in _ROLES:
        raise ValueError(f"Unknown showcase harness: {name!r}")


def native_harness(name: str) -> Path:
    """Inspect static Nix wrappers without executing profile or credential helpers."""
    _role(name)
    found = shutil.which(name)
    if found is None:
        raise FileNotFoundError(f"Native {name} missing; enter nix develop .#showcase")
    path = Path(found).resolve(strict=True)
    with path.open("rb") as stream:
        if stream.read(4) == b"\x7fELF":
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
        r"|export (?:PI_SKIP_VERSION_CHECK='1'|PI_TELEMETRY='0'|DISABLE_AUTOUPDATER='1'|DISABLE_INSTALLATION_CHECKS='1')"
        r"|export PI_PACKAGE_DIR='/nix/store/[\w.+/-]+'"
        r"|export (?:FORCE_AUTOUPDATE_PLUGINS|DISABLE_NON_ESSENTIAL_MODEL_CALLS)=\$\{(?:FORCE_AUTOUPDATE_PLUGINS|DISABLE_NON_ESSENTIAL_MODEL_CALLS)-'1'\}"
        r"|export CLAUDE_CODE_CERT_STORE=\$\{CLAUDE_CODE_CERT_STORE-'system'\}"
    )
    if any(not assignment.fullmatch(line) for line in lines[1:-1]):
        raise ValueError(f"Refusing non-static Nix wrapper: {path}")
    target = re.fullmatch(r'exec (?:-a "claude" )?"(/nix/store/[\w.+/-]+)"\s+"\$@"\s*', lines[-1])
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
    """Keep prompts and model selection on each harness's supported native CLI."""
    _role(name)
    match name:
        case "pi":
            return ["--model", f"{_PROVIDER}/pi", "--thinking", "off", "--", _PROMPT]
        case "claude":
            return ["--model", "claude-sonnet-4-6", "--effort", "low", "--permission-mode", "default", "--", _PROMPT]
        case "codex":
            return ["--no-alt-screen", "Stand by for patch review while Claude and Pi coordinate the Unicode handoff. Do not call tools."]


def prepare_profiles(home: Path, base_url: str) -> dict[str, str]:
    """Create supported private provider settings without native consent state."""
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
    pi = home / ".pi"
    pi.mkdir(mode=0o700)
    agent = pi / "agent"
    agent.mkdir(mode=0o700)
    model = {"id": "pi", "name": "Scripted Pi", "reasoning": False,
             "input": ["text"], "contextWindow": 128000, "maxTokens": 4096,
             "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}
    _json_file(agent / "models.json", {"providers": {_PROVIDER: {
        "api": "openai-completions", "baseUrl": base_url,
        "apiKey": FIXTURE_API_KEY, "models": [model]}}})
    _json_file(agent / "settings.json", {
        "defaultProvider": _PROVIDER, "defaultModel": "pi", "theme": "dark",
        "compaction": {"enabled": False}, "cacheWarming": "off",
        "retry": {"enabled": False}, "enableInstallTelemetry": False,
        "defaultTools": ["+codemode"],
    })
    _json_file(agent / "mcp.json", {"mcpServers": {}, "autoEnableCodemode": True})
    claude = home / ".claude"
    claude.mkdir(mode=0o700)
    codex = home / ".codex"
    codex.mkdir(mode=0o700)
    config = codex / "config.toml"
    config.write_text(f'''model = "codex"
model_provider = "showcase"
check_for_update_on_startup = false
model_reasoning_effort = "low"
[model_providers.showcase]
name = "Scripted loopback demo"
base_url = "{base_url}"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
''')
    config.chmod(0o600)
    return {"HOME": str(home), "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', os.defpath)}",
            "PI_OFFLINE": "1", "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0", "PI_NO_TITLE": "1",
            "CLAUDE_CONFIG_DIR": str(claude), "CODEX_HOME": str(codex),
            "ANTHROPIC_BASE_URL": base_url.removesuffix("/v1"), "ANTHROPIC_API_KEY": FIXTURE_API_KEY,
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_NON_ESSENTIAL_MODEL_CALLS": "1",
            "DISABLE_AUTOUPDATER": "1", "DISABLE_INSTALLATION_CHECKS": "1", "ENABLE_TOOL_SEARCH": "false",
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1", "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1",
            "OTEL_SDK_DISABLED": "true"}


def _events(role: str, index: int, model: str, response: dict) -> bytes:
    identifier = f"showcase_{role}_{index}"
    tool = response.get("tool")
    if role == "pi":
        delta = {"role": "assistant"}
        if tool:
            delta["tool_calls"] = [{"index": 0, "id": identifier, "type": "function",
                                    "function": {"name": tool, "arguments": json.dumps(response["arguments"])}}]
        else:
            delta["content"] = response["text"]
        chunks = [{"id": identifier, "object": "chat.completion.chunk", "created": 0, "model": model,
                   "choices": [{"index": 0, "delta": value, "finish_reason": finish}]}
                  for value, finish in ((delta, None), ({}, "tool_calls" if tool else "stop"))]
        return ("".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
    if role == "claude":
        block = ({"type": "tool_use", "id": identifier, "name": tool, "input": response["arguments"]}
                 if tool else {"type": "text", "text": response["text"]})
        message = {"id": identifier, "type": "message", "role": "assistant", "model": model,
                   "content": [], "stop_reason": None, "stop_sequence": None,
                   "usage": {"input_tokens": 20, "output_tokens": 0}}
        events = [("message_start", {"message": message}),
                  ("content_block_start", {"index": 0, "content_block":
                      {**block, "input": {}} if tool else {"type": "text", "text": ""}}),
                  ("content_block_delta", {"index": 0, "delta":
                      {"type": "input_json_delta", "partial_json": json.dumps(response["arguments"])}
                      if tool else {"type": "text_delta", "text": response["text"]}}),
                  ("content_block_stop", {"index": 0}),
                  ("message_delta", {"delta": {"stop_reason": "tool_use" if tool else "end_turn", "stop_sequence": None},
                                     "usage": {"output_tokens": 10}}),
                  ("message_stop", {})]
    else:
        item = ({"id": identifier, "type": "function_call", "call_id": identifier,
                 "name": tool, "arguments": json.dumps(response["arguments"]), "status": "completed"}
                if tool else {"id": identifier, "type": "message", "role": "assistant", "status": "completed",
                              "content": [{"type": "output_text", "text": response["text"], "annotations": []}]})
        if tool:
            item["namespace"] = response["namespace"]
        result = {"id": f"resp_{identifier}", "object": "response", "created_at": 0, "status": "completed",
                  "model": model, "output": [item],
                  "usage": {"input_tokens": 20, "output_tokens": 10, "total_tokens": 30}}
        events = [("response.created", {"response": {**result, "status": "in_progress", "output": []}}),
                  ("response.output_item.added", {"output_index": 0, "item":
                      {**item, "status": "in_progress", "arguments": ""} if tool else {**item, "status": "in_progress", "content": []}})]
        if tool:
            events.extend((
                ("response.function_call_arguments.delta", {"item_id": identifier, "output_index": 0, "delta": item["arguments"]}),
                ("response.function_call_arguments.done", {"item_id": identifier, "output_index": 0, "arguments": item["arguments"]})))
        else:
            events.extend((
                ("response.content_part.added", {"item_id": identifier, "output_index": 0, "content_index": 0,
                                                  "part": {"type": "output_text", "text": "", "annotations": []}}),
                ("response.output_text.delta", {"item_id": identifier, "output_index": 0, "content_index": 0, "delta": response["text"]}),
                ("response.output_text.done", {"item_id": identifier, "output_index": 0, "content_index": 0, "text": response["text"]}),
                ("response.content_part.done", {"item_id": identifier, "output_index": 0, "content_index": 0, "part": item["content"][0]})))
        events.extend((("response.output_item.done", {"output_index": 0, "item": item}),
                       ("response.completed", {"response": result})))
    return "".join(f"event: {kind}\ndata: {json.dumps({'type': kind, **body})}\n\n" for kind, body in events).encode()


class PreflightProxy:
    """Terminate only two native HTTPS connectivity probes; never forward a request."""

    PATHS = {"api.anthropic.com": "/api/hello", "platform.claude.com": "/v1/oauth/hello"}

    def __init__(self, evidence: Path, certificate: Path, key: Path):
        self._evidence = evidence
        self._receipts = []
        self._lock = threading.Lock()
        self._closed = False
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(certificate, key)
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            timeout = 10

            def log_message(self, *_):
                pass

            def do_CONNECT(self):
                host, separator, port = self.path.partition(":")
                if not separator or port != "443" or host not in proxy.PATHS:
                    proxy._record("CONNECT", "rejected", "rejected", 403)
                    self.send_error(403, "No upstream connections are permitted")
                    return
                self.close_connection = True
                self.connection.settimeout(10)
                self.send_response(200, "Connection established")
                self.end_headers()

                class Probe(BaseHTTPRequestHandler):
                    timeout = 10

                    def log_message(self, *_):
                        pass

                    def do_GET(self):
                        valid = (self.path == proxy.PATHS[host]
                                 and self.headers.get("Host") in (host, host + ":443"))
                        proxy._record("GET", host, self.path if valid else "rejected", 200 if valid else 403)
                        self.close_connection = True
                        if not valid:
                            self.send_error(403, "Only the exact native connectivity probe is permitted")
                            return
                        body = b"{}"
                        self.send_response(200)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Content-Length", str(len(body)))
                        self.end_headers()
                        self.wfile.write(body)

                    def do_POST(self):
                        proxy._record("POST", host, "rejected", 403)
                        self.send_error(403, "Authentication and OAuth endpoints are not implemented")

                try:
                    with context.wrap_socket(self.connection, server_side=True) as connection:
                        Probe(connection, self.client_address, self.server)
                except OSError as error:
                    proxy._record("TLS", host, type(error).__name__, 0)

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = False
        self.url = f"http://127.0.0.1:{self._server.server_port}"
        self._thread = threading.Thread(target=self._server.serve_forever, name="showcase-preflight", daemon=True)
        self._thread.start()

    def _record(self, method, host, path, status):
        with self._lock:
            if len(self._receipts) >= 32:
                raise ValueError("Native preflight request count exceeds bound")
            self._receipts.append({"method": method, "host": host, "path": path, "status": status})

    def close(self):
        """Stop owned handlers and retain no CA or private key material."""
        if self._closed:
            return
        self._closed = True
        self._server.shutdown()
        self._server.server_close()
        self._thread.join()
        _json_file(self._evidence / "preflight-summary.json", {
            "requests": self._receipts, "upstream_connections": 0, "closed": True,
            "tls_verification": "Native private CA and normal hostname verification",
        })


class ScriptedProvider:
    """Serve native APIs and accept receipts only from subsequent native requests."""

    def __init__(self, evidence: Path):
        self._evidence = evidence
        evidence.mkdir(mode=0o700, parents=True, exist_ok=True)
        self._condition = threading.Condition()
        self._actions: dict[str, deque] = {role: deque() for role in _ROLES}
        self._calls: dict[str, dict[str, dict[str, Any]]] = {role: {} for role in _ROLES}
        self._counts = dict.fromkeys(_ROLES, 0)
        self._helper_count = 0
        self._mcp_waited = False
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
                    path = urlsplit(self.path).path
                    role = {"/v1/chat/completions": "pi", "/v1/messages": "claude",
                            "/v1/responses": "codex", "/v1/messages/count_tokens": "claude"}.get(path)
                    if role is None:
                        raise ValueError("Unexpected provider endpoint")
                    length = int(self.headers.get("Content-Length", "0"))
                    if not 0 < length <= _MAX_BODY:
                        raise ValueError("Provider request exceeds body bound")
                    raw = self.rfile.read(length)
                    if len(raw) != length:
                        raise ValueError("Truncated provider request")
                    body = json.loads(raw)
                    if role == "pi" and body.get("model") != "pi":
                        raise ValueError("Unexpected Pi model")
                    if role == "codex" and body.get("model") != "codex":
                        raise ValueError("Unexpected Codex model")
                    inventory = provider._inventory(role, body)
                    auxiliary = role == "claude" and not inventory
                    with provider._condition:
                        if path.endswith("/count_tokens") or auxiliary:
                            provider._helper_count += 1
                            index = provider._helper_count
                            if index > 40:
                                raise ValueError("Native utility request count exceeds bound")
                            _json_file(evidence / f"claude-utility-{index:02}.json", sanitize({"path": path, "body": body}))
                            if path.endswith("/count_tokens"):
                                data = b'{"input_tokens":20}'
                                content_type = "application/json"
                            else:
                                data = _events(role, index, body["model"], {"text": "Search-index Unicode review"})
                                content_type = "text/event-stream"
                        else:
                            provider._counts[role] += 1
                            index = provider._counts[role]
                            if index > 80:
                                raise ValueError("Provider request count exceeds bound")
                            provider._observe(role, body)
                            _json_file(evidence / f"{role}-request-{index:02}.json", sanitize(body))
                            provider._condition.notify_all()
                            provider._condition.wait_for(lambda: provider._closed or provider._errors or provider._actions[role])
                            if provider._closed or provider._errors:
                                self.send_error(503, "Scripted provider closed")
                                return
                            action = provider._actions[role].popleft()
                            response = provider._response(role, index, body, action)
                            data = _events(role, index, body["model"], response)
                            content_type = "text/event-stream"
                    self.send_response(200)
                    self.send_header("Content-Type", content_type)
                    self.send_header("Content-Length", str(len(data)))
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    self.wfile.write(data)
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

    @staticmethod
    def _inventory(role: str, body: dict) -> set[str]:
        if role == "pi":
            return {tool["function"]["name"] for tool in body.get("tools", [])}
        if role == "codex":
            return {f"{namespace['name']}.{tool['name']}"
                    for namespace in body.get("tools", []) if namespace.get("type") == "namespace"
                    for tool in namespace.get("tools", []) if tool.get("type") == "function"}
        return {tool["name"] for tool in body.get("tools", []) if "name" in tool}

    @property
    def receipts(self) -> list[dict[str, Any]]:
        """Sanitized native results, deduplicated by emitted native tool-call ID."""
        with self._condition:
            return sanitize(deepcopy(self._receipts))

    def pending_call(self, role: str) -> dict[str, Any] | None:
        """Return the one emitted native call without an observed result, never infer a call from UI text."""
        _role(role)
        with self._condition:
            pending = [{"tool_call_id": identifier, **call} for identifier, call in self._calls[role].items()
                       if (role, identifier) not in self._seen]
            if len(pending) > 1:
                raise RuntimeError("Ambiguous native calls cannot receive fixture UI approval")
            return deepcopy(pending[0]) if pending else None

    def tool(self, role: str, name: str, arguments: dict[str, Any] | None = None) -> None:
        """Queue an operation translated through the native advertised tool inventory."""
        if not re.fullmatch(r"[a-z][a-z0-9_]*", name):
            raise ValueError("Invalid Bkpr MCP tool name")
        self._enqueue(role, {"kind": "tool", "name": name, "arguments": deepcopy(arguments or {})})

    def stop(self, role: str, text: str) -> None:
        """Queue assistant text, never a fabricated tool result."""
        self._enqueue(role, {"kind": "stop", "text": text})

    def _enqueue(self, role: str, action: dict[str, Any]) -> None:
        _role(role)
        with self._condition:
            if self._closed:
                raise RuntimeError("Scripted provider is closed")
            self._actions[role].append(action)
            self._condition.notify_all()

    def _response(self, role: str, index: int, body: dict, action: dict) -> dict:
        if action["kind"] == "stop":
            return {"text": action["text"]}
        name, arguments = action["name"], action["arguments"]
        if role == "pi":
            tool = "codemode"
            code = f'''const r = await tools.mcp__bkpr__{name}({json.dumps(arguments)});
text("BKPR_RESULT:" + JSON.stringify({{name:{json.dumps(name)},result:r.structuredContent ?? r}}));'''
            parameters = {"code": code}
        elif role == "codex":
            tool = name
            parameters = arguments
        else:
            tool = f"mcp__bkpr__{name}"
            parameters = arguments
        advertised = f"mcp__bkpr.{tool}" if role == "codex" else tool
        inventory = self._inventory(role, body)
        if advertised not in inventory:
            if role == "claude" and not self._mcp_waited and "WaitForMcpServers" in inventory:
                self._mcp_waited = True
                self._actions[role].appendleft(action)
                self._calls[role][f"showcase_{role}_{index}"] = {
                    "name": "native_mcp_startup", "arguments": {"servers": ["bkpr"]}}
                return {"tool": "WaitForMcpServers", "arguments": {"servers": ["bkpr"]}}
            raise ValueError(f"Native {role} did not offer required {advertised} tool")
        self._calls[role][f"showcase_{role}_{index}"] = {"name": name, "arguments": deepcopy(arguments)}
        response = {"tool": tool, "arguments": parameters}
        if role == "codex":
            response["namespace"] = "mcp__bkpr"
        return response

    def _observe(self, role: str, body: dict) -> None:
        results = []
        if role == "claude":
            for message in body.get("messages", []):
                if message.get("role") != "user" or not isinstance(message.get("content"), list):
                    continue
                results.extend((block.get("tool_use_id"), block.get("content"), block.get("is_error", False))
                               for block in message["content"] if block.get("type") == "tool_result")
        elif role == "codex":
            results.extend((item.get("call_id"), item.get("output"), False)
                           for item in body.get("input", []) if item.get("type") == "function_call_output")
        else:
            results.extend((message.get("tool_call_id"), message.get("content"), False)
                           for message in body.get("messages", []) if message.get("role") == "tool")
        for call_id, value, failed in results:
            call = self._calls[role].get(call_id)
            if call is None or (role, call_id) in self._seen:
                continue
            name = call["name"]
            content = _text(value)
            if role == "pi":
                marker = "BKPR_RESULT:"
                if marker not in content:
                    raise ValueError(f"Native Pi tool {name} returned no MCP result marker")
                wrapped, _ = json.JSONDecoder().raw_decode(content.split(marker, 1)[1])
                if wrapped["name"] != name:
                    raise ValueError("Native Pi result marker does not match emitted tool")
                result = _decode_result(wrapped["result"])
            elif role == "claude" and name != "native_mcp_startup" and not failed:
                result = _claude_mcp_result(content)
            else:
                result = _decode_result(content)
            if failed:
                result = {"isError": True, "content": result}
            self._seen.add((role, call_id))
            occurrence = 1 + sum(row["role"] == role and row["name"] == name for row in self._receipts)
            self._receipts.append({"role": role, "name": name, "occurrence": occurrence,
                                   "tool_call_id": call_id, "result": result,
                                   "observed_in_request": self._counts[role]})
            if name == "native_mcp_startup" and (
                    failed or not isinstance(result, str)
                    or not re.match(r"\Aready: true\nConnected \(their tools are now available — call them directly\): bkpr(?:\n|$)", result)):
                raise ValueError("Native WaitForMcpServers did not report ready for the requested bkpr server")

    def wait_result(self, role: str, name: str, occurrence: int = 1, timeout: float = 60) -> Any:
        """Require a genuine following native request containing the exact emitted call."""
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
        """Wake blocked handlers and join all owned server threads."""
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
