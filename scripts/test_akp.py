import importlib.machinery
import importlib.util
import json
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


if __name__ == "__main__":
    unittest.main()
