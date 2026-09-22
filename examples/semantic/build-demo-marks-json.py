#!/usr/bin/env python3
"""examples/semantic/demo-marks.json を demo.json から生成する。

marks モード（`docs/design/marks-only-and-review-mode.md` 0 節）の fixture。
`--semantic-mode marks --semantic examples/semantic/demo-marks.json` で、
**Jev を 1 度も呼ばずに**つまみ・本数・入れ子・核の絞り込みを触れる。

    python3 examples/semantic/build-demo-marks-json.py

境界（Atom と Unit の切り方）と `source_sha256` は `demo.json` から**そのまま
借りる** — 2 つの fixture が同じ文書の同じ切り方を指していないと、片方を
直したときにもう片方が黙ってずれる。

**スコアは手で置いた値である。実測ではない。** 段 1
（`examples/semantic/measurements/marks-presets.md`）の分布の形
—「広い問いは高く平ら、狭い問いは 0 の近くに潰れて上だけが立つ」— を
なぞってあるだけで、この文書にその問いを実際に聞いた値ではない。fixture が
測っているのは**投影の側**（つまみ・足切り・核の絞り込み）であって、判定の
質ではない。
"""

from __future__ import annotations

import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent

#: Unit ごとのスコア（`demo.json` の u1..u13 の順）。
#:
#: 足切り 0.20 を**7 本が越え、6 本が越えない**ように置いてある。つまみを
#: 100 % まで上げても 7 本で止まるので、「N % を上げれば全部光る」ではなく
#: 「足切りが上限を作る」が fixture の上で見える。
#:
#: 0.22 と 0.18 を足切りの両側に 1 本ずつ置いてあるのは、境目が動いたときに
#: テストが気づくためである。
SCORES = [
    0.94,  # u1 看板（見出し + 主題の一文）
    0.72,  # u2 結論
    0.55,  # u3 背景の締め
    0.38,  # u4 制約
    0.31,  # u5 影響範囲
    0.26,  # u6 補足
    0.22,  # u7 足切りのすぐ上
    0.18,  # u8 足切りのすぐ下
    0.12,  # u9
    0.09,  # u10
    0.06,  # u11
    0.04,  # u12
    0.02,  # u13
]

#: この fixture が答えている問い。ステータス行に出る。
QUESTION = "settled"


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
    dim = json.loads((HERE / "demo.json").read_text(encoding="utf-8"))
    units = dim["units"]
    if len(units) != len(SCORES):
        raise SystemExit(
            f"demo.json の Unit が {len(units)} 本、SCORES が {len(SCORES)} 本。"
            " demo.md を変えたなら SCORES も置き直すこと"
        )
    out = {
        "atoms": dim["atoms"],
        "units": [
            {
                "id": unit["id"],
                "atoms": unit["atoms"],
                # marks の答えは Tier を使わない。`detail` は安全側で、
                # この fixture を budget モードで開いても何も光らない。
                "reading_tier": "detail",
                "core_atoms": core_of(dim["atoms"], unit["atoms"]),
                "score": score,
                "relations": [],
            }
            for unit, score in zip(units, SCORES)
        ],
        "source_sha256": dim["source_sha256"],
        "question": QUESTION,
    }
    path = HERE / "demo-marks.json"
    path.write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    above = sum(1 for s in SCORES if s >= 0.20)
    total = len(units)
    print(f"{path.name}: {total} Unit、足切り 0.20 を越えるのは {above} 本")
    # 本数の数え方は `semantic_reading::marks::take_count` と同じ —
    # **N % は全 Unit に対して**で、足切りを越えた数で頭打ちになる。
    for share in (1, 10, 20, 50, 100):
        take = min(max(1, (total * share + 50) // 100), above)
        print(f"  MARK {share:3d}% -> {take} 本")


if __name__ == "__main__":
    main()
