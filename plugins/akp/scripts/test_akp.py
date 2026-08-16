import importlib.machinery
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock


def load_akp():
    path = Path(__file__).with_name("akp")
    loader = importlib.machinery.SourceFileLoader("akp", str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


akp = load_akp()


class CodexSupportTest(unittest.TestCase):
    def test_finds_timestamp_prefixed_codex_rollout_by_session_id(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sid = "019f1803-7fbe-7500-aa1c-c986814ad35c"
            rollout = root / "2026" / "06" / "30" / f"rollout-2026-06-30T19-11-50-{sid}.jsonl"
            rollout.parent.mkdir(parents=True)
            rollout.touch()
            agent = {
                "agent": "codex",
                "agent_session": {"kind": "id", "value": sid},
            }

            with mock.patch.object(akp, "JSONL_ROOTS", [root]):
                self.assertEqual(akp.session_path(agent), ("jsonl", str(rollout)))

    def test_extracts_codex_response_items_without_event_duplicates(self):
        records = [
            {
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "phase": "commentary",
                    "content": [{"type": "output_text", "text": "途中経過"}],
                },
            },
            {
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": "重複コピー"},
            },
            {
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "phase": "final_answer",
                    "content": [
                        {"type": "output_text", "text": "最終回答1"},
                        {"type": "output_text", "text": "最終回答2"},
                    ],
                },
            },
        ]
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8") as transcript:
            for record in records:
                transcript.write(json.dumps(record, ensure_ascii=False) + "\n")
            transcript.flush()

            self.assertEqual(
                akp._latest_texts_jsonl(transcript.name, 10),
                ["途中経過", "最終回答1\n最終回答2"],
            )

    def test_keeps_existing_claude_message_shape(self):
        record = {
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "Claude response"}],
            }
        }
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8") as transcript:
            transcript.write(json.dumps(record) + "\n")
            transcript.flush()
            self.assertEqual(
                akp._latest_texts_jsonl(transcript.name, 10),
                ["Claude response"],
            )


class AgentPickTest(unittest.TestCase):
    """pick_agent: focused-agent preference for multi-agent tabs."""

    LISTING = [
        {"agent": "claude", "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1"},
        {"agent": "pi", "pane_id": "w1:p2", "tab_id": "w1:t1", "workspace_id": "w1"},
        {"agent": "pi", "pane_id": "w1:p3", "tab_id": "w1:t2", "workspace_id": "w1"},
    ]

    def setUp(self):
        self.env = mock.patch.dict(
            os.environ,
            {"HERDR_TAB_ID": "w1:t1", "HERDR_WORKSPACE_ID": "w1"},
            clear=False,
        )
        self.env.start()

    def tearDown(self):
        self.env.stop()

    def test_multi_agent_tab_refuses_without_focused_pane(self):
        # 2 agents in the tab: ambiguous without a focused hint.
        with self.assertRaises(SystemExit):
            akp.pick_agent(self.LISTING, me=None)

    def test_focused_agent_disambiguates(self):
        got = akp.pick_agent(self.LISTING, me=None, focused="w1:p2")
        self.assertEqual(got["agent"], "pi")
        self.assertEqual(got["pane_id"], "w1:p2")

    def test_focused_non_agent_falls_through_to_refusal(self):
        # Focused pane is not an agent: ambiguity remains -> refuse.
        with self.assertRaises(SystemExit):
            akp.pick_agent(self.LISTING, me=None, focused="w1:p9")

    def test_focused_agent_in_other_tab_ignored(self):
        # The focused pane is an agent, but in another tab.
        with self.assertRaises(SystemExit):
            akp.pick_agent(self.LISTING, me=None, focused="w1:p3")

    def test_sole_agent_still_wins_without_focused_hint(self):
        listing = [
            {"agent": "pi", "pane_id": "w1:p2", "tab_id": "w1:t1", "workspace_id": "w1"}
        ]
        got = akp.pick_agent(listing, me=None)
        self.assertEqual(got["pane_id"], "w1:p2")

    def test_self_exclusion_still_applies(self):
        # me == focused: focused is excluded by real(); the remaining sole
        # in-tab agent (claude) wins.
        got = akp.pick_agent(self.LISTING, me="w1:p2", focused="w1:p2")
        self.assertEqual(got["pane_id"], "w1:p1")

    def test_recorded_target_resolves_refresh_from_akp_pane(self):
        # Refresh presses fire while the akp pane itself is focused (not
        # an agent): the recorded target from the fresh launch must win.
        got = akp.pick_agent(
            self.LISTING, me=None, focused="w1:p9", preferred="w1:p2"
        )
        self.assertEqual(got["pane_id"], "w1:p2")

    def test_focused_agent_beats_recorded_target(self):
        # The user focused another agent: that agent wins over the
        # recorded target (the existing pane switches to it).
        got = akp.pick_agent(
            self.LISTING, me=None, focused="w1:p1", preferred="w1:p2"
        )
        self.assertEqual(got["pane_id"], "w1:p1")

    def test_stale_recorded_target_falls_through(self):
        # The recorded pane is gone from the listing: fall back to the
        # sole-agent rules (here: refusal, 2 agents remain).
        with self.assertRaises(SystemExit):
            akp.pick_agent(
                self.LISTING, me=None, focused="w1:p9", preferred="w1:p99"
            )


if __name__ == "__main__":
    unittest.main()
