#!/usr/bin/env python3
"""examples/semantic/demo.json を demo.md から生成する。

byte range は手で書かない。ここに書くのは「どの文を Atom にするか」と
「どの Atom がどの Semantic Unit に属し、その Unit の Reading Tier は何か」
だけで、offset は本文を検索して求める。demo.md を編集したらこれを走らせ直す
こと（`source_sha256` が変わるので、忘れると akapen が fixture を拒否する）。

    python3 examples/semantic/build-demo-json.py

標準出力には Reading Policy の keep 順と累積 % の表を出す。テストが使う
budget の閾値はこの表から取っている（勘で選ばない）。
"""

from __future__ import annotations

import hashlib
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
MD = HERE / "demo.md"
JSON_OUT = HERE / "demo.json"

# (unit id, AtomKind, 本文, 末尾に足すバイト数)
#
# 末尾に足すバイト数は 0 か 1。1 は「行末の改行まで含める」という意味で、
# 2 つの用途がある:
#
#   * 段落内の soft line break。renderer は行を半角スペース 1 個で繋ぎ、
#     その合成 span の attribution は改行 1 バイト（exact ではない）。
#     Atom がそこを覆わないと、MARKED の帯に 1 セルの穴が空く。
#   * fenced code block。中身全体（末尾改行を含む）が 1 つの非 exact な
#     attribution なので、完全に覆わないと何も装飾されない。
#
# 見出しは `## ` を含めない。renderer が付ける attribution は marker を
# 除いた見出しテキストそのもの（exact）なので、本文だけを指せば足りる。
# リスト項目も同じ理由で本文だけ（`- ` マーカーは明るいまま残る — Phase 2
# が固定した MVP の割り切り）。
ATOMS: list[tuple[str, str, str, int]] = [
    ("u1", "heading", "通知基盤リニューアル設計メモ", 0),
    ("u2", "sentence", "この文書は社内通知基盤の作り直しについての設計メモである。", 0),
    ("u2", "sentence", "現状の課題、採用する方式、制約、移行手順の順に記す。", 0),
    ("u3", "heading", "結論", 0),
    # ここが本命。同じ source 行に u3(ESSENTIAL) と u4(DETAIL) が並ぶ。
    ("u3", "sentence", "採用する方式は差分配信である。", 0),
    ("u4", "sentence", "詳細は付録にまとめた。", 0),
    ("u5", "sentence", "現行の一括配信は購読者数に比例して遅くなるため、配信対象を差分だけに絞る。", 0),
    ("u6", "heading", "背景", 0),
    ("u6", "sentence", "通知基盤は導入から四年が経ち、購読者数は当初の想定を大きく超えた。", 1),
    ("u6", "sentence", "初期の設計では全購読者へ毎回全件を配信していた。", 1),
    ("u6", "sentence", "当時は購読者が数百人規模だったので、全件配信でも問題にならなかった。", 0),
    ("u7", "heading", "制約", 0),
    ("u7", "list_item", "既存の購読者向け API は変更しない。", 0),
    ("u7", "list_item", "移行期間中も配信の取りこぼしを出さない。", 0),
    ("u8", "heading", "影響範囲", 0),
    ("u8", "sentence", "配信遅延の監視ダッシュボードと、管理画面の配信履歴の二箇所が影響を受ける。", 1),
    ("u8", "sentence", "どちらも読み取り側なので、配信そのものの整合性は今回の変更では変わらない。", 0),
    ("u9", "heading", "補足", 0),
    # 2 つめの行内ペア。u9 は REDUNDANT、u10 は DETAIL。
    ("u9", "sentence", "つまり、配信対象を差分だけに絞るということである。", 0),
    ("u10", "sentence", "念のため繰り返しておく。", 0),
    ("u11", "block_quote", "全件配信は購読者数が増えるほど配信時間が線形に伸びる。", 0),
    ("u12", "heading", "数値の目安", 0),
    ("u12", "sentence", "購読者一万人で約二秒、十万人で約二十秒かかっている。", 0),
    ("u12", "sentence", "実測は社内計測環境のものである。", 0),
    ("u12", "code_block", "delivery: incremental", 1),
    ("u13", "sentence", "なお、差分の算出そのものは既存の差分計算モジュールをほぼそのまま使える。", 1),
    ("u13", "sentence", "細かい差異は実装時に吸収する予定なので、ここでは触れない。", 0),
]

# (unit id, Reading Tier, REDUNDANT_WITH の相手 or None)
UNITS: list[tuple[str, str, str | None]] = [
    ("u1", "essential", None),      # タイトル
    ("u2", "supporting", None),     # 何の文書かの前置き
    ("u3", "essential", None),      # 結論
    ("u4", "detail", None),         # 付録への言及（結論と同じ行）
    ("u5", "supporting", None),     # 結論の理由
    ("u6", "context", None),        # 背景
    ("u7", "essential", None),      # 制約
    ("u8", "context", None),        # 影響範囲
    ("u9", "supporting", "u3"),     # 補足 = 結論の言い換え
    ("u10", "detail", None),        # 念押し（補足と同じ行）
    ("u11", "context", None),       # 引用
    ("u12", "detail", None),        # 数値の目安とコード
    ("u13", "detail", None),        # 余談
]

TIER_RANK = {"essential": 0, "supporting": 1, "context": 2, "detail": 3}
WEAKENED = {"essential": "supporting", "supporting": "context", "context": "detail", "detail": "detail"}


def locate(source: str) -> list[dict]:
    """ATOMS を文書順に検索して byte range を与える。"""
    atoms = []
    cursor = 0  # 文字オフセット。文書順を強制する（同じ文言の取り違え防止）。
    for unit, kind, text, trail in ATOMS:
        at = source.find(text, cursor)
        if at < 0:
            raise SystemExit(f"demo.md に見つかりません: {text!r}")
        if source.find(text, at + 1) >= 0:
            # 一意でなくても文書順で確定はするが、編集で入れ替わると黙って
            # 別の場所を指すので落としておく。
            raise SystemExit(f"demo.md に 2 回以上現れます: {text!r}")
        start = len(source[:at].encode())
        end = start + len(text.encode()) + trail
        atoms.append({"unit": unit, "kind": kind, "text": text, "start": start, "end": end})
        cursor = at + len(text)
    for a, b in zip(atoms, atoms[1:]):
        if a["end"] > b["start"]:
            raise SystemExit(f"Atom が重なっています: {a['text']!r} と {b['text']!r}")
    return atoms


def keep_order(atoms: list[dict], units: list[dict]) -> list[dict]:
    """policy::keep_order と同じ鍵で並べる（Rust 側の実装の写し）。

    鍵は `(実効 Tier, redundant か, バイト長, 先頭バイト位置, Unit の並び順)`
    で、小さいほど残りやすい。REDUNDANT な Unit は 1 段弱い Tier として扱う。
    """
    for u in units:
        mine = [a for a in atoms if a["unit"] == u["id"]]
        u["length"] = sum(a["end"] - a["start"] for a in mine)
        u["start"] = min(a["start"] for a in mine)

    def key(u: dict) -> tuple:
        tier = WEAKENED[u["tier"]] if u["redundant"] else u["tier"]
        return (TIER_RANK[tier], u["redundant"], u["length"], u["start"], u["index"])

    return sorted(units, key=key)


def main() -> None:
    source = MD.read_text(encoding="utf-8")
    atoms = locate(source)

    units = [
        {"id": uid, "tier": tier, "redundant": redundant_with is not None,
         "redundant_with": redundant_with, "index": i}
        for i, (uid, tier, redundant_with) in enumerate(UNITS)
    ]
    ids = {a["unit"] for a in atoms}
    missing = ids - {u["id"] for u in units}
    if missing:
        raise SystemExit(f"Unit の定義がありません: {sorted(missing)}")
    empty = {u["id"] for u in units} - ids
    if empty:
        raise SystemExit(f"Atom を 1 つも持たない Unit: {sorted(empty)}")

    doc = {
        "atoms": [
            {"range": {"start": a["start"], "end": a["end"]}, "kind": a["kind"]}
            for a in atoms
        ],
        "units": [
            {
                "id": u["id"],
                "atoms": [i for i, a in enumerate(atoms) if a["unit"] == u["id"]],
                "reading_tier": u["tier"],
                "relations": (
                    [{"redundant_with": u["redundant_with"]}] if u["redundant"] else []
                ),
            }
            for u in units
        ],
        "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
    }
    JSON_OUT.write_text(json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    order = keep_order(atoms, units)
    total = sum(u["length"] for u in units)
    print(f"{MD.name}: {len(source.encode())} バイト / Atom {len(atoms)} / Unit {len(units)}")
    print(f"Unit の合計バイト長: {total}\n")
    print("| 順 | unit | 実効 Tier  | 長さ | 累積 | 累積 % | この Unit が残る上限 budget |")
    print("| -- | ---- | ---------- | ---- | ---- | ------ | -------------------------- |")
    spent = 0
    for rank, u in enumerate(order, 1):
        spent += u["length"]
        pct = spent * 100 / total
        eff = WEAKENED[u["tier"]] if u["redundant"] else u["tier"]
        mark = "※" if u["redundant"] else "  "
        # decorate は `累積 * 100 <= budget * total` で判定するので、
        # この Unit が残る最小の budget は ceil(累積 % )。
        need = -(-spent * 100 // total)
        print(
            f"| {rank:2} | {u['id']:4} | {eff:10}{mark}| {u['length']:4} | {spent:4} "
            f"| {pct:5.1f}% | budget >= {need:3} |"
        )
    print("\n※ = REDUNDANT（元 Tier から 1 段弱めた実効 Tier で並ぶ）")
    print(f"\n書き出し: {JSON_OUT}")


if __name__ == "__main__":
    main()
