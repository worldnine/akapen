"""`jev-annotate.py` のテスト — **API を叩かない**。

    python3 -m unittest discover -s examples/semantic -p 'test_*.py'

外部ネットワークに依存するテストは CI で落ちるので、ここで確かめるのは

- Unit の作り方（散文の Atom 1 つが Unit 1 つ。見出し・コード・表のヘッダ行は
  Unit を持たない）と、各 Unit の `core_atoms` がその Atom であること
- ラウンドがスコアの 1 本だけで、問いの数が散文 Atom の数に等しいこと。
  境界のキャッシュを読まない・書かないこと
- `--dry-run` が送ろうとするリクエストの形（state / model / questions）
- リクエストの分割と予算（64k と 32k の 2 つの制約）
- 鍵の取り出し（環境変数 → macOS のキーチェーン → 案内つきで停止）

だけである。判定の質そのものは実測（`docs/` と README の表）で見る。

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
import threading
import time
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


def atom(index, kind, text):
    start = index * 100
    return {
        "index": index,
        "kind": kind,
        "range": {"start": start, "end": start + len(text.encode())},
        "text": text,
    }


def atoms_from(source, *items):
    """実際の Markdown から、**バイト位置の正しい** Atom 列を作る。

    `items` は `(種別, 本文)` の並びで、本文は `source` の中を先頭から順に探す。
    上の `atom()` は `range` を `index * 100` で捏造するので、位置が効く
    テスト（リストの項目を並べる解析など）にはこちらを使う。

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


class ProseUnitsTest(unittest.TestCase):
    """Unit = 散文の Atom 1 つ。**境界は聞かない**（2026-09-24）。

    見出し・コードブロック・表のヘッダ行には Unit を作らない。境界も核も
    Jev に決めさせない理由は `jev-annotate.py` 冒頭の「なぜ文ごとか」。
    """

    ATOMS = [
        atom(0, "heading", "## 結論"),
        atom(1, "sentence", "採用する方式は差分配信である。"),
        atom(2, "sentence", "帯域は 3 割減る見込み。"),
        atom(3, "list_item", "- 移行は 2 段で行う。"),
        atom(4, "list_item", "  - 1 段目は読み取りだけ。"),
        atom(5, "block_quote", "> 旧方式は残さない。"),
        atom(6, "code_block", "```\nkey: value\n```"),
        atom(7, "table", "| 項目 | 値 |\n| --- | --- |"),
        atom(8, "table_row", "| 応答 | 1.2 秒 |"),
        atom(9, "table_row", "| 費用 | 5 円 |"),
    ]

    def test_every_prose_atom_is_its_own_unit(self):
        # 文・リスト項目（子項目も）・引用・表のデータ行。**束ねない** —
        # 同じ段落の 2 文も、親項目と子項目も、表の 2 行も別々の Unit になる。
        self.assertEqual(jev.prose_units(self.ATOMS), [1, 2, 3, 4, 5, 8, 9])

    def test_headings_code_and_table_headers_get_no_unit(self):
        units = set(jev.prose_units(self.ATOMS))
        for atom_ in self.ATOMS:
            if atom_["kind"] in ("heading", "code_block", "table"):
                self.assertNotIn(atom_["index"], units, atom_["kind"])
        self.assertEqual(
            jev.PROSE_KINDS, {"sentence", "list_item", "block_quote", "table_row"}
        )

    def test_an_empty_atom_gets_no_unit(self):
        # Jev に見せても判断材料が無く、光らせる中身も無い。
        atoms = [atom(0, "sentence", "本文がある。"), atom(1, "sentence", "   ")]
        self.assertEqual(jev.prose_units(atoms), [0])

    def test_the_boundary_and_core_machinery_is_gone(self):
        # 戻すと何が戻ってくるかは冒頭の docstring に書いてある。名前が
        # 残っていると「まだ使っている」と読まれるので、無いことを留める。
        for name in (
            "BOUNDARY_CRITERIA", "boundary_rule", "plan_boundaries",
            "boundary_questions", "apply_boundary_answers", "group_units",
            "load_boundaries", "save_boundaries", "atoms_fingerprint",
            "boundary_cache_path", "boundary_turn", "BOUNDARY_WAIT_LIMIT",
            "CORE_INSTRUCTIONS", "CORE_FRAME", "core_instructions",
            "core_candidates", "core_fits", "core_questions",
            "assign_lone_cores", "apply_core_answers", "wants_core",
        ):
            self.assertFalse(hasattr(jev, name), name)


class ReferenceImplementationTest(unittest.TestCase):
    """隣の決定論的な参照実装 `annotate-doc.py` が本番と同じ約束に乗ること。

    約束は `jev-annotate.py` と同じ — **散文の Atom 1 つが Unit 1 つ**で、
    見出し・コード・表のヘッダ行には Unit を作らない。各 Unit の `core_atoms`
    はその Atom を明示する。`core_atoms` の空の配列は「**核を持たない** =
    MARKED にならない」という別の意味を持つ
    （`crates/semantic-reading/src/protocol.rs`）ので、**`[]` と「省略」と
    `[その Atom]` を取り違えない**ことを固定する。
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

    def test_prose_gets_its_own_atom_as_its_core_and_a_heading_gets_no_unit(self):
        answer = json.loads(self.run_reference(self.request()).stdout)
        units = answer["units"]
        # Atom 0 は見出し。Unit を作らない（本番の判定器と同じ）。
        self.assertEqual([u["atoms"] for u in units], [[1], [2], [3], [4]])
        # 散文は自分自身が核。**省略しない** — 省いても 1 Atom の Unit なら
        # 光り方は同じだが、3 値のどれを言っているかを字面に出す。
        for unit in units:
            self.assertEqual(unit["core_atoms"], unit["atoms"])

    def test_a_request_without_a_question_is_refused(self):
        # DIM 版は 2026-09-22 に削除された。問いを持たない要求は、黙って
        # 別のものを返すのではなく**明確なエラーで終わる**。
        proc = self.run_reference(self.request(with_question=False))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("no question", proc.stderr)
        self.assertEqual(proc.stdout, "")


class BudgetTest(unittest.TestCase):
    """question の上限は state の大きさから決まる。

    制約は「`state` + 最長 question ≦ 32k」なので、固定の数では正しく
    ならない（`docs/gotchas.md`）。
    """

    def test_the_budget_shrinks_as_the_state_grows(self):
        small = jev.RequestBudget.estimated("あ" * 100)
        large = jev.RequestBudget.estimated("あ" * 100_000)
        self.assertGreater(small.pair, large.pair)
        self.assertGreater(small.whole, large.whole)

    def test_a_huge_state_leaves_no_budget_at_all(self):
        # state だけで 32k を使い切る文書では、どの question も送れない。
        self.assertLessEqual(jev.RequestBudget.estimated("あ" * 40_000).pair, 0)


class DryRunTest(unittest.TestCase):
    """送るリクエストの形。実際の demo.md / demo-marks.json を材料にする。"""

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

    def test_there_is_one_round_the_score_round(self):
        rounds = self.payload()["rounds"]
        self.assertEqual([r["round"] for r in rounds], ["marks"])

    def test_the_state_is_the_whole_document(self):
        for round_ in self.payload()["rounds"]:
            self.assertEqual(round_["state"], self.source)

    def test_the_model_is_sent_on_every_round(self):
        for round_ in self.payload()["rounds"]:
            self.assertEqual(round_["model"], jev.DEFAULT_MODEL)

    def test_one_noul_per_prose_atom(self):
        atoms = self.request["atoms"]
        asked = self.payload()["rounds"][0]["questions"]
        prose = [a for a in atoms if a["kind"] in jev.PROSE_KINDS and a["text"].strip()]
        # **テストが空振りしないこと。** demo.md は見出しと散文の両方を持つ。
        self.assertTrue(prose)
        self.assertLess(len(prose), len(atoms), "散文でない Atom がある文書")
        self.assertEqual(len(asked), len(prose), "問いの数 = 散文 Atom の数")
        for (key, question), atom_ in zip(asked.items(), prose):
            self.assertTrue(key.startswith("marks:u"), key)
            self.assertEqual(question["type"], "noul")
            # 問いの文面は akapen が送ってきたものそのまま。枠と、その Atom の
            # 本文だけを足す。
            self.assertTrue(question["instructions"].startswith("テスト用の問い。"))
            self.assertTrue(
                question["instructions"].endswith(
                    "――― 対象 ―――\n" + atom_["text"].strip() + "\n―――――――――"
                ),
                key,
            )

    def test_headings_and_code_are_never_asked(self):
        asked = "\n".join(
            q["instructions"] for q in self.payload()["rounds"][0]["questions"].values()
        )
        for atom_ in self.request["atoms"]:
            if atom_["kind"] in ("heading", "code_block", "table"):
                self.assertNotIn(
                    "――― 対象 ―――\n" + atom_["text"].strip() + "\n", asked, atom_["kind"]
                )

    def test_the_dry_run_names_what_can_differ_from_production(self):
        # 答えについての仮定はもう無い。state のトークン数の出どころだけが違いうる。
        assumptions = " ".join(self.payload()["assumptions"])
        self.assertIn("estimate", assumptions)
        self.assertNotIn("new_unit", assumptions)
        self.assertNotIn("core floor", assumptions)

    def test_a_dry_run_without_a_question_is_refused(self):
        request = dict(self.request)
        request.pop("question")
        proc = self.run_script(["--dry-run"], request)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("question", proc.stderr)


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

    def test_ask_jev_names_itself_instead_of_the_urllib_default(self):
        """urllib の既定の User-Agent は Cloudflare に 403（error 1010）で弾かれる。"""
        sent = []

        class FakeResponse:
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

            def read(self):
                return json.dumps({"answers": {"q": {"choice": "a"}}}).encode()

        def fake_urlopen(request, *a, **k):
            sent.append(request)
            return FakeResponse()

        real_urlopen = jev.urllib.request.urlopen
        real_key = os.environ.get("TYPESAFE_API_KEY")
        real_stderr = jev.sys.stderr
        jev.urllib.request.urlopen = fake_urlopen
        jev.sys.stderr = self.Capturing()
        os.environ["TYPESAFE_API_KEY"] = "stub"
        try:
            jev.ask_jev("state", {"q": {"type": "choice", "criteria": {}}}, "m", 5.0)
        finally:
            jev.urllib.request.urlopen = real_urlopen
            jev.sys.stderr = real_stderr
            if real_key is None:
                os.environ.pop("TYPESAFE_API_KEY", None)
            else:
                os.environ["TYPESAFE_API_KEY"] = real_key

        agent = sent[0].get_header("User-agent")
        self.assertEqual(agent, jev.USER_AGENT)
        self.assertNotIn("Python-urllib", agent)

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

    **スコアのラウンド専用のテストにしないこと。** 分割はラウンドの事情を
    知らない 1 つの実装なので、ここで確かめるのも question の中身に依存しない
    性質だけである（トークンで切る / `pair` を超えたら送らない / 取りこぼさない）。
    """

    def question(self, body_bytes: int) -> dict:
        return {
            "type": "choice",
            "instructions": "問い。" + "x" * body_bytes,
            "criteria": {"yes": "あ", "no": "い"},
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
        （[`FROZEN_DESIGN_DOC`]）はスコアのラウンドが 1 チャンクでなければ
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
                    self.assertEqual(len(chunks), 1, f"round {entry['round']}")
                # スコアは必ず聞く（テストが空振りしないこと）。
                self.assertTrue(plan["rounds"][0]["questions"])
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
        """黙って上書きしない。`noul_of` と同じ作法。"""
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
        """欠けた答えは、後段の [`noul_of`] が失敗させる。"""
        budget = jev.RequestBudget(state_tokens=1_000)
        answers, _, _ = with_fake_ask(
            lambda state, chunk, model, timeout: {"answers": {}},
            lambda: jev.send_in_chunks(
                "state", {"marks:u1": self.question(10)}, budget, "m", 1.0
            ),
        )
        with self.assertRaises(jev.JevError):
            jev.noul_of(answers, "marks:u1")

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
    （2026-09-22 に DIM 版を削除した）。2026-09-24 からは散文の Atom ごとに
    Noul 1 問の 1 ラウンドだけで、境界も核も聞かない。
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
        """marks は `scores` の順（Unit の通し番号）。probe には答えるだけ
        （`usage` を返さないので見積もりに戻る）。それ以外は来たら落とす。"""
        def ask(state, chunk, model, timeout):
            answers = {}
            for key in chunk:
                if key in jev.PROBE_QUESTION:
                    answers[key] = {"noul": 0.5}
                    continue
                if not key.startswith("marks:"):
                    raise AssertionError(f"スコア以外の question を送った: {key}")
                number = int(key.split("u")[1])
                answers[key] = {"noul": scores[number - 1]}
            return {"answers": answers}
        return ask

    def setUp(self):
        # **実ユーザーの ~/.cache を書かない。** 判定器はもう何も書かない
        # はずだが、書いたときに実ユーザーのキャッシュを汚さないよう、また
        # 「何も書かない」を確かめられるよう、自分の tmpdir を指す。
        self._tmp = tempfile.TemporaryDirectory()
        self._saved = {
            name: os.environ.get(name) for name in ("AKAPEN_CACHE_DIR", "XDG_CACHE_HOME")
        }
        os.environ["AKAPEN_CACHE_DIR"] = str(Path(self._tmp.name) / "akapen-cache")
        os.environ["XDG_CACHE_HOME"] = str(Path(self._tmp.name) / "xdg")

    def tearDown(self):
        for name, value in self._saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self._tmp.cleanup()

    def test_a_request_without_a_question_is_refused(self):
        """**問いは必須である。** 黙って別のものを返すと、akapen 側では
        「0 本」と区別が付かない — そこは「答えている箇所が無い」という
        意味を持つ場所なので、混ぜてはならない。"""
        with self.assertRaises(jev.JevError) as caught:
            with_fake_ask(
                self.fake([0.9, 0.9, 0.9]),
                lambda: jev.annotate(self.request(question=False), "m", 1.0),
            )
        self.assertIn("no question", str(caught.exception))

    def test_each_prose_atom_is_one_unit_with_its_own_score(self):
        out = with_fake_ask(
            self.fake([0.95, 0.30, 0.05]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        self.assertEqual(out["question"], "settled")
        # 見出し（Atom 0）には Unit が無い。散文の 3 つがそれぞれ Unit。
        self.assertEqual([u["atoms"] for u in out["units"]], [[1], [2], [3]])
        self.assertEqual([u["id"] for u in out["units"]], ["u1", "u2", "u3"])
        self.assertEqual([u["score"] for u in out["units"]], [0.95, 0.30, 0.05])

    def test_every_core_is_its_own_atom_even_below_the_floor(self):
        """`core_atoms` は `[その Atom]` を**明示する**。

        省けば「絞り込み無し」で光り方は同じだが、`[]`（核を持たない ＝
        光らない）と取り違えない。足切り未満の Unit も同じ — 光るかどうかは
        akapen がスコアとつまみで決める。
        """
        out = with_fake_ask(
            self.fake([0.95, 0.30, 0.05]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        for unit in out["units"]:
            self.assertEqual(unit["core_atoms"], unit["atoms"], unit["id"])

    def test_only_the_score_round_is_sent(self):
        """ラウンドはスコアの 1 本だけ。問いの数 = 散文 Atom の数。"""
        sent = []

        def watching(state, chunk, model, timeout):
            sent.append(list(chunk))
            return self.fake([0.9, 0.9, 0.9])(state, chunk, model, timeout)

        out = with_fake_ask(watching, lambda: jev.annotate(self.request(), "m", 1.0))
        self.assertEqual(sent, [["marks:u1", "marks:u2", "marks:u3"]])
        self.assertEqual([r["round"] for r in out["jev"]["rounds"]], ["marks"])
        self.assertEqual(out["jev"]["rounds"][0]["questions"], 3)

    def test_the_report_carries_no_boundaries_and_no_core(self):
        out = with_fake_ask(
            self.fake([0.9, 0.9, 0.9]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        for key in ("boundaries", "boundaries_cached", "core_floor"):
            self.assertNotIn(key, out["jev"])
        for unit in out["units"]:
            self.assertEqual(set(unit), {"id", "atoms", "score", "core_atoms"})

    def test_the_core_floor_is_read_and_ignored(self):
        """akapen はまだ `core_floor` を送ってくる。あっても無くても同じ答え。"""
        with_floor = with_fake_ask(
            self.fake([0.95, 0.30, 0.05]),
            lambda: jev.annotate(self.request(), "m", 1.0),
        )
        request = self.request()
        request["question"].pop("core_floor")
        without = with_fake_ask(
            self.fake([0.95, 0.30, 0.05]),
            lambda: jev.annotate(request, "m", 1.0),
        )
        request["question"]["core_floor"] = "junk"
        junk = with_fake_ask(
            self.fake([0.95, 0.30, 0.05]),
            lambda: jev.annotate(request, "m", 1.0),
        )
        self.assertEqual(with_floor["units"], without["units"])
        self.assertEqual(with_floor["units"], junk["units"])

    def test_nothing_is_written_under_the_cache(self):
        """**境界のキャッシュは読まない・書かない**（もう無い）。

        `AKAPEN_CACHE_DIR` と `XDG_CACHE_HOME` の下に何も作らない。解析を
        2 回（問いを変えて）走らせても同じで、2 回目も同じ数だけ聞く。
        """
        sent = []

        def watching(state, chunk, model, timeout):
            sent.append(len(chunk))
            return self.fake([0.9, 0.9, 0.9])(state, chunk, model, timeout)

        with_fake_ask(watching, lambda: jev.annotate(self.request(), "m", 1.0))
        second = self.request()
        second["question"] = dict(second["question"], id="unsettled", text="別の問い。")
        with_fake_ask(watching, lambda: jev.annotate(second, "m", 1.0))
        self.assertEqual(sent, [3, 3], "キャッシュで減る問いは無い")
        self.assertEqual(list(Path(self._tmp.name).rglob("*")), [])

    def test_a_unit_whose_question_could_not_be_sent_has_no_score(self):
        """32k 枠を超えた question の Unit はスコアを持たない（光らない）。

        Unit そのものは残す。**1 つでも送れていれば失敗させない** — 全部が
        送れなかったときだけ止まる（下の test）。
        """
        request = self.request()
        huge = "長い。" * 20_000
        request["atoms"][2] = dict(request["atoms"][2], text=huge)
        out = with_fake_ask(
            self.fake([0.9, 0.9, 0.9]),
            lambda: jev.annotate(request, "m", 1.0),
        )
        self.assertEqual([u["atoms"] for u in out["units"]], [[1], [2], [3]])
        self.assertEqual([u.get("score") for u in out["units"]], [0.9, None, 0.9])
        self.assertEqual(out["jev"]["unsent"], {"marks": ["marks:u2"]})

    def test_a_document_too_large_for_any_question_fails_loudly(self):
        """全部「スコア無し」は akapen 側で 0 本と区別が付かないので、止まる。"""
        request = self.request()
        request["source"] = "あ" * 40_000
        with self.assertRaises(jev.JevError) as caught:
            with_fake_ask(
                self.fake([0.9, 0.9, 0.9]),
                lambda: jev.annotate(request, "m", 1.0),
            )
        self.assertIn("too large", str(caught.exception))

    #: `- 決定A。` / `- 決定B。` / `- 決定C。` の 3 項目の 1 本のリスト。
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

        リストの項目が全部光るのは「答えが全部光る」で正しく、量はつまみが
        受け持つ。
        """
        out = with_fake_ask(
            self.fake([0.95, 0.94, 0.93]),
            lambda: jev.annotate(self.list_request(), "m", 1.0),
        )
        self.assertEqual([u["atoms"] for u in out["units"]], [[0], [1], [2]])
        self.assertEqual([u["core_atoms"] for u in out["units"]], [[0], [1], [2]])
        self.assertEqual([u["score"] for u in out["units"]], [0.95, 0.94, 0.93])

    def test_a_question_without_text_is_refused(self):
        request = self.request()
        request["question"] = {"id": "settled", "core_floor": 0.2}
        with self.assertRaises(jev.JevError):
            with_fake_ask(
                self.fake([0.9, 0.9, 0.9]),
                lambda: jev.annotate(request, "m", 1.0),
            )

    def test_the_question_text_reaches_jev_verbatim(self):
        claims = []

        def watching(state, chunk, model, timeout):
            claims.extend(q.get("instructions", "") for k, q in chunk.items() if k.startswith("marks:"))
            return self.fake([0.9, 0.9, 0.9])(state, chunk, model, timeout)

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

    **問いを足す。** `dump-request` は Atom だけを見る用途なので問いを載せないが、
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
