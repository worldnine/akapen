#!/usr/bin/env python3
"""akapen の `--semantic-cmd` プロトコルを喋る参照実装（LLM 不使用）。

    akapen doc.md --semantic-cmd 'python3 examples/semantic/annotate-doc.py'

stdin から

    {"version": 1,
     "source": "<文書全文>",
     "atoms": [{"index": 0, "kind": "heading", "range": {...}, "text": "..."}]}

を受け取り、stdout へ

    {"version": 1,
     "units": [{"id": "u1", "atoms": [0], "reading_tier": "essential",
                "relations": []}]}

を返す。**range は返さない** — 返すのは Atom の index だけで、位置の
管理は akapen 側に残る。これがこのプロトコルの安全性の芯で、外部コマンド
が壊れた位置を返して文書の違う場所を装飾する事故が原理的に起きない。

このスクリプトの目的は、**API キー無しでパイプライン全体を端から端まで
動かせること**である。判断そのものは意図的に素朴で、LLM のプロンプト設計
（Jev の本体）はここには無い。

判断規則（完全に決定論的）:

    heading                  -> ESSENTIAL
    heading 直後の 1 Atom    -> SUPPORTING
    code block / table       -> DETAIL
    それ以外                 -> DETAIL

そのうえで、直前の Atom と語が大きく重なる文には REDUNDANT_WITH を付ける。
Atom 1 つが Unit 1 つで、意味境界の判断（複数 Atom を 1 Unit に束ねる）は
していない — そこは Jev の仕事である。
"""

from __future__ import annotations

import json
import sys

VERSION = 1

#: 直前の Atom と語がこの割合以上重なったら REDUNDANT_WITH を付ける。
REDUNDANCY_THRESHOLD = 0.6

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


def tier_for(atom: dict, previous_kind: str | None) -> str:
    """その Atom の Reading Tier。

    見出しは文書の骨格なので ESSENTIAL、その直後の 1 文は見出しが名指した
    話の本体なので SUPPORTING。残りは DETAIL に落とす。
    """
    kind = atom.get("kind")
    if kind == "heading":
        return "essential"
    if previous_kind == "heading":
        return "supporting"
    return "detail"


def annotate(request: dict) -> dict:
    version = request.get("version")
    if version != VERSION:
        raise ValueError(f"unsupported protocol version: {version!r}")

    atoms = request.get("atoms") or []
    units = []
    previous_kind: str | None = None
    previous: list[tuple[str, set[str]]] = []

    for atom in atoms:
        index = atom["index"]
        text = atom.get("text", "")
        unit_id = f"u{index + 1}"
        tier = tier_for(atom, previous_kind)

        # 直前までに出た Atom のうち、語の重なりがいちばん大きいもの。
        relations = []
        bag = words(text)
        if atom.get("kind") not in ("heading", "code_block"):
            best = max(
                (
                    (overlap(bag, other_bag), other_id)
                    for other_id, other_bag in previous
                ),
                default=(0.0, None),
            )
            if best[1] is not None and best[0] >= REDUNDANCY_THRESHOLD:
                relations.append({"redundant_with": best[1]})

        units.append(
            {
                "id": unit_id,
                "atoms": [index],
                "reading_tier": tier,
                "relations": relations,
            }
        )
        previous.append((unit_id, bag))
        previous_kind = atom.get("kind")

    return {"version": VERSION, "units": units}


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
