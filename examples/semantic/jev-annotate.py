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

## ラウンド構成 — 3 つ、どれも 1 段

スコアの question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

    ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice }
                 → Unit を確定（**キャッシュに当たれば 0 問**）
    ラウンド2  state=文書全文, questions={ Unit ごとに、いまの問いへの Noul }
                 → 誰の核を聞くかが確定
    ラウンド3  state=文書全文, questions={ 足切りを超えた Unit の核(Choice) }
                 → 光る箇所が確定（**狭い問いではこのラウンドごと消える**）

Jev は question を**並列・独立に**評価するので、ラウンド 2 の時点では
「どの Unit が足切りを超えるか」をまだ誰も知らない。だから畳めない。

**どのラウンドも 1 段である**（設計書「Jev への問いは 1 段に保つ」）。前の
答えを次の問いの**前提に差し込む**連鎖は無い — ラウンド 3 が前の答えを使うのは
「どの Unit に聞くか」の絞り込みだけで、問いの文面は Unit の本文しか見ない。

**問いの文面は akapen が送ってくる。** 正本は akapen 側の
`assets/marks-questions.json` 1 か所で、このスクリプトは枠（`MARKS_FRAME`）と
本文を足すだけの汎用の器である。2 か所に置くとずれる。

akapen 側のプロトコルは 1 往復（atoms in / units out）のままで、3 ラウンドは
このスクリプトの内部事情である。

## context window は 2 つの制約で縛られている

公式値は「**64k tokens per request; 32k tokens for `state` plus the longest
question**」（`docs.typesafe.ai/models.md`）。**後者を見落とすと、合計が 64k に
収まっているのに 400 で落ちる** — 実測でも state 30k + question 3k（合計 33k）
が失敗した。核 question は Unit の全散文 Atom を選択肢として引用するので
ここに当たりうる。上限は [`RequestBudget`] が state の大きさから毎回計算する。

## 境界は「構造は聞かない。散文どうしだけ聞く」

設計書「Jevに判断させないもの: syntax parsing」のとおり、見出し・コードブロック
・リスト項目・引用が絡む境界は**パーサが既に知っている**。実測（demo.md の境界
26 件）でも、全部 Jev に聞くと質問を浪費したうえ誤りが増えた。

    全部 Jev に聞く（文面 v1）   20/26   質問 26
    全部 Jev に聞く（文面 v2）   17/26   質問 26
    構造ルール + 散文だけ v2     18/22   質問 13   ← この方針を採用

ローカルの構造ルールは [`boundary_rule`] にある。

## 鍵は環境変数、無ければ macOS のキーチェーン

`TYPESAFE_API_KEY` が先。**無ければ macOS のキーチェーン**
（`security find-generic-password -s typesafe-jev -w`）を見る。

**mac 固有の読み方をここに埋めてよい**（2026-09-22 の判断。以前の「埋めない」
を上書きした）。理由は**環境変数が普遍の逃げ道として先にあるから**である。
他 OS の人は `TYPESAFE_API_KEY` を export すれば済み、mac の分岐は
`sys.platform == "darwin"` の内側にしか無いので誰も縛らない。逆に埋めないと、
mac の人は akapen を起動するシェルの環境に鍵を持ち込む必要があり、
`export AKAPEN_SEMANTIC_CMD=…` を `~/.zshrc` に 1 行書くだけで
`akapen foo.md` が marks で開く、という形が作れない。

`op` は埋めない。**生体認証が毎回出る**（akapen は再解析のたびにこの
スクリプトを起動し直す）。`security` は無音で済む。それでも 1 プロセスの中で
何度も叩かないよう、[`api_key`] は取り出した鍵を 1 回だけ覚える。

鍵は stdout にも stderr にも出さない。`security` の出力も同じで、
**失敗したときの診断にも一文字も混ぜない**（stdout は鍵そのものである）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

VERSION = 1

#: `docs/jev.md` の SDK 定数に合わせる（`DEFAULT_BASE_URL` / `DEFAULT_MODEL`）。
DEFAULT_BASE_URL = "https://api.typesafe.ai"
DEFAULT_MODEL = "jev-latest"
API_PATH = "/v1/systemone"

#: 1 リクエストあたりのタイムアウト（秒）。**ラウンドやプロセス全体ではない。**
#:
#: Jev の SDK 既定は 10 秒（`docs/jev.md`）。**1 リクエストは実測でその 1/6 で
#: 済んでいる** —— 30 ラン / 680 リクエストの最遅が 1.72 秒で、20 秒には
#: **11.6 倍**の余裕がある（2026-09-22。
#: `examples/semantic/measurements/speed-and-limits.md` 第 2 版）。
#:
#: | 文書 | リクエスト | 1 req 最遅 |
#: | --- | ---: | ---: |
#: | 1,664 B | 12 | 0.76 秒 |
#: | 9,857 B | 19〜23 | 1.48 秒 |
#: | 18,250 B | 14〜16 | 1.31 秒 |
#: | 22,685 B | 27〜28 | 1.56 秒 |
#: | 35,021 B | 40〜41 | 1.64 秒 |
#:
#: ## 「3 × 20 = 60 でちょうど並ぶ」という理屈は捨てた
#:
#: かつてここには「3 ラウンドと Python の起動コストを akapen の 60 秒に
#: 収めるため」と書いてあった。**前提が 2 つとも崩れている:**
#:
#: - ラウンドは 3 本ではない。probe / boundary / tier / core /
#:   redundancy_gate / context1 / pick / context3 / pick2 の **9 種**あり、
#:   チャンクに割られてリクエストは 12〜41 本になる（**この数字は 2026-09-22 に
#:   削除した DIM 版の実測である**。いまは probe / boundary / marks / core の
#:   4 種で、境界がキャッシュに当たれば 2 種になる）
#: - akapen はもうプロセスを壁時計で殺さない。最後の出力からの**無音時間**で
#:   見ていて（`src/semantic.rs` の `COMMAND_IDLE_TIMEOUT` = 30 秒）、
#:   進捗が続くかぎり待つ。Python の起動コストは実測で 0.1〜0.2 秒しかなく、
#:   気にする量ではなかった
#:
#: ## いまの 20 秒の役目は「akapen より先に理由を言う」ことである
#:
#: **ここは akapen の無音の上限（30 秒）より短くなければならない。**
#: 固まったリクエストを 20 秒でこちらが諦めて stderr に理由を書けば、
#: 読み手はステータス行でそれを読める。逆にここが 30 秒を越えると、
#: akapen が先に子を kill して**理由が消える**。
#:
#: 大文書で先に当たるのは**時間ではなく context window** である
#: （[`http_error_message`]）。そこは 400 で即座に返るので、ここを延ばしても
#: 何も救われない。
DEFAULT_TIMEOUT = 20.0

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
#: 「重要な部分はどれか」とは聞かない。それだと「どれも重要」という答え方が
#: できてしまい、Unit を丸ごと光らせていた元の状態に戻る。**1 つへ強制的に
#: 倒す問い方**でなければならない。
#:
#: ## なぜ「損失ベース」なのか
#:
#: 旧文面は「このまとまりから **1 か所だけ**読むとしたら、どこを読めば要点が
#: 取れるか」だった。これだと
#:
#: - 後続をまとめている**導入文**（「方針は次のとおり。」のような、後ろを指すだけの文）
#: - その節の**主題そのもののラベル**
#:
#: が選ばれる。**Jev は問いに正しく答えている** — 「1 か所だけ読んで概要を
#: 掴む」なら導入文が正解である。悪いのは問いの方で、我々が欲しいのは
#: 「概要への入口」ではなく「**落とすと取り違える中身**」だった。
#:
#: だから損失ベースにする。設計書 `docs/design/semantic-reading-layer.md` が
#: 「Jev にさせる小さな意味判断」として挙げる例は**すべてこの形**である
#: （「ここを飛ばすと要点を失う？」「これは主要な主張を支えている？」）。
#: 定型「要点」の文面（`assets/marks-questions.json` の `essential`）も
#: 「落とすと文書の要点、結論、制約、未決の論点や宿題などを取り違える
#: 可能性が高い」という損失の言い方をしている。
#: 核は光る Unit の中をさらに同じ軸で絞る操作なので、**軸を揃えるのが筋**で
#: あって、ここだけ「1 か所だけ読むなら」という別の軸を混ぜる理由が無い。
#:
#: 損失ベースの文面は**前任者（`corequestion`）の実測で効いていた** — KEEP
#: 3/3、決定事項の Unit の核選択にも効いた。実装されなかったのは合格条件の
#: 置き方が誤っていたためで（落ちるべき項目がそもそもラウンド 3 に到達して
#: いなかったので、文面をどう変えても満たせなかった）、文面が否定されたわけ
#: ではない。**その実測は引き継いだ記録であって、ここで取り直したものではない。**
CORE_INSTRUCTIONS = (
    "次の選択肢は、この文書の中の連続した 1 つのまとまりを構成する各部分の"
    "本文である。このまとまりの中で、**これを読み飛ばすと要点を失う**のは"
    "どれか。"
)

#: 1 question に並べる選択肢の上限。これを超える Unit には核を聞かない
#: （核が空 = 絞り込み無し = Unit 全体が MARKED という従来の表示に戻る）。
#:
#: **この値では切れていない** — 45.6 KB の実文書でいちばん大きい Unit でも
#: Atom は 96 個だった。上限に当たる文書を測っていないので、当たったときの
#: 振る舞いを「安全側（従来どおり）」に倒してあるだけである。
#: 1 リクエスト全体のトークン上限。Jev の context window の**1 つ目の**制約。
#:
#: 公式値は「64k tokens per request」（`docs.typesafe.ai/models.md`）＝ 65,536。
#: 実測の切れ目は `usage.input_tokens` で 65,771 成功 / 65,874 失敗と公式値より
#: 約 235 上にあるが（`docs/gotchas/open-questions.md` 未解決 5）、
#: **分母には公式値を使う** —
#: budget の数え方が `usage` と違うようなので、実測の切れ目に寄せる理由が無い。
#:
#: **この制約は分割で外せる。** state を毎回送り直せば、question をいくつの
#: リクエストに分けても各 question の答えは変わらない（Jev は question を
#: 並列・独立に評価する。`docs/design/jev.md`）。外せないのは下の
#: [`STATE_PLUS_QUESTION_LIMIT`] のほうで、**そちらが本当の天井**である。
REQUEST_LIMIT = 65_536

#: Jev の context window の**2 つ目の**制約（`docs/gotchas.md`）。
#: 公式は「64k tokens per request; **32k tokens for `state` plus the longest
#: question**」で、後者は 1 つの question の大きさを直接縛る。実測でも
#: state 30k + question 3k（合計 33k）は 64k に収まっていながら失敗した。
#:
#: **こちらは分割しても外せない。** state はどのチャンクにも丸ごと乗るので、
#: リクエストを増やしても `state + その question` は小さくならない。
STATE_PLUS_QUESTION_LIMIT = 32_768

#: バイト数からトークン数を見積もる係数。**多めに出る側へ倒してある。**
#:
#: 実測（2026-09-21、4 文書の state）は 0.34〜0.39 tokens/byte、核 question の
#: 選択肢本文は 0.496 tokens/byte だった。上限の判定に使うので、**足りないより
#: 多く見積もる方が安全**である（見積もりが小さすぎると上限を超えた question を
#: 送って 400 で落ちる）。日本語と英語が混ざる文書で最も高かった 0.496 を丸めた。
TOKENS_PER_BYTE = 0.5

#: question 1 つあたりの器（型と criteria のキー名）の実測値。
#:
#: `docs/gotchas/open-questions.md` 未解決 5 の内訳表の
#: 「器（型と criteria のキー名）68」。
#: 残りの内訳（criteria の説明文 65 / instructions の枠組み文 65）は
#: [`question_tokens`] が本文と一緒に [`TOKENS_PER_BYTE`] で数えるので、
#: ここで二重に足さないこと。Tier question 全体の実測 181 に対して
#: [`question_tokens`] は 257 を返す（1.42 倍の過大評価）。定型文が日本語で、
#: 実レートが 0.376 tokens/byte しかないためで、**上限の判定には安全側**である。
QUESTION_OVERHEAD = 68

#: リクエスト全体の予算から引く安全マージン。内訳は [`CORE_QUESTION_MARGIN`]
#: と同じ（見積もり誤差と、公式値と実測の切れ目のずれ）。
REQUEST_MARGIN = 2_048

#: 核 question の予算から引く安全マージン。内訳:
#:
#: - question の定型文と criteria のキー名（実測で 1 question あたり 68 tokens、
#:   選択肢 1 つにつき数 tokens）
#: - [`TOKENS_PER_BYTE`] の見積もり誤差。state が大きいほど絶対値で効く
#: - 公式値 32k と実測の切れ目のずれ（state 20k + question 12k = 32,305 は
#:   通り、合計 35k は落ちた。境界は 32,768 付近だが厳密には詰めていない）
#:
#: 45.6 KB の実文書（state 実測 17,561 tokens）で最大の Unit は選択肢 82 個・
#: question 実測 5,469 tokens。この値でも予算 7,895 に収まる。
CORE_QUESTION_MARGIN = 2_048

#: state のトークン数を**実測しに行く**閾値（見積もりがこれを超えたら測る）。
#:
#: 32k 枠の半分。ここを下回っていれば、[`TOKENS_PER_BYTE`] が実測の 1.4 倍まで
#: 過大評価していても真値は 16k を超えず、1 つの question に 14k 以上が残る。
#: 実測でいちばん大きかった question は 5,469 tokens（45.6 KB の文書の 96 Atom
#: の Unit）なので、3 倍近い余裕がある。
#:
#: **超えたら見積もりでは判定できない。** 0.5 tokens/byte は 65 KB の文書の
#: state を 32,612 と見積もるが、実レート 0.34〜0.39 での真値は 22〜26k で、
#: 見積もりのままでは「question を 1 つも送れない文書」に見えてしまう。
#: そこだけ [`measure_state_tokens`] が 1 リクエスト使って実測する。
STATE_PROBE_THRESHOLD = STATE_PLUS_QUESTION_LIMIT // 2

#: state のトークン数を測るためだけの、いちばん小さい question。
#:
#: 答えは使わない。欲しいのは応答の `usage.input_tokens` だけである。
#: 本文を持たないので、`usage` はほぼ state そのもののトークン数になる
#: （器のぶんだけ多めに出る ＝ 安全側）。
PROBE_QUESTION = {
    "state-probe": {"type": "noul", "instructions": "この文書は日本語で書かれている。"}
}

#: 単独の Unit にする Atom 種別。中身は散文ではないので、隣の散文と読む優先度を
#: 共有しない。
#:
#: **`table` は表のヘッダ行**（＋区切り行）である。表の入口は必ずヘッダなので、
#: これが規則 3 に当たることで表は周囲から切れる。データ行（`table_row`）は
#: ここに入れない — 入れると表が行ごとに割れる（[`boundary_rule`] の規則 3）。
STANDALONE_KINDS = frozenset({"code_block", "table"})

#: 核（ラウンド 3 の選択肢）になれる Atom の種別 — **散文だけ**。
#: 見出し・コードブロック・テーブルは「1 か所だけ読むならどこか」の答えに
#: ならない（[`core_candidates`]）。
#:
#: **引用（`block_quote`）は散文なので入る。** 2026-09-22 まで外れていたが、
#: 外す理由（要点の言い換えではない）はコードと表の話で、引用には当たらない。
#: 引用だらけの文書では、引用を含む Unit の核が空になり**一度も光らなかった**
#: （marks では核の無い Unit は光らない）。
#: **表のデータ行（`table_row`）も入る**（2026-09-22）。表は 1 つの Unit の
#: ままだが、核はその中の 1 行まで下りる。ヘッダ行（`table`）は入れない —
#: 列の名前を挙げても中身を言ったことにならないので、見出しと同じ扱いである。
PROSE_KINDS = frozenset({"sentence", "list_item", "block_quote", "table_row"})


class JevError(Exception):
    """akapen のステータス行に 1 行で出したい失敗。"""


# ---------------------------------------------------------------------------
# 境界 — 構造ルールと、散文どうしだけの question
# ---------------------------------------------------------------------------


#: 行頭マーカー（`-` / `*` / `+` / `1.` / `3)`）を捕まえる正規表現。
#:
#: `crates/semantic-reading/src/atomize.rs` の `marker_len` と同じ規則である。
#: `atomize` は list item の Atom を**マーカーから**始めるので、この正規表現に
#: 当たるかどうかが「項目の 1 文目か、それとも継続文か」をそのまま分ける。
LIST_MARKER = re.compile(r"[-*+]|\d+[.)]")


def list_marker_indent(source: bytes, atom: dict) -> int | None:
    """list_item の Atom が**項目の先頭**なら、その行頭インデント幅を返す。

    返り値が `None` なら、その Atom は項目の先頭ではない — 同じ項目の 2 文目
    以降（継続文）か、そもそもマーカーが読めなかった場合である。

    **`range` はバイト位置なので `source` もバイト列で受ける。** Python の
    文字列添字は符号位置なので、日本語の文書では全 Atom がずれる（実測: 45.6 KB
    の文書で 372/372 件が不一致）。`docs/gotchas.md` にも書いた。

    判別は構造だけで行う。設計書「Jev に判断させないもの: **syntax parsing**」の
    とおり、ここに Jev は出てこない。手がかりは 2 つ:

    - `atomize` は項目の Atom を**マーカーから**始める（`- 一つ目。` /
      `1. 一文目。`）。継続文にはマーカーが無い（`二文目。`）
    - 行頭から Atom の先頭までが引用符と空白だけなら、その Atom は行頭に立って
      いる。途中から始まっていれば、それは同じ行の 2 文目である

    引用マーカーは**インデントとして数えない**。CommonMark の `>` は「`>` と、
    その直後の空白 1 つ」までが引用の印なので、そこまでを読み飛ばして数え直す。
    こうしないと `> - 一つ目` の指標が 1 になり、引用の中のリストが丸ごと
    「子項目」に見える。

    インデント幅は 0 か否かだけを見る（[`boundary_rule`] の規則 4）ので、タブ幅の
    厳密さは要らない。
    """
    start = atom["range"]["start"]
    line_start = source.rfind(b"\n", 0, start) + 1
    prefix = source[line_start:start].decode("utf-8", "replace")
    if prefix.strip(" \t>"):
        # 行の途中から始まっている = 同じ行の 2 文目。
        return None
    lead = prefix + (atom.get("text") or "")
    i = indent = 0
    while i < len(lead):
        char = lead[i]
        if char == ">":
            # 引用の印は `>` と直後の空白 1 つ。そこまではインデントではない。
            i += 1
            if i < len(lead) and lead[i] == " ":
                i += 1
            indent = 0
        elif char == " ":
            indent += 1
            i += 1
        elif char == "\t":
            indent += 4
            i += 1
        else:
            break
    return indent if LIST_MARKER.match(lead, i) else None


def boundary_rule(
    current_kind: str | None,
    next_kind: str | None,
    next_indent: int | None = None,
) -> tuple[str | None, str]:
    """隣り合う 2 つの Atom の境界を、構造だけで決められるなら決める。

    `next_indent` は後ろの Atom が項目の先頭なら [`list_marker_indent`] が返す
    インデント幅、そうでなければ `None`。規則 4 だけがこれを見る。

    返り値は `(SAME / NEW / None, 理由)`。`None` は「構造では決まらないので
    Jev に聞く」を意味する。

    設計書「Jevに判断させないもの: syntax parsing」に従い、**パーサが既に
    知っていることは聞かない**。実測でも、構造が絡む境界を Jev に聞くと質問を
    浪費したうえ誤りが増えた。

    規則は上から順に当てる（順序に意味がある）:

    1. 次が heading         -> NEW  見出しは必ず新しいまとまりを始める
    2. 現在が heading       -> SAME 見出しは直後の内容に付く
    3. どちらかが code_block / table -> NEW  単独の Unit にする
       （ただし**次が table_row なら SAME** — 表は行へ割れても 1 Unit）
    4. どちらも list_item   -> 別項目なら NEW / 同じ項目の中なら SAME
    5. どちらも sentence    -> None Jev に聞く
    6. それ以外             -> NEW  既定（引用と散文の間など）

    規則 1 と 2 の順序は「見出しの直前」が「見出しの直後」に勝つということで、
    これがないと見出しが前の段落に吸われる。

    **規則 2 が規則 3 に勝つ**ことも意図的で、`## 設定例` + コードブロックは
    1 つの Unit になる（見出しだけの Unit は単独では Tier を判定しづらい）。
    ただし **この組み合わせは測っていない** — demo.md に出てこない。

    ## 規則 2 は境界だけでは守りきれない — 続きは `section_of` にある

    規則 2 が防いでいるのは「見出しが中身から切り離されて、単独では読めない
    Unit になる」ことである。**規則 4 が入ってから、境界だけではこれを防げ
    なくなった。** 箇条書きを項目ごとに割ると、見出しの Unit は「見出し ＋
    せいぜい最初の項目」になり、2 つ目以降の項目は別 Unit として浮く。中身は
    残っているのに、それが何なのかを言う見出しのほうが沈む（2026-09-22 に
    削除した DIM 版での報告が 2 件）。

    **新しい規則を足して直してはいない。** 同じ意図を「中身が複数 Unit に
    なった場合」について言い直したのが [`assign_sections`] の `section_of`
    である。**いまそれを読む人は居ない** — マーカーは問いが選ぶので、見出しを
    構造で戻す必要が無い。**規則 2 とそれは 1 つのことである** — 似た規則が
    2 つあると読んで、片方だけ直したり統合したりしないこと。

    ## 規則 3 — 表は行へ割れても 1 つの Unit のまま

    `atomize` は 2026-09-22 から表を行ごとの Atom へ割る（ヘッダ行 ＋ 区切り行
    が `table`、データ行が `table_row`）。**それでも表は 1 つの Unit である。**
    表は 1 つのまとまりで、行に下りるのは核だけ（[`core_candidates`]）である。

    そのために「**次が `table_row` なら SAME**」を規則 3 の先に置く。表の入口は
    必ずヘッダ行（`table`）なので、表の手前の境界は規則 3 の standalone に当たって
    NEW になり、表の中だけがつながる。

        本文。           ← ここで NEW（次が table = standalone）
        | 列 a | 列 b |  ← 表の枕
        | --- | --- |
        | 値 1 | 値 2 |  ← SAME（次が table_row）
        | 値 3 | 値 4 |  ← SAME
        後文。           ← ここで NEW（規則 6 の既定）

    **「両方が表の種別なら SAME」と書いてはいけない。** 空行だけを挟んで表が
    2 つ並ぶと、後ろの表のヘッダも「表の種別」なので 2 つの表が 1 Unit へ融合する。
    見るのは**次**だけである。

    ## 規則 4 — 箇条書きは項目ごとに割る。ただし**トップレベルだけ**

    以前はリスト項目どうしを一律 SAME にしていた。それだと箇条書きが丸ごと
    1 Unit になり、**10 個の決定事項が全部同じ Tier**になる。しょうもない
    決定事項が個別に沈めない。

    いま割るのは**トップレベルの項目の切れ目だけ**である。

        - 決定A          ← ここで NEW
          - 詳細A1       ← 親に SAME（インデント > 0）
          - 詳細A2       ← 親に SAME
        - 決定B          ← ここで NEW（インデント 0）
          二文目。       ← 同じ項目の中なので SAME（マーカーが無い）

    子項目を割らないのは、ユーザーの言う「決定事項」が**親項目＋その詳細の
    束**だからである。そこで割ると、下の「半分だけ DIM のリスト」が親と子の
    あいだで起きる。

    規則 6 の引用の扱い（block_quote どうしは NEW になる）は**測っていない**。
    判断の根拠は、引用は自己完結した挿入で、「引用した」こと自体が周囲の散文と
    読む優先度が違うという著者の表明である、というもの。連続する引用は別々の
    引用なので規則 6 で NEW になる。

    **半分だけ DIM のリストは、依然として起こしてはいけない。** いま起こらない
    のは、割る単位を「親 + その子 + 継続文」に揃えているからで、規則 4 を
    さらに細かくするなら、まずそこを測ること。
    """
    if next_kind == "heading":
        return NEW, "rule:next_is_heading"
    if current_kind == "heading":
        return SAME, "rule:current_is_heading"
    if next_kind == "table_row":
        return SAME, "rule:same_table"
    if current_kind in STANDALONE_KINDS or next_kind in STANDALONE_KINDS:
        return NEW, "rule:standalone_block"
    if current_kind == "list_item" and next_kind == "list_item":
        if next_indent == 0:
            return NEW, "rule:new_list_item"
        if next_indent is None:
            return SAME, "rule:same_list_item"
        return SAME, "rule:nested_list_item"
    if current_kind == "sentence" and next_kind == "sentence":
        return None, "jev"
    return NEW, "rule:default"


def atom_text(atom: dict) -> str:
    """Atom の本文。前後の空白は落とす（末尾の改行を含む Atom がある）。"""
    return (atom.get("text") or "").strip()


def plan_boundaries(atoms: list[dict], source: str) -> list[dict]:
    """すべての境界について、構造で決まったか Jev に聞くかを並べる。

    要素は `{"after_atom": i, "decision": SAME/NEW/None, "by": 理由}`。
    `decision` が `None` のものだけがラウンド 1 の question になる。

    `source` が要るのは規則 4 のためだけである — 項目のインデントは Atom の
    `range` の**手前**（行頭からマーカーまで）にあるので、Atom だけでは読めない。
    `range` はバイト位置なので、ここで 1 度だけバイト列にして渡す。
    """
    raw = source.encode()
    plan = []
    for i in range(len(atoms) - 1):
        following = atoms[i + 1]
        decision, why = boundary_rule(
            atoms[i].get("kind"),
            following.get("kind"),
            list_marker_indent(raw, following)
            if following.get("kind") == "list_item"
            else None,
        )
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


# ---------------------------------------------------------------------------
# 答えの取り出し（緩めない）
# ---------------------------------------------------------------------------


def answer_of(answers: dict, key: str) -> dict:
    answer = answers.get(key)
    if not isinstance(answer, dict):
        raise JevError(f"Jev answered nothing for {key}")
    return answer


def choice_of(answers: dict, key: str, criteria: dict) -> str:
    got = answer_of(answers, key).get("choice")
    if not isinstance(got, str) or got not in criteria:
        raise JevError(f"Jev returned an unknown choice for {key}: {got!r}")
    return got


def confidence_of(answers: dict, key: str) -> float | None:
    """`confidence` は**無くてよい**ので、答えが丸ごと無いときも None を返す。

    `choice` / `noul` と違ってここで黙って埋めているわけではない — 記録用の
    付加情報で、判定には使っていない（`docs/gotchas/open-questions.md`
    未解決 3）。送れなかった question の Unit もこの経路を通るので、
    落とさないこと。
    """
    answer = answers.get(key)
    if not isinstance(answer, dict):
        return None
    got = answer.get("confidence")
    return float(got) if isinstance(got, (int, float)) else None


def noul_of(answers: dict, key: str) -> float:
    got = answer_of(answers, key).get("noul")
    if not isinstance(got, (int, float)):
        raise JevError(f"Jev returned no noul for {key}: {got!r}")
    return float(got)


# ---------------------------------------------------------------------------
# Unit の組み立て
# ---------------------------------------------------------------------------


#: setext 見出しの下線（`=====` / `-----`）。深さはこれで決まる。
SETEXT_UNDERLINE = re.compile(r"^(=+|-+)$")


def heading_level(atom: dict) -> int | None:
    """見出し Atom の深さ（`#` の数）。見出しでなければ `None`。

    **構造だけで決まる。** 設計書「Jev に判断させないもの: syntax parsing」の
    とおり、ここに Jev は出てこない。

    `atomize` は見出しの Atom を**マーカーから**始めるので（`## 節`）、
    `#` を数えれば深さになる。setext（`見出し` + `=====`）は `#` を持たない
    ので、下線の種類で 1 / 2 に落とす。

    引用の中の見出し（`> ## 節`）も `atomize` は `heading` にする。ここでも
    見出しとして扱い、`>` は深さに数えない。**引用された文書が `#` で
    始まっていると、そこで外側の節が閉じる** — 測った 4 文書の見出し 81 件に
    引用の中のものは 1 件も無かったので、直していない。

    **setext も実文書では出ていない**（同じ 81 件で 0 件）。下の分岐は
    `test_jev_annotate.py` の作り物でしか通っていない。

    深さが読めない見出しは**いちばん深い 6** に倒す。浅い側へ倒すと外側の節を
    誤って閉じるが、深い側なら「現在の節の下に小さい節ができる」だけで済み、
    次の本物の見出しがそれを閉じる。
    """
    if atom.get("kind") != "heading":
        return None
    text = atom.get("text") or ""
    lead = text.lstrip(" \t>")
    if lead.startswith("#"):
        return min(len(lead) - len(lead.lstrip("#")), 6)
    lines = [line.strip(" \t>") for line in text.splitlines()]
    if len(lines) >= 2 and SETEXT_UNDERLINE.match(lines[-1]):
        return 1 if lines[-1].startswith("=") else 2
    return 6


def assign_sections(atoms: list[dict], units: list[list[int]], built: list[dict]) -> None:
    """各 Unit に、自分が属する節の見出し Unit（`section_of`）を書き込む。

    **構文から決まる値である**（設計書「Jev に判断させないもの: syntax
    parsing」）。Jev は 1 度も出てこない。

    **いまこれを読む人は居ない。** マーカーは問いが選ぶので、節の見出しを
    構造で戻す必要が無い（2026-09-22 に DIM 版と一緒にその仕組みを削除した）。
    それでも書いているのは、プロトコルが持つフィールドで、判定を眺める人が
    「どの節の話か」を追えるからである。

    **見出し Unit 自身も `section_of` を持つ**。値は**親の節**の見出し Unit
    で、こうしておくと入れ子が属性だけで伝わる（`### 費用` が戻れば
    `## 決定事項` が戻り、それが `#` を戻す）。crate 側は `#` の数を知らずに
    済む。

    節の外（最初の見出しより前の前書き）は `section_of` を持たない。
    """
    stack: list[tuple[int, str]] = []
    for indices, unit in zip(units, built):
        level = next(
            (
                depth
                for index in indices
                if (depth := heading_level(atoms[index])) is not None
            ),
            None,
        )
        if level is not None:
            # 同じ深さ以浅の節はここで閉じる。
            while stack and stack[-1][0] >= level:
                stack.pop()
        if stack:
            unit["section_of"] = stack[-1][1]
        if level is not None:
            stack.append((level, unit["id"]))


def wants_core(unit: dict) -> bool:
    """この Unit に核を聞く意味があるか — **足切りを越えたなら聞く。**

    核は「Unit の中で落とすと取り違える部分」で、つまみには依存しない
    （つまみはこのスクリプトから見えないし、見る必要もない）。越えなかった
    Unit は光らないので、核を聞いても答えが画面に出ない。

    旗は [`marks_annotate`] が立てる（`score >= core_floor`）。**`reading_tier`
    を内部の運び屋に使っていたのを 2026-09-22 にやめた** — フィールドが
    ワイヤから消えたのに、判定器の中だけで生き残っているのは読み違えのもと
    である。
    """
    return bool(unit.get("wants_core"))


def core_candidates(atoms: list[dict], unit: dict) -> dict:
    """核の選択肢。**散文の Atom だけ**を候補にする。

    `heading` / `code_block` / `table` は「このまとまりから 1 か所だけ読むなら
    どこか」の答えにならない。見出しは中身を持たないし、コード例や表は要点の
    言い換えではない。種別は構文の話なので、設計書「Jev に判断させないもの:
    **syntax parsing**」のとおりここで落とす — 実測でも、候補に見出しがあると
    Jev は見出しを選んだ（demo.md の `## 結論` / `## 制約`）。

    **引用（`block_quote`）は落とさない。** 引用は散文で、要点そのものを
    述べていることがある。落としていたのは上の規則に巻き込まれた事故で、
    引用の多い文書では核が空のまま Unit が一度も光らなかった。

    本文が空の Atom は選択肢にしない（選ばれても光らせる中身が無い）。
    """
    return {
        f"atom:{i}": atom_text(atoms[i])
        for i in unit["atoms"]
        if atom_text(atoms[i]) and atoms[i].get("kind") in PROSE_KINDS
    }


def core_fits(options: dict, budget: RequestBudget) -> bool:
    """この選択肢の集合が 1 question として送れるか（見積もり）。

    見るのは `pair`（32k の側）である。**`whole` ではない** — 核 question が
    大きすぎて落ちるのは「1 リクエストに何個並ぶか」の問題ではなく、
    「`state` とこの question の 2 つだけで 32k を超える」問題だからで、
    そこは分割で救えない（[`RequestBudget`]）。1 リクエストに並べきれない
    ぶんは [`plan_chunks`] が別のリクエストへ回す。
    """
    question = {"type": "choice", "instructions": CORE_INSTRUCTIONS, "criteria": options}
    return question_tokens(question) <= budget.pair


def assign_lone_cores(atoms: list[dict], units: list[dict]) -> None:
    """候補が 1 つしかない Unit の核を、聞かずに決める。

    絞り込んだ結果 1 つになった場合に**聞かないだけ**だと、核が空のまま
    Unit 全体（見出しを含む）が MARKED に戻ってしまう。候補が 1 つなら答えは
    決まっているので、question を使わずにそれを核にする。

    実測ではこれで核の question が 33〜89% 減った（design.md は 19 Unit 中
    17 が聞かずに決まった）。

    """
    for unit in units:
        if not wants_core(unit):
            continue
        options = core_candidates(atoms, unit)
        if len(options) == 1:
            index = int(next(iter(options)).split(":")[1])
            unit["core_atoms"] = [index]
            unit["jev"]["core_choice"] = f"atom:{index}"
            unit["jev"]["core_by"] = "rule:only_prose_atom"


def core_questions(atoms: list[dict], units: list[dict], budget: RequestBudget) -> dict:
    """ラウンド 3 の questions のうち核の分。

    選択肢は Unit を構成する散文 Atom の本文そのもので、キーは `atom:<index>`。

    **候補が 1 つの Unit には聞かない** — 答えが決まっているので
    [`assign_lone_cores`] が先に埋めている。**散文が 1 つも無い Unit にも
    聞かない**。このとき核は空のままで Unit 全体が MARKED になるが、実測では
    4 文書のいずれにも「MARKED になる Atom 2 個以上の Unit で散文が 0 個」は
    無かった。

    **予算を超える Unit にも聞かない**（[`core_fits`]）。核が無ければ Unit
    全体が MARKED になる — 絞り込めないだけで、注釈としては壊れない安全側の
    振る舞いである。
    """
    questions = {}
    for unit in units:
        if not wants_core(unit):
            continue
        options = core_candidates(atoms, unit)
        if len(options) < 2 or not core_fits(options, budget):
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


def stderr_prefix() -> str:
    """直に走らせたときだけ `jev-annotate:` と名乗る。

    akapen 経由では stderr はパイプで、受けた側（`src/export.rs` の
    `run_child`）が `(--semantic-cmd exited non-zero)` を添えるので、ここでも
    名乗ると**接頭辞が 2 段になる**。ステータス行の幅は端末しだいで、接頭辞に
    食われたぶんだけ肝心の一文が枠の外へ出る。人が直接シェルで走らせたときは
    どのコマンドの声か分からないと困るので、そのときだけ名乗る。

    `isatty` は閉じた stream や差し替えられた stream で例外を投げうるので、
    迷ったら**名乗らない**側に倒す（akapen 経由のほうが多いため）。
    """
    try:
        return "jev-annotate: " if sys.stderr.isatty() else ""
    except (AttributeError, ValueError):
        return ""


#: これまでに投げたリクエストの本数（[`progress`] が数える）。
_requests_sent = 0


def progress(questions: int, elapsed: float) -> None:
    """1 リクエスト終わるたびに stderr へ 1 行出す。**akapen への生存信号。**

    akapen は `--semantic-cmd` の子を**壁時計では殺さない**。最後に何か
    言ってからの無音時間で見ていて、進捗が続くかぎり待つ
    （`src/export.rs` の `Deadline::WhileProgressing`、上限は
    `src/semantic.rs` の `COMMAND_IDLE_TIMEOUT` = 30 秒）。
    **この行が出ないと、その仕組みは働かない** —— 何も言わない子は
    固まった子と区別がつかず、無音の上限がそのまま全予算になる。

    ## 1 リクエストごとに出せば足りる

    リクエストの最中は黙るが、そこは [`DEFAULT_TIMEOUT`]（20 秒）が
    受け持つ。固まったリクエストは 20 秒でこちらが諦めて**理由を書く**ので、
    akapen の 30 秒より先に必ず声が出る。この順序が崩れると、子が自分で
    報告できたはずの障害が kill に潰される（`COMMAND_IDLE_TIMEOUT` の
    コメントが同じことを反対側から書いている）。

    ## 置き場所は [`ask_jev`] であって [`send_in_chunks`] ではない

    進捗は「リクエストを 1 本投げ終えた」という事実で、分割の都合とは別の
    話である。`send_in_chunks` に数え上げを持たせると、probe（分割を通らない
    1 本）が漏れるうえ、分割の関心に無関係なものが混ざる。

    ## 失敗しても解析は止めない

    stderr が閉じている・差し替えられている場合に例外を投げうるので、
    **黙って諦める**。進捗行が出ないと akapen の猶予は縮むが、
    解析そのものは壊れない。
    """
    global _requests_sent
    _requests_sent += 1
    try:
        print(
            f"{stderr_prefix()}request {_requests_sent}: "
            f"{questions} questions in {elapsed:.2f}s",
            file=sys.stderr,
            flush=True,
        )
    except (AttributeError, ValueError, OSError):
        pass


#: キーチェーンに鍵を入れてあるサービス名（macOS）。
#:
#: 保存は `security add-generic-password -a "$USER" -s typesafe-jev -w` の 1 回
#: だけで、**保存形式の説明は要らない**（そういうものだから）。
KEYCHAIN_SERVICE = "typesafe-jev"

#: [`api_key`] が一度取り出した鍵。**ここから先へ出さない。**
#:
#: [`ask_jev`] が 1 リクエストごとに [`api_key`] を呼ぶので、覚えないと
#: 1 ラン（実測で 20 前後のリクエスト）のあいだ `security` を叩き続ける。
_api_key: str | None = None


def keychain_key() -> str:
    """macOS のキーチェーンから鍵を読む。取れなければ空文字。

    **取れない理由は言い分けない。** `security` が無い（他 OS・PATH に無い）、
    項目が無い（終了コード非 0）、キーチェーンがロックされていてダイアログ待ち
    （`timeout`）のどれでも「無い」で同じで、呼んだ側は次の手段へ進むだけである。

    **`security` の出力を戻り値以外のどこにも渡さない。** stdout は鍵そのもの
    なので、例外に添える・ログに出すといった普通の親切がそのまま漏洩になる。
    """
    if sys.platform != "darwin":
        return ""
    try:
        proc = subprocess.run(
            ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    if proc.returncode != 0:
        return ""
    return proc.stdout.strip()


def api_key() -> str:
    """`TYPESAFE_API_KEY`、無ければ macOS のキーチェーン。値は絶対に出力しない。

    優先順は **環境変数が先**である。mac 固有の読み方を埋めてよいのは、
    この普遍の逃げ道が先にあるからで（冒頭の docstring）、環境変数が立って
    いるときはキーチェーンを見にいかない。

    どちらも無ければ止まる。エラーの一文には**保存のしかたも入れる** ——
    [`main`] が [`one_line`] で改行を潰すので、akapen のステータス行に届くのは
    1 行だけであり、「環境変数が無い」と「どう保存するか」が別の行になれない。
    """
    global _api_key
    if _api_key is not None:
        return _api_key
    key = os.environ.get("TYPESAFE_API_KEY", "").strip() or keychain_key()
    if not key:
        raise JevError(
            "TYPESAFE_API_KEY is not set. Export it, or on macOS save it once: "
            'security add-generic-password -a "$USER" -s typesafe-jev -w'
        )
    _api_key = key
    return key


def http_error_message(code: int, detail: str) -> str:
    """HTTP エラーを、ステータス行に出して意味が通る 1 行にする。

    `max_tokens_exceeded` はこの経路でいちばん現実的な失敗なので、生の JSON
    ではなく原因と対処を出す。

    **「文書を分けてください」とはもう言わない。** リクエストの分割は
    [`send_in_chunks`] が自動でやるので、ここまで来たということは
    **`state` だけで 32k 枠を使い切っている**ということである。`state` は
    どのチャンクにも丸ごと乗るので、**分割では外せない**
    （`docs/gotchas/open-questions.md` 未解決 5 の 2 つ目の制約）。
    文書そのものを小さくするしかない。

    `docs/design/jev.md` は「context window は需要に応じて変わりうる」と書いて
    いるので、**数値を断定せず実測値として**出す。ここで切れるのは時間ではなく
    大きさなので、akapen 側のタイムアウトを延ばしても何も直らない。
    """
    if "max_tokens_exceeded" in detail:
        return (
            "Document too large for Jev — shrink it. Splitting is automatic, "
            "so the text alone (state) fills the 32k-token window. Not a "
            "timeout: waiting will not help."
        )
    return f"Jev returned HTTP {code}: {detail}"


def ask_jev(state: str, questions: dict, model: str, timeout: float) -> dict:
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
        raise JevError(http_error_message(e.code, detail)) from e
    except urllib.error.URLError as e:
        raise JevError(f"cannot reach Jev: {one_line(str(e.reason))}") from e
    except json.JSONDecodeError as e:
        raise JevError(f"Jev's answer is not JSON: {one_line(str(e))}") from e
    except TimeoutError as e:
        raise JevError(f"Jev did not answer within {timeout}s") from e
    if not isinstance(payload.get("answers"), dict):
        raise JevError("Jev's answer has no answers")
    payload["elapsed_s"] = round(time.monotonic() - started, 3)
    # akapen への生存信号。**`return` の前に出す** —— 呼び出し側が答えを
    # 使い終わるまで黙っていると、そのぶん無音が伸びる（[`progress`]）。
    progress(len(questions), payload["elapsed_s"])
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
# 予算とリクエスト分割
#
# **ここはどのラウンドからも使う。** ラウンド 2 専用にしないこと — ラウンド 1
# （境界）もラウンド 3（redundancy と核）も同じ崖を持っている。実測の見積もり
# では 58 KB の `examples/semantic/README.md` は 3 ラウンドとも天井の 2 倍を
# 超える。1 つのラウンドだけ直すと、次の文書で別のラウンドが落ちる。
# ---------------------------------------------------------------------------


def estimate_tokens(text: str) -> int:
    """バイト数からトークン数を見積もる（[`TOKENS_PER_BYTE`]）。"""
    return int(len(text.encode()) * TOKENS_PER_BYTE)


def question_tokens(question: dict) -> int:
    """question 1 つのトークン数の見積もり。

    器（[`QUESTION_OVERHEAD`]）＋ instructions と criteria のテキスト。
    criteria の**キーも数える** — `atom:123` のようなキーは選択肢の個数だけ
    並ぶので、選択肢が多い核 question では無視できない。

    **個数で切らずにこれで切る。** 同じ 10 個の question でも、`demo.md` の
    Unit なら 2,000 tokens、45 KB の文書の大きな Unit なら 20,000 tokens に
    なる。`docs/gotchas/open-questions.md`「核の question は個数ではなく
    トークンで切る」と同じ理由で、固定の個数はどの文書でも正しくない。
    """
    text = question.get("instructions") or ""
    for key, value in (question.get("criteria") or {}).items():
        text += key + value
    return QUESTION_OVERHEAD + estimate_tokens(text)


class RequestBudget:
    """1 文書ぶんの予算。**2 つの制約を 1 か所で持つ。**

    - `whole` … 1 リクエスト全体に使える question の合計（64k の側）。
      **分割で外せる**ので、これはチャンクの切れ目を決めるためだけに使う
    - `pair` … `state` + question 1 つに使える question 1 つ分（32k の側）。
      **分割しても外せない。** これを超える question は、どう分けても送れない

    `state_tokens` の出どころは `measured`（`usage.input_tokens` を読んだ）か
    `estimate`（[`estimate_tokens`]）。報告に効くので捨てずに持つ。
    """

    def __init__(self, state_tokens: int, measured: bool = False) -> None:
        self.state_tokens = state_tokens
        self.measured = measured
        #: 実測に使ったリクエストの記録（測らなかったときは None）。
        #: **報告で数えられるように残す** — これを `rounds` に載せないと、
        #: リクエスト数と input tokens の合計が食い違う（probe のぶんだけ
        #: 少なく出る。実測で 7 % ずれた）。
        self.probe: dict | None = None
        self.whole = REQUEST_LIMIT - REQUEST_MARGIN - state_tokens
        self.pair = STATE_PLUS_QUESTION_LIMIT - CORE_QUESTION_MARGIN - state_tokens

    @classmethod
    def estimated(cls, state: str) -> "RequestBudget":
        """API を叩かずに見積もりだけで作る（`--dry-run` とテスト用）。"""
        return cls(estimate_tokens(state), measured=False)

    def record(self) -> dict:
        return {
            "state_tokens": self.state_tokens,
            "state_tokens_by": "usage" if self.measured else "estimate",
            "whole": self.whole,
            "pair": self.pair,
        }


def measure_state_tokens(state: str, model: str, timeout: float) -> RequestBudget:
    """この文書の `state` のトークン数を決める。

    見積もりが [`STATE_PROBE_THRESHOLD`] 以下なら、そのまま使って**測らない**
    （小さい文書に余分なリクエストを課金しない）。超えたら
    [`PROBE_QUESTION`] だけを付けた 1 リクエストを投げ、`usage.input_tokens`
    を読む。

    **見積もりのままでは 32k 側の判定が壊れる。** 0.5 tokens/byte は 65 KB の
    文書を 32,612 tokens と見積もるが、実レートでの真値は 22〜26k である。
    見積もりを信じると「question を 1 つも送れない」と誤って結論し、**分割で
    救えるはずの文書を落とす**。32k の側は分割で外せない本当の天井なので、
    ここだけは実測が要る。

    probe 自体が 400 で落ちるなら、`state` だけで 32k 枠を使い切っている
    ということで、**その文書は分割しても通らない**。[`http_error_message`] が
    そう言う。
    """
    estimate = estimate_tokens(state)
    if estimate <= STATE_PROBE_THRESHOLD:
        return RequestBudget(estimate, measured=False)
    payload = ask_jev(state, dict(PROBE_QUESTION), model, timeout)
    used = (payload.get("usage") or {}).get("input_tokens")
    record = round_record(payload, len(PROBE_QUESTION))
    record["round"] = "probe"
    if not isinstance(used, (int, float)):
        # usage を返さない相手でも止まらない。見積もりへ戻すだけ。
        budget = RequestBudget(estimate, measured=False)
    else:
        budget = RequestBudget(int(used), measured=True)
    budget.probe = record
    return budget


def plan_chunks(questions: dict, budget: RequestBudget) -> tuple[list[dict], list[str]]:
    """questions を「1 リクエストに収まる塊」の列へ分ける。

    返り値は `(チャンクの列, 送れなかった question のキー)`。

    切り方は**入力の順のまま**の貪欲詰めである。並べ替えない —
    question のキーは Unit や境界の番号を持っていて、順序が変わると
    デバッグのとき突き合わせられなくなる。答えはキーで戻すので、分け方が
    答えを変えることはない。

    **`pair` を超える question は誰にも送れない。** 分割はリクエストの数を
    増やすだけで `state` を小さくしないので、`state + その question` が 32k を
    超える question は、チャンクを 1 つにしても救えない。呼び手がそれぞれの
    落とし先を決める（[`annotate`] を見ること）。
    """
    chunks: list[dict] = []
    dropped: list[str] = []
    current: dict = {}
    spent = 0
    for key, question in questions.items():
        cost = question_tokens(question)
        if cost > budget.pair:
            dropped.append(key)
            continue
        if current and spent + cost > budget.whole:
            chunks.append(current)
            current, spent = {}, 0
        current[key] = question
        spent += cost
    if current:
        chunks.append(current)
    return chunks, dropped


def send_in_chunks(
    state: str,
    questions: dict,
    budget: RequestBudget,
    model: str,
    timeout: float,
) -> tuple[dict, list[dict], list[str]]:
    """questions を予算に収まるリクエストへ分けて送り、答えをマージする。

    返り値は `(answers, リクエストごとの記録, 送れなかったキー)`。

    ## なぜ分けてよいのか

    Jev は question を**同じ state に対して並列かつ独立に**評価する
    （`docs/design/jev.md`）。LLM のような文脈の持ち越しが無いので、同じ
    state に対して question を複数のリクエストへ分けても、各 question の答えは
    変わらない**はず**である。**「はず」なので測ってある** —
    `examples/semantic/measurements/request-splitting.md` に、分割版と
    非分割版の Tier 一致率を揺れの床と並べて置いた。

    ## state は毎回丸ごと送る

    織り込み済みのコストである。分割で増えるのは `(チャンク数 − 1) × state`
    ちょうどで、それ以外は増えない。`state` を切り詰めて安くする案は採らない
    — 設計書が「全文を Context として扱う」と決めていて、それは分割の話では
    なく設計の話である。

    ## 取りこぼさない

    マージで、**同じキーが 2 つのチャンクから返ってきたら失敗させる**。
    答えが足りないほうは各ラウンドの [`choice_of`] / [`noul_of`] が捕まえる
    （どちらも黙って埋めない）。
    """
    answers: dict = {}
    records: list[dict] = []
    chunks, dropped = plan_chunks(questions, budget)
    for chunk in chunks:
        payload = ask_jev(state, chunk, model, timeout)
        for key, answer in payload["answers"].items():
            if key in answers:
                raise JevError(f"{key} was answered by two chunks")
            answers[key] = answer
        records.append(round_record(payload, len(chunk)))
    return answers, records, dropped


# ---------------------------------------------------------------------------
# 入り口
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# marks モード — 問いに答えている箇所だけを光らせる
# （`docs/design/marks-only-and-review-mode.md` 0 節）
# ---------------------------------------------------------------------------


#: 問いの文面に付ける枠。**全問共通**で、段 1 の実測
#: （`examples/semantic/measurements/marks-presets.md`）と 1 バイトも違わない。
#: 文面そのものは akapen が送ってくる（正本は akapen 側の
#: `assets/marks-questions.json`）。枠だけがここにあるのは、`{body}` を持って
#: いるのが判定器だからである。
MARKS_FRAME = "\n\n――― 対象 ―――\n{body}\n―――――――――"

#: 境界のキャッシュの置き場（`~/.cache/akapen/semantic/boundaries/v1/`）。
#:
#: **問いを変えるたびに境界を取り直さないため**にある。akapen 側のキャッシュは
#: (コマンド行, 文書, 問い) で引くので、問いが変わればこのプロセスがもう一度
#: 起きる — そのとき境界のラウンドまで回し直すと、設計書 0 節の「境界を 1 回
#: 取る（キャッシュ）」が成り立たない。
#:
#: **中身に本文は入らない。** Atom の添字と境界の判定（`same_unit` /
#: `new_unit`）と理由だけである。それでも節の切れ目は業務文書を語るので、
#: `docs/gotchas/public-repo.md` の扱いに合わせてホームの下・0600/0700 に置く。
BOUNDARY_CACHE_VERSION = 1


def atoms_fingerprint(atoms: list[dict]) -> str:
    """Atom 列の指紋 — `(kind, start, end)` の並びの sha256。

    **境界のキャッシュは Atom の添字で持っている。** `atomize` が変われば
    添字は全部ずれるので、文書が同じでも古い判定を当ててはいけない。当てると
    節の切れ目が 1 つずつずれた、**それらしく見えて間違った注釈**になる。

    件数の一致（[`load_boundaries`]）だけでは足りない。`atomize` の変更が
    その文書で Atom 数を変えないことがある（2026-09-22 の表の行割りでも、
    データ行が 1 行の表は 1 → 1 のままである）。そのとき長さは合い、位置だけが
    ずれる。

    **鍵ではなく payload に入れて照合する。** 版のディレクトリを手で上げる方式に
    しなかったのは、次に `atomize` を触る人が上げ忘れたら同じ事故が戻るからで、
    指紋なら誰も憶えていなくても自動で外れる。指紋を持たない古い項目は
    [`load_boundaries`] が外す。

    **本文は入れない。** 種別とバイト位置だけである（キャッシュの中身の約束を
    変えない）。`range` は形を決め打ちせず JSON のまま混ぜる — ワイヤの形は
    `{"start": …, "end": …}` だが、ここで読み違えても外れが増えるだけで済む
    ようにしておく。
    """
    material = "\n".join(
        f"{atom.get('kind')}:{json.dumps(atom.get('range'), sort_keys=True)}"
        for atom in atoms
    )
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def cache_root() -> pathlib.Path | None:
    """解析キャッシュの根。akapen（`src/semantic_cache.rs`）と同じ規則。"""
    for name in ("AKAPEN_CACHE_DIR", "XDG_CACHE_HOME"):
        value = os.environ.get(name)
        if value:
            base = pathlib.Path(value)
            return base if name == "AKAPEN_CACHE_DIR" else base / "akapen"
    home = os.environ.get("HOME")
    return pathlib.Path(home) / ".cache" / "akapen" if home else None


def boundary_cache_path(source: str) -> pathlib.Path | None:
    root = cache_root()
    if root is None:
        return None
    digest = hashlib.sha256(source.encode("utf-8")).hexdigest()
    return (
        root
        / "semantic"
        / "boundaries"
        / f"v{BOUNDARY_CACHE_VERSION}"
        / f"{digest}.json"
    )


def load_boundaries(source: str, plan: list[dict], atoms: list[dict]) -> bool:
    """キャッシュした境界を `plan` へ流し込む。当たれば `True`。

    **読めない項目は「外れ」である**（akapen 側のキャッシュと同じ作法）。
    Atom の数が合わない項目も、**Atom 列の指紋が合わない項目も**外れにする —
    別の割り方の境界を当てるよりは、もう一度聞くほうが安い
    （[`atoms_fingerprint`]）。指紋を持たない古い項目もここで落ちる。
    """
    path = boundary_cache_path(source)
    if path is None:
        return False
    try:
        cached = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    if cached.get("atoms") != atoms_fingerprint(atoms):
        return False
    decisions = cached.get("decisions")
    if not isinstance(decisions, list) or len(decisions) != len(plan):
        return False
    if any(decision not in (SAME, NEW) for decision in decisions):
        return False
    for entry, decision in zip(plan, decisions):
        entry["decision"] = decision
        entry["by"] = entry["by"] or "cache"
    return True


def save_boundaries(source: str, plan: list[dict], atoms: list[dict]) -> None:
    """境界の判定を残す。書けなくても解析は続ける（次にもう一度払うだけ）。"""
    path = boundary_cache_path(source)
    if path is None:
        return
    try:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        temp = path.with_suffix(f".tmp-{os.getpid()}")
        payload = {
            "version": BOUNDARY_CACHE_VERSION,
            "atoms": atoms_fingerprint(atoms),
            "decisions": [entry["decision"] for entry in plan],
        }
        # 0600 で作ってから rename。一瞬でも 0644 のファイルを作らない。
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False)
        os.replace(temp, path)
    except OSError:
        return


def marks_questions(atoms: list[dict], units: list[list[int]], text: str) -> dict:
    """スコアのラウンド — **Unit ごとに Noul 1 問**。

    問いの文面は akapen が送ってきたものをそのまま使い、枠と本文だけを
    付ける。**1 段の問いである**（`docs/design/jev.md`。前の答えを前提に
    しない）。
    """
    return {
        f"marks:u{number}": {
            "type": "noul",
            # **`instructions` である。** Noul の主張は `instructions` に置く
            # （`PROBE_QUESTION` / `REDUNDANCY_PAIR` / `CONTEXT_STAGE1` と
            # 同じ形）。別の名前で送ると HTTP 400
            # 「Noul question must have criteria or instructions」になる。
            "instructions": text + MARKS_FRAME.format(body=unit_body(atoms, indices)),
        }
        for number, indices in enumerate(units, start=1)
    }


def marks_annotate(request: dict, question: dict, model: str, timeout: float) -> dict:
    """解析の本体 — 境界 → スコア → 核 の 3 ラウンド。**唯一の入口である。**

    **run キャップは掛けない。** 1 本のリストの項目が全部光るのは「答えが
    全部光る」で正しい — **問いが既に選んでいる**ためである。量はつまみが
    受け持つ（`measurements/marks-mode.md`）。

    **問いの連鎖は無い。** 核のラウンドは「どの Unit に聞くか」を前の答えで
    絞るだけで、問いの文面は Unit の本文しか見ない（設計書「Jev への問いは
    1 段」）。
    """
    state = request.get("source") or ""
    atoms = request.get("atoms") or []
    text = question.get("text")
    if not isinstance(text, str) or not text.strip():
        raise JevError("the request carries a question with no text")
    core_floor = question.get("core_floor")
    core_floor = float(core_floor) if isinstance(core_floor, (int, float)) else 1.0

    budget = measure_state_tokens(state, model, timeout)
    rounds = [budget.probe] if budget.probe else []
    unsent: dict[str, list[str]] = {}

    def ask(questions: dict, label: str) -> dict:
        answers, records, dropped = send_in_chunks(state, questions, budget, model, timeout)
        for record in records:
            record["round"] = label
        rounds.extend(records)
        if dropped:
            unsent[label] = dropped
        return answers

    # --- ラウンド 1: 境界（キャッシュに当たれば 0 問）-------------------
    plan = plan_boundaries(atoms, state)
    cached = load_boundaries(state, plan, atoms)
    if not cached:
        questions = boundary_questions(atoms, plan)
        if questions:
            answers = ask(questions, "boundary")
            for entry in plan:
                key = f"boundary:{entry['after_atom']}"
                if entry["decision"] is None and key not in answers:
                    entry["decision"], entry["by"] = NEW, "rule:question_too_large"
            apply_boundary_answers(plan, answers)
        save_boundaries(state, plan, atoms)
    units = group_units(atoms, plan)

    # --- ラウンド 2: 問いへのスコア（Unit ごとに Noul 1 問）-------------
    #
    # **1 つも送れなかったら失敗させる。** 全部が「スコア無し」の応答は、
    # akapen 側では 0 本と区別が付かない（`marks::has_scores`）。そこは
    # 「答えている箇所が無い」という意味を持つ場所なので、送れなかったことを
    # そこへ混ぜてはならない。
    questions = marks_questions(atoms, units, text)
    answers = dict(ask(questions, "marks"))
    unanswered = set(unsent.get("marks", ()))
    if questions and len(unanswered) == len(questions):
        raise JevError(
            "Document too large for Jev — shrink it. Splitting cannot help: "
            f"state alone is {budget.state_tokens} tokens, leaving "
            f"{max(budget.pair, 0)} of the 32k budget, so not one question fits."
        )
    scores = [
        None if f"marks:u{n}" in unanswered else noul_of(answers, f"marks:u{n}")
        for n in range(1, len(units) + 1)
    ]

    # --- ラウンド 3: 核（足切りを超えた Unit にだけ）---------------------
    #
    # **狭い問いではこのラウンドごと消える。** 足切りを超える Unit が無ければ
    # question が 0 本になり、リクエストも 0 回である（設計書の費用の項）。
    #
    # `wants_core` は**この旗だけ**を見る。ワイヤに出ないので、名前も
    # ワイヤのフィールドを借りない（2026-09-22 まで `reading_tier` を
    # 内部の運び屋にしていた）。
    #
    # **Unit ごとの経路だけを通す。** 足切りを超えた Unit はそれぞれ核を持つ
    # （候補が 1 つなら [`assign_lone_cores`] が聞かずに埋める）。散文の候補が
    # 0 本の Unit と、選択肢が予算を超えた Unit だけが核を持たないままになる。
    provisional = [
        {
            "id": f"u{n}",
            "atoms": list(ix),
            "wants_core": scores[n - 1] is not None and scores[n - 1] >= core_floor,
            "core_atoms": [],
            "jev": {"score": scores[n - 1]},
        }
        for n, ix in enumerate(units, start=1)
    ]
    questions = core_questions(atoms, provisional, budget)
    if questions:
        third = ask(questions, "core")
        for key in unsent.get("core", ()):
            questions.pop(key, None)
        answers.update(third)
    assign_lone_cores(atoms, provisional)
    apply_core_answers(provisional, questions, answers)

    # 節の見出し。**Jev は出てこない**（構文だけで決まる）ので、ラウンドも
    # リクエストも増えない。読む人はまだ居ないが、プロトコルのフィールドで
    # あり、答えの JSON を眺める人が「どの節の話か」を追える。
    assign_sections(atoms, units, provisional)

    built = []
    for position, unit in enumerate(provisional):
        score = scores[position]
        out = {
            "id": unit["id"],
            "atoms": unit["atoms"],
            "jev": unit["jev"],
        }
        if unit.get("section_of"):
            out["section_of"] = unit["section_of"]
        if score is not None:
            out["score"] = score
        # 足切りを越えなかった Unit は核を聞いていない。`core_atoms` を
        # 省くと「絞り込み無し」＝ Unit 全体が核になるが、スコアが低いので
        # 光らない。**空で埋める**のは、つまみを 100 % まで上げたときに
        # 核を持たない Unit が丸ごと光らないようにするためである。
        out["core_atoms"] = list(unit.get("core_atoms") or [])
        built.append(out)

    report = {
        "rounds": rounds,
        "boundaries": plan,
        "budget": budget.record(),
        "question": question.get("id"),
        "core_floor": core_floor,
        "boundaries_cached": cached,
    }
    if unsent:
        report["unsent"] = unsent
    return {
        "version": VERSION,
        "question": question.get("id"),
        "units": built,
        "jev": report,
    }


def dry_run(request: dict, model: str, state_tokens: int | None = None) -> dict:
    """API を叩かずに、送るリクエストの形を出す。

    後のラウンドは前のラウンドの答えに依存するので、仮定を置いて組む。

    - ラウンド 1: 境界。**キャッシュは見ない**（当たれば 0 問になるが、
      形として知りたいのは「当たらなかったとき何を送るか」である）
    - ラウンド 2: スコア。Unit ごとに Noul 1 問で、仮定は「Jev に聞く境界は
      すべて NEW_UNIT だった」だけ（構造ルールで決まった境界はそのまま効く）
    - ラウンド 3: 核。**すべての Unit が足切りを越えた**と仮定する。本番では
      越えた Unit にしか聞かないので、実際に送る question はこれより少ない。
      **狭い問いではこのラウンドごと消える**

    **問いが要る。** 問いを持たない要求は本番と同じく断る — 形だけ見たい
    場合でも、問いの文面が question の大きさをそのまま決めるので、載せずに
    出した数字は本番の予測にならない。

    仮定は戻り値の `assumptions` にも載せる — 形だけ見て「これが本番で送る
    question 数だ」と読まれると困るため。
    """
    atoms = request.get("atoms") or []
    state = request.get("source") or ""
    question = request.get("question")
    if not isinstance(question, dict) or not isinstance(question.get("text"), str):
        raise JevError("--dry-run needs a request that carries a question")
    text = question["text"]

    plan = plan_boundaries(atoms, state)
    first = boundary_questions(atoms, plan)
    for entry in plan:
        if entry["decision"] is None:
            entry["decision"] = NEW
    units = group_units(atoms, plan)
    budget = (
        RequestBudget(state_tokens, measured=True)
        if state_tokens is not None
        else RequestBudget.estimated(state)
    )
    second = marks_questions(atoms, units, text)
    # 全 Unit が足切りを越えた場合の核。
    above_floor = [
        {"id": f"u{number}", "atoms": indices, "wants_core": True, "jev": {}}
        for number, indices in enumerate(units, start=1)
    ]
    third = core_questions(atoms, above_floor, budget)

    rounds = []
    for number, questions in enumerate((first, second, third), start=1):
        chunks, dropped = plan_chunks(questions, budget)
        rounds.append(
            {
                "round": number,
                "state": state,
                "model": model,
                "questions": questions,
                "plan": {
                    "chunks": len(chunks),
                    "tokens": [
                        budget.state_tokens + sum(map(question_tokens, c.values()))
                        for c in chunks
                    ],
                    "unsent": dropped,
                },
            }
        )
    return {
        "assumptions": [
            "round 1 ignores the boundary cache, which would make it 0 questions",
            "round 2 assumes every boundary Jev is asked about is new_unit",
            "round 3 assumes every Unit scored above the core floor "
            "(production asks only the ones that did, so it sends fewer "
            "questions; a narrow question drops the round entirely)",
            "when the state token count is an estimate (0.5 tokens/byte) this "
            "splits into more chunks than production, which measures the count "
            "on large documents (--state-tokens passes a measured value in)",
        ],
        "budget": budget.record(),
        "rounds": rounds,
    }


def annotate(request: dict, model: str, timeout: float) -> dict:
    """**唯一の入口。** 版を検めて [`marks_annotate`] へ渡す。

    **`question` の無い要求は断る。** 問いを持たない解析はこの層に無い
    （2026-09-22 に DIM 版を削除した）。黙って別のものを返すと、akapen 側
    では「0 本」と区別が付かない — そこは「答えている箇所が無い」という
    意味を持つ場所なので、混ぜてはならない。
    """
    version = request.get("version")
    if version != VERSION:
        raise JevError(f"unsupported protocol version: {version!r}")

    question = request.get("question")
    if not isinstance(question, dict) or not isinstance(question.get("text"), str):
        raise JevError(
            "this request carries no question — akapen must send one "
            "(the DIM version was removed on 2026-09-22)"
        )

    atoms = request.get("atoms") or []
    if not atoms:
        return {"version": VERSION, "question": question.get("id"), "units": []}

    return marks_annotate(request, question, model, timeout)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="adapter that wires akapen's --semantic-cmd to Jev",
    )
    parser.add_argument(
        "--model",
        default=os.environ.get("TYPESAFE_DEFAULT_MODEL", DEFAULT_MODEL),
        help=f"Jev model (default {DEFAULT_MODEL}; TYPESAFE_DEFAULT_MODEL also sets it)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=DEFAULT_TIMEOUT,
        help=f"timeout in seconds for one request (default {DEFAULT_TIMEOUT})",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the shape of the rounds without calling the API "
        "(round 2 assumes every boundary is new_unit, round 3 that every "
        "Unit scored above the core floor)",
    )
    parser.add_argument(
        "--state-tokens",
        type=int,
        default=None,
        help="measured token count of state, for --dry-run. Without it a "
        "0.5 tokens/byte estimate is used, which runs about 1.4x high on large "
        "documents and splits into more chunks than production does",
    )
    args = parser.parse_args()

    try:
        request = json.load(sys.stdin)
    except json.JSONDecodeError as e:
        print(
            f"{stderr_prefix()}stdin is not JSON: {one_line(str(e))}",
            file=sys.stderr,
        )
        return 1
    try:
        if args.dry_run:
            response = dry_run(request, args.model, args.state_tokens)
        else:
            response = annotate(request, args.model, args.timeout)
    except JevError as e:
        # akapen はステータス行に stderr の**最後の非空行**を 160 字まで出す
        # （`src/export.rs` の `Capture::tail`）。だから 1 行に収める。
        print(f"{stderr_prefix()}{one_line(str(e))}", file=sys.stderr)
        return 1
    except (KeyError, TypeError, ValueError) as e:
        print(
            f"{stderr_prefix()}cannot read the request: {one_line(str(e))}",
            file=sys.stderr,
        )
        return 1
    json.dump(response, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
