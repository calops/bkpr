"""Unit HTTP clients model the provider boundary, not native-runtime acceptance."""
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import urlsplit

from provider import ScriptedProvider, native_harness, prepare_profiles


def request(provider, role, messages, tools):
    endpoint = urlsplit(provider.base_url)
    connection = HTTPConnection(endpoint.hostname, endpoint.port, timeout=5)
    try:
        connection.request("POST", "/v1/chat/completions", json.dumps({
            "model": role, "stream": True, "messages": messages,
            "tools": [{"type": "function", "function": {"name": name, "parameters": {}}}
                      for name in tools],
        }), {"Content-Type": "application/json"})
        response = connection.getresponse()
        raw = response.read().decode()
        if response.status != 200:
            return response.status, raw
        chunks = [json.loads(line.removeprefix("data: "))
                  for line in raw.splitlines() if line.startswith("data: ") and line != "data: [DONE]"]
        return response.status, chunks
    finally:
        connection.close()


def assistant(chunks):
    return chunks[0]["choices"][0]["delta"]


class ScriptedProviderTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.evidence = Path(self.temporary.name)
        self.provider = ScriptedProvider(self.evidence)
        self.addCleanup(self.temporary.cleanup)
        self.addCleanup(self.provider.close)

    def test_native_utility_requests_do_not_consume_agent_tool_plans(self):
        self.provider.tool("omp", "whoami")
        status, chunks = request(self.provider, "omp-helper", [{"role": "user", "content": "Compress a routing hint."}], [])
        self.assertEqual(status, 200)
        self.assertIn("authenticated peer messages", assistant(chunks)["content"])
        status, chunks = request(self.provider, "omp", [
            {"role": "system", "content": "xd://mcp__bkpr_whoami"}], ["write"])
        self.assertEqual(status, 200)
        self.assertEqual(assistant(chunks)["tool_calls"][0]["function"]["name"], "write")

    def test_omp_stream_selects_advertised_native_write_and_observes_result(self):
        self.provider.tool("omp", "whoami")
        messages = [{"role": "system", "content": '"whoami" → `xd://mcp__bkpr_whoami`'}]
        status, chunks = request(self.provider, "omp", messages, ["write", "read"])
        self.assertEqual(status, 200)
        call = assistant(chunks)["tool_calls"][0]
        self.assertEqual(call["function"]["name"], "write")
        arguments = json.loads(call["function"]["arguments"])
        self.assertEqual(arguments["path"], "xd://mcp__bkpr_whoami")
        self.assertEqual(json.loads(arguments["content"]), {})
        self.assertEqual(chunks[-1]["choices"][0]["finish_reason"], "tool_calls")
        with ThreadPoolExecutor() as executor:
            ready = executor.submit(self.provider.wait_result, "omp", "whoami", timeout=5)
            self.provider.stop("omp", "Ready to coordinate.")
            messages.extend([assistant(chunks), {"role": "tool", "tool_call_id": call["id"],
                                                "content": '{"launch_id":"owned-omp"}'}])
            request(self.provider, "omp", messages, ["write"])
            self.assertEqual(ready.result(timeout=5), {"launch_id": "owned-omp"})
        self.assertEqual(self.provider.receipts[0]["observed_in_request"], 2)

    def test_pi_codemode_stream_extracts_real_following_request_marker(self):
        self.provider.tool("pi", "set_title", {"title": "Search index"})
        _, chunks = request(self.provider, "pi", [], ["codemode", "bash"])
        call = assistant(chunks)["tool_calls"][0]
        self.assertEqual(call["function"]["name"], "codemode")
        code = json.loads(call["function"]["arguments"])["code"]
        self.assertIn('await tools.mcp__bkpr__set_title({"title": "Search index"})', code)
        self.assertIn("BKPR_RESULT:", code)
        self.assertIn("result:r.structuredContent ?? r", code)
        with self.assertRaises(TimeoutError):
            self.provider.wait_result("pi", "set_title", timeout=0)
        self.provider.stop("pi", "Index plan agreed.")
        result = {"content": [{"type": "text", "text": '{"title":"Search index"}'}],
                  "structuredContent": {"title": "Search index"}}
        messages = [assistant(chunks), {"role": "tool", "tool_call_id": call["id"],
                    "content": [{"type": "text", "text": "BKPR_RESULT:" + json.dumps({
                        "name": "set_title", "result": result}) + "\n"}]}]
        request(self.provider, "pi", messages, ["codemode"])
        self.assertEqual(self.provider.wait_result("pi", "set_title", timeout=0), {"title": "Search index"})
        self.provider.stop("pi", "Still ready.")
        request(self.provider, "pi", messages, ["codemode"])
        self.assertEqual(len(self.provider.receipts), 1)

    def test_occurrences_follow_distinct_emitted_tool_calls(self):
        messages = [{"role": "system", "content": "xd://mcp__bkpr_read_inbox"}]
        for count in (1, 2):
            self.provider.tool("omp", "read_inbox")
            _, chunks = request(self.provider, "omp", messages, ["write"])
            call = assistant(chunks)["tool_calls"][0]
            messages.extend([assistant(chunks), {"role": "tool", "tool_call_id": call["id"],
                                                "content": json.dumps({"count": count})}])
        self.provider.stop("omp", "Done.")
        request(self.provider, "omp", messages, ["write"])
        self.assertEqual(self.provider.wait_result("omp", "read_inbox", occurrence=2, timeout=0), {"count": 2})

    def test_missing_native_inventory_fails_without_fabricating_result(self):
        self.provider.tool("pi", "whoami")
        status, _ = request(self.provider, "pi", [], ["bash"])
        self.assertEqual(status, 500)
        with self.assertRaisesRegex(RuntimeError, "did not offer required codemode"):
            self.provider.wait_result("pi", "whoami", timeout=0)
        self.assertEqual(self.provider.receipts, [])

    def test_omp_does_not_guess_a_missing_mcp_route(self):
        self.provider.tool("omp", "whoami")
        status, _ = request(self.provider, "omp", [{"role": "system", "content": "xd://mcp__other_whoami"}], ["write"])
        self.assertEqual(status, 500)
        self.assertEqual(self.provider.receipts, [])

    def test_sanitized_evidence_and_receipts_preserve_public_ids(self):
        self.provider.tool("pi", "whoami")
        _, chunks = request(self.provider, "pi", [], ["codemode"])
        call = assistant(chunks)["tool_calls"][0]
        self.provider.stop("pi", "Done.")
        request(self.provider, "pi", [{"role": "tool", "tool_call_id": call["id"], "content":
            "BKPR_RESULT:" + json.dumps({"name": "whoami", "result": {"structuredContent": {
                "launch_id": "public-id", "token": "private-fixture-value"}}})}], ["codemode"])
        self.assertEqual(self.provider.receipts[0]["result"], {"launch_id": "public-id", "token": "[REDACTED]"})
        self.provider.close()
        for path in self.evidence.glob("*.json"):
            self.assertNotIn("private-fixture-value", path.read_text())

    def test_close_wakes_result_waiters_and_is_idempotent(self):
        with ThreadPoolExecutor() as executor:
            pending = executor.submit(self.provider.wait_result, "omp", "whoami", timeout=5)
            self.provider.close()
            with self.assertRaisesRegex(RuntimeError, "closed"):
                pending.result(timeout=5)
        self.provider.close()
        with self.assertRaisesRegex(RuntimeError, "closed"):
            self.provider.stop("pi", "Never sent")
        self.assertTrue(json.loads((self.evidence / "provider-summary.json").read_text())["closed"])

    def test_close_releases_a_handler_waiting_for_a_plan(self):
        with ThreadPoolExecutor() as executor:
            pending = executor.submit(request, self.provider, "omp", [], [])
            with self.provider._condition:
                self.assertTrue(self.provider._condition.wait_for(lambda: self.provider._counts["omp"] == 1, 5))
            self.provider.close()
            self.assertEqual(pending.result(timeout=5)[0], 503)


class NativeProfileTests(unittest.TestCase):
    def test_rejects_profile_credential_wrapper_without_executing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            wrapper = Path(directory) / "omp"
            wrapper.write_text("#!/bin/sh\nexec credential-manager read secret\n")
            with patch("provider.shutil.which", return_value=str(wrapper)):
                with self.assertRaisesRegex(ValueError, "profile wrapper"):
                    native_harness("omp")

    def test_profiles_are_distinct_private_native_configurations(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            native = home / "native"
            native.write_bytes(b"\x7fELF")
            with patch("provider.native_harness", return_value=native):
                additions = prepare_profiles(home, "http://127.0.0.1:1234/v1")
            omp = json.loads((home / ".omp/agent/models.yml").read_text())
            pi = json.loads((home / ".pi/agent/models.json").read_text())
            self.assertEqual(omp["providers"]["showcase"]["models"][0]["id"], "omp")
            self.assertEqual(omp["providers"]["showcase"]["models"][1]["id"], "omp-helper")
            settings = json.loads((home / ".omp/agent/config.yml").read_text())
            self.assertEqual(settings["modelRoles"]["smol"], "showcase/omp-helper")
            self.assertEqual(settings["modelRoles"]["default"], "showcase/omp")
            self.assertTrue(settings["display"]["hideToolActivity"])
            self.assertEqual(pi["providers"]["showcase"]["models"][0]["id"], "pi")
            self.assertNotIn("PI_CODING_AGENT_DIR", additions)
            self.assertNotIn("PI_PACKAGE_DIR", additions)
            self.assertEqual(additions["PI_OFFLINE"], "1")
            for path in (*home.glob(".omp/agent/*"), *home.glob(".pi/agent/*")):
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertNotIn("approval", path.read_text().lower())

    def test_profiles_reject_non_loopback_provider_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "127.0.0.1"):
                prepare_profiles(Path(directory), "https://api.example.com/v1")
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
