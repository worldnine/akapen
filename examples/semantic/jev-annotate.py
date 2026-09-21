#!/usr/bin/env python3
"""akapen の `--semantic-cmd` プロトコルを Jev に繋ぐアダプタ。

    TYPESAFE_API_KEY=... \
      akapen doc.md --semantic-cmd 'python3 examples/semantic/jev-annotate.py'

隣の `annotate-doc.py` は判断をしない決定論的な参照実装で、こちらが**本番の
判定器**である。判断は Jev（TypeSafe の System One モデル。**LLM ではない** —
typed な question を state に対して並列評価して構造化された値を返す。
`docs/jev.md`）に委譲する。

stdin / stdout の形は `annotate-doc.py` と同じで、**range は返さない**。返すのは
Atom の index だけなので、このスクリプトが壊れた位置を返して文書の違う場所を
装飾する事故は原理的に起きない。

---

## 3 ラウンド構成

Tier の question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

    ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice }
                 → Unit を確定
    ラウンド2  state=文書全文, questions={ Unit ごとの Tier(Choice) と
                                            redundancy(Noul) }
                 → どの Unit が MARKED になるかが確定
    ラウンド3  state=文書全文, questions={ MARKED になる Unit の核を Choice }

ラウンド 3 も前のラウンドの答えに依存するので畳めない。Jev は question を
**並列・独立に**評価するので、ラウンド 2 の時点では「どの Unit が ESSENTIAL
か」をまだ誰も知らない。そしてラウンド 3 が要るのは ESSENTIAL かつ非
REDUNDANT な Unit だけなので、先に聞くと大半が無駄になる（実測: 45.6 KB の
文書で 34 Unit 中 11 前後が ESSENTIAL、うち Atom が 2 つ以上あって核を聞く
意味があるのは 7〜8 つ。幅があるのは、ラウンド 2 の Tier が実行ごとに 1 Unit
揺れるためである）。

akapen 側のプロトコルは 1 往復（atoms in / units out）のままで、3 ラウンドは
このスクリプトの内部事情である。

## 境界は「構造は聞かない。散文どうしだけ聞く」

設計書「Jevに判断させないもの: syntax parsing」のとおり、見出し・コードブロック
・リスト項目・引用が絡む境界は**パーサが既に知っている**。実測（demo.md の境界
26 件）でも、全部 Jev に聞くと質問を浪費したうえ誤りが増えた。

    全部 Jev に聞く（文面 v1）   20/26   質問 26
    全部 Jev に聞く（文面 v2）   17/26   質問 26
    構造ルール + 散文だけ v2     18/22   質問 13   ← この方針を採用

ローカルの構造ルールは [`boundary_rule`] にある。

## 鍵は環境変数だけを見る

`TYPESAFE_API_KEY` **のみ**。キーチェーンや `op` をここに埋めない。akapen は
再解析のたびにこのスクリプトを起動し直すので、毎回 `security` や `op read` を
叩くのは無駄（`op` なら生体認証が毎回出る）。そして akapen は OSS なので、
macOS 固有の手段を埋めると他 OS で動かない。鍵の取り出し方の例は
`examples/semantic/README.md` にある。

鍵は stdout にも stderr にも出さない。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

VERSION = 1

#: `docs/jev.md` の SDK 定数に合わせる（`DEFAULT_BASE_URL` / `DEFAULT_MODEL`）。
DEFAULT_BASE_URL = "https://api.typesafe.ai"
DEFAULT_MODEL = "jev-latest"
API_PATH = "/v1/systemone"

#: 1 リクエストあたりのタイムアウト（秒）。
#:
#: Jev の SDK 既定は 10 秒（`docs/jev.md`）で、実測は 52 question を 1 リクエスト
#: にまとめて 0.84 秒だった。akapen 側は子プロセスを 60 秒で殺すので、3 ラウンド
#: と Python の起動コストを 60 秒に収めるためここは 20 秒に留める。
#:
#: **3 ラウンド × この 20 秒はちょうど 60 秒**で、理屈の上では akapen の
#: 打ち切りにぴったり並ぶ。実測ではそこまで行かない（下のとおり最遅でも
#: 1 ラウンド 1.65 秒）が、**これは測った値であって保証ではない**。
#: ラウンドを 4 つ目まで増やすなら、ここを見直すこと。
#:
#: **大きな文書でもこの 20 秒は余っている**（2026-09-21 の実測）。45,650 バイトの
#: 実業務文書で 1 ラウンドあたり 0.98〜1.53 秒、2 ラウンド込みのプロセス全体で
#: 2.42〜2.63 秒（7 回）。いちばん遅かった条件でも 1 ラウンド 1.65 秒で、
#: 20 秒には 12 倍の余裕がある。核を聞くラウンド 3 を足した後も同じで、
#: ラウンド 3 は 0.99〜1.02 秒、プロセス全体で 3.54〜3.57 秒だった（2 回）。
#:
#: 大文書で先に当たるのは**時間ではなく context window** である
#: （[`http_error_message`]）。そこは 400 で即座に返るので、ここを延ばしても
#: 何も救われない。
DEFAULT_TIMEOUT = 20.0

#: Noul がこの値以上なら REDUNDANT_WITH を付ける。
#:
#: **実測 2 点から置いた暫定値で、掃引していない。** demo.md の 1 回の実行で、
#: 言い直しの u9 が 0.92、結論の u3 が 0.36 だった。その間を取っている。
REDUNDANCY_THRESHOLD = 0.7

#: Reading Tier の criteria。設計書 `docs/semantic-reading-layer.md` の
#: 「Reading Tier」の定義の逐語。**言い換えないこと** — ここが判定品質を支配する。
TIER_CRITERIA = {
    "essential": "落とすと文書の要点、結論、制約などを取り違える可能性が高い。",
    "supporting": "ESSENTIAL な内容の理解・納得に役立つ。",
    "context": "背景や前提、理解補助。",
    "detail": "例、細部、追加説明。",
}

#: 意味境界の criteria。
#:
#: 「話題が同じか」ではなく「**拾い読みするとき一緒に読む必要があるか**」を
#: 聞いている。話題の同一性を聞く文面（旧 v1「片方だけ読むと意味が欠ける」）は
#: `採用する方式は差分配信である。| 詳細は付録にまとめた。` を同一 Unit に
#: まとめてしまい、**行の途中で表示状態が切り替わるという製品の看板を消した**。
#: 実測でも v1 20/26 → v2 17/26（構造ルール併用で 18/22）と、この文面の方が
#: 境界の質が良い。
BOUNDARY_CRITERIA = {
    "same_unit": "この 2 つは読む優先度が同じで切り離せない。片方だけ残しても意味を成さない。",
    "new_unit": "この 2 つは読む優先度が違いうる。片方を飛ばしても、もう片方の要点は失われない。",
}

SAME, NEW = "same_unit", "new_unit"

#: 核（MARKED を絞る先）を選ばせる question の文面。
#:
#: **Unit の本文をここに書かない。** 選択肢そのものが Unit の全文になるので、
#: instructions にも本文を入れると同じテキストを 2 回送ることになり、
#: context window（実測の天井 ≒65,536 tokens）を無駄に食う。
#:
#: 「重要な部分はどれか」ではなく「**1 か所だけ読むならどこか**」と聞いて
#: いる。前者だと「どれも重要」という答え方ができてしまい、Unit を丸ごと
#: 光らせていた元の状態に戻る。
CORE_INSTRUCTIONS = (
    "次の選択肢は、この文書の中の連続した 1 つのまとまりを構成する各部分の"
    "本文である。このまとまりから **1 か所だけ**読むとしたら、どこを読めば"
    "要点が取れるか。"
)

#: 1 question に並べる選択肢の上限。これを超える Unit には核を聞かない
#: （核が空 = 絞り込み無し = Unit 全体が MARKED という従来の表示に戻る）。
#:
#: **この値では切れていない** — 45.6 KB の実文書でいちばん大きい Unit でも
#: Atom は 96 個だった。上限に当たる文書を測っていないので、当たったときの
#: 振る舞いを「安全側（従来どおり）」に倒してあるだけである。
MAX_CORE_CHOICES = 255

#: 単独の Unit にする Atom 種別。中身は散文ではないので、隣の散文と読む優先度を
#: 共有しない。
STANDALONE_KINDS = frozenset({"code_block", "table"})

#: redundancy の参照先を選ぶときに無視する、内容を持たない語
#: （`annotate-doc.py` の同名定数と同じ）。
STOP_WORDS = frozenset(
    "の は が を に へ と で も や か ね よ です ます である だ する した"
    " こと もの ため よう この その あの the a an is are of to and or in on".split()
)


class JevError(Exception):
    """akapen のステータス行に 1 行で出したい失敗。"""


class Trace:
    """**計測用**。ラウンドごとの実測を、失敗しても残るように書き出す。

    本番経路では使わない（`--trace` を渡したときだけ作られる）。`annotate` は
    ラウンドの途中で失敗すると例外を投げてそこまでの計測を捨ててしまうが、
    どのラウンドで天井に当たったかは測りたい情報そのものである。だから
    1 ラウンド終わるたびに**上書きで**ファイルへ流す。
    """

    def __init__(self, path: str) -> None:
        self.path = path
        self.data: dict = {"rounds": [], "units": None}

    def round(
        self,
        number: int,
        count: int,
        usage: dict | None,
        elapsed_s: float,
        error: str | None,
    ) -> None:
        self.data["rounds"].append(
            {
                "round": number,
                "questions": count,
                "usage": usage,
                "elapsed_s": elapsed_s,
                "error": error,
            }
        )
        self.flush()

    def note(self, **fields: object) -> None:
        self.data.update(fields)
        self.flush()

    def flush(self) -> None:
        with open(self.path, "w", encoding="utf-8") as f:
            json.dump(self.data, f, ensure_ascii=False)


# ---------------------------------------------------------------------------
# 境界 — 構造ルールと、散文どうしだけの question
# ---------------------------------------------------------------------------


def boundary_rule(current_kind: str | None, next_kind: str | None) -> tuple[str | None, str]:
    """隣り合う 2 つの Atom の境界を、構造だけで決められるなら決める。

    返り値は `(SAME / NEW / None, 理由)`。`None` は「構造では決まらないので
    Jev に聞く」を意味する。

    設計書「Jevに判断させないもの: syntax parsing」に従い、**パーサが既に
    知っていることは聞かない**。実測でも、構造が絡む境界を Jev に聞くと質問を
    浪費したうえ誤りが増えた。

    規則は上から順に当てる（順序に意味がある）:

    1. 次が heading         -> NEW  見出しは必ず新しいまとまりを始める
    2. 現在が heading       -> SAME 見出しは直後の内容に付く
    3. どちらかが code_block / table -> NEW  単独の Unit にする
    4. どちらも list_item   -> SAME 同じリストの項目は 1 つのまとまり
    5. どちらも sentence    -> None Jev に聞く
    6. それ以外             -> NEW  既定（引用と散文の間など）

    規則 1 と 2 の順序は「見出しの直前」が「見出しの直後」に勝つということで、
    これがないと見出しが前の段落に吸われる。

    **規則 2 が規則 3 に勝つ**ことも意図的で、`## 設定例` + コードブロックは
    1 つの Unit になる（見出しだけの Unit は単独では Tier を判定しづらい）。
    ただし **この組み合わせは測っていない** — demo.md に出てこない。

    規則 4 と規則 6 の引用の扱い（block_quote どうしは NEW になる）も
    **測っていない**。判断の根拠は:

    - list_item どうし: 箇条書きは著者が既に 1 つのまとまりとして束ねた構造で、
      半分だけ DIM になったリストは読み物として壊れる。なお Atom には
      ネスト段階が載らないので、別々のリストが隣接していても区別できない。
    - block_quote: 引用は自己完結した挿入で、「引用した」こと自体が周囲の散文と
      読む優先度が違うという著者の表明である。連続する引用は別々の引用なので
      規則 6 で NEW になる。
    """
    if next_kind == "heading":
        return NEW, "rule:next_is_heading"
    if current_kind == "heading":
        return SAME, "rule:current_is_heading"
    if current_kind in STANDALONE_KINDS or next_kind in STANDALONE_KINDS:
        return NEW, "rule:standalone_block"
    if current_kind == "list_item" and next_kind == "list_item":
        return SAME, "rule:same_list"
    if current_kind == "sentence" and next_kind == "sentence":
        return None, "jev"
    return NEW, "rule:default"


def atom_text(atom: dict) -> str:
    """Atom の本文。前後の空白は落とす（末尾の改行を含む Atom がある）。"""
    return (atom.get("text") or "").strip()


#: **計測用**の境界モード。既定の `current` が本番の挙動で、`a` / `b` は
#: 「散文どうしの境界をどこまでローカルに決めるか」を測るための切り替えである
#: （どれを採るかはまだ決まっていない）。規則 1〜4 はどのモードでも変わらず、
#: **規則 5（sentence どうし）の扱いだけ**が違う。
#:
#:     current  すべて Jev に聞く（本番）
#:     a        空行で区切られていれば NEW_UNIT、同じ段落の中だけ Jev に聞く
#:     b        空行で区切られていれば NEW_UNIT、でなければ SAME_UNIT
#:              （境界 question を一切出さない）
BOUNDARY_MODES = ("current", "a", "b")


def paragraph_break(source: bytes, atoms: list[dict], i: int) -> bool:
    """Atom `i` と `i+1` の間に**空行**があるか。

    `range` は Rust の `&str` 由来で**バイト**単位なので、`source` も bytes の
    まま切る（Python の str で切ると多バイト文字の後がずれる）。

    Atom の range は末尾の改行を含むことがあるので、Atom 自身の末尾空白と
    Atom 間の隙間を**繋げてから**改行を数える。改行が 2 つ以上あれば空行
    （＝段落境界）である。

    隙間に `---` のような水平線が入ることがあるが、水平線は前後に空行を伴う
    ので、この判定では空行として拾われる。
    """
    own = source[atoms[i]["range"]["start"] : atoms[i]["range"]["end"]]
    gap = source[atoms[i]["range"]["end"] : atoms[i + 1]["range"]["start"]]
    tail = own[len(own.rstrip()) :]
    return (tail + gap).count(b"\n") >= 2


def plan_boundaries(
    atoms: list[dict],
    source: str = "",
    mode: str = "current",
) -> list[dict]:
    """すべての境界について、構造で決まったか Jev に聞くかを並べる。

    要素は `{"after_atom": i, "decision": SAME/NEW/None, "by": 理由}`。
    `decision` が `None` のものだけがラウンド 1 の question になる。

    `mode` は**計測用**の切り替えで、既定の `current` は本番の挙動。
    `a` / `b` は [`BOUNDARY_MODES`] を参照（`source` が要る）。
    """
    raw = source.encode() if mode != "current" else b""
    plan = []
    for i in range(len(atoms) - 1):
        decision, why = boundary_rule(atoms[i].get("kind"), atoms[i + 1].get("kind"))
        if decision is None and mode != "current" and paragraph_break(raw, atoms, i):
            decision, why = NEW, "rule:paragraph_break"
        elif decision is None and mode == "b":
            decision, why = SAME, "rule:same_paragraph"
        # 本文が空の Atom は Jev に見せても判断材料が無い。既定側へ倒す。
        if decision is None and not (atom_text(atoms[i]) and atom_text(atoms[i + 1])):
            decision, why = NEW, "rule:empty_text"
        plan.append({"after_atom": i, "decision": decision, "by": why})
    return plan


def boundary_questions(atoms: list[dict], plan: list[dict]) -> dict:
    """ラウンド 1 の questions（構造で決まらなかった境界だけ）。"""
    questions = {}
    for entry in plan:
        if entry["decision"] is not None:
            continue
        i = entry["after_atom"]
        questions[f"boundary:{i}"] = {
            "type": "choice",
            "instructions": (
                "この文書を拾い読みするとき、次の 2 つの連続する部分は"
                "必ず一緒に読まなければならないか、それとも片方だけ飛ばせるか。\n\n"
                f"――― 前 ―――\n{atom_text(atoms[i])}\n"
                f"――― 後 ―――\n{atom_text(atoms[i + 1])}\n―――――――――"
            ),
            "criteria": dict(BOUNDARY_CRITERIA),
        }
    return questions


def apply_boundary_answers(plan: list[dict], answers: dict) -> None:
    """ラウンド 1 の答えを plan へ書き戻す（`confidence` も残す）。

    答えが無い / criteria に無い値だった question は**失敗させる**。既定へ
    倒さないのは、黙って埋めた境界が「それらしく見えるが間違っている注釈」に
    なるため（プロトコル側の「部分適用は何もしないより悪い」と同じ理由）。
    """
    for entry in plan:
        if entry["decision"] is not None:
            continue
        key = f"boundary:{entry['after_atom']}"
        entry["decision"] = choice_of(answers, key, BOUNDARY_CRITERIA)
        entry["confidence"] = confidence_of(answers, key)


def group_units(atoms: list[dict], plan: list[dict]) -> list[list[int]]:
    """境界の答えから Unit（Atom index の並び）を組む。"""
    if not atoms:
        return []
    units = [[0]]
    for entry in plan:
        index = entry["after_atom"] + 1
        if entry["decision"] == SAME:
            units[-1].append(index)
        else:
            units.append([index])
    return units


# ---------------------------------------------------------------------------
# Tier と redundancy
# ---------------------------------------------------------------------------


def unit_body(atoms: list[dict], indices: list[int]) -> str:
    """Unit の本文。Tier / redundancy の question に埋める対象。"""
    return " ".join(filter(None, (atom_text(atoms[i]) for i in indices)))


def unit_questions(atoms: list[dict], units: list[list[int]]) -> dict:
    """ラウンド 2 の questions（Unit ごとの Tier と redundancy）。

    redundancy は**方向を必ず指定する**。「これより前の箇所ですでに述べられた
    内容を言い直しているだけか」であって、対称に「重複しているか」と聞いては
    ならない。実測では対称な文面だと結論の u3 が 0.71 を出し（結論は文書中で
    何度も触れられるので「重複」に見える）、方向ありへ直すと u3 は 0.36 に落ち、
    本当の言い直しである u9 が 0.92 になった。

    これは設計書の「研究的背景」と整合する。Duggan & Payne の satisficing は
    「読み進めて information gain が落ちたら次へ移る」という**逐次的**なモデル
    で、設計書も「**既読内容との** redundancy」と書いている。redundancy は
    既読との相対で決まるので、方向が本質である。

    先頭の Unit には redundancy を聞かない —「これより前」が存在せず、
    REDUNDANT_WITH の参照先も作れない。
    """
    questions = {}
    for number, indices in enumerate(units, start=1):
        uid = f"u{number}"
        body = unit_body(atoms, indices)
        questions[f"tier:{uid}"] = {
            "type": "choice",
            "instructions": (
                "この文書の中で、次の部分はどの読む優先度に当たりますか。\n\n"
                f"――― 対象 ―――\n{body}\n―――――――――"
            ),
            "criteria": dict(TIER_CRITERIA),
        }
        if number > 1:
            questions[f"redundant:{uid}"] = {
                "type": "noul",
                "instructions": (
                    "次の部分は、これより**前**の箇所ですでに述べられた内容を"
                    "言い直しているだけで、新しい情報を加えていない。\n\n"
                    f"――― 対象 ―――\n{body}\n―――――――――"
                ),
            }
    return questions


def words(text: str) -> set[str]:
    """redundancy の**参照先**を選ぶための語の集合（`annotate-doc.py` と同じ）。

    日本語には分かち書きが無いので 2 文字の連続を語の代わりに使う。
    """
    cleaned = "".join(c if c.isalnum() else " " for c in text)
    tokens = [t for t in cleaned.split() if t not in STOP_WORDS]
    bag: set[str] = set()
    for token in tokens:
        if token.isascii():
            bag.add(token.lower())
        else:
            bag.update(token[i : i + 2] for i in range(max(len(token) - 1, 1)))
    return bag


def overlap(a: set[str], b: set[str]) -> float:
    """2 つの語集合の重なり（小さい方に対する割合）。"""
    if not a or not b:
        return 0.0
    return len(a & b) / min(len(a), len(b))


def redundancy_target(bodies: list[str], number: int) -> str | None:
    """`number` 番目（1 始まり）の Unit が言い直している**先**を選ぶ。

    Jev の Noul が答えるのは「前に述べられたことの言い直しか」までで、
    **どの Unit かは答えない**。参照先は語の重なりがいちばん大きい先行 Unit を
    ローカルに選ぶ。

    設計書の分担で言えば、これは Jev の判断ではなくローカル rule である。
    なお現在の `policy::keep_order` が見ているのは `is_redundant()`
    （relations が空でないか）だけで、**参照先の id は実在検査以外に使われて
    いない** — 選び方が表示に効くようになるのは relation の使い道が増えてから。
    """
    bag = words(bodies[number - 1])
    best_score, best = 0.0, None
    for earlier in range(1, number):
        score = overlap(bag, words(bodies[earlier - 1]))
        if score > best_score:
            best_score, best = score, f"u{earlier}"
    return best


# ---------------------------------------------------------------------------
# 答えの取り出し（緩めない）
# ---------------------------------------------------------------------------


def answer_of(answers: dict, key: str) -> dict:
    answer = answers.get(key)
    if not isinstance(answer, dict):
        raise JevError(f"Jev の応答に {key} の答えがありません")
    return answer


def choice_of(answers: dict, key: str, criteria: dict) -> str:
    got = answer_of(answers, key).get("choice")
    if not isinstance(got, str) or got not in criteria:
        raise JevError(f"Jev が {key} に未知の choice を返しました: {got!r}")
    return got


def confidence_of(answers: dict, key: str) -> float | None:
    got = answer_of(answers, key).get("confidence")
    return float(got) if isinstance(got, (int, float)) else None


def noul_of(answers: dict, key: str) -> float:
    got = answer_of(answers, key).get("noul")
    if not isinstance(got, (int, float)):
        raise JevError(f"Jev が {key} に noul を返しませんでした: {got!r}")
    return float(got)


# ---------------------------------------------------------------------------
# Unit の組み立て
# ---------------------------------------------------------------------------


def build_units(atoms: list[dict], units: list[list[int]], answers: dict) -> list[dict]:
    """ラウンド 2 の答えから、プロトコルの `units` を組む。

    `confidence` と `noul` は**捨てず**、各 Unit の `jev` フィールドに記録する
    （プロトコルは未知のフィールドを拒否しないので akapen 側は無視する）。
    使い道は実測してから決める。閾値で判定を倒すことは**していない** —
    実測で閾値が値の真上に乗り、実行ごとに答えが揺れたため。
    """
    bodies = [unit_body(atoms, indices) for indices in units]
    out = []
    for number, indices in enumerate(units, start=1):
        uid = f"u{number}"
        tier = choice_of(answers, f"tier:{uid}", TIER_CRITERIA)
        record: dict = {
            "tier_choice": tier,
            "tier_confidence": confidence_of(answers, f"tier:{uid}"),
        }
        relations: list[dict] = []
        if number > 1:
            noul = noul_of(answers, f"redundant:{uid}")
            record["redundancy_noul"] = noul
            if noul >= REDUNDANCY_THRESHOLD:
                target = redundancy_target(bodies, number)
                record["redundant_with"] = target
                if target is not None:
                    relations.append({"redundant_with": target})
        out.append(
            {
                "id": uid,
                "atoms": list(indices),
                "reading_tier": tier,
                "relations": relations,
                "jev": record,
            }
        )
    return out


# ---------------------------------------------------------------------------
# 核 — Unit の中で「これだけ読めば要点が取れる」Atom
# ---------------------------------------------------------------------------


def wants_core(unit: dict) -> bool:
    """この Unit に核を聞く意味があるか。

    聞くのは **MARKED になりうる Unit だけ**である。`policy::decorate` が
    MARKED にするのは「Budget に残った ESSENTIAL かつ非 REDUNDANT」なので、
    ここで Tier と relations を見れば足りる（Budget はこのスクリプトから
    見えないし、見る必要もない — 核の選択は Budget に依存しない）。

    絞り込むことで question 数が減る。実測（45.6 KB の文書）では 34 Unit の
    うち 11 が ESSENTIAL かつ非 REDUNDANT で、さらに Atom が 2 つ以上ある
    7 つだけがラウンド 3 の question になった。
    """
    return unit["reading_tier"] == "essential" and not unit["relations"]


def core_questions(atoms: list[dict], units: list[dict]) -> dict:
    """ラウンド 3 の questions（MARKED になる Unit の核）。

    選択肢は Unit を構成する Atom の本文そのもので、キーは `atom:<index>`。
    本文が空の Atom は選択肢にしない（選ばれても光らせる中身が無い）。

    **Atom が 1 つしかない Unit には聞かない。** 選択肢が 1 つの Choice は
    答えが決まっていて、question を 1 つ使う意味が無い。このとき核は空の
    ままになり、Unit 全体が MARKED になる — Atom が 1 つなのだから同じこと
    である。
    """
    questions = {}
    for unit in units:
        if not wants_core(unit):
            continue
        options = {
            f"atom:{i}": atom_text(atoms[i])
            for i in unit["atoms"]
            if atom_text(atoms[i])
        }
        if not 2 <= len(options) <= MAX_CORE_CHOICES:
            continue
        questions[f"core:{unit['id']}"] = {
            "type": "choice",
            "instructions": CORE_INSTRUCTIONS,
            "criteria": options,
        }
    return questions


def apply_core_answers(units: list[dict], questions: dict, answers: dict) -> None:
    """ラウンド 3 の答えを `core_atoms` として書き戻す。

    **核は 1 Unit につき 1 つだけ**にしてある。Choice が返すのは 1 つで、
    `probabilities` を閾値で切って複数採る案は採らない — 閾値を判定に使うのは
    以前の実測で不安定だった（`docs/design/jev.md`「confidence の閾値ガードは
    不採用」）。同じ轍を踏むなら、まず閾値の安定性を実測してからになる。

    答えが無い / criteria に無い値だった question は [`choice_of`] が失敗
    させる。境界や Tier と同じで、黙って埋めない。
    """
    for unit in units:
        key = f"core:{unit['id']}"
        question = questions.get(key)
        if question is None:
            continue
        choice = choice_of(answers, key, question["criteria"])
        unit["core_atoms"] = [int(choice.split(":")[1])]
        unit["jev"]["core_choice"] = choice
        unit["jev"]["core_confidence"] = confidence_of(answers, key)


# ---------------------------------------------------------------------------
# Jev の呼び出し
# ---------------------------------------------------------------------------


def one_line(text: str) -> str:
    """ステータス行に載せるために改行と連続空白を潰す。"""
    return re.sub(r"\s+", " ", text).strip()


def api_key() -> str:
    """環境変数 `TYPESAFE_API_KEY` **だけ**を見る。値は絶対に出力しない。"""
    key = os.environ.get("TYPESAFE_API_KEY", "").strip()
    if not key:
        raise JevError(
            "TYPESAFE_API_KEY が未設定です。export TYPESAFE_API_KEY=... して "
            "akapen を起動し直してください（鍵の取り出し方は "
            "examples/semantic/README.md）"
        )
    return key


def http_error_message(code: int, detail: str) -> str:
    """HTTP エラーを、ステータス行に出して意味が通る 1 行にする。

    とくに `max_tokens_exceeded` は**この経路でいちばん現実的な失敗**なので、
    生の JSON ではなく原因と対処を出す。実測（2026-09-21）では:

    - 45,650 バイトの実文書は成功し、ラウンド 2 の input が 60,518〜63,152
      tokens。**天井の 92〜96 %** に載っている
    - 同じ文書を 1.06 倍にすると `max_tokens_exceeded` で失敗する
    - 天井は二分探索で input 65,033 tokens が成功・約 65.5k が失敗 —
      つまり **65,536 (2^16) tokens** と読める（TypeSafe の公式値は未確認）

    `docs/design/jev.md` は「context window は需要に応じて変わりうる」と書いて
    いるので、**数値を断定せず実測値として**出す。ここで切れるのは時間ではなく
    大きさなので、akapen 側のタイムアウトを延ばしても何も直らない。
    """
    if "max_tokens_exceeded" in detail:
        return (
            "文書が大きすぎて Jev の context window に入りません"
            "（実測では input 約 65,000 tokens が上限で、45KB 程度の文書が"
            "その 9 割超を使います）。タイムアウトではないので待っても変わり"
            "ません。文書を分けてください"
        )
    return f"Jev が HTTP {code} を返しました: {detail}"


def ask_jev(
    state: str,
    questions: dict,
    model: str,
    timeout: float,
    trace: Trace | None = None,
    number: int = 0,
    count: int = 0,
) -> dict:
    """1 リクエストで questions をまとめて評価させる。

    Jev は「すべての question を同じ state に対して並列かつ独立に評価する」
    設計なので（`docs/jev.md`）、まとめるのはコスト上の妥協ではなく想定された
    使い方である。
    """
    base = os.environ.get("TYPESAFE_BASE_URL", DEFAULT_BASE_URL).rstrip("/")
    body = json.dumps({"state": state, "model": model, "questions": questions}).encode()
    request = urllib.request.Request(
        base + API_PATH,
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {api_key()}",
            "Content-Type": "application/json",
        },
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.loads(response.read())
    except urllib.error.HTTPError as e:
        # 本文には鍵は載らない（載せていない）。要点だけ 1 行にする。
        detail = one_line(e.read().decode("utf-8", "replace"))[:200]
        if trace is not None:
            trace.round(number, count, None, round(time.monotonic() - started, 3), detail)
        raise JevError(http_error_message(e.code, detail)) from e
    except urllib.error.URLError as e:
        raise JevError(f"Jev に接続できません: {one_line(str(e.reason))}") from e
    except json.JSONDecodeError as e:
        raise JevError(f"Jev の応答が JSON ではありません: {one_line(str(e))}") from e
    except TimeoutError as e:
        raise JevError(f"Jev が {timeout} 秒以内に応答しませんでした") from e
    if not isinstance(payload.get("answers"), dict):
        raise JevError("Jev の応答に answers がありません")
    payload["elapsed_s"] = round(time.monotonic() - started, 3)
    if trace is not None:
        trace.round(number, count, payload.get("usage"), payload["elapsed_s"], None)
    return payload


def round_record(payload: dict, count: int) -> dict:
    """報告用に 1 ラウンド分の実測を残す。"""
    return {
        "questions": count,
        "elapsed_s": payload.get("elapsed_s"),
        "model": payload.get("model"),
        "usage": payload.get("usage"),
    }


# ---------------------------------------------------------------------------
# 入り口
# ---------------------------------------------------------------------------


def annotate(
    request: dict,
    model: str,
    timeout: float,
    mode: str = "current",
    trace: Trace | None = None,
) -> dict:
    version = request.get("version")
    if version != VERSION:
        raise JevError(f"対応していないプロトコル版です: {version!r}")
    state = request.get("source") or ""
    atoms = request.get("atoms") or []
    if not atoms:
        return {"version": VERSION, "units": []}

    rounds = []

    # --- ラウンド 1: 散文どうしの境界 -> Unit --------------------------
    plan = plan_boundaries(atoms, state, mode)
    questions = boundary_questions(atoms, plan)
    if questions:
        payload = ask_jev(state, questions, model, timeout, trace, 1, len(questions))
        apply_boundary_answers(plan, payload["answers"])
        rounds.append(round_record(payload, len(questions)))
    units = group_units(atoms, plan)
    if trace is not None:
        trace.note(boundaries=plan, units=len(units))

    # --- ラウンド 2: Unit ごとの Tier と redundancy ---------------------
    questions = unit_questions(atoms, units)
    payload = ask_jev(state, questions, model, timeout, trace, 2, len(questions))
    rounds.append(round_record(payload, len(questions)))
    built = build_units(atoms, units, payload["answers"])

    # --- ラウンド 3: MARKED になる Unit の核 ---------------------------
    questions = core_questions(atoms, built)
    if questions:
        payload = ask_jev(state, questions, model, timeout, trace, 3, len(questions))
        apply_core_answers(built, questions, payload["answers"])
        rounds.append(round_record(payload, len(questions)))

    return {
        "version": VERSION,
        "units": built,
        "jev": {"rounds": rounds, "boundaries": plan, "boundary_mode": mode},
    }


def dry_run(request: dict, model: str, mode: str = "current") -> dict:
    """API を叩かずに、送る 3 ラウンドのリクエストの形を出す。

    後のラウンドは前のラウンドの答えに依存するので、仮定を置いて組む。

    - ラウンド 2: **Jev に聞く境界はすべて NEW_UNIT だった**と仮定する
      （構造ルールで決まった境界はそのまま効く）
    - ラウンド 3: **すべての Unit が ESSENTIAL かつ非 REDUNDANT だった**と
      仮定する。本番では [`wants_core`] がここを絞るので、実際に送る
      question はこれより少ない

    仮定は戻り値の `assumptions` にも載せる — 形だけ見て「これが本番で送る
    question 数だ」と読まれると困るため。
    """
    atoms = request.get("atoms") or []
    state = request.get("source") or ""
    plan = plan_boundaries(atoms, state, mode)
    first = boundary_questions(atoms, plan)
    for entry in plan:
        if entry["decision"] is None:
            entry["decision"] = NEW
    units = group_units(atoms, plan)
    # ラウンド 3 は Tier の答えを要るので、全部 ESSENTIAL だと仮定して組む。
    as_essential = [
        {"id": f"u{number}", "atoms": indices, "reading_tier": "essential", "relations": []}
        for number, indices in enumerate(units, start=1)
    ]
    return {
        "assumptions": [
            "ラウンド 2 は「Jev に聞く境界はすべて new_unit」と仮定して組んでいる",
            "ラウンド 3 は「すべての Unit が essential かつ非 redundant」と仮定して"
            "組んでいる（本番はここが絞られるので question はもっと少ない）",
        ],
        "rounds": [
            {"round": 1, "state": state, "model": model, "questions": first},
            {
                "round": 2,
                "state": state,
                "model": model,
                "questions": unit_questions(atoms, units),
            },
            {
                "round": 3,
                "state": state,
                "model": model,
                "questions": core_questions(atoms, as_essential),
            },
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="akapen の --semantic-cmd を Jev に繋ぐアダプタ",
    )
    parser.add_argument(
        "--model",
        default=os.environ.get("TYPESAFE_DEFAULT_MODEL", DEFAULT_MODEL),
        help=f"Jev のモデル（既定 {DEFAULT_MODEL}、TYPESAFE_DEFAULT_MODEL でも指定可）",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help=f"1 リクエストのタイムアウト秒（既定 {DEFAULT_TIMEOUT}）",
    )
    parser.add_argument(
        "--boundary-mode",
        choices=BOUNDARY_MODES,
        default="current",
        help="【計測用】散文どうしの境界の決め方（既定 current が本番の挙動）",
    )
    parser.add_argument(
        "--trace",
        metavar="PATH",
        help="【計測用】ラウンドごとの usage と所要をこのファイルへ逐次書き出す"
        "（途中で失敗しても残る）",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="API を叩かず、送る 3 ラウンドのリクエストの形だけを出す"
        "（ラウンド 2 は全境界 new_unit、ラウンド 3 は全 Unit essential を仮定）",
    )
    args = parser.parse_args()

    try:
        request = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(f"jev-annotate: stdin が JSON ではありません: {one_line(str(e))}", file=sys.stderr)
        return 1
    try:
        if args.dry_run:
            response = dry_run(request, args.model, args.boundary_mode)
        else:
            trace = Trace(args.trace) if args.trace else None
            response = annotate(
                request, args.model, args.timeout, args.boundary_mode, trace
            )
    except JevError as e:
        # akapen はステータス行に stderr の**最後の非空行**を 160 字まで出す
        # （`src/export.rs` の `Capture::tail`）。だから 1 行に収める。
        print(f"jev-annotate: {one_line(str(e))}", file=sys.stderr)
        return 1
    except (KeyError, TypeError, ValueError) as e:
        print(f"jev-annotate: 要求を読めません: {one_line(str(e))}", file=sys.stderr)
        return 1
    json.dump(response, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
