"""Unit coverage for orchestration; these are not native screenshot evidence."""
import json
from pathlib import Path
import tempfile
import unittest

from capture import FIXTURE_BROKER_TOOLS, broker_conversation, client_command, field, ensure_success, graphics_readiness, is_fixture_broker_dialog, is_fixture_key_prompt, is_fixture_project_prompt, is_security_notice, managed_bindings, Session
from runtime import Changes, private_environment, native
from provider import FIXTURE_API_KEY


class BrokerScenario:
    def __init__(self):
        self.calls = []
        self.summaries = {}

    def tool(self, role, name, arguments=None):
        self.calls.append((role, name, arguments))

    def wait_result(self, role, name, **_):
        match name:
            case "whoami":
                return {"launch_id": role}
            case "discover_agents":
                return {"agents": [{"launch_id": "pi"}]}
            case "send_message":
                return {"message_id": "request" if role == "claude" else "reply", "durable": True}
            case "read_inbox":
                return {"messages": [{"id": "request" if role == "pi" else "reply"}]}
            case "ack_message":
                return {"acknowledged": True}
            case "inspect_outbox":
                return {"receipts": [{"message_id": "request" if role == "claude" else "reply",
                                      "read_at": 1, "acknowledged_at": 2}]}
        raise AssertionError(name)

    def stop(self, role, text):
        self.summaries[role] = text


class CaptureTests(unittest.TestCase):
    def test_rich_readiness_requires_host_reply_and_complete_placed_frame(self):
        output = (b"\x1b_Ga=t,f=32,s=1,v=1,i=7,m=0;AAAAAA==\x1b\\"
                  b"\x1b[3;2H\x1b_Ga=p,i=7,p=1,q=1\x1b\\"
                  b"\x1b[2;40H\x1b_Ga=p,i=7,p=2,q=1\x1b\\")
        reply = b"\x1b_Gi=99;OK\x1b\\"
        self.assertIsNone(graphics_readiness(output, reply))
        output += b"\x1b[?2026l"
        self.assertIsNone(graphics_readiness(output, b""))
        self.assertEqual(graphics_readiness(output, reply)["sidebar_placements"], 1)
        self.assertIsNone(graphics_readiness(output + b"\x1b_Ga=d,d=I,i=7\x1b\\", reply))
        with self.assertRaises(RuntimeError):
            graphics_readiness(output, b"\x1b_Gi=99;EINVAL\x1b\\")

    def test_broker_uses_quiet_native_tools_and_exact_processed_ids(self):
        provider = BrokerScenario()
        result = broker_conversation(provider)
        sends = [arguments for _, name, arguments in provider.calls if name == "send_message"]
        self.assertEqual([value["delivery"] for value in sends], ["quiet", "quiet"])
        acks = [(role, arguments) for role, name, arguments in provider.calls if name == "ack_message"]
        self.assertEqual(acks, [("pi", {"ids": ["request"]}), ("claude", {"ids": ["reply"]})])
        self.assertEqual(set(provider.summaries), {"claude", "pi"})
        self.assertEqual(result["send"]["message_id"], "request")

    def test_broker_notifies_the_approval_controller_for_exact_planned_calls(self):
        provider = BrokerScenario()
        observed = []

        def notify(role, name, arguments, occurrence):
            self.assertEqual(provider.calls[-1], (role, name, arguments))
            observed.append((role, name, arguments, occurrence))

        broker_conversation(provider, notify)
        self.assertEqual([row[:3] for row in observed], provider.calls)
        self.assertTrue(all(row[3] == 1 for row in observed))
        self.assertEqual({name for role, name, _, _ in observed if role == "claude"}, set(FIXTURE_BROKER_TOOLS))

    def test_native_broker_dialog_is_limited_to_one_known_tool_and_one_call_yes(self):
        prompt = """Tool use
bkpr — Whoami Tool: (MCP)
About the bkpr — Whoami Tool:
│ Current authenticated launch, binding, ownership revisions and readiness.
(ctrl+o to expand description)
Do you want to proceed?
❯ 1. Yes
2. No
Esc to cancel · Tab to amend
"""
        self.assertTrue(is_fixture_broker_dialog(prompt, "whoami"))
        self.assertFalse(is_fixture_broker_dialog(prompt, "discover_agents"))
        self.assertFalse(is_fixture_broker_dialog(prompt, "terminal_input"))
        self.assertFalse(is_fixture_broker_dialog(prompt.replace("bkpr —", "other —"), "whoami"))
        self.assertFalse(is_fixture_broker_dialog(prompt + "Always allow", "whoami"))
        self.assertFalse(is_fixture_broker_dialog(prompt.replace("2. No", """2. Allow for this session
3. No"""), "whoami"))
        with_arguments = prompt.replace("Whoami Tool", "Discover Agents Tool").replace(
            "About the", """╌╌╌╌
scope: "workspace"
╌╌╌╌
About the""", 1)
        self.assertTrue(is_fixture_broker_dialog(with_arguments, "discover_agents"))


    def test_bindings_require_three_actual_native_capture_sources(self):
        snapshot = {"panes": [
            {"id": role + "-pane", "harness": {"kind": role, "managed": {
                "launch_id": role + "-launch", "conversation": {"id": role + "-conversation", "source": source}}}}
            for role, source in (("claude", "claude_hook"), ("pi", "pi_extension"), ("codex", "codex_start"))]}
        self.assertEqual(set(managed_bindings(snapshot)), {"claude", "pi", "codex"})
        snapshot["panes"][-1]["harness"]["managed"]["conversation"] = None
        self.assertEqual(managed_bindings(snapshot), {})
        snapshot["panes"][-1]["harness"]["managed"]["conversation"] = {"id": "invented", "source": "title"}
        with self.assertRaisesRegex(RuntimeError, "unexpected capture source"):
            managed_bindings(snapshot)

    def test_encoded_mcp_content_is_decoded_for_receipts(self):
        value = {"content": [{"text": json.dumps({"message_id": "native-message"})}]}
        self.assertEqual(field(value, "message_id"), "native-message")
        with self.assertRaises(RuntimeError):
            ensure_success({"content": [{"isError": True}]})

    def test_dummy_key_gate_requires_the_exact_native_prompt_and_configured_fixture_key(self):
        prompt = """Detected a custom API key in your environment
ANTHROPIC_API_KEY: sk-ant-...ine-not-a-credential
Do you want to use this API key?
  Yes
❯ No (recommended)
Enter to confirm · Esc to cancel
"""
        self.assertTrue(is_fixture_key_prompt(prompt, FIXTURE_API_KEY))
        self.assertFalse(is_fixture_key_prompt(prompt, "different-key"))
        self.assertFalse(is_fixture_key_prompt(prompt.replace("ine-not-a-credential", "different-key"), FIXTURE_API_KEY))
        self.assertFalse(is_fixture_key_prompt("Do you want to use this API key?", FIXTURE_API_KEY))
        self.assertFalse(is_fixture_key_prompt("Do you trust this folder?", FIXTURE_API_KEY))

    def test_security_notice_gate_cannot_accept_trust_authentication_or_legal_prompts(self):
        notice = """Security notes:
1. Claude can make mistakes.
You're responsible for Claude's actions and should always review them, especially when running code.
2. Due to prompt injection risks, only use it with code you trust
Learn more: https://code.claude.com/docs/en/security
Press Enter to continue…
"""
        self.assertTrue(is_security_notice(notice))
        self.assertTrue(is_security_notice("""Welcome to Claude Code v2.1.322
░░░ * █▓
""" + notice))
        self.assertFalse(is_security_notice(notice + "Do you trust this folder?"))
        self.assertFalse(is_security_notice("""Terms of Service
""" + notice))
        self.assertFalse(is_security_notice("Do you want to use this API key?"))
        self.assertFalse(is_security_notice(notice.replace("Claude can make mistakes.", "New conditions apply.")))

    def test_project_trust_gate_is_bound_to_the_exact_private_generated_repository(self):
        with tempfile.TemporaryDirectory(prefix="bs-", dir="/tmp") as directory:
            root = Path(directory)
            home = root / "h"
            home.mkdir(mode=0o700)
            projects = home / "projects"
            projects.mkdir(mode=0o700)
            repo = projects / "search-index"
            repo.mkdir(mode=0o700)

            def prompt(path):
                return f"""─────────────────────────────────────────────────────────
Accessing workspace:
{path}
Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first.
Claude Code'll be able to read, edit, and execute files here.
Security guide
❯ No, exit
Yes, I trust this folder
Enter to confirm · Esc to cancel
"""

            self.assertTrue(is_fixture_project_prompt(prompt(repo), repo, home))
            self.assertFalse(is_fixture_project_prompt(prompt(home), repo, home))
            self.assertFalse(is_fixture_project_prompt(prompt(repo) + "Terms of Service", repo, home))
            other = projects / "other-project"
            other.mkdir(mode=0o700)
            self.assertFalse(is_fixture_project_prompt(prompt(other), other, home))
            alias = projects / "alias"
            alias.symlink_to(repo, target_is_directory=True)
            self.assertFalse(is_fixture_project_prompt(prompt(alias), alias, home))
            repo.chmod(0o755)
            self.assertFalse(is_fixture_project_prompt(prompt(repo), repo, home))

    def test_environment_does_not_inherit_credentials_or_managed_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            env = private_environment(Path(directory), {"PATH": "/bin", "HOME": "/host",
                "BKPR_CAPTURE_TOKEN": "secret", "OPENAI_API_KEY": "secret", "SSH_AUTH_SOCK": "/host/agent"})
            self.assertFalse({"BKPR_CAPTURE_TOKEN", "OPENAI_API_KEY", "SSH_AUTH_SOCK"} & env.keys())
            self.assertEqual(Path(env["HOME"]).stat().st_mode & 0o777, 0o700)

    def test_optional_frontend_changes_only_the_tui_entrypoint_contract(self):
        pair, frontend, config, remote, socket = map(Path, ("/pair", "/frontend", "/ssh-config", "/remote-helper", "/local/socket"))
        default = client_command(pair, None, config, remote, socket)
        custom = client_command(pair, frontend, config, remote, socket)
        self.assertEqual(default[0], pair / "bkpr")
        self.assertEqual(default[default.index("--bkprd") + 1], pair / "bkprd")
        self.assertEqual(default[-2:], ["--clipboard", "deny"])
        self.assertEqual(custom, [frontend, "--session", "workbench", "--session", "forgebox:forge",
                                  "--remote-helper", remote, "--ssh-config", config, "--socket", socket])

    def test_named_session_ancestors_are_private_at_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env = private_environment(root, {})
            session = Session("test", env, root, None, root)
            for path in (session.endpoint.parent, session.endpoint.parent.parent,
                         session.state, session.state.parent, session.state.parent.parent):
                self.assertEqual(path.stat().st_mode & 0o777, 0o700)

    def test_changes_observes_writes_without_read_feedback(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ready"
            path.touch()
            with Changes([path]) as changes:
                path.read_text()
                self.assertFalse(changes.wait(0))
                path.write_text("ready")
                self.assertEqual(changes.until(lambda: path.read_text(), "ready", timeout=1), "ready")

    def test_native_preserves_multicall_executable_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "multicall").write_bytes(b"\x7fELF")
            (root / "env").symlink_to(root / "multicall")
            self.assertEqual(native(root / "env"), root / "env")


if __name__ == "__main__":
    unittest.main()
