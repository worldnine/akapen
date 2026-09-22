#!/usr/bin/env python3
"""examples/semantic/demo-marks.json を demo.md から生成する。

この層の fixture。`--semantic examples/semantic/demo-marks.json` で、
**Jev を 1 度も呼ばずに**つまみ・本数・入れ子・核の絞り込みを触れる。

    python3 examples/semantic/build-demo-marks-json.py

byte range は手で書かない。ここに書くのは「どの文を Atom にするか」「どの
Atom がどの Unit に属するか」「その Unit のスコアはいくつか」だけで、offset は
本文を検索して求める。demo.md を編集したらこれを走らせ直すこと
（`source_sha256` が変わるので、忘れると akapen が fixture を拒否する）。

**スコアは手で置いた値である。実測ではない。** 段 1
（`examples/semantic/measurements/marks-presets.md`）の分布の形
—「広い問いは高く平ら、狭い問いは 0 の近くに潰れて上だけが立つ」— を
なぞってあるだけで、この文書にその問いを実際に聞いた値ではない。fixture が
測っているのは**投影の側**（つまみ・足切り・核の絞り込み）であって、判定の
質ではない。

2026-09-22 までは `build-demo-json.py` が DIM 版の `demo.json` を作り、
この スクリプトがそこから境界を借りていた。DIM 版を削除したので 2 つを
1 本に畳んである。
"""

from __future__ import annotations

import hashlib
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
MD = HERE / "demo.md"
JSON_OUT = HERE / "demo-marks.json"

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


#: Unit ごとのスコア（`UNITS` の並び順）。
#:
#: 足切り 0.20 を**7 本が越え、6 本が越えない**ように置いてある。つまみを
#: 100 % まで上げても 7 本で止まるので、「N % を上げれば全部光る」ではなく
#: 「足切りが上限を作る」が fixture の上で見える。
#:
#: 0.22 と 0.18 を足切りの両側に 1 本ずつ置いてあるのは、境目が動いたときに
#: テストが気づくためである。
SCORES = [
    0.94,  # u1 看板（見出し + 主題の一文）
    0.72,  # u2 前置き
    0.55,  # u3 結論
    0.38,  # u4 付録への言及（結論と同じ行）
    0.31,  # u5 結論の理由
    0.26,  # u6 背景
    0.22,  # u7 足切りのすぐ上（制約）
    0.18,  # u8 足切りのすぐ下（影響範囲）
    0.12,  # u9 補足
    0.09,  # u10 念押し（補足と同じ行）
    0.06,  # u11 引用
    0.04,  # u12 数値の目安とコード
    0.02,  # u13 余談
]

#: Unit の並び（`ATOMS` の `unit` 欄が指す先）。
UNITS = [f"u{n}" for n in range(1, 14)]

#: この fixture が答えている問い。読み出しに出る。
QUESTION = "settled"


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


def core_of(atoms: list[dict], indices: list[int]) -> list[int]:
    """核の一文 — その Unit の最初の**見出しでない** Atom。

    見出しを核にすると、光るのが行の飾りだけになって中身が光らない。
    見出ししか無い Unit では、その見出しが核になる。
    """
    for index in indices:
        if atoms[index]["kind"] != "heading":
            return [index]
    return indices[:1]


def main() -> None:
    source = MD.read_text(encoding="utf-8")
    atoms = locate(source)

    ids = {a["unit"] for a in atoms}
    missing = ids - set(UNITS)
    if missing:
        raise SystemExit(f"Unit の定義がありません: {sorted(missing)}")
    empty = set(UNITS) - ids
    if empty:
        raise SystemExit(f"Atom を 1 つも持たない Unit: {sorted(empty)}")
    if len(UNITS) != len(SCORES):
        raise SystemExit(
            f"Unit が {len(UNITS)} 本、SCORES が {len(SCORES)} 本。"
            " demo.md を変えたなら SCORES も置き直すこと"
        )

    shaped = [
        {"range": {"start": a["start"], "end": a["end"]}, "kind": a["kind"]}
        for a in atoms
    ]
    units = []
    for uid, score in zip(UNITS, SCORES):
        indices = [i for i, a in enumerate(atoms) if a["unit"] == uid]
        units.append(
            {
                "id": uid,
                "atoms": indices,
                "core_atoms": core_of(shaped, indices),
                "score": score,
            }
        )
    doc = {
        "atoms": shaped,
        "units": units,
        "source_sha256": hashlib.sha256(source.encode()).hexdigest(),
        "question": QUESTION,
    }
    JSON_OUT.write_text(json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    above = sum(1 for s in SCORES if s >= 0.20)
    total = len(units)
    print(f"{MD.name}: {len(source.encode())} バイト / Atom {len(atoms)} / Unit {total}")
    print(f"{JSON_OUT.name}: 足切り 0.20 を越えるのは {above} 本")
    # 本数の数え方は `semantic_reading::marks::take_count` と同じ —
    # **N % は全 Unit に対して**で、足切りを越えた数で頭打ちになる。
    for share in (1, 10, 20, 50, 100):
        take = min(max(1, (total * share + 50) // 100), above)
        print(f"  {share:3d}% -> {take} 本")
    print(f"\n書き出し: {JSON_OUT}")


if __name__ == "__main__":
    main()
