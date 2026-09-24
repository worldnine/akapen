#!/usr/bin/env python3
"""akapen の `--semantic-cmd` プロトコルを喋る参照実装（Jev を呼ばない）。

    akapen doc.md --semantic-cmd 'python3 examples/semantic/annotate-doc.py'

stdin から

    {"version": 1,
     "source": "<文書全文>",
     "question": {"id": "essential", "text": "…", "core_floor": 0.2},
     "atoms": [{"index": 0, "kind": "heading", "range": {...}, "text": "..."}]}

を受け取り、stdout へ

    {"version": 1, "question": "essential",
     "units": [{"id": "u2", "atoms": [1], "score": 0.7, "core_atoms": [1]}]}

を返す。**range は返さない** — 返すのは Atom の index だけで、位置の
管理は akapen 側に残る。これがこのプロトコルの安全性の芯で、外部コマンド
が壊れた位置を返して文書の違う場所を装飾する事故が原理的に起きない。

このスクリプトの目的は、**API キー無しでパイプライン全体を端から端まで
動かせること**である。判断そのものは意図的に素朴で、Jev への question
設計（`docs/design/jev.md`）はここには無い。**問いの文面は読まない** —
どの問いで呼ばれても同じスコアを返す。

**Unit の作り方は本番の判定器（`jev-annotate.py`）と同じ約束に乗る**
（2026-09-24）: 散文の Atom（文・リスト項目・引用・表のデータ行）1 つが
Unit 1 つで、各 Unit の `core_atoms` はその Atom 自身を明示する。見出し・
コードブロック・表のヘッダ行には Unit を作らない ＝ スコアを持たず光らない。
複数の Atom を 1 Unit に束ねる判断は、本番でももうしていない。

判断規則（完全に決定論的。散文の Atom にだけ付ける）:

    見出しの直後の 1 Atom    -> 0.70
    それ以外                 -> 0.30

そのうえで、直前までに出た Atom と語が大きく重なる文はスコアを半分に
する（言い直しは答えとして弱い、という素朴な代用）。

**`question` の無い要求は断る。** 問いを持たない解析はこの層に無い
（2026-09-22 に DIM 版を削除した）。
"""

from __future__ import annotations

import json
import sys

VERSION = 1

#: 直前までの Atom と語がこの割合以上重なったら、言い直しとみなす。
REDUNDANCY_THRESHOLD = 0.6

#: 言い直しとみなした Atom のスコアに掛ける係数。
REDUNDANCY_PENALTY = 0.5

#: Unit を作る Atom の種別。`jev-annotate.py` の `PROSE_KINDS` と同じで、
#: 見出し・コード・表のヘッダ行には Unit を作らない。
PROSE_KINDS = frozenset({"sentence", "list_item", "block_quote", "table_row"})

#: redundancy を見るときに無視する、内容を持たない語。
STOP_WORDS = frozenset(
    "の は が を に へ と で も や か ね よ です ます である だ する した"
    " こと もの ため よう この その あの the a an is are of to and or in on".split()
)


def words(text: str) -> set[str]:
    """redundancy 判定のための語の集合。

    日本語には分かち書きが無いので、2 文字の連続（bigram）を語の代わりに
    使う。形態素解析を入れないのは、この参照実装の目的が「プロトコルを
    端から端まで動かすこと」であって判断の質ではないため。
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


def score_for(previous_kind: str | None) -> float:
    """散文の Atom のスコア（問いにどれだけ答えているか、の代用）。

    見出しの直後の 1 文は見出しが名指した話の本体なので高く、それ以外は
    低い。**問いの文面は見ていない** — この参照実装は判断をしない。
    """
    if previous_kind == "heading":
        return 0.70
    return 0.30


def annotate(request: dict) -> dict:
    version = request.get("version")
    if version != VERSION:
        raise ValueError(f"unsupported protocol version: {version!r}")

    question = request.get("question")
    if not isinstance(question, dict) or not question.get("text"):
        raise ValueError(
            "this request carries no question — akapen must send one "
            "(the DIM version was removed on 2026-09-22)"
        )

    atoms = request.get("atoms") or []
    units = []
    previous_kind: str | None = None
    previous: list[set[str]] = []

    for atom in atoms:
        index = atom["index"]
        text = atom.get("text", "")
        bag = words(text)

        # 散文で本文のある Atom だけが Unit になる（`jev-annotate.py` の
        # `prose_units` と同じ）。散文でない Atom も「直前までに出た語」には
        # 数える — 見出しをなぞっただけの文は言い直しとみなす。
        if atom.get("kind") in PROSE_KINDS and text.strip():
            score = score_for(previous_kind)
            # 直前までに出た Atom のうち、語の重なりがいちばん大きいもの。
            best = max((overlap(bag, other) for other in previous), default=0.0)
            if best >= REDUNDANCY_THRESHOLD:
                score *= REDUNDANCY_PENALTY
            # 核は Atom 自身を**明示する**。省いても 1 Atom の Unit なら
            # 光り方は同じだが、`[]`（核を持たない ＝ 光らない）と取り違え
            # ないよう、プロトコルの 3 値のどれかを字面に出す。
            units.append(
                {
                    "id": f"u{index + 1}",
                    "atoms": [index],
                    "score": round(score, 2),
                    "core_atoms": [index],
                }
            )
        previous.append(bag)
        previous_kind = atom.get("kind")

    return {"version": VERSION, "question": question.get("id"), "units": units}


def main() -> int:
    try:
        request = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(f"annotate-doc: stdin is not JSON: {e}", file=sys.stderr)
        return 1
    try:
        response = annotate(request)
    except (KeyError, TypeError, ValueError) as e:
        print(f"annotate-doc: {e}", file=sys.stderr)
        return 1
    json.dump(response, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
