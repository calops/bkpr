"""Unit coverage for orchestration; these are not native screenshot evidence."""
import json
from pathlib import Path
import tempfile
import unittest

from capture import broker_conversation, field, ensure_success, graphics_readiness, Session
from runtime import Changes, private_environment, native


class BrokerScenario:
    def __init__(self):
        self.calls = []
        self.summaries = {}

    def tool(self, role, name, arguments):
        self.calls.append((role, name, arguments))

    def wait_result(self, role, name, **_):
        match name:
            case "whoami":
                return {"launch_id": role}
            case "discover_agents":
                return {"agents": [{"launch_id": "pi"}]}
            case "send_message":
                return {"message_id": "request" if role == "omp" else "reply", "durable": True}
            case "read_inbox":
                return {"messages": [{"id": "request" if role == "pi" else "reply"}]}
            case "ack_message":
                return {"acknowledged": True}
            case "inspect_outbox":
                return {"receipts": [{"message_id": "request" if role == "omp" else "reply",
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
        self.assertEqual(acks, [("pi", {"ids": ["request"]}), ("omp", {"ids": ["reply"]})])
        self.assertEqual(set(provider.summaries), {"omp", "pi"})
        self.assertEqual(result["send"]["message_id"], "request")

    def test_encoded_mcp_content_is_decoded_for_receipts(self):
        value = {"content": [{"text": json.dumps({"message_id": "native-message"})}]}
        self.assertEqual(field(value, "message_id"), "native-message")
        with self.assertRaises(RuntimeError):
            ensure_success({"content": [{"isError": True}]})

    def test_environment_does_not_inherit_credentials_or_managed_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            env = private_environment(Path(directory), {"PATH": "/bin", "HOME": "/host",
                "BKPR_CAPTURE_TOKEN": "secret", "OPENAI_API_KEY": "secret", "SSH_AUTH_SOCK": "/host/agent"})
            self.assertFalse({"BKPR_CAPTURE_TOKEN", "OPENAI_API_KEY", "SSH_AUTH_SOCK"} & env.keys())
            self.assertEqual(Path(env["HOME"]).stat().st_mode & 0o777, 0o700)

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
