"""`jev-annotate.py` のテスト — **API を叩かない**。

    python3 -m unittest discover -s examples/semantic -p 'test_*.py'

外部ネットワークに依存するテストは CI で落ちるので、ここで確かめるのは

- 構造ルール（見出し / コード / リスト / 引用 / 散文の各組み合わせ）
- Jev のレスポンスを模したフィクスチャから Unit を組み立てる処理
- ラウンド 3（Unit の核）を聞く対象の絞り方と、答えの書き戻し
- `--dry-run` が送ろうとするリクエストの形（state / model / questions）
- `TYPESAFE_API_KEY` 未設定時のエラー経路

の 5 つだけである。判定の質そのものは実測（`docs/` と README の表）で見る。

ファイル名にハイフンが入っていて普通の import ができないので、
`plugins/akp/scripts/test_akp.py` と同じく SourceFileLoader で読む。
"""

import importlib.machinery
import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "jev-annotate.py"


def load_module():
    loader = importlib.machinery.SourceFileLoader("jev_annotate", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


jev = load_module()

SAME, NEW = jev.SAME, jev.NEW


def atom(index, kind, text):
    start = index * 100
    return {
        "index": index,
        "kind": kind,
        "range": {"start": start, "end": start + len(text.encode())},
        "text": text,
    }


class BoundaryRuleTest(unittest.TestCase):
    """構造ルール — パーサが既に知っている境界は Jev に聞かない。"""

    def decide(self, current, following):
        return jev.boundary_rule(current, following)[0]

    def test_a_heading_always_starts_a_new_unit(self):
        for kind in ("sentence", "list_item", "code_block", "block_quote", "heading"):
            self.assertEqual(self.decide(kind, "heading"), NEW, kind)

    def test_a_heading_attaches_to_what_follows_it(self):
        for kind in ("sentence", "list_item", "block_quote", "table"):
            self.assertEqual(self.decide("heading", kind), SAME, kind)

    def test_the_heading_rule_wins_over_the_standalone_block_rule(self):
        # `## 設定例` + コードブロックは 1 つの Unit。**未実測の判断**で、
        # demo.md にこの組み合わせは出てこない。
        self.assertEqual(self.decide("heading", "code_block"), SAME)

    def test_code_blocks_and_tables_stand_alone(self):
        for kind in ("code_block", "table"):
            self.assertEqual(self.decide("sentence", kind), NEW, kind)
            self.assertEqual(self.decide(kind, "sentence"), NEW, kind)
            self.assertEqual(self.decide(kind, kind), NEW, kind)

    def test_consecutive_list_items_stay_in_one_unit(self):
        self.assertEqual(self.decide("list_item", "list_item"), SAME)

    def test_a_block_quote_never_merges_with_its_neighbour(self):
        self.assertEqual(self.decide("sentence", "block_quote"), NEW)
        self.assertEqual(self.decide("block_quote", "sentence"), NEW)
        self.assertEqual(self.decide("block_quote", "block_quote"), NEW)

    def test_only_two_sentences_are_asked_of_jev(self):
        self.assertIsNone(self.decide("sentence", "sentence"))
        for pair in (
            ("sentence", "list_item"),
            ("list_item", "sentence"),
            ("other", "sentence"),
            ("sentence", "other"),
        ):
            self.assertIsNotNone(self.decide(*pair), pair)

    def test_the_reason_says_which_rule_fired(self):
        self.assertEqual(jev.boundary_rule("sentence", "heading")[1], "rule:next_is_heading")
        self.assertEqual(jev.boundary_rule("sentence", "sentence")[1], "jev")


class PlanAndGroupTest(unittest.TestCase):
    def test_an_empty_atom_is_not_asked_about(self):
        atoms = [atom(0, "sentence", "本文がある。"), atom(1, "sentence", "   ")]
        plan = jev.plan_boundaries(atoms)
        self.assertEqual(plan[0]["decision"], NEW)
        self.assertEqual(plan[0]["by"], "rule:empty_text")
        self.assertEqual(jev.boundary_questions(atoms, plan), {})

    def test_units_are_grouped_by_the_boundary_decisions(self):
        atoms = [atom(i, "sentence", f"文{i}。") for i in range(4)]
        plan = [
            {"after_atom": 0, "decision": SAME, "by": "jev"},
            {"after_atom": 1, "decision": NEW, "by": "jev"},
            {"after_atom": 2, "decision": SAME, "by": "jev"},
        ]
        self.assertEqual(jev.group_units(atoms, plan), [[0, 1], [2, 3]])

    def test_a_boundary_answer_is_written_back_with_its_confidence(self):
        plan = [{"after_atom": 0, "decision": None, "by": "jev"}]
        jev.apply_boundary_answers(
            plan, {"boundary:0": {"choice": SAME, "confidence": 0.61}}
        )
        self.assertEqual(plan[0]["decision"], SAME)
        self.assertEqual(plan[0]["confidence"], 0.61)

    def test_a_missing_or_unknown_boundary_answer_fails_loudly(self):
        # 既定へ倒すと「それらしく見えるが間違っている注釈」になるので、
        # 黙って埋めずに失敗させる。
        with self.assertRaises(jev.JevError):
            jev.apply_boundary_answers([{"after_atom": 0, "decision": None}], {})
        with self.assertRaises(jev.JevError):
            jev.apply_boundary_answers(
                [{"after_atom": 0, "decision": None}], {"boundary:0": {"choice": "maybe"}}
            )


class BuildUnitsTest(unittest.TestCase):
    """Jev のレスポンスを模したフィクスチャから Unit を組み立てる。"""

    ATOMS = [
        atom(0, "heading", "結論"),
        atom(1, "sentence", "採用する方式は差分配信である。"),
        atom(2, "sentence", "詳細は付録にまとめた。"),
        atom(3, "sentence", "つまり、採用する方式は差分配信だということである。"),
    ]
    UNITS = [[0, 1], [2], [3]]
    ANSWERS = {
        "tier:u1": {"choice": "essential", "confidence": 0.88},
        "tier:u2": {"choice": "detail", "confidence": 0.71},
        "redundant:u2": {"noul": 0.12},
        "tier:u3": {"choice": "supporting", "confidence": 0.64},
        "redundant:u3": {"noul": 0.93},
    }

    def build(self):
        return jev.build_units(self.ATOMS, self.UNITS, self.ANSWERS)

    def test_units_keep_the_protocol_shape(self):
        units = self.build()
        self.assertEqual([u["id"] for u in units], ["u1", "u2", "u3"])
        self.assertEqual([u["atoms"] for u in units], self.UNITS)
        self.assertEqual(
            [u["reading_tier"] for u in units], ["essential", "detail", "supporting"]
        )

    def test_a_high_noul_becomes_a_redundant_with_pointing_backwards(self):
        units = self.build()
        self.assertEqual(units[1]["relations"], [])
        self.assertEqual(units[2]["relations"], [{"redundant_with": "u1"}])

    def test_confidence_and_noul_are_recorded_not_thrown_away(self):
        units = self.build()
        self.assertEqual(units[0]["jev"]["tier_confidence"], 0.88)
        self.assertNotIn("redundancy_noul", units[0]["jev"])
        self.assertEqual(units[2]["jev"]["redundancy_noul"], 0.93)
        self.assertEqual(units[2]["jev"]["redundant_with"], "u1")

    def test_confidence_never_flips_a_decision(self):
        # 閾値で倒すのは採用していない（実測で閾値が値の真上に乗り、実行ごとに
        # 答えが揺れたため）。confidence が低くても choice がそのまま通る。
        answers = dict(self.ANSWERS)
        answers["tier:u1"] = {"choice": "essential", "confidence": 0.01}
        units = jev.build_units(self.ATOMS, self.UNITS, answers)
        self.assertEqual(units[0]["reading_tier"], "essential")

    def test_a_missing_tier_answer_fails_loudly(self):
        answers = {k: v for k, v in self.ANSWERS.items() if k != "tier:u2"}
        with self.assertRaises(jev.JevError):
            jev.build_units(self.ATOMS, self.UNITS, answers)

    def test_a_missing_noul_fails_loudly(self):
        answers = dict(self.ANSWERS)
        answers["redundant:u3"] = {"confidence": 0.5}
        with self.assertRaises(jev.JevError):
            jev.build_units(self.ATOMS, self.UNITS, answers)

    def test_the_relation_is_dropped_when_no_earlier_unit_overlaps(self):
        # Noul が高くても参照先が選べなければ relation は付けない
        # （プロトコルは実在しない参照先を拒否する）。値は記録に残す。
        atoms = [atom(0, "sentence", "AAAA"), atom(1, "sentence", "ZZZZ")]
        units = jev.build_units(
            atoms,
            [[0], [1]],
            {
                "tier:u1": {"choice": "essential", "confidence": 0.5},
                "tier:u2": {"choice": "detail", "confidence": 0.5},
                "redundant:u2": {"noul": 0.99},
            },
        )
        self.assertEqual(units[1]["relations"], [])
        self.assertEqual(units[1]["jev"]["redundancy_noul"], 0.99)
        self.assertIsNone(units[1]["jev"]["redundant_with"])


class CoreQuestionTest(unittest.TestCase):
    """ラウンド 3 — MARKED になる Unit の核だけを聞く。"""

    ATOMS = [
        atom(0, "heading", "結論"),
        atom(1, "list_item", "採用する方式は差分配信である。"),
        atom(2, "list_item", "帯域は 3 割減る見込み。"),
        atom(3, "sentence", "詳細は付録にまとめた。"),
        atom(4, "sentence", ""),
    ]

    def units(self, **overrides):
        unit = {
            "id": "u1",
            "atoms": [0, 1, 2],
            "reading_tier": "essential",
            "relations": [],
            "jev": {},
        }
        unit.update(overrides)
        return [unit]

    def test_a_marked_unit_offers_each_of_its_atoms_as_a_choice(self):
        asked = jev.core_questions(self.ATOMS, self.units())
        self.assertEqual(list(asked), ["core:u1"])
        question = asked["core:u1"]
        self.assertEqual(question["type"], "choice")
        self.assertEqual(
            question["criteria"],
            {
                "atom:0": "結論",
                "atom:1": "採用する方式は差分配信である。",
                "atom:2": "帯域は 3 割減る見込み。",
            },
        )

    def test_the_question_does_not_repeat_the_body_it_already_sends_as_choices(self):
        # 本文を instructions にも入れると同じテキストを 2 回送ることになり、
        # context window を無駄に食う。
        question = jev.core_questions(self.ATOMS, self.units())["core:u1"]
        for text in question["criteria"].values():
            self.assertNotIn(text, question["instructions"])
        self.assertIn("1 か所だけ", question["instructions"])

    def test_only_units_that_can_become_marked_are_asked(self):
        # MARKED は「ESSENTIAL かつ非 REDUNDANT」だけ。
        for tier in ["supporting", "context", "detail"]:
            self.assertEqual(
                jev.core_questions(self.ATOMS, self.units(reading_tier=tier)), {}
            )
        self.assertEqual(
            jev.core_questions(
                self.ATOMS, self.units(relations=[{"redundant_with": "u0"}])
            ),
            {},
        )

    def test_a_unit_with_one_usable_atom_is_not_asked(self):
        # 選択肢が 1 つの Choice は答えが決まっている。核は空のままになり、
        # Atom が 1 つなのだから Unit 全体 = その Atom が MARKED になる。
        self.assertEqual(jev.core_questions(self.ATOMS, self.units(atoms=[1])), {})
        # 本文が空の Atom は選択肢にならないので、これも 1 つ扱い。
        self.assertEqual(jev.core_questions(self.ATOMS, self.units(atoms=[3, 4])), {})

    def test_an_answer_becomes_a_single_core_atom(self):
        units = self.units()
        questions = jev.core_questions(self.ATOMS, units)
        jev.apply_core_answers(
            units, questions, {"core:u1": {"choice": "atom:2", "confidence": 0.42}}
        )
        self.assertEqual(units[0]["core_atoms"], [2])
        self.assertEqual(units[0]["jev"]["core_confidence"], 0.42)

    def test_confidence_never_flips_the_core(self):
        # 閾値で倒すのは採用していない（`probabilities` も見ない）。
        units = self.units()
        questions = jev.core_questions(self.ATOMS, units)
        jev.apply_core_answers(
            units, questions, {"core:u1": {"choice": "atom:0", "confidence": 0.01}}
        )
        self.assertEqual(units[0]["core_atoms"], [0])

    def test_a_missing_or_unknown_core_answer_fails_loudly(self):
        units = self.units()
        questions = jev.core_questions(self.ATOMS, units)
        with self.assertRaises(jev.JevError):
            jev.apply_core_answers(units, questions, {})
        with self.assertRaises(jev.JevError):
            # 聞いていない Atom を返されたら通さない。
            jev.apply_core_answers(units, questions, {"core:u1": {"choice": "atom:9"}})

    def test_a_unit_that_was_not_asked_keeps_no_core(self):
        units = self.units(reading_tier="detail")
        jev.apply_core_answers(units, {}, {"core:u1": {"choice": "atom:0"}})
        self.assertNotIn("core_atoms", units[0])


class DryRunTest(unittest.TestCase):
    """送るリクエストの形。実際の demo.md / demo.json を材料にする。"""

    @classmethod
    def setUpClass(cls):
        source = (HERE / "demo.md").read_text()
        fixture = json.loads((HERE / "demo.json").read_text())
        raw = source.encode()
        atoms = [
            {
                "index": i,
                "kind": a["kind"],
                "range": a["range"],
                "text": raw[a["range"]["start"] : a["range"]["end"]].decode(),
            }
            for i, a in enumerate(fixture["atoms"])
        ]
        cls.source = source
        cls.request = {"version": 1, "source": source, "atoms": atoms}
        cls.out = cls.run_script(["--dry-run"], cls.request)

    @staticmethod
    def run_script(args, request, env=None):
        environ = dict(os.environ)
        environ.pop("TYPESAFE_API_KEY", None)
        environ.update(env or {})
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env=environ,
            timeout=60,
        )
        return proc

    def payload(self):
        return json.loads(self.out.stdout)

    def test_dry_run_succeeds_without_an_api_key(self):
        self.assertEqual(self.out.returncode, 0, self.out.stderr)

    def test_the_state_is_the_whole_document(self):
        for round_ in self.payload()["rounds"]:
            self.assertEqual(round_["state"], self.source)

    def test_the_model_is_sent_on_every_round(self):
        for round_ in self.payload()["rounds"]:
            self.assertEqual(round_["model"], jev.DEFAULT_MODEL)

    def test_round_one_asks_only_about_sentence_pairs(self):
        atoms = self.request["atoms"]
        asked = self.payload()["rounds"][0]["questions"]
        self.assertTrue(asked, "散文どうしの境界が 1 つも無い")
        for key, question in asked.items():
            self.assertEqual(question["type"], "choice")
            self.assertEqual(set(question["criteria"]), {"same_unit", "new_unit"})
            i = int(key.split(":")[1])
            self.assertEqual(atoms[i]["kind"], "sentence")
            self.assertEqual(atoms[i + 1]["kind"], "sentence")

    def test_round_one_does_not_ask_about_structural_boundaries(self):
        atoms = self.request["atoms"]
        asked = {int(k.split(":")[1]) for k in self.payload()["rounds"][0]["questions"]}
        structural = {
            i
            for i in range(len(atoms) - 1)
            if "sentence" != atoms[i]["kind"] or "sentence" != atoms[i + 1]["kind"]
        }
        self.assertFalse(asked & structural)

    def test_round_two_asks_a_tier_for_every_unit_and_redundancy_for_the_rest(self):
        asked = self.payload()["rounds"][1]["questions"]
        tiers = sorted(k for k in asked if k.startswith("tier:"))
        redundant = sorted(k for k in asked if k.startswith("redundant:"))
        self.assertEqual(len(tiers), len(redundant) + 1, "先頭 Unit だけ聞かない")
        self.assertNotIn("redundant:u1", asked)
        for key in tiers:
            self.assertEqual(asked[key]["type"], "choice")
            self.assertEqual(set(asked[key]["criteria"]), set(jev.TIER_CRITERIA))
        for key in redundant:
            self.assertEqual(asked[key]["type"], "noul")
            # redundancy は方向を必ず指定する（対称に聞くと結論まで拾う）。
            self.assertIn("これより**前**の箇所", asked[key]["instructions"])

    def test_round_three_asks_for_the_core_of_multi_atom_units_only(self):
        rounds = self.payload()["rounds"]
        self.assertEqual(len(rounds), 3)
        units = {
            f"u{n}": indices
            for n, indices in enumerate(
                jev.group_units(
                    self.request["atoms"],
                    [
                        dict(entry, decision=entry["decision"] or jev.NEW)
                        for entry in jev.plan_boundaries(self.request["atoms"])
                    ],
                ),
                start=1,
            )
        }
        asked = rounds[2]["questions"]
        self.assertTrue(asked, "核を聞ける Unit が 1 つも無い")
        for key, question in asked.items():
            uid = key.split(":", 1)[1]
            self.assertEqual(question["type"], "choice")
            self.assertGreaterEqual(len(question["criteria"]), 2)
            # 選択肢はその Unit の Atom だけ。
            self.assertLessEqual(
                {int(k.split(":")[1]) for k in question["criteria"]},
                set(units[uid]),
            )
        # Atom が 1 つの Unit は聞かれない。
        for uid, indices in units.items():
            if len(indices) == 1:
                self.assertNotIn(f"core:{uid}", asked)

    def test_the_dry_run_names_its_assumptions(self):
        # 形だけ見て「本番もこの question 数だ」と読まれないように。
        assumptions = " ".join(self.payload()["assumptions"])
        self.assertIn("new_unit", assumptions)
        self.assertIn("essential", assumptions)

    def test_the_tier_criteria_are_the_design_documents_wording(self):
        # 言い換えると判定品質が落ちるので、逐語であることを固定する。
        self.assertEqual(
            jev.TIER_CRITERIA["essential"],
            "落とすと文書の要点、結論、制約などを取り違える可能性が高い。",
        )
        self.assertEqual(jev.TIER_CRITERIA["detail"], "例、細部、追加説明。")

    def test_the_boundary_question_asks_about_skimming_not_topic(self):
        # v1 の「話題が同じか」に戻すと、結論の行が 1 つの Unit にまとまって
        # 行の途中で表示が切り替わらなくなる。
        question = next(iter(self.payload()["rounds"][0]["questions"].values()))
        self.assertIn("拾い読み", question["instructions"])
        self.assertIn("読む優先度", question["criteria"]["same_unit"])


class MissingKeyTest(unittest.TestCase):
    def test_a_missing_key_exits_non_zero_with_one_actionable_line(self):
        request = {
            "version": 1,
            "source": "本文。",
            "atoms": [atom(0, "sentence", "本文。")],
        }
        out = DryRunTest.run_script([], request)
        self.assertNotEqual(out.returncode, 0)
        self.assertEqual(out.stdout, "")
        lines = [line for line in out.stderr.splitlines() if line.strip()]
        self.assertEqual(len(lines), 1, out.stderr)
        # akapen はステータス行に stderr の最後の非空行を 160 字まで出す。
        self.assertLessEqual(len(lines[0]), 160)
        self.assertIn("TYPESAFE_API_KEY", lines[0])

    def test_a_broken_request_never_reaches_the_network(self):
        out = DryRunTest.run_script([], {"version": 99, "source": "", "atoms": []})
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("プロトコル版", out.stderr)


class HttpErrorMessageTest(unittest.TestCase):
    """HTTP エラーがステータス行で意味を持つか（**API は叩かない**）。

    `max_tokens_exceeded` は実測でこの経路のいちばん現実的な失敗で
    （45.6KB の実文書が context window の 92〜96 % を使う）、生の JSON が
    出ると「タイムアウトした」と読み違えられる。
    """

    MAX_TOKENS = '{"detail":{"error_type":"max_tokens_exceeded"}}'

    def test_max_tokens_says_it_is_size_not_time(self):
        line = jev.http_error_message(400, self.MAX_TOKENS)
        self.assertIn("大きすぎ", line)
        self.assertIn("タイムアウトではない", line)
        # 生の JSON を出さない（読み手に何も伝えないので）。
        self.assertNotIn("error_type", line)

    def test_every_message_fits_the_status_line(self):
        # akapen はステータス行に 160 字まで出す（`src/export.rs` の
        # `Capture::tail`）。
        for code, detail in ((400, self.MAX_TOKENS), (401, "unauthorized"), (500, "boom")):
            with self.subTest(code=code):
                self.assertLessEqual(len(jev.http_error_message(code, detail)), 160)

    def test_other_errors_keep_the_code_and_the_body(self):
        line = jev.http_error_message(401, "unauthorized")
        self.assertIn("401", line)
        self.assertIn("unauthorized", line)


if __name__ == "__main__":
    unittest.main()
