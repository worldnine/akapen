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

    def decide(self, current, following, next_indent=None):
        return jev.boundary_rule(current, following, next_indent)[0]

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

    def test_a_new_top_level_list_item_starts_a_new_unit(self):
        # 別項目どうしは NEW。箇条書きが丸ごと 1 Unit だと、10 個の決定事項が
        # 全部同じ Tier になってしまう。
        self.assertEqual(self.decide("list_item", "list_item", 0), NEW)

    def test_a_nested_list_item_stays_with_its_parent(self):
        # 「決定事項」は親項目 + その詳細の束なので、そこでは割らない。
        for indent in (1, 2, 3, 4, 8):
            self.assertEqual(self.decide("list_item", "list_item", indent), SAME, indent)

    def test_a_continuation_sentence_stays_in_the_same_item(self):
        # マーカーが無い = 同じ項目の 2 文目以降。
        self.assertEqual(self.decide("list_item", "list_item", None), SAME)

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
        self.assertEqual(
            jev.boundary_rule("list_item", "list_item", 0)[1], "rule:new_list_item"
        )
        self.assertEqual(
            jev.boundary_rule("list_item", "list_item", 2)[1], "rule:nested_list_item"
        )
        self.assertEqual(
            jev.boundary_rule("list_item", "list_item", None)[1], "rule:same_list_item"
        )


def atoms_from(source, *items):
    """実際の Markdown から、**バイト位置の正しい** Atom 列を作る。

    `items` は `(種別, 本文)` の並びで、本文は `source` の中を先頭から順に探す。
    上の `atom()` は `range` を `index * 100` で捏造するので、行頭を source から
    探す `list_marker_indent` のテストには使えない。

    `range` はバイト位置である。Python の文字列添字は符号位置なので、日本語を
    含む文書ではここを取り違えると全 Atom がずれる。
    """
    raw = source.encode()
    atoms, cursor = [], 0
    for index, (kind, text) in enumerate(items):
        start = raw.index(text.encode(), cursor)
        end = start + len(text.encode())
        atoms.append(
            {
                "index": index,
                "kind": kind,
                "range": {"start": start, "end": end},
                "text": text,
            }
        )
        cursor = end
    return atoms


class ListMarkerIndentTest(unittest.TestCase):
    """項目の先頭か継続文か、先頭ならどの深さか — 構造だけで判別する。"""

    def indents(self, source, *items):
        raw = source.encode()
        return [jev.list_marker_indent(raw, a) for a in atoms_from(source, *items)]

    def test_top_level_items_are_indent_zero(self):
        self.assertEqual(
            self.indents(
                "- 一つ目。\n- 二つ目。\n",
                ("list_item", "- 一つ目。"),
                ("list_item", "- 二つ目。"),
            ),
            [0, 0],
        )

    def test_an_ordered_marker_counts_too(self):
        self.assertEqual(
            self.indents(
                "1. 一つ目\n2) 二つ目\n",
                ("list_item", "1. 一つ目"),
                ("list_item", "2) 二つ目"),
            ),
            [0, 0],
        )

    def test_a_continuation_sentence_has_no_marker(self):
        # 同じ行の 2 文目。行の途中から始まるので項目の先頭ではない。
        self.assertEqual(
            self.indents(
                "1. 一文目。二文目。\n",
                ("list_item", "1. 一文目。"),
                ("list_item", "二文目。"),
            ),
            [0, None],
        )

    def test_a_continuation_on_its_own_line_has_no_marker_either(self):
        self.assertEqual(
            self.indents(
                "- 一文目。\n  二文目。\n",
                ("list_item", "- 一文目。"),
                ("list_item", "二文目。"),
            ),
            [0, None],
        )

    def test_nested_items_carry_their_indent(self):
        self.assertEqual(
            self.indents(
                "- 親\n  - 子 1\n  - 子 2\n- 別の親\n",
                ("list_item", "- 親"),
                ("list_item", "- 子 1"),
                ("list_item", "- 子 2"),
                ("list_item", "- 別の親"),
            ),
            [0, 2, 2, 0],
        )

    def test_a_quote_marker_is_not_indentation(self):
        # `>` と直後の空白 1 つまでが引用の印。ここを数えると、引用の中の
        # トップレベルの項目がすべて「子項目」に見えてしまう。
        self.assertEqual(
            self.indents(
                "> - 一つ目。\n> - 二つ目。\n",
                ("list_item", "> - 一つ目。"),
                ("list_item", "> - 二つ目。"),
            ),
            [0, 0],
        )

    def test_nesting_inside_a_quote_still_counts(self):
        self.assertEqual(
            self.indents(
                "> - 親\n>   - 子\n",
                ("list_item", "> - 親"),
                ("list_item", ">   - 子"),
            ),
            [0, 2],
        )

    def test_the_whole_rule_runs_end_to_end(self):
        source = "- 決定A\n  - 詳細A1。二文目。\n- 決定B\n"
        atoms = atoms_from(
            source,
            ("list_item", "- 決定A"),
            ("list_item", "- 詳細A1。"),
            ("list_item", "二文目。"),
            ("list_item", "- 決定B"),
        )
        plan = jev.plan_boundaries(atoms, source)
        self.assertEqual(
            [(e["decision"], e["by"]) for e in plan],
            [
                (SAME, "rule:nested_list_item"),
                (SAME, "rule:same_list_item"),
                (NEW, "rule:new_list_item"),
            ],
        )


class PlanAndGroupTest(unittest.TestCase):
    def test_an_empty_atom_is_not_asked_about(self):
        atoms = [atom(0, "sentence", "本文がある。"), atom(1, "sentence", "   ")]
        plan = jev.plan_boundaries(atoms, "")
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

    TIERS = ["essential", "detail", "supporting"]

    def build(self):
        return jev.build_units(self.ATOMS, self.UNITS, self.TIERS, self.ANSWERS)

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
        units = jev.build_units(self.ATOMS, self.UNITS, self.TIERS, answers)
        self.assertEqual(units[0]["reading_tier"], "essential")

    def test_a_missing_noul_fails_loudly(self):
        answers = dict(self.ANSWERS)
        answers["redundant:u3"] = {"confidence": 0.5}
        with self.assertRaises(jev.JevError):
            jev.build_units(self.ATOMS, self.UNITS, self.TIERS, answers)

    def test_a_unit_that_was_not_asked_about_redundancy_has_no_relation(self):
        # CONTEXT / DETAIL には redundancy を聞かないので、答えが無い。
        # 黙って REDUNDANT 扱いにも非 REDUNDANT 扱いにもせず、relation を
        # 付けずに通す（聞いていないことは記録にも残らない）。
        answers = {k: v for k, v in self.ANSWERS.items() if not k.startswith("redundant:")}
        units = jev.build_units(self.ATOMS, self.UNITS, self.TIERS, answers)
        self.assertEqual([u["relations"] for u in units], [[], [], []])
        for unit in units:
            self.assertNotIn("redundancy_noul", unit["jev"])

    def test_the_relation_is_dropped_when_no_earlier_unit_overlaps(self):
        # Noul が高くても参照先が選べなければ relation は付けない
        # （プロトコルは実在しない参照先を拒否する）。値は記録に残す。
        atoms = [atom(0, "sentence", "AAAA"), atom(1, "sentence", "ZZZZ")]
        units = jev.build_units(
            atoms,
            [[0], [1]],
            ["essential", "supporting"],
            {
                "tier:u1": {"choice": "essential", "confidence": 0.5},
                "tier:u2": {"choice": "supporting", "confidence": 0.5},
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

    SOURCE = "結論\n\n採用する方式は差分配信である。帯域は 3 割減る見込み。\n"

    def ask(self, units):
        return jev.core_questions(self.ATOMS, units, jev.RequestBudget.estimated(self.SOURCE))

    def test_a_marked_unit_offers_its_prose_atoms_as_choices(self):
        # 見出しは候補に入らない（「1 か所だけ読むなら」の答えにならない）。
        asked = self.ask(self.units())
        self.assertEqual(list(asked), ["core:u1"])
        question = asked["core:u1"]
        self.assertEqual(question["type"], "choice")
        self.assertEqual(
            question["criteria"],
            {
                "atom:1": "採用する方式は差分配信である。",
                "atom:2": "帯域は 3 割減る見込み。",
            },
        )

    def test_code_blocks_and_tables_are_not_core_candidates(self):
        atoms = [
            atom(0, "sentence", "設定はこうする。"),
            atom(1, "code_block", "```\nkey: value\n```"),
            atom(2, "table", "| a | b |"),
        ]
        unit = {"id": "u1", "atoms": [0, 1, 2], "reading_tier": "essential",
                "relations": [], "jev": {}}
        self.assertEqual(set(jev.core_candidates(atoms, unit)), {"atom:0"})

    def test_a_unit_with_no_prose_is_not_asked(self):
        atoms = [atom(0, "heading", "## 設定例"), atom(1, "code_block", "```\nx\n```")]
        unit = {"id": "u1", "atoms": [0, 1], "reading_tier": "essential",
                "relations": [], "jev": {}}
        self.assertEqual(jev.core_questions(atoms, [unit], "## 設定例"), {})
        jev.assign_lone_cores(atoms, [unit])
        # 核が付かないので Unit 全体が MARKED になる（安全側）。
        self.assertNotIn("core_atoms", unit)

    def test_a_single_prose_candidate_becomes_the_core_without_asking(self):
        # 聞かないだけだと核が空になり、見出しごと MARKED に戻ってしまう。
        atoms = [atom(0, "heading", "## 結論"), atom(1, "sentence", "差分配信にする。")]
        unit = {"id": "u1", "atoms": [0, 1], "reading_tier": "essential",
                "relations": [], "jev": {}}
        self.assertEqual(jev.core_questions(atoms, [unit], "## 結論"), {})
        jev.assign_lone_cores(atoms, [unit])
        self.assertEqual(unit["core_atoms"], [1])
        self.assertEqual(unit["jev"]["core_by"], "rule:only_prose_atom")

    def test_the_question_does_not_repeat_the_body_it_already_sends_as_choices(self):
        # 本文を instructions にも入れると同じテキストを 2 回送ることになり、
        # context window を無駄に食う。
        question = self.ask(self.units())["core:u1"]
        for text in question["criteria"].values():
            self.assertNotIn(text, question["instructions"])
        self.assertIn("読み飛ばす", question["instructions"])

    def test_the_core_question_is_phrased_as_a_loss_not_as_a_single_read(self):
        # 旧文面「1 か所だけ読むとしたら、どこを読めば要点が取れるか」は、
        # 後続をまとめる導入文や節の主題ラベルを選ばせる（Jev は正しく答えて
        # いて、問いの方が違う軸を聞いていた）。設計書が挙げる判断の例は
        # すべて「ここを飛ばすと要点を失う？」という損失の形である。
        self.assertIn("読み飛ばす", jev.CORE_INSTRUCTIONS)
        self.assertIn("要点を失う", jev.CORE_INSTRUCTIONS)
        self.assertNotIn("1 か所だけ", jev.CORE_INSTRUCTIONS)
        # 「重要な部分はどれか」に戻すと「どれも重要」と答えられてしまう。
        self.assertNotIn("重要", jev.CORE_INSTRUCTIONS)
        # 本文を instructions に書かない約束はそのまま。
        self.assertLess(len(jev.CORE_INSTRUCTIONS), 200)

    def test_only_units_that_can_become_marked_are_asked(self):
        # MARKED は「ESSENTIAL かつ非 REDUNDANT」だけ。
        for tier in ["supporting", "context", "detail"]:
            self.assertEqual(self.ask(self.units(reading_tier=tier)), {})
        self.assertEqual(
            self.ask(self.units(relations=[{"redundant_with": "u0"}])), {}
        )

    def test_a_unit_with_one_usable_atom_is_not_asked(self):
        # 選択肢が 1 つの Choice は答えが決まっている。核は空のままになり、
        # Atom が 1 つなのだから Unit 全体 = その Atom が MARKED になる。
        self.assertEqual(self.ask(self.units(atoms=[1])), {})
        # 本文が空の Atom は選択肢にならないので、これも 1 つ扱い。
        self.assertEqual(self.ask(self.units(atoms=[3, 4])), {})

    def test_an_answer_becomes_a_single_core_atom(self):
        units = self.units()
        questions = self.ask(units)
        jev.apply_core_answers(
            units, questions, {"core:u1": {"choice": "atom:2", "confidence": 0.42}}
        )
        self.assertEqual(units[0]["core_atoms"], [2])
        self.assertEqual(units[0]["jev"]["core_confidence"], 0.42)

    def test_confidence_never_flips_the_core(self):
        # 閾値で倒すのは採用していない（`probabilities` も見ない）。
        units = self.units()
        questions = self.ask(units)
        jev.apply_core_answers(
            units, questions, {"core:u1": {"choice": "atom:1", "confidence": 0.01}}
        )
        self.assertEqual(units[0]["core_atoms"], [1])

    def test_a_missing_or_unknown_core_answer_fails_loudly(self):
        units = self.units()
        questions = self.ask(units)
        with self.assertRaises(jev.JevError):
            jev.apply_core_answers(units, questions, {})
        with self.assertRaises(jev.JevError):
            # 聞いていない Atom を返されたら通さない。
            jev.apply_core_answers(units, questions, {"core:u1": {"choice": "atom:9"}})

    def test_a_unit_that_was_not_asked_keeps_no_core(self):
        units = self.units(reading_tier="detail")
        jev.apply_core_answers(units, {}, {"core:u1": {"choice": "atom:0"}})
        self.assertNotIn("core_atoms", units[0])


class RunCapTest(unittest.TestCase):
    """1 本のリストにつき核は 1 つ。Tier（沈む側）は項目ごとのまま。"""

    #: `- 決定A。` / `- 決定B。` / `- 決定C。` の 3 項目。
    SOURCE = "- 決定A。\n- 決定B。\n- 決定C。\n"

    def setUp(self):
        self.atoms = atoms_from(
            self.SOURCE,
            ("list_item", "- 決定A。"),
            ("list_item", "- 決定B。"),
            ("list_item", "- 決定C。"),
        )
        self.plan = jev.plan_boundaries(self.atoms, self.SOURCE)

    def units(self, *tiers):
        return [
            {
                "id": f"u{n}",
                "atoms": [n - 1],
                "reading_tier": tier,
                "relations": [],
                "jev": {},
            }
            for n, tier in enumerate(tiers, start=1)
        ]

    def test_a_run_is_the_stretch_joined_by_rule_four(self):
        # 3 項目とも別 Unit で、規則4 の境界 2 本でつながっている。
        self.assertEqual(
            [e["by"] for e in self.plan],
            ["rule:new_list_item", "rule:new_list_item"],
        )
        self.assertEqual(jev.unit_runs([[0], [1], [2]], self.plan), [[0, 1, 2]])

    def test_a_boundary_that_is_not_rule_four_breaks_the_run(self):
        # 見出しや散文で切れたら別のリスト。
        plan = [
            {"after_atom": 0, "decision": jev.NEW, "by": "rule:new_list_item"},
            {"after_atom": 1, "decision": jev.NEW, "by": "rule:next_is_heading"},
            {"after_atom": 2, "decision": jev.NEW, "by": "rule:new_list_item"},
        ]
        self.assertEqual(
            jev.unit_runs([[0], [1], [2], [3]], plan), [[0, 1], [2, 3]]
        )

    def test_one_question_covers_the_whole_run(self):
        units = self.units("essential", "essential", "essential")
        questions, fixed, scope, handled = jev.plan_run_cores(
            self.atoms, units, [[0, 1, 2]], jev.RequestBudget.estimated(self.SOURCE)
        )
        self.assertEqual(list(questions), ["core:run:1"])
        self.assertEqual(
            questions["core:run:1"]["criteria"],
            {"atom:0": "- 決定A。", "atom:1": "- 決定B。", "atom:2": "- 決定C。"},
        )
        self.assertEqual(scope, {"core:run:1": [0, 1, 2]})
        self.assertEqual(handled, frozenset({0, 1, 2}))
        self.assertEqual(fixed, {})

    def test_the_losers_of_a_run_get_an_empty_core_not_a_missing_one(self):
        # ここが肝。`[]` は「核を持たない」で MARKED にならない。省くと
        # 「絞り込み無し」になって Unit 全体が光る。
        units = self.units("essential", "essential", "essential")
        questions, fixed, scope, _ = jev.plan_run_cores(
            self.atoms, units, [[0, 1, 2]], jev.RequestBudget.estimated(self.SOURCE)
        )
        jev.apply_run_cores(
            units, questions, fixed, scope, {"core:run:1": {"choice": "atom:1"}}
        )
        self.assertEqual(units[0]["core_atoms"], [])
        self.assertEqual(units[1]["core_atoms"], [1])
        self.assertEqual(units[2]["core_atoms"], [])
        self.assertEqual(units[0]["jev"]["core_by"], "rule:run_cap")
        self.assertEqual(units[1]["jev"]["core_choice"], "atom:1")

    def test_only_units_that_can_become_marked_join_the_run(self):
        # CONTEXT の項目は核の話に加わらない（Tier は項目ごとのまま効く）。
        units = self.units("essential", "context", "essential")
        questions, _, scope, handled = jev.plan_run_cores(
            self.atoms, units, [[0, 1, 2]], jev.RequestBudget.estimated(self.SOURCE)
        )
        self.assertEqual(
            questions["core:run:1"]["criteria"],
            {"atom:0": "- 決定A。", "atom:2": "- 決定C。"},
        )
        self.assertEqual(scope["core:run:1"], [0, 2])
        self.assertEqual(handled, frozenset({0, 2}))

    def test_a_run_with_a_single_marked_unit_is_left_alone(self):
        # 畳む相手がいないので従来どおり。
        units = self.units("essential", "context", "context")
        questions, fixed, scope, handled = jev.plan_run_cores(
            self.atoms, units, [[0, 1, 2]], jev.RequestBudget.estimated(self.SOURCE)
        )
        self.assertEqual((questions, fixed, scope, handled), ({}, {}, {}, frozenset()))

    def test_a_single_candidate_in_a_run_is_settled_without_asking(self):
        source = "- 決定A。\n-\n"
        atoms = atoms_from(source, ("list_item", "- 決定A。"))
        units = self.units("essential", "essential")
        units[1]["atoms"] = []          # 本文の無い項目は候補を出せない
        questions, fixed, scope, handled = jev.plan_run_cores(
            atoms, units, [[0, 1]], source
        )
        self.assertEqual(questions, {})
        self.assertEqual(fixed, {0: [0], 1: []})
        self.assertEqual(handled, frozenset({0, 1}))
        self.assertEqual(units[0]["jev"]["core_by"], "rule:only_prose_atom_in_run")
        self.assertEqual(units[1]["jev"]["core_by"], "rule:run_cap")

    def test_a_run_over_budget_falls_back_to_per_unit_cores(self):
        """run では予算を超えるが Unit ごとなら収まる、という境目を必ず通す。

        **実文書では 1 度も通っていない枝である**（5 文書で 0 回。`CLAUDE.md` の
        いちばん大きい run も収まった）。ここが間違っていると
        **そのリストの全項目が光る**という最悪の壊れ方をするので、
        予算を人工的に挟んでここで踏んでおく。
        """
        prose = "- " + "あ" * 400                  # 1 Atom あたり約 1,202 バイト
        atoms = [atom(i, "list_item", prose) for i in range(6)]
        units = [
            {
                "id": f"u{n + 1}",
                "atoms": [2 * n, 2 * n + 1],
                "reading_tier": "essential",
                "relations": [],
                "jev": {},
            }
            for n in range(3)
        ]
        body = len(prose.encode()) * jev.TOKENS_PER_BYTE
        # 1 Unit は 2 Atom、run 全体は 6 Atom。その真ん中に予算が来る state を選ぶ。
        target = (2 * body + 6 * body) / 2
        source = "x" * int(
            (jev.STATE_PLUS_QUESTION_LIMIT - jev.CORE_QUESTION_MARGIN - target)
            / jev.TOKENS_PER_BYTE
        )
        budget = jev.RequestBudget.estimated(source)
        self.assertLessEqual(2 * body, budget.pair, "1 Unit ぶんは収まる予算であること")
        self.assertGreater(6 * body, budget.pair, "run 全体は収まらない予算であること")

        questions, fixed, scope, handled = jev.plan_run_cores(
            atoms, units, [[0, 1, 2]], budget
        )
        # run としては面倒を見ない。
        self.assertEqual((questions, fixed, scope, handled), ({}, {}, {}, frozenset()))
        # そして Unit ごとの経路がちゃんと拾う — ここが「全項目が光る」との分かれ目。
        per_unit = jev.core_questions(atoms, units, budget, handled)
        self.assertEqual(sorted(per_unit), ["core:u1", "core:u2", "core:u3"])
        for question in per_unit.values():
            self.assertEqual(len(question["criteria"]), 2)
        jev.apply_core_answers(
            units, per_unit, {key: {"choice": f"atom:{2 * n}"}
                              for n, key in enumerate(sorted(per_unit))}
        )
        for n, unit in enumerate(units):
            self.assertEqual(unit["core_atoms"], [2 * n])
            self.assertNotEqual(unit["core_atoms"], [], "核を持たない扱いにしない")

    def test_the_per_unit_path_skips_what_the_run_already_handled(self):
        units = self.units("essential", "essential", "essential")
        _, _, _, handled = jev.plan_run_cores(
            self.atoms, units, [[0, 1, 2]], jev.RequestBudget.estimated(self.SOURCE)
        )
        self.assertEqual(
            jev.core_questions(
                self.atoms, units, jev.RequestBudget.estimated(self.SOURCE), handled
            ),
            {},
        )
        jev.assign_lone_cores(self.atoms, units, handled)
        for unit in units:
            self.assertNotIn("core_atoms", unit, "run キャップの決定を上書きしない")


class ReferenceImplementationTest(unittest.TestCase):
    """隣の決定論的な参照実装 `annotate-doc.py` が 3 値の意味を壊さないこと。

    `core_atoms` の空の配列は「**核を持たない** = MARKED にならない」という
    意味を持つようになった（`crates/semantic-reading/src/protocol.rs`）。
    `annotate-doc.py` は run のロジックを持たないので、ここが `[]` を出すと
    **意味が変わって黙って何も光らなくなる。**

    いまは `core_atoms` を一切出さない（= `None` = 絞り込み無し = Unit 全体が
    MARKED）。それが正しい振る舞いなので、**出さないことを固定する**。
    """

    REFERENCE = HERE / "annotate-doc.py"

    def units(self):
        source = "# 見出し\n\n本文である。二文目。\n\n- 一つ目。\n- 二つ目。\n"
        raw = source.encode()
        atoms, cursor = [], 0
        for kind, text in [
            ("heading", "# 見出し"),
            ("sentence", "本文である。"),
            ("sentence", "二文目。"),
            ("list_item", "- 一つ目。"),
            ("list_item", "- 二つ目。"),
        ]:
            start = raw.index(text.encode(), cursor)
            atoms.append(
                {
                    "index": len(atoms),
                    "kind": kind,
                    "range": {"start": start, "end": start + len(text.encode())},
                    "text": text,
                }
            )
            cursor = start + len(text.encode())
        proc = subprocess.run(
            [sys.executable, str(self.REFERENCE)],
            input=json.dumps({"version": 1, "source": source, "atoms": atoms}),
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout)["units"]

    def test_the_reference_never_emits_core_atoms(self):
        units = self.units()
        self.assertTrue(units, "Unit が 1 つも返っていない")
        for unit in units:
            self.assertNotIn(
                "core_atoms",
                unit,
                "`[]` を出すと「核を持たない」の意味になり、何も光らなくなる",
            )


class CoreBudgetTest(unittest.TestCase):
    """核 question の上限は state の大きさから決まる。

    制約は「`state` + 最長 question ≦ 32k」なので、固定の選択肢数では
    正しくならない（`docs/gotchas.md`）。
    """

    def test_the_budget_shrinks_as_the_state_grows(self):
        small = jev.RequestBudget.estimated("あ" * 100)
        large = jev.RequestBudget.estimated("あ" * 100_000)
        self.assertGreater(small.pair, large.pair)
        self.assertGreater(small.whole, large.whole)

    def test_a_huge_state_leaves_no_budget_at_all(self):
        # state だけで 32k を使い切る文書では、核はどうやっても聞けない。
        self.assertLessEqual(jev.RequestBudget.estimated("あ" * 40_000).pair, 0)

    def test_the_measured_worst_case_still_fits(self):
        # 実測（2026-09-21）: 45,650 バイトの文書の最大 Unit は選択肢 82 個・
        # 本文 11,026 バイトで、question は 5,469 tokens だった。
        budget = jev.RequestBudget.estimated("x" * 45_650)
        self.assertGreater(budget.pair, 0)
        self.assertTrue(
            jev.core_fits({f"atom:{i}": "x" * 134 for i in range(82)}, budget)
        )

    def test_a_unit_over_the_budget_is_not_asked(self):
        # 上限を超えたら核を聞かない。核が無ければ Unit 全体が MARKED に
        # なるだけで、注釈としては壊れない（安全側）。
        source = "あ" * 20_000
        atoms = [atom(i, "sentence", "文" * 4_000) for i in range(4)]
        unit = {"id": "u1", "atoms": [0, 1, 2, 3], "reading_tier": "essential",
                "relations": [], "jev": {}}
        self.assertEqual(
            jev.core_questions(atoms, [unit], jev.RequestBudget.estimated(source)), {}
        )
        # 同じ Unit でも state が小さければ聞ける。
        self.assertIn(
            "core:u1",
            jev.core_questions(atoms, [unit], jev.RequestBudget.estimated("短い。")),
        )


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

    def test_round_two_asks_only_a_tier_and_asks_it_for_every_unit(self):
        # redundancy はここでは聞かない — Tier が決まるまで「誰に聞くか」が
        # 分からないため。
        asked = self.payload()["rounds"][1]["questions"]
        self.assertTrue(asked)
        self.assertEqual([k for k in asked if not k.startswith("tier:")], [])
        for key, question in asked.items():
            self.assertEqual(question["type"], "choice")
            self.assertEqual(set(question["criteria"]), set(jev.TIER_CRITERIA))

    def test_round_three_asks_redundancy_only_for_supporting_or_better(self):
        # dry-run は全 Unit を essential と仮定するので、先頭以外すべてに付く。
        asked = self.payload()["rounds"][2]["questions"]
        redundant = sorted(k for k in asked if k.startswith("redundant:"))
        tiers = self.payload()["rounds"][1]["questions"]
        self.assertEqual(len(redundant), len(tiers) - 1, "先頭 Unit だけ聞かない")
        self.assertNotIn("redundant:u1", asked)
        for key in redundant:
            self.assertEqual(asked[key]["type"], "noul")
            # redundancy は方向を必ず指定する（対称に聞くと結論まで拾う）。
            self.assertIn("これより**前**の箇所", asked[key]["instructions"])

    def test_redundancy_skips_context_and_detail(self):
        atoms = [atom(i, "sentence", f"文{i}。") for i in range(4)]
        units = [[0], [1], [2], [3]]
        tiers = ["essential", "supporting", "context", "detail"]
        asked = jev.redundancy_questions(atoms, units, tiers)
        # u1 は先頭なので聞かない。u3 / u4 は CONTEXT / DETAIL なので聞かない。
        self.assertEqual(sorted(asked), ["redundant:u2"])

    def test_round_three_asks_for_the_core_of_multi_prose_units_only(self):
        rounds = self.payload()["rounds"]
        self.assertEqual(len(rounds), 3)
        units = {
            f"u{n}": indices
            for n, indices in enumerate(
                jev.group_units(
                    self.request["atoms"],
                    [
                        dict(entry, decision=entry["decision"] or jev.NEW)
                        for entry in jev.plan_boundaries(self.request["atoms"], self.request["source"])
                    ],
                ),
                start=1,
            )
        }
        asked = {k: v for k, v in rounds[2]["questions"].items() if k.startswith("core:")}
        self.assertTrue(asked, "核を聞ける Unit が 1 つも無い")
        kinds = {a["index"]: a["kind"] for a in self.request["atoms"]}
        for key, question in asked.items():
            uid = key.split(":", 1)[1]
            self.assertEqual(question["type"], "choice")
            self.assertGreaterEqual(len(question["criteria"]), 2)
            chosen = {int(k.split(":")[1]) for k in question["criteria"]}
            # 選択肢はその Unit の Atom だけで、かつ散文だけ。
            self.assertLessEqual(chosen, set(units[uid]))
            for index in chosen:
                self.assertIn(kinds[index], jev.PROSE_KINDS)
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


class SendInChunksTest(unittest.TestCase):
    """リクエスト分割 — [`plan_chunks`] / [`send_in_chunks`] / [`RequestBudget`]。

    **ラウンド 2 専用のテストにしないこと。** 分割はどのラウンドからも使う
    1 つの実装なので、ここで確かめるのも question の中身に依存しない性質だけ
    である（トークンで切る / `pair` を超えたら送らない / 取りこぼさない）。
    """

    def question(self, body_bytes: int) -> dict:
        return {
            "type": "choice",
            "instructions": "問い。" + "x" * body_bytes,
            "criteria": dict(jev.TIER_CRITERIA),
        }

    # --- 個数ではなくトークンで切る ------------------------------------

    def test_chunks_are_cut_by_tokens_not_by_count(self):
        """同じ**個数**でも、本文が大きければチャンクは増える。"""
        budget = jev.RequestBudget(state_tokens=1_000)
        small = {f"q{i}": self.question(100) for i in range(20)}
        large = {f"q{i}": self.question(20_000) for i in range(20)}
        self.assertEqual(len(jev.plan_chunks(small, budget)[0]), 1)
        self.assertGreater(len(jev.plan_chunks(large, budget)[0]), 1)

    def test_every_chunk_fits_the_whole_request_limit(self):
        budget = jev.RequestBudget(state_tokens=5_000)
        questions = {f"q{i}": self.question(9_000) for i in range(40)}
        chunks, dropped = jev.plan_chunks(questions, budget)
        self.assertEqual(dropped, [])
        self.assertGreater(len(chunks), 1)
        for chunk in chunks:
            spent = sum(jev.question_tokens(q) for q in chunk.values())
            self.assertLessEqual(budget.state_tokens + spent, jev.REQUEST_LIMIT)
            self.assertLessEqual(spent, budget.whole)

    def test_nothing_is_lost_or_duplicated_by_the_split(self):
        budget = jev.RequestBudget(state_tokens=5_000)
        questions = {f"q{i}": self.question(9_000) for i in range(40)}
        chunks, _ = jev.plan_chunks(questions, budget)
        seen = [key for chunk in chunks for key in chunk]
        self.assertEqual(seen, list(questions), "順も中身も変えない")

    # --- 32k の側のガード ------------------------------------------------

    def test_a_question_too_large_for_the_pair_limit_is_never_sent(self):
        """`state` + その question が 32k を超えるものは、どう分けても送れない。"""
        budget = jev.RequestBudget(state_tokens=20_000)
        questions = {
            "ok": self.question(1_000),
            "huge": self.question(40_000),
        }
        chunks, dropped = jev.plan_chunks(questions, budget)
        self.assertEqual(dropped, ["huge"])
        self.assertEqual([sorted(c) for c in chunks], [["ok"]])

    def test_the_pair_limit_is_what_binds_not_the_whole_limit(self):
        """`pair` は必ず `whole` より小さい。

        だから `pair` を通った question は、空のチャンクには必ず収まる
        （収まらないと無限ループになる）。
        """
        for state_tokens in (0, 1_000, 20_000, 31_000):
            with self.subTest(state_tokens=state_tokens):
                budget = jev.RequestBudget(state_tokens=state_tokens)
                self.assertLess(budget.pair, budget.whole)

    def test_a_state_that_fills_the_pair_limit_sends_nothing(self):
        budget = jev.RequestBudget(state_tokens=32_000)
        chunks, dropped = jev.plan_chunks({"q": self.question(10)}, budget)
        self.assertEqual((chunks, dropped), ([], ["q"]))

    # --- 小さい文書の挙動が変わらない ------------------------------------

    def test_small_documents_still_go_in_one_request(self):
        """合格条件 3 — 分割が不要なら 1 リクエストのまま。

        実文書で確かめる。`demo.md` と `design/semantic-reading-layer.md` は
        3 ラウンドとも 1 チャンクでなければならない（見積もりは実測の
        1.4 倍まで過大評価するので、ここが本番より厳しい側の判定になる）。
        """
        for name in ("demo.md", "../../docs/design/semantic-reading-layer.md"):
            with self.subTest(document=name):
                source = (HERE / name).read_text(encoding="utf-8")
                request = dump_request(HERE / name)
                plan = jev.dry_run(request, "jev-latest")
                for entry in plan["rounds"]:
                    self.assertEqual(
                        entry["plan"]["chunks"], 1, f"round {entry['round']}"
                    )
                    self.assertEqual(entry["plan"]["unsent"], [])
                self.assertEqual(
                    plan["budget"]["state_tokens"], jev.estimate_tokens(source)
                )

    # --- 見積もりの精度 ---------------------------------------------------

    def test_the_estimate_is_conservative_but_not_wildly_so(self):
        """実測 181 tokens の Tier question を、見積もりが 1.0〜1.6 倍で返す。

        **下回ってはいけない** — 見積もりが小さいと上限を超えた question を
        送って 400 で落ちる。**大きすぎてもいけない** — 収まる文書を無駄に
        分割し、`state` を余分に課金する。実測の内訳（`docs/gotchas.md`
        未解決 5）は器 68 / criteria 65 / 枠組み文 65 = 181。
        """
        tier = jev.unit_questions(
            [{"kind": "sentence", "text": "", "range": [0, 0]}], [[0]]
        )["tier:u1"]
        estimate = jev.question_tokens(tier)
        self.assertGreaterEqual(estimate, 181)
        self.assertLessEqual(estimate, int(181 * 1.6))

    def test_the_criteria_keys_are_counted(self):
        """`atom:123` のようなキーは選択肢の数だけ並ぶので、無視できない。"""
        few = {"type": "choice", "instructions": "問い。", "criteria": {"a": "x"}}
        many = {
            "type": "choice",
            "instructions": "問い。",
            "criteria": {f"atom:{i}": "x" for i in range(100)},
        }
        self.assertGreater(
            jev.question_tokens(many) - jev.question_tokens(few),
            100,
            "キーを数えていないと、この差はほぼ 0 になる",
        )

    # --- state のトークン数 -----------------------------------------------

    def test_a_small_state_is_estimated_without_spending_a_request(self):
        calls = []
        budget = self.measure("短い文書。" * 10, calls)
        self.assertEqual(calls, [], "小さい文書で余分なリクエストを使わない")
        self.assertFalse(budget.measured)
        self.assertIsNone(budget.probe)

    def test_a_large_state_is_measured_from_usage(self):
        calls = []
        budget = self.measure("あ" * 40_000, calls)
        self.assertEqual(len(calls), 1, "実測は 1 リクエストだけ")
        self.assertEqual(sorted(calls[0]), ["state-probe"])
        self.assertTrue(budget.measured)
        self.assertEqual(budget.state_tokens, 12_345)
        # probe も記録に残す。残さないとリクエスト数と tokens の合計が食い違う。
        self.assertEqual(budget.probe["round"], "probe")
        self.assertEqual(budget.probe["usage"], {"input_tokens": 12_345})
        # 見積もり（0.5/byte で 60,000）を信じていたら pair は負になっていた。
        self.assertGreater(budget.pair, 0)

    def test_a_probe_without_usage_falls_back_to_the_estimate(self):
        calls = []
        budget = self.measure("あ" * 40_000, calls, usage=None)
        self.assertFalse(budget.measured)
        self.assertEqual(budget.state_tokens, jev.estimate_tokens("あ" * 40_000))

    def measure(self, state, calls, usage={"input_tokens": 12_345}):
        payload = {"answers": {"state-probe": {"noul": 0.5}}}
        if usage is not None:
            payload["usage"] = usage

        def fake(state_, questions, model, timeout):
            calls.append(questions)
            return dict(payload)

        return with_fake_ask(fake, lambda: jev.measure_state_tokens(state, "m", 1.0))

    # --- マージ -----------------------------------------------------------

    def test_answers_from_every_chunk_are_merged(self):
        budget = jev.RequestBudget(state_tokens=1_000)
        questions = {f"q{i}": self.question(30_000) for i in range(8)}

        def fake(state, chunk, model, timeout):
            return {
                "answers": {key: {"choice": key} for key in chunk},
                "usage": {"input_tokens": 1},
            }

        answers, records, dropped = with_fake_ask(
            fake,
            lambda: jev.send_in_chunks("state", questions, budget, "m", 1.0),
        )
        self.assertEqual(sorted(answers), sorted(questions))
        self.assertEqual(dropped, [])
        self.assertGreater(len(records), 1, "この大きさは 1 リクエストに入らない")
        self.assertEqual(sum(r["questions"] for r in records), len(questions))

    def test_the_same_key_from_two_chunks_fails_loudly(self):
        """黙って上書きしない。`choice_of` と同じ作法。"""
        budget = jev.RequestBudget(state_tokens=1_000)
        questions = {f"q{i}": self.question(30_000) for i in range(8)}

        def fake(state, chunk, model, timeout):
            # どのチャンクも同じキーを返す、壊れた相手。
            return {"answers": {"q0": {"choice": "q0"}}}

        with self.assertRaises(jev.JevError) as caught:
            with_fake_ask(
                fake,
                lambda: jev.send_in_chunks("state", questions, budget, "m", 1.0),
            )
        self.assertIn("q0", str(caught.exception))

    def test_a_missing_answer_is_not_filled_in_silently(self):
        """欠けた答えは、後段の [`choice_of`] が失敗させる。"""
        budget = jev.RequestBudget(state_tokens=1_000)
        answers, _, _ = with_fake_ask(
            lambda state, chunk, model, timeout: {"answers": {}},
            lambda: jev.send_in_chunks(
                "state", {"tier:u1": self.question(10)}, budget, "m", 1.0
            ),
        )
        with self.assertRaises(jev.JevError):
            jev.choice_of(answers, "tier:u1", jev.TIER_CRITERIA)

    def test_the_state_is_sent_whole_with_every_chunk(self):
        """`state` は毎回丸ごと乗る — 切り詰めない（設計書「全文を Context」）。"""
        budget = jev.RequestBudget(state_tokens=1_000)
        questions = {f"q{i}": self.question(30_000) for i in range(8)}
        seen = []

        def fake(state, chunk, model, timeout):
            seen.append(state)
            return {"answers": {key: {"choice": key} for key in chunk}}

        with_fake_ask(
            fake,
            lambda: jev.send_in_chunks("文書全文", questions, budget, "m", 1.0),
        )
        self.assertGreater(len(seen), 1)
        self.assertEqual(set(seen), {"文書全文"})


class UnansweredTierTest(unittest.TestCase):
    """送れなかった Tier question の落とし先（[`UNANSWERED_TIER`]）。

    **実文書では 1 度も通っていない枝である**（5 文書で 0 件）。この経路に
    来るには 1 つの Unit の本文が 18 KB 前後になる必要がある。決めた形を
    ここで固定しておく。
    """

    def test_the_fallback_never_becomes_marked(self):
        """既定の Tier が何であれ、`core_atoms: []` で MARKED にならない。"""
        self.assertEqual(jev.UNANSWERED_TIER, "detail")

    def test_an_oversized_unit_gets_the_fallback_and_an_empty_core(self):
        # state + この Unit の本文だけで 32k を超える文書を作る。
        body = "あ" * 12_000                       # 36,000 バイト
        source = "あ" * 10_000 + "\n\n" + body
        atoms = [
            {"index": 0, "kind": "sentence", "text": "短い文。", "range": [0, 12]},
            {"index": 1, "kind": "heading", "text": "見出し", "range": [12, 21]},
            {"index": 2, "kind": "sentence", "text": body, "range": [21, 36_021]},
        ]
        request = {"version": jev.VERSION, "source": source, "atoms": atoms}

        def fake(state, chunk, model, timeout):
            answers = {}
            for key in chunk:
                if key == "state-probe":
                    return {"answers": {key: {"noul": 0.1}},
                            "usage": {"input_tokens": 20_000}}
                answers[key] = {"choice": "essential", "noul": 0.1}
            return {"answers": answers}

        out = with_fake_ask(fake, lambda: jev.annotate(request, "m", 1.0))
        unsent = out["jev"]["unsent"]["tier"]
        self.assertEqual(len(unsent), 1, "巨大な Unit の Tier だけが送れない")
        number = int(unsent[0].split("u")[1])
        unit = out["units"][number - 1]
        self.assertEqual(unit["reading_tier"], jev.UNANSWERED_TIER)
        self.assertEqual(
            unit["core_atoms"], [], "核を持たない = MARKED にならない"
        )
        self.assertEqual(unit["jev"]["tier_by"], "rule:question_too_large")
        # 残りの Unit は普通に判定されている — 文書全体を失敗させない。
        others = [u for n, u in enumerate(out["units"], 1) if n != number]
        self.assertTrue(others)
        for other in others:
            self.assertEqual(other["reading_tier"], "essential")

    def test_a_document_where_nothing_can_be_asked_fails_loudly(self):
        """全部が既定値の注釈を exit 0 で返さない。

        **実測で踏んだ枝である。** 83 KB の文書は `state` が 30,808 tokens で
        32k の probe は通るのに、question に残る余地が **負**になる。ここを
        塞ぐ前は **530 Unit すべてが `detail`** の応答を exit 0 で返していた
        — `state` が収まっているぶん、いちばん気づきにくい壊れ方をする。

        [`UNANSWERED_TIER`] は個別の巨大な Unit のための落とし先であって、
        文書全体の落とし先ではない。
        """
        source = "あ" * 30_000
        atoms = [atom(i, "sentence", "文" * 300) for i in range(3)]
        request = {"version": jev.VERSION, "source": source, "atoms": atoms}

        def fake(state, chunk, model, timeout):
            if "state-probe" in chunk:
                return {"answers": {"state-probe": {"noul": 0.1}},
                        "usage": {"input_tokens": 30_800}}
            raise AssertionError("1 つも送れないはずのリクエストが飛んだ")

        with self.assertRaises(jev.JevError) as caught:
            with_fake_ask(fake, lambda: jev.annotate(request, "m", 1.0))
        self.assertIn("大きすぎ", str(caught.exception))
        self.assertIn("30800", str(caught.exception).replace(",", ""))

    def test_one_answerable_question_is_enough_to_keep_going(self):
        """一部だけ送れないなら、既定値へ倒して続ける（文書は失敗させない）。"""
        source = "あ" * 10_000
        atoms = [
            atom(0, "sentence", "短い文。"),
            atom(1, "heading", "見出し"),
            atom(2, "sentence", "あ" * 12_000),
        ]
        request = {"version": jev.VERSION, "source": source, "atoms": atoms}

        def fake(state, chunk, model, timeout):
            if "state-probe" in chunk:
                return {"answers": {"state-probe": {"noul": 0.1}},
                        "usage": {"input_tokens": 20_000}}
            return {"answers": {k: {"choice": "essential", "noul": 0.1} for k in chunk}}

        out = with_fake_ask(fake, lambda: jev.annotate(request, "m", 1.0))
        tiers = [u["reading_tier"] for u in out["units"]]
        self.assertIn(jev.UNANSWERED_TIER, tiers)
        self.assertIn("essential", tiers)

    def test_the_unit_is_kept_so_the_budget_denominator_does_not_move(self):
        """送れなかった Unit も落とさない。

        落とすと `decorate` の `total`（全 Unit のバイト長の合計）が黙って縮み、
        「Budget 50 %」が指す量が文書によって変わる。
        """
        source = "あ" * 10_000
        atoms = [
            atom(0, "sentence", "短い文。"),
            atom(1, "heading", "見出し"),
            atom(2, "sentence", "あ" * 12_000),
        ]
        request = {"version": jev.VERSION, "source": source, "atoms": atoms}

        def fake(state, chunk, model, timeout):
            if "state-probe" in chunk:
                return {"answers": {"state-probe": {"noul": 0.1}},
                        "usage": {"input_tokens": 20_000}}
            return {"answers": {k: {"choice": "essential", "noul": 0.1} for k in chunk}}

        out = with_fake_ask(fake, lambda: jev.annotate(request, "m", 1.0))
        covered = sorted(i for unit in out["units"] for i in unit["atoms"])
        self.assertEqual(covered, [0, 1, 2], "Atom を 1 つも取りこぼさない")


def with_fake_ask(fake, body):
    """[`ask_jev`] を差し替えて `body()` を呼ぶ（API は叩かない）。"""
    real = jev.ask_jev
    jev.ask_jev = fake
    try:
        return body()
    finally:
        jev.ask_jev = real


def dump_request(path):
    """`dump-request` と同じ形の要求 JSON を、本物の `atomize` から作る。"""
    out = subprocess.run(
        ["cargo", "run", "--quiet", "-p", "semantic-reading",
         "--example", "dump-request", "--", str(path)],
        capture_output=True, text=True, cwd=str(HERE.parents[1]), check=True,
    )
    return json.loads(out.stdout)


if __name__ == "__main__":
    unittest.main()
