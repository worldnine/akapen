"""`jev-annotate.py` のテスト — **API を叩かない**。

    python3 -m unittest discover -s examples/semantic -p 'test_*.py'

外部ネットワークに依存するテストは CI で落ちるので、ここで確かめるのは

- 構造ルール（見出し / コード / リスト / 引用 / 散文の各組み合わせ）
- Jev のレスポンスを模したフィクスチャから Unit を組み立てる処理
- ラウンド 3（Unit の核）を聞く対象の絞り方と、答えの書き戻し
- `--dry-run` が送ろうとするリクエストの形（state / model / questions）
- 鍵の取り出し（環境変数 → macOS のキーチェーン → 案内つきで停止）

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
import tempfile
import unittest
import unittest.mock
from pathlib import Path

HERE = Path(__file__).resolve().parent

#: 設計書の凍結コピー（`HERE` からの相対パス）。
#:
#: **正典（`docs/design/semantic-reading-layer.md`）は読まない。** 直接読んで
#: いた頃は、この下の `test_small_documents_still_go_in_one_request` が設計書の
#: 大きさに上限を掛けていて、**書き足せる余地は実測で 30〜45 バイト**しか
#: 残っていなかった。正典を自由に伸ばせるように 2026-09-22 の内容で凍結した
#: コピーへ移した。ここに要るのは「1 リクエストに収まる実文書」という性質
#: だけなので、**コピーの内容は更新しない**。コピーは逐語ではなく、注記の
#: ぶんの余地を作るために 2 節を落としてある（残りは実測 2,282 tokens）。
#: 経緯は `crates/semantic-reading/tests/fixtures/README.md`。
FROZEN_DESIGN_DOC = (
    "../../crates/semantic-reading/tests/fixtures/design-doc-frozen-2026-09-22.md"
)
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

    def test_a_table_stays_one_unit_even_though_it_splits_into_rows(self):
        # 表は 1 つのまとまり。行に下りるのは核だけである。
        self.assertEqual(self.decide("table", "table_row"), SAME)
        self.assertEqual(self.decide("table_row", "table_row"), SAME)
        # 表の手前と後ろでは切れる。
        self.assertEqual(self.decide("sentence", "table"), NEW)
        self.assertEqual(self.decide("table_row", "sentence"), NEW)
        self.assertEqual(self.decide("table_row", "heading"), NEW)

    def test_two_adjacent_tables_do_not_merge(self):
        # 「どちらも表の種別なら SAME」と書くと、空行で隣り合う 2 つの表が
        # 1 Unit へ融合する。見るのは**次**だけである。
        self.assertEqual(self.decide("table_row", "table"), NEW)
        self.assertEqual(self.decide("table", "table"), NEW)

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
        self.assertEqual(jev.boundary_rule("table", "table_row")[1], "rule:same_table")
        self.assertEqual(
            jev.boundary_rule("table_row", "table")[1], "rule:standalone_block"
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


class SectionTest(unittest.TestCase):
    """節の見出し（`section_of`）は構造だけで決まる。Jev は出てこない。"""

    def sections(self, atoms, units):
        built = [{"id": f"u{n}"} for n, _ in enumerate(units, start=1)]
        jev.assign_sections(atoms, units, built)
        return [unit.get("section_of") for unit in built]

    def test_a_heading_level_is_the_number_of_hashes(self):
        self.assertEqual(jev.heading_level(atom(0, "heading", "## 節")), 2)
        self.assertEqual(jev.heading_level(atom(0, "heading", "#### 深い節")), 4)
        # 引用の印は深さに数えない。
        self.assertEqual(jev.heading_level(atom(0, "heading", "> ### 引用の中")), 3)
        # 見出しでない Atom は None。
        self.assertIsNone(jev.heading_level(atom(0, "sentence", "## ではない")))

    def test_a_setext_heading_gets_its_level_from_the_underline(self):
        self.assertEqual(jev.heading_level(atom(0, "heading", "見出し\n=====")), 1)
        self.assertEqual(jev.heading_level(atom(0, "heading", "見出し\n-----")), 2)

    def test_an_unreadable_heading_falls_to_the_deepest_level(self):
        # 浅い側へ倒すと外側の節を誤って閉じる。深い側なら次の見出しが閉じる。
        self.assertEqual(jev.heading_level(atom(0, "heading", "")), 6)

    def test_content_belongs_to_the_nearest_heading_above_it(self):
        atoms = [
            atom(0, "heading", "# 表題"),
            atom(1, "heading", "## 節"),
            atom(2, "list_item", "- 一つ目"),
            atom(3, "list_item", "- 二つ目"),
        ]
        # 規則 4 で項目ごとに割れた形。2 つ目以降が別 Unit として浮く。
        self.assertEqual(
            self.sections(atoms, [[0], [1, 2], [3]]),
            [None, "u1", "u2"],
        )

    def test_a_heading_points_at_its_parent_section(self):
        # 入れ子はこのフィールドだけで伝わる（crate は `#` の数を知らない）。
        atoms = [
            atom(0, "heading", "# 表題"),
            atom(1, "heading", "## 決定"),
            atom(2, "heading", "### 費用"),
            atom(3, "sentence", "本文。"),
        ]
        self.assertEqual(
            self.sections(atoms, [[0], [1], [2], [3]]),
            [None, "u1", "u2", "u3"],
        )

    def test_a_sibling_heading_closes_the_previous_section(self):
        atoms = [
            atom(0, "heading", "## A"),
            atom(1, "heading", "### A1"),
            atom(2, "sentence", "本文。"),
            atom(3, "heading", "## B"),
            atom(4, "sentence", "本文。"),
        ]
        self.assertEqual(
            self.sections(atoms, [[0], [1], [2], [3], [4]]),
            [None, "u1", "u2", None, "u4"],
        )

    def test_text_before_the_first_heading_has_no_section(self):
        atoms = [
            atom(0, "sentence", "前書き。"),
            atom(1, "heading", "# 表題"),
            atom(2, "sentence", "本文。"),
        ]
        self.assertEqual(self.sections(atoms, [[0], [1], [2]]), [None, None, "u2"])


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
            "wants_core": True,
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

    def test_code_blocks_and_table_headers_are_not_core_candidates(self):
        # `table` は表の**ヘッダ行**（＋区切り行）である。列の名前を挙げても
        # 中身を言ったことにならないので、見出しと同じく候補から外す。
        atoms = [
            atom(0, "sentence", "設定はこうする。"),
            atom(1, "code_block", "```\nkey: value\n```"),
            atom(2, "table", "| a | b |\n| - | - |"),
        ]
        unit = {"id": "u1", "atoms": [0, 1, 2], "wants_core": True, "jev": {}}
        self.assertEqual(set(jev.core_candidates(atoms, unit)), {"atom:0"})

    def test_table_rows_are_core_candidates_but_the_header_is_not(self):
        # 表の中身が一度も光らなかったのを直した分（2026-09-22）。候補は
        # データ行だけで、そこから「1 行だけ読むならどれか」を聞く。
        atoms = [
            atom(0, "table", "| 項目 | 値 |\n| --- | --- |"),
            atom(1, "table_row", "| 応答 | 1.2 秒 |"),
            atom(2, "table_row", "| 費用 | 5 円 |"),
        ]
        unit = {"id": "u1", "atoms": [0, 1, 2], "wants_core": True, "jev": {}}
        self.assertEqual(set(jev.core_candidates(atoms, unit)), {"atom:1", "atom:2"})

    def test_a_table_with_one_row_gets_its_core_without_asking(self):
        atoms = [
            atom(0, "table", "| 項目 | 値 |\n| --- | --- |"),
            atom(1, "table_row", "| 応答 | 1.2 秒 |"),
        ]
        unit = {"id": "u1", "atoms": [0, 1], "wants_core": True, "jev": {}}
        self.assertEqual(jev.core_questions(atoms, [unit], "| 項目 | 値 |"), {})
        jev.assign_lone_cores(atoms, [unit])
        self.assertEqual(unit["core_atoms"], [1])

    def test_a_unit_with_no_prose_is_not_asked(self):
        atoms = [atom(0, "heading", "## 設定例"), atom(1, "code_block", "```\nx\n```")]
        unit = {"id": "u1", "atoms": [0, 1], "wants_core": True, "jev": {}}
        self.assertEqual(jev.core_questions(atoms, [unit], "## 設定例"), {})
        jev.assign_lone_cores(atoms, [unit])
        # 核が付かないので Unit 全体が MARKED になる（安全側）。
        self.assertNotIn("core_atoms", unit)

    def test_a_single_prose_candidate_becomes_the_core_without_asking(self):
        # 聞かないだけだと核が空になり、見出しごと MARKED に戻ってしまう。
        atoms = [atom(0, "heading", "## 結論"), atom(1, "sentence", "差分配信にする。")]
        unit = {"id": "u1", "atoms": [0, 1], "wants_core": True, "jev": {}}
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

    def test_only_units_above_the_core_floor_are_asked(self):
        # 核を聞くのは足切りを越えた Unit だけ。越えなかった Unit は光らない
        # ので、核を聞いても答えが画面に出ない（[`wants_core`]）。
        self.assertEqual(self.ask(self.units(wants_core=False)), {})
        self.assertEqual(sorted(self.ask(self.units())), ["core:u1"])

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
        units = self.units(wants_core=False)
        jev.apply_core_answers(units, {}, {"core:u1": {"choice": "atom:0"}})
        self.assertNotIn("core_atoms", units[0])


class ReferenceImplementationTest(unittest.TestCase):
    """隣の決定論的な参照実装 `annotate-doc.py` が 3 値の意味を守ること。

    `core_atoms` の空の配列は「**核を持たない** = MARKED にならない」という
    意味を持つ（`crates/semantic-reading/src/protocol.rs`）。参照実装は
    Atom 1 つを Unit 1 つにするので、散文ならその Atom 自身が核、見出しや
    コードなら `[]` になる。**`[]` と「省略」を取り違えない**ことを固定する。
    """

    REFERENCE = HERE / "annotate-doc.py"

    def request(self, with_question=True):
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
        request = {"version": 1, "source": source, "atoms": atoms}
        if with_question:
            request["question"] = {
                "id": "essential",
                "text": "テスト用の問い。",
                "core_floor": 0.2,
            }
        return request

    def run_reference(self, request):
        return subprocess.run(
            [sys.executable, str(self.REFERENCE)],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            timeout=60,
        )

    def test_the_reference_scores_every_unit_and_echoes_the_question(self):
        proc = self.run_reference(self.request())
        self.assertEqual(proc.returncode, 0, proc.stderr)
        answer = json.loads(proc.stdout)
        self.assertEqual(answer["question"], "essential", "問いの id を echo する")
        units = answer["units"]
        self.assertTrue(units, "Unit が 1 つも返っていない")
        for unit in units:
            self.assertIn("score", unit, "スコアの無い Unit は光らない")
            self.assertIsInstance(unit["core_atoms"], list)

    def test_prose_gets_its_own_atom_as_its_core_and_a_heading_gets_none(self):
        answer = json.loads(self.run_reference(self.request()).stdout)
        units = answer["units"]
        # Atom 0 は見出し。核の候補にならないので `[]`（= 核を持たない）。
        self.assertEqual(units[0]["core_atoms"], [], "見出しは核を持たない")
        # 散文は自分自身が核。**省略しない** — 省くと「絞り込み無し」に
        # なり、Unit 全体が光る。
        for position in (1, 2, 3, 4):
            self.assertEqual(units[position]["core_atoms"], [position])

    def test_a_request_without_a_question_is_refused(self):
        # DIM 版は 2026-09-22 に削除された。問いを持たない要求は、黙って
        # 別のものを返すのではなく**明確なエラーで終わる**。
        proc = self.run_reference(self.request(with_question=False))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("no question", proc.stderr)
        self.assertEqual(proc.stdout, "")


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
        unit = {"id": "u1", "atoms": [0, 1, 2, 3], "wants_core": True, "jev": {}}
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
        fixture = json.loads((HERE / "demo-marks.json").read_text())
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
        cls.request = {
            "version": 1,
            "source": source,
            "atoms": atoms,
            # **問いが要る。** 文面が question の大きさをそのまま決めるので、
            # 載せずに出した数字は本番の予測にならない。
            "question": {"id": "essential", "text": "テスト用の問い。", "core_floor": 0.2},
        }
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

    def test_round_two_asks_the_question_once_per_unit(self):
        asked = self.payload()["rounds"][1]["questions"]
        self.assertTrue(asked)
        self.assertEqual([k for k in asked if not k.startswith("marks:")], [])
        for question in asked.values():
            self.assertEqual(question["type"], "noul")
            # 問いの文面は akapen が送ってきたものそのまま。枠と本文だけ足す。
            self.assertTrue(question["instructions"].startswith("テスト用の問い。"))
            self.assertIn("――― 対象 ―――", question["instructions"])

    def test_round_three_asks_for_the_core_of_multi_prose_units_only(self):
        rounds = self.payload()["rounds"]
        # 境界 → スコア → 核 の 3 ラウンド。
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
        self.assertEqual([k for k in rounds[2]["questions"] if not k.startswith("core:")], [])
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
        self.assertIn("core floor", assumptions)

    def test_a_dry_run_without_a_question_is_refused(self):
        request = dict(self.request)
        request.pop("question")
        proc = self.run_script(["--dry-run"], request)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("question", proc.stderr)

    def test_the_boundary_question_asks_about_skimming_not_topic(self):
        # v1 の「話題が同じか」に戻すと、結論の行が 1 つの Unit にまとまって
        # 行の途中で表示が切り替わらなくなる。
        question = next(iter(self.payload()["rounds"][0]["questions"].values()))
        self.assertIn("拾い読み", question["instructions"])
        self.assertIn("読む優先度", question["criteria"]["same_unit"])


class ApiKeyTest(unittest.TestCase):
    """鍵の取り出し（[`jev.api_key`]）。**鍵の値はこのファイルに書かない。**

    ここで使う `"k-" + "stub"` のような文字列は**テストの中で組み立てた偽物**で
    あり、本物は環境変数か macOS のキーチェーンにしかない。`security` を本当に
    走らせるテストは 1 つも無い（走らせると本物が返ってきて、失敗時の差分や
    テストログに載りうる）。
    """

    STUB_KEY = "k-" + "stub-not-a-real-key"

    def setUp(self):
        # [`jev.api_key`] は取り出した鍵を覚えるので、テスト間で持ち越さない。
        jev._api_key = None
        self.addCleanup(setattr, jev, "_api_key", None)

    def env(self, **values):
        """`os.environ` を差し替える。既定では `TYPESAFE_API_KEY` を外す。

        `patch.dict` は `start()` の時点の中身を丸ごと覚えて `stop()` で戻すので、
        **`start()` の後に**消すぶんには元に戻る（前に消すと戻らない）。
        """
        patch = unittest.mock.patch.dict(os.environ, values, clear=False)
        patch.start()
        self.addCleanup(patch.stop)
        if "TYPESAFE_API_KEY" not in values:
            os.environ.pop("TYPESAFE_API_KEY", None)

    def test_the_environment_variable_wins(self):
        self.env(TYPESAFE_API_KEY=self.STUB_KEY)
        with unittest.mock.patch.object(jev.subprocess, "run") as run:
            self.assertEqual(jev.api_key(), self.STUB_KEY)
        run.assert_not_called()

    def test_the_keychain_answers_when_the_variable_is_unset(self):
        self.env()
        completed = subprocess.CompletedProcess([], 0, self.STUB_KEY + "\n", "")
        with unittest.mock.patch.object(jev.sys, "platform", "darwin"):
            with unittest.mock.patch.object(
                jev.subprocess, "run", return_value=completed
            ) as run:
                self.assertEqual(jev.api_key(), self.STUB_KEY)
        argv = run.call_args.args[0]
        self.assertEqual(argv[0], "security")
        self.assertIn(jev.KEYCHAIN_SERVICE, argv)
        self.assertNotIn("shell", run.call_args.kwargs)
        self.assertEqual(run.call_args.kwargs["timeout"], 5)

    def test_the_keychain_is_read_once_per_process(self):
        """[`jev.ask_jev`] が 1 リクエストごとに呼ぶので、覚えないと叩き続ける。"""
        self.env()
        completed = subprocess.CompletedProcess([], 0, self.STUB_KEY, "")
        with unittest.mock.patch.object(jev.sys, "platform", "darwin"):
            with unittest.mock.patch.object(
                jev.subprocess, "run", return_value=completed
            ) as run:
                jev.api_key()
                jev.api_key()
        self.assertEqual(run.call_count, 1)

    def test_a_non_zero_security_falls_through_to_the_error(self):
        self.env()
        completed = subprocess.CompletedProcess([], 44, "", "The specified item…")
        with unittest.mock.patch.object(jev.sys, "platform", "darwin"):
            with unittest.mock.patch.object(
                jev.subprocess, "run", return_value=completed
            ):
                with self.assertRaises(jev.JevError) as caught:
                    jev.api_key()
        self.assertIn("TYPESAFE_API_KEY", str(caught.exception))
        self.assertIn("add-generic-password", str(caught.exception))

    def test_a_missing_security_binary_is_just_absence(self):
        self.env()
        with unittest.mock.patch.object(jev.sys, "platform", "darwin"):
            with unittest.mock.patch.object(
                jev.subprocess, "run", side_effect=FileNotFoundError
            ):
                with self.assertRaises(jev.JevError):
                    jev.api_key()

    def test_a_locked_keychain_does_not_hang_the_analysis(self):
        """ダイアログ待ちは `timeout` で切れて「無い」になる。"""
        self.env()
        expired = subprocess.TimeoutExpired(["security"], 5)
        with unittest.mock.patch.object(jev.sys, "platform", "darwin"):
            with unittest.mock.patch.object(
                jev.subprocess, "run", side_effect=expired
            ):
                with self.assertRaises(jev.JevError):
                    jev.api_key()

    def test_other_platforms_never_run_security(self):
        self.env()
        with unittest.mock.patch.object(jev.sys, "platform", "linux"):
            with unittest.mock.patch.object(jev.subprocess, "run") as run:
                with self.assertRaises(jev.JevError):
                    jev.api_key()
        run.assert_not_called()


class MissingKeyTest(unittest.TestCase):
    def test_a_missing_key_exits_non_zero_with_one_actionable_line(self):
        request = {
            "version": 1,
            "source": "本文。",
            "atoms": [atom(0, "sentence", "本文。")],
            "question": {"id": "essential", "text": "問い。", "core_floor": 0.2},
        }
        # **キーチェーンに届かせない。** ここは別プロセスなので mock が効かず、
        # mac で走らせると本物の鍵が見つかって成功してしまう。`security` の
        # 無い PATH を渡して「鍵がどこにも無い」状況そのものを作る（`PATH=""`
        # だと cwd 相対で探しにいくので、空のディレクトリを 1 つ置く）。
        with tempfile.TemporaryDirectory() as empty:
            out = DryRunTest.run_script([], request, env={"PATH": empty})
        self.assertNotEqual(out.returncode, 0)
        self.assertEqual(out.stdout, "")
        lines = [line for line in out.stderr.splitlines() if line.strip()]
        # akapen はステータス行に stderr の最後の非空行を 160 字まで出す。
        # **保存のしかたを別の行にできない**のはこれが理由で、`main` の
        # `one_line` も改行を潰す。
        self.assertEqual(len(lines), 1, out.stderr)
        self.assertLessEqual(len(lines[0]), 160)
        self.assertIn("TYPESAFE_API_KEY", lines[0])
        self.assertIn("add-generic-password", lines[0])

    def test_a_broken_request_never_reaches_the_network(self):
        out = DryRunTest.run_script([], {"version": 99, "source": "", "atoms": []})
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("protocol version", out.stderr)

    def test_akapen_does_not_get_a_second_prefix(self):
        """akapen 経由（stderr はパイプ）では `jev-annotate:` と名乗らない。

        受けた側が `(--semantic-cmd exited non-zero)` を添えるので、ここでも
        名乗ると接頭辞が 2 段になり、ステータス行の幅を肝心の一文から奪う。
        """
        out = DryRunTest.run_script([], {"version": 99, "source": "", "atoms": []})
        self.assertNotIn("jev-annotate:", out.stderr)
        self.assertTrue(out.stderr.startswith("unsupported"), out.stderr)


class ProgressTest(unittest.TestCase):
    """1 リクエストごとの生存信号（[`jev.progress`]）。

    **akapen はこの行に頼って子を殺さない判断をしている** —— 壁時計ではなく
    最後の出力からの無音時間で見ていて（`src/export.rs` の
    `Deadline::WhileProgressing`、上限は `src/semantic.rs` の
    `COMMAND_IDLE_TIMEOUT` = 30 秒）、黙って働くアダプタは固まったアダプタと
    区別がつかない。だから「出ること」をテストで留める。
    """

    class Capturing:
        """`isatty` を持つ、書いた行を溜めるだけの stderr。"""

        def __init__(self, tty=False, boom=None):
            self.lines = []
            self._tty = tty
            self._boom = boom

        def isatty(self):
            return self._tty

        def write(self, text):
            if self._boom:
                raise self._boom
            self.lines.append(text)

        def flush(self):
            if self._boom:
                raise self._boom

    def setUp(self):
        self.real_stderr = jev.sys.stderr
        self.real_count = jev._requests_sent
        jev._requests_sent = 0

    def tearDown(self):
        jev.sys.stderr = self.real_stderr
        jev._requests_sent = self.real_count

    def test_one_line_per_request_numbered_in_order(self):
        fake = self.Capturing()
        jev.sys.stderr = fake
        jev.progress(9, 0.712)
        jev.progress(42, 1.2)
        written = "".join(fake.lines).splitlines()
        self.assertEqual(len(written), 2, written)
        self.assertEqual(written[0], "request 1: 9 questions in 0.71s")
        self.assertEqual(written[1], "request 2: 42 questions in 1.20s")

    def test_the_line_fits_the_status_bar(self):
        # akapen は stderr の最後の非空行を 160 字まで出す。進捗行が
        # そこに載ることがあるので、溢れさせない。
        fake = self.Capturing()
        jev.sys.stderr = fake
        jev.progress(99999, 123.456)
        self.assertLessEqual(len("".join(fake.lines).strip()), 160)

    def test_a_closed_stderr_does_not_break_the_analysis(self):
        # 進捗行が出せなくても解析は続ける（akapen の猶予は縮むだけ）。
        jev.sys.stderr = self.Capturing(boom=ValueError("closed"))
        jev.progress(1, 0.1)  # 例外を投げないこと
        jev.sys.stderr = self.Capturing(boom=OSError("broken pipe"))
        jev.progress(1, 0.1)

    def test_the_request_timeout_stays_under_akapens_idle_limit(self):
        """**順序の約束を Python 側からも留める。**

        akapen は無音 30 秒で子を殺す（`src/semantic.rs` の
        `COMMAND_IDLE_TIMEOUT`）。`DEFAULT_TIMEOUT` がそれを越えると、
        固まったリクエストを子が自分で諦めて理由を stderr に書く前に
        akapen の kill が来て、**理由が消える**。

        Rust 側は `the_default_deadline_watches_silence_not_the_wall_clock`
        が `COMMAND_IDLE_TIMEOUT > 20 秒` を留めている。片側だけだと
        こちらを 40 秒に上げても両方のテストが緑のまま約束が壊れるので、
        反対側からも留める（`docs/gotchas/external-processes.md`
        「どちらかを動かすなら両方を見ること」）。
        """
        self.assertLess(jev.DEFAULT_TIMEOUT, 30.0)

    def test_ask_jev_reports_every_request(self):
        """**`ask_jev` が出す** —— `send_in_chunks` ではない。

        probe は分割を通らない 1 本なので、分割側に置くと漏れる。
        """
        fake = self.Capturing()
        jev.sys.stderr = fake

        class FakeResponse:
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

            def read(self):
                return json.dumps({"answers": {"q": {"choice": "a"}}}).encode()

        real_urlopen = jev.urllib.request.urlopen
        real_key = os.environ.get("TYPESAFE_API_KEY")
        jev.urllib.request.urlopen = lambda *a, **k: FakeResponse()
        os.environ["TYPESAFE_API_KEY"] = "stub"
        try:
            for _ in range(3):
                jev.ask_jev("state", {"q": {"type": "choice", "criteria": {}}}, "m", 5.0)
        finally:
            jev.urllib.request.urlopen = real_urlopen
            if real_key is None:
                os.environ.pop("TYPESAFE_API_KEY", None)
            else:
                os.environ["TYPESAFE_API_KEY"] = real_key

        written = "".join(fake.lines).splitlines()
        self.assertEqual(len(written), 3, written)
        self.assertTrue(written[-1].startswith("request 3: 1 questions in "), written)


class StderrPrefixTest(unittest.TestCase):
    """人が直接走らせたときだけ名乗る（[`stderr_prefix`]）。"""

    class FakeStderr:
        def __init__(self, tty):
            self._tty = tty

        def isatty(self):
            if self._tty is None:
                raise ValueError("I/O operation on closed file")
            return self._tty

    def with_stderr(self, fake):
        real = jev.sys.stderr
        jev.sys.stderr = fake
        try:
            return jev.stderr_prefix()
        finally:
            jev.sys.stderr = real

    def test_a_terminal_gets_the_name(self):
        self.assertEqual(self.with_stderr(self.FakeStderr(True)), "jev-annotate: ")

    def test_a_pipe_does_not(self):
        self.assertEqual(self.with_stderr(self.FakeStderr(False)), "")

    def test_a_closed_stream_falls_back_to_no_name(self):
        # 迷ったら名乗らない側へ倒す — akapen 経由のほうが多い。
        self.assertEqual(self.with_stderr(self.FakeStderr(None)), "")


class HttpErrorMessageTest(unittest.TestCase):
    """HTTP エラーがステータス行で意味を持つか（**API は叩かない**）。

    `max_tokens_exceeded` は実測でこの経路のいちばん現実的な失敗で
    （45.6KB の実文書が context window の 92〜96 % を使う）、生の JSON が
    出ると「タイムアウトした」と読み違えられる。
    """

    MAX_TOKENS = '{"detail":{"error_type":"max_tokens_exceeded"}}'

    def test_max_tokens_says_it_is_size_not_time(self):
        line = jev.http_error_message(400, self.MAX_TOKENS)
        self.assertIn("too large", line)
        self.assertIn("Not a timeout", line)
        # 用を成す一文が**先頭に**来ていること（枠の幅は端末しだいなので、
        # 後ろに置くと切られる）。
        self.assertLess(line.index("too large"), 50)
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
            "criteria": {"same_unit": "あ", "new_unit": "い"},
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

        実文書で確かめる。`demo.md` と設計書の**凍結コピー**
        （[`FROZEN_DESIGN_DOC`]）は 3 ラウンドとも 1 チャンクでなければ
        ならない（見積もりは実測の 1.4 倍まで過大評価するので、ここが本番より
        厳しい側の判定になる）。
        """
        for name in ("demo.md", FROZEN_DESIGN_DOC):
            with self.subTest(document=name):
                source = (HERE / name).read_text(encoding="utf-8")
                request = dump_request(HERE / name)
                plan = jev.dry_run(request, "jev-latest")
                budget = jev.RequestBudget(plan["budget"]["state_tokens"])
                for entry in plan["rounds"]:
                    chunks, _dropped = jev.plan_chunks(entry["questions"], budget)
                    # **0 も正しい。** 核のラウンドは、どの Unit も散文の
                    # 候補が 1 つしか無ければ question を 1 本も組まない
                    # （[`assign_lone_cores`] が聞かずに埋める）。
                    self.assertLessEqual(len(chunks), 1, f"round {entry['round']}")
                # 境界とスコアは必ず聞く（テストが空振りしないこと）。
                self.assertTrue(plan["rounds"][0]["questions"])
                self.assertTrue(plan["rounds"][1]["questions"])
                self.assertEqual(
                    plan["budget"]["state_tokens"], jev.estimate_tokens(source)
                )

    def test_no_round_hits_the_ceiling_that_splitting_cannot_move(self):
        """**分割で外せない 32k 枠**に、どのラウンドも当たらない。

        `whole`（64k）はチャンクを増やせば外せるが、`state` + question 1 つの
        32k はどう分けても小さくならない。そこに当たった question は
        `unsent` に出て、誰にも送れない。
        """
        for name in ("demo.md", FROZEN_DESIGN_DOC):
            with self.subTest(document=name):
                request = dump_request(HERE / name)
                plan = jev.dry_run(request, "jev-latest")
                for entry in plan["rounds"]:
                    self.assertEqual(
                        entry["plan"]["unsent"], [], f"round {entry['round']}"
                    )

    # --- 見積もりの精度 ---------------------------------------------------

    def test_the_estimate_is_conservative_but_not_wildly_so(self):
        """実測 181 tokens だった器の見積もりを、1.0〜1.6 倍で返す。

        **下回ってはいけない** — 見積もりが小さいと上限を超えた question を
        送って 400 で落ちる。**大きすぎてもいけない** — 収まる文書を無駄に
        分割し、`state` を余分に課金する。実測の内訳
        （`docs/gotchas/open-questions.md` 未解決 5）は
        器 68 / criteria 65 / 枠組み文 65 = 181。

        **かつての Tier question をそのまま組んで測る。** Tier は消えたが、
        測ったのは「この大きさの Choice」であって Tier ではないので、
        実測の数字と突き合わせるには同じ形が要る。
        """
        tier = {
            "type": "choice",
            "instructions": (
                "この文書の中で、次の部分はどの読む優先度に当たりますか。\n\n"
                "――― 対象 ―――\n\n―――――――――"
            ),
            "criteria": {
                "essential": "落とすと文書の要点、結論、制約、未決の論点や宿題などを取り違える可能性が高い。",
                "supporting": "ESSENTIAL な内容の理解・納得に役立つ。",
                "context": "背景や前提、理解補助。",
                "detail": "例、細部、追加説明。",
            },
        }
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
            jev.choice_of(answers, "tier:u1", {"same_unit": "あ", "new_unit": "い"})

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


class MarksModeTest(unittest.TestCase):
    """解析の本体 — 問いに答えている箇所だけを光らせる。

    `docs/design/semantic-reading-layer.md`。**唯一の経路である**
    （2026-09-22 に DIM 版を削除した）。
    """

    SOURCE = "# 見出し\n\n決まったことを述べた文。まだ決まっていない文。\n\n次の段落である。\n"

    def request(self, question=True):
        atoms = [
            {"index": 0, "kind": "heading", "text": "# 見出し", "range": [0, 10]},
            {"index": 1, "kind": "sentence", "text": "決まったことを述べた文。", "range": [12, 48]},
            {"index": 2, "kind": "sentence", "text": "まだ決まっていない文。", "range": [48, 81]},
            {"index": 3, "kind": "sentence", "text": "次の段落である。", "range": [83, 107]},
        ]
        out = {"version": jev.VERSION, "source": self.SOURCE, "atoms": atoms}
        if question:
            out["question"] = {
                "id": "settled",
                "text": "下の「対象」は、決定・合意・確定した事柄を述べている箇所である。",
                "core_floor": 0.20,
            }
        return out

    def fake(self, scores):
        """境界は NEW、marks は `scores` の順、核は最初の選択肢。"""
        def ask(state, chunk, model, timeout):
            answers = {}
            for key in chunk:
                if key.startswith("boundary:"):
                    answers[key] = {"choice": jev.NEW, "confidence": 0.9}
                elif key.startswith("marks:"):
                    number = int(key.split("u")[1])
                    answers[key] = {"noul": scores[number - 1]}
                elif key.startswith("core:"):
                    criteria = chunk[key]["criteria"]
                    answers[key] = {"choice": sorted(criteria)[0], "confidence": 0.8}
                else:
                    answers[key] = {"noul": 0.5, "choice": "detail"}
            return {"answers": answers}
        return ask

    def setUp(self):
        # **実ユーザーの ~/.cache を書かない。** 境界のキャッシュはホームの
        # 下に置くので、テストは自分の tmpdir を指す。
        self._tmp = tempfile.TemporaryDirectory()
        self._saved = os.environ.get("AKAPEN_CACHE_DIR")
        os.environ["AKAPEN_CACHE_DIR"] = self._tmp.name

    def tearDown(self):
        if self._saved is None:
            os.environ.pop("AKAPEN_CACHE_DIR", None)
        else:
            os.environ["AKAPEN_CACHE_DIR"] = self._saved
        self._tmp.cleanup()

    def test_a_request_without_a_question_is_refused(self):
        """**問いは必須である。** 黙って別のものを返すと、akapen 側では
        「0 本」と区別が付かない — そこは「答えている箇所が無い」という
        意味を持つ場所なので、混ぜてはならない。"""
        with self.assertRaises(jev.JevError) as caught:
            with_fake_ask(
                self.fake([0.9, 0.9, 0.9, 0.9]),
                lambda: jev.annotate(self.request(question=False), "m", 1.0),
            )
        self.assertIn("no question", str(caught.exception))

    def test_the_answer_carries_the_question_and_a_score_per_unit(self):
        out = with_fake_ask(
            self.fake([0.10, 0.95, 0.30, 0.05]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        self.assertEqual(out["question"], "settled")
        # 見出しは後続に付くので Unit は Atom より少ない。**Unit ごとに
        # 1 つ、順番どおり**であることを見る。
        scores = [u.get("score") for u in out["units"]]
        self.assertEqual(scores, [0.10, 0.95, 0.30][: len(scores)])
        self.assertTrue(all(s is not None for s in scores))

    def test_the_answer_carries_no_reading_tier_and_no_relations(self):
        """DIM 版のフィールドはワイヤから消えた（2026-09-22）。"""
        out = with_fake_ask(
            self.fake([0.9, 0.9, 0.9, 0.9]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        for unit in out["units"]:
            self.assertNotIn("reading_tier", unit)
            self.assertNotIn("relations", unit)

    def test_the_section_head_is_recorded_without_asking_jev(self):
        """`section_of` は構文だけで決まるので、ラウンドも費用も増えない。"""
        out = with_fake_ask(
            self.fake([0.9, 0.9, 0.9, 0.9]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        # 見出しは直後の内容に付くので u1 が節の頭、以降がその中身。
        self.assertEqual(out["units"][0].get("section_of"), None)
        self.assertEqual(
            [u.get("section_of") for u in out["units"][1:]],
            ["u1"] * (len(out["units"]) - 1),
        )

    def test_the_core_round_only_asks_about_units_over_the_floor(self):
        asked = []

        def watching(state, chunk, model, timeout):
            asked.extend(chunk)
            return self.fake([0.05, 0.95, 0.06, 0.04])(state, chunk, model, timeout)

        with_fake_ask(watching, lambda: jev.annotate(self.request(), "m", 1.0))
        cores = [k for k in asked if k.startswith("core:")]
        # u2 だけが足切りを越えている。そこは 1 Atom なので Choice は
        # 要らず（[`assign_lone_cores`]）、核の question は 0 本になる。
        self.assertEqual(cores, [], f"聞いたのは {cores}")

    def test_a_question_nothing_answers_asks_no_core_round_at_all(self):
        """狭い問いでは核のラウンドごと消える（費用 0）。"""
        rounds = []

        def watching(state, chunk, model, timeout):
            rounds.append(sorted(k.split(":")[0] for k in chunk)[0])
            return self.fake([0.05, 0.07, 0.02, 0.01])(state, chunk, model, timeout)

        out = with_fake_ask(watching, lambda: jev.annotate(self.request(), "m", 1.0))
        self.assertNotIn("core", rounds, f"送ったラウンド: {rounds}")
        self.assertTrue(all(u["core_atoms"] == [] for u in out["units"]))

    #: `- 決定A。` / `- 決定B。` / `- 決定C。` の 3 項目 ＝ 規則 4 でつながる
    #: 1 本のリスト。
    LIST_SOURCE = "- 決定A。\n- 決定B。\n- 決定C。\n"

    def list_request(self):
        atoms = atoms_from(
            self.LIST_SOURCE,
            ("list_item", "- 決定A。"),
            ("list_item", "- 決定B。"),
            ("list_item", "- 決定C。"),
        )
        return dict(self.request(), source=self.LIST_SOURCE, atoms=atoms)

    def test_every_item_of_one_list_can_hold_a_core(self):
        """**run キャップは掛けない。** 問いが既に選んでいる。

        足切りを超えた項目はそれぞれ核を持つ — リストの項目が全部光るのは
        「答えが全部光る」で正しく、量はつまみが受け持つ。
        """
        out = with_fake_ask(
            self.fake([0.95, 0.94, 0.93]),
            lambda: jev.annotate(self.list_request(), "m", 1.0),
        )
        # **テストが空振りしないこと。** 3 項目が 3 つの Unit に割れていな
        # ければ、下の assert は何も守らない。
        self.assertEqual([u["atoms"] for u in out["units"]], [[0], [1], [2]])
        self.assertEqual([u["core_atoms"] for u in out["units"]], [[0], [1], [2]])
        capped = [u["id"] for u in out["units"] if u["jev"].get("core_by") == "rule:run_cap"]
        self.assertEqual(capped, [], f"run キャップに掛かった: {capped}")

    def test_the_boundary_round_runs_once_and_then_comes_from_the_cache(self):
        """**境界を 1 回取る（キャッシュ）** — 問いを変えても回し直さない。"""
        rounds = []

        def watching(state, chunk, model, timeout):
            rounds.append([k for k in chunk if k.startswith("boundary:")])
            return self.fake([0.9, 0.9, 0.9, 0.9])(state, chunk, model, timeout)

        first = with_fake_ask(watching, lambda: jev.annotate(self.request(), "m", 1.0))
        asked_first = sum(len(r) for r in rounds)
        rounds.clear()
        second = self.request()
        second["question"] = dict(second["question"], id="unsettled", text="別の問い。")
        out = with_fake_ask(watching, lambda: jev.annotate(second, "m", 1.0))
        self.assertGreater(asked_first, 0, "1 回目は境界を聞く")
        self.assertEqual(sum(len(r) for r in rounds), 0, "2 回目は聞かない")
        self.assertFalse(first["jev"]["boundaries_cached"])
        self.assertTrue(out["jev"]["boundaries_cached"])
        # 同じ境界であること（Unit の切り方が変わっていない）。
        self.assertEqual(
            [u["atoms"] for u in first["units"]],
            [u["atoms"] for u in out["units"]],
        )

    def test_the_boundary_cache_holds_no_prose(self):
        """キャッシュに本文は 1 バイトも入らない（`public-repo.md` の扱い）。"""
        with_fake_ask(
            self.fake([0.9, 0.9, 0.9, 0.9]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        path = jev.boundary_cache_path(self.SOURCE)
        raw = path.read_text(encoding="utf-8")
        for fragment in ("決まった", "見出し", "段落"):
            self.assertNotIn(fragment, raw, raw)
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_a_different_atom_split_misses_the_boundary_cache(self):
        """**`atomize` が変わったら境界は取り直す。**

        境界のキャッシュは Atom の**添字**で持っている。同じ文書でも割り方が
        変われば添字はずれるので、当ててはいけない — 当たれば節の切れ目が
        ずれた、それらしく見えて間違った注釈になる。件数の一致だけでは足り
        ないので、`(kind, range)` の指紋を照合する。
        """
        with_fake_ask(
            self.fake([0.9, 0.9, 0.9, 0.9]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        plan = [{"decision": None, "by": ""} for _ in range(3)]
        atoms = self.request()["atoms"]
        self.assertTrue(jev.load_boundaries(self.SOURCE, plan, atoms))

        # 件数はそのまま、種別だけが変わった割り方（`atomize` の変更）。
        moved = [dict(a) for a in atoms]
        moved[1] = dict(moved[1], kind="table_row")
        plan = [{"decision": None, "by": ""} for _ in range(3)]
        self.assertFalse(
            jev.load_boundaries(self.SOURCE, plan, moved),
            "割り方が変わったのに古い境界が当たりました",
        )
        self.assertTrue(all(entry["decision"] is None for entry in plan))

    def test_a_boundary_cache_entry_without_a_fingerprint_misses(self):
        """指紋を持たない古い項目（2026-09-22 より前）は当たらない。"""
        path = jev.boundary_cache_path(self.SOURCE)
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        path.write_text(
            json.dumps({"version": 1, "decisions": [jev.NEW, jev.NEW, jev.NEW]}),
            encoding="utf-8",
        )
        plan = [{"decision": None, "by": ""} for _ in range(3)]
        self.assertFalse(
            jev.load_boundaries(self.SOURCE, plan, self.request()["atoms"])
        )

    def test_a_question_without_text_is_refused(self):
        request = self.request()
        request["question"] = {"id": "settled", "core_floor": 0.2}
        with self.assertRaises(jev.JevError):
            with_fake_ask(
                self.fake([0.9, 0.9, 0.9, 0.9]),
                lambda: jev.annotate(request, "m", 1.0),
            )

    def test_the_question_text_reaches_jev_verbatim(self):
        claims = []

        def watching(state, chunk, model, timeout):
            claims.extend(q.get("instructions", "") for k, q in chunk.items() if k.startswith("marks:"))
            return self.fake([0.9, 0.9, 0.9, 0.9])(state, chunk, model, timeout)

        with_fake_ask(watching, lambda: jev.annotate(self.request(), "m", 1.0))
        text = self.request()["question"]["text"]
        self.assertTrue(claims)
        for claim in claims:
            self.assertTrue(claim.startswith(text), claim[:60])
            self.assertIn("――― 対象 ―――", claim)


def with_fake_ask(fake, body):
    """[`ask_jev`] を差し替えて `body()` を呼ぶ（API は叩かない）。"""
    real = jev.ask_jev
    jev.ask_jev = fake
    try:
        return body()
    finally:
        jev.ask_jev = real


def dump_request(path):
    """`dump-request` と同じ形の要求 JSON を、本物の `atomize` から作る。

    **問いを足す。** `dump-request` は境界だけの用途なので問いを載せないが、
    akapen が実際に送るのは必ず問いつきである。文面は `assets/marks-questions.json`
    の `essential` の逐語 — question の大きさをそのまま決めるので、短い
    ダミーを使うと分割の判定が本番より甘くなる。
    """
    out = subprocess.run(
        ["cargo", "run", "--quiet", "-p", "semantic-reading",
         "--example", "dump-request", "--", str(path)],
        capture_output=True, text=True, cwd=str(HERE.parents[1]), check=True,
    )
    request = json.loads(out.stdout)
    presets = json.loads((HERE.parents[1] / "assets/marks-questions.json").read_text())
    essential = next(q for q in presets["presets"] if q["id"] == "essential")
    request["question"] = {
        "id": essential["id"],
        "text": essential["text"],
        "core_floor": 0.20,
    }
    return request


if __name__ == "__main__":
    unittest.main()
