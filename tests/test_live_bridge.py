import copy
import hashlib
import unittest

from live_bridge import CheckFailure, Scenario, check_receipts, payload_digest


class ReceiptProofTests(unittest.TestCase):
    def setUp(self):
        self.scenario = Scenario("run", "codex-secret", "claude-secret", (17, 29), (43, 11))
        self.agents = {"codex": "codex-id", "claude": "claude-id"}
        self.store = {"messages": {}}
        for sequence, step in enumerate(self.scenario.steps(), 1):
            recipient = self.agents[step.recipient]
            self.store["messages"][step.name] = {
                "id": step.name, "from": self.agents[step.sender], "to": recipient,
                "idempotency_key": hashlib.sha256(step.key.encode()).hexdigest(),
                "payload_digest": payload_digest(recipient, step.text), "sequence": sequence,
                "text": None, "created_at": sequence * 10 - 2, "read_at": sequence * 10,
                "acknowledged_at": sequence * 10 + 1, "expired_at": None,
                "delivery": {"outcome": "accepted"},
            }

    def test_processed_receipts_prove_exact_exchange_after_bodies_are_removed(self):
        rows = check_receipts(self.store, self.agents, self.scenario)
        self.assertEqual([row["state"] for row in rows], ["processed"] * 6)

    def test_transport_acceptance_without_read_or_ack_is_not_completion(self):
        first = self.store["messages"]["challenge-1"]
        first["read_at"] = first["acknowledged_at"] = None
        self.store["messages"] = {"challenge-1": first}
        self.assertEqual([row["state"] for row in check_receipts(self.store, self.agents, self.scenario)],
                         ["unread", "waiting", "waiting", "waiting", "waiting", "waiting"])

    def test_wrong_actor_response_or_scope_cannot_pass(self):
        for field, value in (("from", "third-agent"), ("to", "third-agent"),
                             ("payload_digest", payload_digest("claude-id", "guessed answer")),
                             ("all_scope", True)):
            with self.subTest(field=field):
                broken = copy.deepcopy(self.store)
                broken["messages"]["answer-1"][field] = value
                with self.assertRaises(CheckFailure):
                    check_receipts(broken, self.agents, self.scenario)

    def test_out_of_order_or_missing_predecessor_is_rejected(self):
        broken = copy.deepcopy(self.store)
        broken["messages"]["answer-1"]["sequence"] = 1
        with self.assertRaises(CheckFailure):
            check_receipts(broken, self.agents, self.scenario)
        del self.store["messages"]["challenge-1"]
        with self.assertRaises(CheckFailure):
            check_receipts(self.store, self.agents, self.scenario)

    def test_duplicate_logical_send_is_rejected(self):
        self.store["messages"]["duplicate"] = copy.deepcopy(self.store["messages"]["challenge-1"])
        self.store["messages"]["duplicate"]["id"] = "another-message-id"
        with self.assertRaises(CheckFailure):
            check_receipts(self.store, self.agents, self.scenario)

    def test_ack_without_read_is_not_processing_proof(self):
        self.store["messages"]["confirm-2"]["read_at"] = None
        with self.assertRaises(CheckFailure):
            check_receipts(self.store, self.agents, self.scenario)

    def test_late_reads_cannot_retroactively_justify_earlier_responses(self):
        self.store["messages"]["answer-1"]["created_at"] = 9
        with self.assertRaises(CheckFailure):
            check_receipts(self.store, self.agents, self.scenario)

    def test_each_peer_prompt_excludes_the_other_peers_secret(self):
        self.assertNotIn(self.scenario.second_nonce, self.scenario.prompt("codex"))
        self.assertNotIn(self.scenario.first_nonce, self.scenario.prompt("claude"))


if __name__ == "__main__":
    unittest.main()
