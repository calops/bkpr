"""Unit API clients exercise provider gates, not genuine native acceptance."""
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection, HTTPSConnection
import json
from pathlib import Path
import ssl
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from provider import PreflightProxy, ScriptedProvider, _claude_mcp_result, launch_arguments, native_harness, prepare_profiles
from runtime import native, preflight_certificates


def request(provider, role, messages, tools, *, model=None, path=None):
    endpoint = urlsplit(provider.base_url)
    connection = HTTPConnection(endpoint.hostname, endpoint.port, timeout=5)
    match role:
        case "pi":
            route = "/v1/chat/completions"
            body = {"model": model or "pi", "stream": True, "messages": messages,
                    "tools": [{"type": "function", "function": {"name": name, "parameters": {}}}
                              for name in tools]}
        case "claude":
            route = "/v1/messages?beta=true"
            body = {"model": model or "claude-sonnet-4-6", "stream": True, "max_tokens": 4096,
                    "messages": messages, "tools": [{"name": name, "input_schema": {}} for name in tools]}
        case "codex":
            route = "/v1/responses"
            body = {"model": model or "codex", "stream": True, "input": messages,
                    "tools": [{"type": "namespace", "name": "mcp__bkpr", "tools": [
                        {"type": "function", "name": name, "parameters": {}} for name in tools]}]}
    try:
        connection.request("POST", path or route, json.dumps(body), {"Content-Type": "application/json"})
        response = connection.getresponse()
        raw = response.read().decode()
        if response.status != 200:
            return response.status, raw
        if response.getheader("Content-Type") == "application/json":
            return response.status, json.loads(raw)
        chunks = [json.loads(line.removeprefix("data: "))
                  for line in raw.splitlines() if line.startswith("data: ") and line != "data: [DONE]"]
        return response.status, chunks
    finally:
        connection.close()


def call(chunks, role):
    match role:
        case "pi":
            item = chunks[0]["choices"][0]["delta"]["tool_calls"][0]
            return item["id"], item["function"]["name"], json.loads(item["function"]["arguments"])
        case "claude":
            item = next(chunk["content_block"] for chunk in chunks if chunk["type"] == "content_block_start")
            arguments = next(chunk["delta"]["partial_json"] for chunk in chunks if chunk["type"] == "content_block_delta")
            return item["id"], item["name"], json.loads(arguments)
        case "codex":
            item = next(chunk["item"] for chunk in chunks if chunk["type"] == "response.output_item.done")
            return item["call_id"], item["name"], json.loads(item["arguments"])


def result(role, identifier, name, value, *, failed=False):
    match role:
        case "pi":
            content = "BKPR_RESULT:" + json.dumps({"name": name, "result": value})
            return {"role": "tool", "tool_call_id": identifier, "content": content}
        case "claude":
            return {"role": "user", "content": [{"type": "tool_result", "tool_use_id": identifier,
                                                "content": value if isinstance(value, str) else json.dumps(value), "is_error": failed}]}
        case "codex":
            return {"type": "function_call_output", "call_id": identifier, "output": json.dumps(value)}


class ScriptedProviderTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.evidence = Path(temporary.name)
        self.provider = ScriptedProvider(self.evidence)
        self.addCleanup(temporary.cleanup)
        self.addCleanup(self.provider.close)

    def test_claude_utilities_do_not_consume_native_tool_plan(self):
        self.provider.tool("claude", "whoami")
        status, chunks = request(self.provider, "claude", [], [])
        self.assertEqual(status, 200)
        self.assertEqual(chunks[-1]["type"], "message_stop")
        status, body = request(self.provider, "claude", [], [], path="/v1/messages/count_tokens")
        self.assertEqual((status, body), (200, {"input_tokens": 20}))
        _, chunks = request(self.provider, "claude", [], ["mcp__bkpr__whoami"])
        self.assertEqual(call(chunks, "claude")[1], "mcp__bkpr__whoami")
        self.assertEqual(self.provider.receipts, [])

    def test_claude_waits_once_for_native_mcp_inventory_without_consuming_the_broker_call(self):
        self.provider.tool("claude", "whoami")
        _, chunks = request(self.provider, "claude", [], ["WaitForMcpServers"])
        wait_id, tool, arguments = call(chunks, "claude")
        self.assertEqual((tool, arguments), ("WaitForMcpServers", {"servers": ["bkpr"]}))
        with self.assertRaises(TimeoutError):
            self.provider.wait_result("claude", "whoami", timeout=0)
        ready = """ready: true
Connected (their tools are now available — call them directly): bkpr
"""
        messages = [result("claude", wait_id, "native_mcp_startup", ready)]
        _, chunks = request(self.provider, "claude", messages, ["mcp__bkpr__whoami"])
        call_id, tool, arguments = call(chunks, "claude")
        self.assertEqual((tool, arguments), ("mcp__bkpr__whoami", {}))
        self.provider.stop("claude", "Identity observed.")
        messages.append(result("claude", call_id, "whoami", {"launch_id": "actual-native"}))
        request(self.provider, "claude", messages, ["mcp__bkpr__whoami"])
        self.assertEqual(self.provider.wait_result("claude", "whoami", timeout=0), {"launch_id": "actual-native"})
        self.assertEqual([receipt["name"] for receipt in self.provider.receipts], ["native_mcp_startup", "whoami"])

    def test_native_wait_failure_never_emits_the_queued_broker_mutation(self):
        self.provider.tool("claude", "send_message", {"to": "peer", "text": "not emitted"})
        _, chunks = request(self.provider, "claude", [], ["WaitForMcpServers"])
        identifier, _, _ = call(chunks, "claude")
        status, _ = request(self.provider, "claude", [
            result("claude", identifier, "native_mcp_startup", "ready: false")], ["mcp__bkpr__send_message"])
        self.assertEqual(status, 500)
        with self.assertRaisesRegex(RuntimeError, "did not report ready"):
            self.provider.wait_result("claude", "send_message", timeout=0)
        self.assertEqual([call["name"] for call in self.provider._calls["claude"].values()], ["native_mcp_startup"])

    def test_native_startup_wait_is_not_polled_or_repeated(self):
        self.provider.tool("claude", "whoami")
        _, chunks = request(self.provider, "claude", [], ["WaitForMcpServers"])
        identifier, _, _ = call(chunks, "claude")
        ready = """ready: true
Connected (their tools are now available — call them directly): bkpr
"""
        status, _ = request(self.provider, "claude", [
            result("claude", identifier, "native_mcp_startup", ready)], ["WaitForMcpServers"])
        self.assertEqual(status, 500)
        self.assertEqual([call["name"] for call in self.provider._calls["claude"].values()], ["native_mcp_startup"])

    def test_native_api_results_require_exact_emitted_call_and_role(self):
        for role in ("claude", "pi", "codex"):
            with self.subTest(role=role):
                tools = {"pi": ["codemode"], "claude": ["mcp__bkpr__whoami"], "codex": ["whoami"]}[role]
                self.provider.tool(role, "whoami")
                status, chunks = request(self.provider, role, [], tools)
                self.assertEqual(status, 200)
                identifier, name, arguments = call(chunks, role)
                self.assertEqual(name, tools[0])
                self.assertEqual(self.provider.pending_call(role), {
                    "tool_call_id": identifier, "name": "whoami", "arguments": {}})
                if role == "codex":
                    item = next(chunk["item"] for chunk in chunks if chunk["type"] == "response.output_item.done")
                    self.assertEqual(item["namespace"], "mcp__bkpr")
                if role == "pi":
                    self.assertIn("await tools.mcp__bkpr__whoami({})", arguments["code"])
                    self.assertIn("result:r.structuredContent ?? r", arguments["code"])
                else:
                    self.assertEqual(arguments, {})
                with self.assertRaises(TimeoutError):
                    self.provider.wait_result(role, "whoami", timeout=0)
                self.provider.stop(role, "Identity verified.")
                request(self.provider, role, [result(role, identifier, "whoami", {
                    "structuredContent": {"launch_id": role}})], tools)
                self.assertEqual(self.provider.wait_result(role, "whoami", timeout=0), {"launch_id": role})
                self.assertIsNone(self.provider.pending_call(role))
        self.assertEqual({row["role"] for row in self.provider.receipts}, {"claude", "pi", "codex"})

    def test_wrong_and_cross_role_ids_never_create_receipts(self):
        self.provider.tool("claude", "whoami")
        _, chunks = request(self.provider, "claude", [], ["mcp__bkpr__whoami"])
        identifier, _, _ = call(chunks, "claude")
        self.provider.stop("codex", "No receipt.")
        request(self.provider, "codex", [result("codex", identifier, "whoami", {"launch_id": "wrong"})], [])
        self.provider.stop("claude", "No receipt.")
        request(self.provider, "claude", [result("claude", "invented", "whoami", {"launch_id": "wrong"})], ["mcp__bkpr__whoami"])
        self.assertEqual(self.provider.receipts, [])

    def test_occurrences_follow_distinct_emitted_calls_and_deduplicate_history(self):
        messages = []
        for count in (1, 2):
            self.provider.tool("claude", "read_inbox")
            _, chunks = request(self.provider, "claude", messages, ["mcp__bkpr__read_inbox"])
            identifier, _, _ = call(chunks, "claude")
            messages.append(result("claude", identifier, "read_inbox", {"count": count}))
        self.provider.stop("claude", "Done.")
        request(self.provider, "claude", messages, ["mcp__bkpr__read_inbox"])
        self.assertEqual(self.provider.wait_result("claude", "read_inbox", occurrence=2, timeout=0), {"count": 2})
        self.assertEqual(len(self.provider.receipts), 2)

    def test_pending_call_refuses_ambiguous_unobserved_native_calls(self):
        self.provider.tool("claude", "whoami")
        request(self.provider, "claude", [], ["mcp__bkpr__whoami"])
        self.provider.tool("claude", "read_inbox")
        request(self.provider, "claude", [], ["mcp__bkpr__read_inbox"])
        with self.assertRaisesRegex(RuntimeError, "Ambiguous"):
            self.provider.pending_call("claude")

    def test_missing_advertised_tool_fails_closed(self):
        self.provider.tool("claude", "whoami")
        status, _ = request(self.provider, "claude", [], ["mcp__other__whoami"])
        self.assertEqual(status, 500)
        with self.assertRaisesRegex(RuntimeError, "did not offer required mcp__bkpr__whoami"):
            self.provider.wait_result("claude", "whoami", timeout=0)
        self.assertEqual(self.provider.receipts, [])

    def test_codex_does_not_guess_namespace_function_names(self):
        self.provider.tool("codex", "whoami")
        status, _ = request(self.provider, "codex", [], ["other_whoami"])
        self.assertEqual(status, 500)
        with self.assertRaisesRegex(RuntimeError, "did not offer required mcp__bkpr.whoami"):
            self.provider.wait_result("codex", "whoami", timeout=0)
        self.assertEqual(self.provider.receipts, [])

    def test_pi_marker_must_name_exact_emitted_operation(self):
        self.provider.tool("pi", "whoami")
        _, chunks = request(self.provider, "pi", [], ["codemode"])
        identifier, _, _ = call(chunks, "pi")
        self.provider.stop("pi", "Never accepted.")
        status, _ = request(self.provider, "pi", [result("pi", identifier, "send_message", {})], ["codemode"])
        self.assertEqual(status, 500)
        self.assertEqual(self.provider.receipts, [])

    def test_native_claude_error_flag_is_preserved(self):
        self.provider.tool("claude", "whoami")
        _, chunks = request(self.provider, "claude", [], ["mcp__bkpr__whoami"])
        identifier, _, _ = call(chunks, "claude")
        self.provider.stop("claude", "No success claim.")
        request(self.provider, "claude", [result("claude", identifier, "whoami", "Denied", failed=True)], ["mcp__bkpr__whoami"])
        self.assertTrue(self.provider.wait_result("claude", "whoami", timeout=0)["isError"])

    def test_endpoint_cannot_select_another_roles_model(self):
        status, _ = request(self.provider, "pi", [], ["codemode"], model="codex")
        self.assertEqual(status, 500)
        self.assertEqual(self.provider.receipts, [])

    def test_sanitized_evidence_preserves_public_ids_not_credentials(self):
        self.provider.tool("pi", "whoami")
        _, chunks = request(self.provider, "pi", [], ["codemode"])
        identifier, _, _ = call(chunks, "pi")
        self.provider.stop("pi", "Done.")
        request(self.provider, "pi", [result("pi", identifier, "whoami", {
            "structuredContent": {"launch_id": "public-id", "token": "private-fixture-value"}})], ["codemode"])
        self.assertEqual(self.provider.receipts[0]["result"], {"launch_id": "public-id", "token": "[REDACTED]"})
        self.provider.close()
        for path in self.evidence.glob("*.json"):
            self.assertNotIn("private-fixture-value", path.read_text())

    def test_close_releases_waiters_and_is_idempotent(self):
        with ThreadPoolExecutor() as executor:
            pending = executor.submit(self.provider.wait_result, "claude", "whoami", timeout=5)
            self.provider.close()
            with self.assertRaisesRegex(RuntimeError, "closed"):
                pending.result(timeout=5)
        self.provider.close()
        with self.assertRaisesRegex(RuntimeError, "closed"):
            self.provider.stop("pi", "Never sent")

    def test_close_releases_a_handler_waiting_for_a_plan(self):
        with ThreadPoolExecutor() as executor:
            pending = executor.submit(request, self.provider, "codex", [], [])
            with self.provider._condition:
                self.assertTrue(self.provider._condition.wait_for(lambda: self.provider._counts["codex"] == 1, 5))
            self.provider.close()
            self.assertEqual(pending.result(timeout=5)[0], 503)


class NativeResultTests(unittest.TestCase):
    def test_native_budget_annotation_preserves_the_exact_broker_json_and_error(self):
        annotation = """<system-reminder>
<total_tokens>14999970 tokens left</total_tokens>
</system-reminder>"""
        identity = {"launch_id": "actual-native", "conversation": {"id": "native-session"}}
        self.assertEqual(_claude_mcp_result(json.dumps(identity) + "\n\n" + annotation), identity)
        error = {"isError": True, "content": "Broker rejected this operation"}
        self.assertEqual(_claude_mcp_result(json.dumps(error) + "\n\n" + annotation), error)

    def test_unknown_trailers_cannot_be_mistaken_for_native_result_annotations(self):
        for trailer in ("arbitrary text", "{}", "<system-reminder>different instructions</system-reminder>"):
            with self.subTest(trailer=trailer):
                with self.assertRaisesRegex(ValueError, "unknown non-JSON trailer"):
                    _claude_mcp_result('{"launch_id":"actual-native"}' + trailer)


class PreflightProxyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.root = Path(temporary.name)
        self.addCleanup(temporary.cleanup)

        class Commands:
            def command(self, arguments):
                return subprocess.run(list(map(str, arguments)), env={}, stdin=subprocess.DEVNULL,
                                      capture_output=True, check=True)

        self.ca, certificate, key = preflight_certificates(Commands(), native("openssl"), self.root)
        evidence = self.root / "evidence"
        evidence.mkdir(mode=0o700)
        self.proxy = PreflightProxy(evidence, certificate, key)
        self.addCleanup(self.proxy.close)
        self.endpoint = urlsplit(self.proxy.url)

    def connect(self, host, *, trust=True):
        context = ssl.create_default_context(cafile=str(self.ca)) if trust else ssl.create_default_context()
        connection = HTTPSConnection(self.endpoint.hostname, self.endpoint.port, timeout=5, context=context)
        connection.set_tunnel(host, 443)
        self.addCleanup(connection.close)
        return connection

    def test_only_exact_host_path_pairs_are_served_with_verified_tls(self):
        for host, path in PreflightProxy.PATHS.items():
            connection = self.connect(host)
            connection.request("GET", path)
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(), b"{}")

    def test_other_hosts_paths_and_authentication_are_rejected(self):
        connection = self.connect("api.anthropic.com")
        connection.request("GET", "/v1/oauth/token")
        response = connection.getresponse()
        self.assertEqual(response.status, 403)
        response.read()
        connection = self.connect("platform.claude.com")
        connection.request("POST", "/v1/oauth/token", body=b"")
        response = connection.getresponse()
        self.assertEqual(response.status, 403)
        response.read()
        with self.assertRaisesRegex(OSError, "403"):
            self.connect("example.com").request("GET", "/api/hello")

    def test_fixture_ca_is_required_and_never_written_to_evidence(self):
        with self.assertRaises(ssl.SSLCertVerificationError):
            self.connect("api.anthropic.com", trust=False).request("GET", "/api/hello")
        self.proxy.close()
        files = list((self.root / "evidence").iterdir())
        self.assertEqual([path.name for path in files], ["preflight-summary.json"])
        self.assertNotIn("PRIVATE KEY", files[0].read_text())
        self.assertEqual(json.loads(files[0].read_text())["upstream_connections"], 0)


class NativeProfileTests(unittest.TestCase):
    def test_rejects_credential_wrapper_without_executing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            wrapper = Path(directory) / "claude"
            wrapper.write_text("""#!/bin/sh
exec credential-manager read secret
""")
            with patch("provider.shutil.which", return_value=str(wrapper)):
                with self.assertRaisesRegex(ValueError, "profile wrapper"):
                    native_harness("claude")

    def test_profiles_are_private_supported_provider_configuration_without_approvals(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            native = home / "native"
            native.write_bytes(b"\x7fELF")
            with patch("provider.native_harness", return_value=native):
                additions = prepare_profiles(home, "http://127.0.0.1:1234/v1")
            pi = json.loads((home / ".pi/agent/models.json").read_text())
            self.assertEqual(pi["providers"]["showcase"]["models"][0]["id"], "pi")
            codex = tomllib.loads((home / ".codex/config.toml").read_text())
            self.assertEqual(codex["model_providers"]["showcase"]["wire_api"], "responses")
            self.assertFalse(codex["model_providers"]["showcase"]["requires_openai_auth"])
            self.assertEqual(additions["ANTHROPIC_BASE_URL"], "http://127.0.0.1:1234")
            self.assertEqual(additions["ENABLE_TOOL_SEARCH"], "false")
            self.assertEqual(list((home / ".claude").iterdir()), [])
            self.assertFalse((home / ".claude.json").exists())
            self.assertFalse((home / ".omp").exists())
            self.assertNotIn("PI_CODING_AGENT_DIR", additions)
            self.assertNotIn("PI_PACKAGE_DIR", additions)
            for path in (*home.glob(".pi/agent/*"), home / ".codex/config.toml"):
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertNotIn("approval", path.read_text().lower())
            arguments = launch_arguments("claude")
            self.assertEqual(arguments[arguments.index("--permission-mode") + 1], "default")
            for role in ("claude", "pi", "codex"):
                arguments = launch_arguments(role)
                self.assertFalse(any("skip-permission" in item or "yolo" in item for item in arguments))

    def test_profiles_reject_non_loopback_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "127.0.0.1"):
                prepare_profiles(Path(directory), "https://api.example.com/v1")
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
