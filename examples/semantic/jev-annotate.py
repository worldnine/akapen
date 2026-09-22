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

## ラウンド構成 — 3 + 波

Tier の question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

    ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice }
                 → Unit を確定
    ラウンド2  state=文書全文, questions={ Unit ごとの Tier(Choice) }
                 → 誰に redundancy を聞くか / 誰の核を聞くかが確定
    ラウンド3  state=文書全文, questions={ SUPPORTING 以上の redundancy(Choice)
                                            と、ESSENTIAL な Unit の核(Choice) }
                 → **MARKED になりうる Unit**が確定
    ラウンド4+ state=文書全文, context preservation の前提を波で辿る
                 （1 波 = 段階 1〜4 の 4 往復。実測で波は 2〜4）

Jev は question を**並列・独立に**評価するので、ラウンド 2 の時点では
「どの Unit が ESSENTIAL か」をまだ誰も知らない。だから畳めない。

**ラウンド 4 以降だけ本数が固定でない。** 前提の前提を聞くには、前の波の
答えを見てからでないと相手が決まらないためである（[`trace_prerequisites`]）。
波は新しい前提が出なくなると自然に止まる。

**redundancy はラウンド 2 から 3 へ移してある。** 全 Unit に聞くのをやめ、
Tier が SUPPORTING 以上の Unit だけに聞く — `policy::decorate` は REDUNDANT な
Unit の Tier を `weakened()` で 1 段落とすだけなので、もともと下にいる
CONTEXT / DETAIL は聞いても表示が変わらない。実測で question が 55〜95% 減り
（`examples/semantic/README.md` の 113 Unit 中、聞くのは 8 個だけ）、28.2 KB の
文書が context window に入るようになった。

核をラウンド 3 で redundancy と**同時に**聞いているのは、本来なら
「ESSENTIAL かつ非 REDUNDANT」に絞りたいがそれだとラウンドが 4 つになるため。
実測では 4 文書とも「ESSENTIAL かつ REDUNDANT」は 0〜1 件しかなく、捨てる
question はほぼ出ない。

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
#: **この 20 秒はもう 3 回ぶんではない。** リクエストは分割されるので、上限は
#: 「20 秒 × チャンクの総数 + probe」になる。実測（2026-09-21）では 65 KB の
#: `docs/gotchas.md` が 10 リクエスト（probe 1 + チャンク 9）で、理屈の上では
#: 200 秒 — akapen の `COMMAND_TIMEOUT`（60 秒）を**超えうる**。
#:
#: **実測では超えない。** 同じ文書のプロセス全体が 12.7〜13.0 秒で、1 リクエスト
#: あたりは最遅でも 1.5 秒である。20 秒はその 13 倍で、そこまで遅くなるなら
#: 打ち切られるべきでもある。**だが「3 × 20 = 60 でちょうど並ぶ」という以前の
#: 理屈はもう成り立たない** — 天井に張り付く文書を足すときは、ここではなく
#: リクエストの総数を見ること。
#:
#: **大きな文書でもこの 20 秒は余っている**（2026-09-21 の実測）。1 リクエスト
#: あたりは 65 KB の文書でも 1.5 秒以内で、20 秒には 13 倍の余裕がある。
#: プロセス全体は 1.7 秒（1.6 KB）〜13.0 秒（65 KB）だった。
#:
#: 大文書で先に当たるのは**時間ではなく context window** である
#: （[`http_error_message`]）。そこは 400 で即座に返るので、ここを延ばしても
#: 何も救われない。
DEFAULT_TIMEOUT = 20.0

#: Reading Tier の criteria。設計書 `docs/design/semantic-reading-layer.md` の
#: 「Reading Tier」の定義の逐語。**言い換えないこと** — ここが判定品質を支配する。
#:
#: **ESSENTIAL の例示は 2026-09-22 に広げた。設計書と同時に、逐語のまま。**
#: ここが drift したのではない。実機で未決を述べた節が 10/10 DETAIL になり、
#: 「制約」は既に入っているのに例が全部「決まったもの」の名詞だったのが原因
#: だったので、例示に未決の側を足した。**軸は 1 つのままである** —
#: 経緯と却下した別案は `examples/semantic/measurements/essential-unsettled.md`
#: とコミットメッセージに。
#:
#: `supporting` だけは設計書と一字一句では一致していない（設計書
#: 「ESSENTIALの理解・納得に役立つ。」/ ここ「ESSENTIAL な内容の…」）。
#: **2026-09-22 以前からある差で、今回は触っていない** — 測定の最中に
#: 2 つ目の criteria を動かすと、どちらが効いたのか帰属できなくなる。
TIER_CRITERIA = {
    "essential": "落とすと文書の要点、結論、制約、未決の論点や宿題などを取り違える可能性が高い。",
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
#: Reading Tier の ESSENTIAL の定義（`TIER_CRITERIA`）も「落とすと文書の要点、
#: 結論、制約、未決の論点や宿題などを取り違える可能性が高い」という損失の
#: 言い方をしている。
#: 核は ESSENTIAL な Unit の中をさらに同じ軸で絞る操作なので、**軸を揃えるのが
#: 筋**であって、ここだけ「1 か所だけ読むなら」という別の軸を混ぜる理由が無い。
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
STANDALONE_KINDS = frozenset({"code_block", "table"})

#: 核（ラウンド 3 の選択肢）になれる Atom の種別 — **散文だけ**。
#: 見出し・コードブロック・テーブルは「1 か所だけ読むならどこか」の答えに
#: ならない（[`core_candidates`]）。
PROSE_KINDS = frozenset({"sentence", "list_item"})


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
    残っているのに、それが何なのかを言う見出しのほうが沈む。業務議事録を
    READ 30 % で表示したときの報告は 2 件で、中身は 17 atom 中 8 / 13 atom 中 2
    が残っていた。

    **新しい規則を足して直してはいない。** 同じ意図を「中身が複数 Unit に
    なった場合」について言い直したのが [`assign_sections`] の `section_of` で、
    実行するのは `policy::decorate` である（「節の中の Unit が 1 つでも残るなら
    見出しも残す」）。**規則 2 とそれは 1 つのことである** — 似た規則が 2 つ
    あると読んで、片方だけ直したり統合したりしないこと。

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


def unit_questions(atoms: list[dict], units: list[list[int]]) -> dict:
    """ラウンド 2 の questions（Unit ごとの Tier）。

    **redundancy はここでは聞かない。** Tier が決まってからでないと「誰に
    聞くべきか」が分からないためで、[`redundancy_questions`] が後のラウンドで
    SUPPORTING 以上の Unit にだけ聞く。

    Tier と redundancy を 1 つの Choice（「既出の言い直し」を 5 つ目の選択肢に
    足す）へ畳む案は**採らなかった**。設計書が「重複は Reading Tier とは
    別軸」と定めていて、`policy::decorate` も 2 つを独立に使う
    （`redundant -> weakened(tier)`）。実測でも畳むと軸が消えた — demo.md の
    u9 / u10 は 4 つの Tier の probability がすべて 0.0 になり、Tier は
    tie-break 次第（同じ question で detail と context の両方が出た）。
    """
    questions = {}
    for number, indices in enumerate(units, start=1):
        questions[f"tier:u{number}"] = {
            "type": "choice",
            "instructions": (
                "この文書の中で、次の部分はどの読む優先度に当たりますか。\n\n"
                f"――― 対象 ―――\n{unit_body(atoms, indices)}\n―――――――――"
            ),
            "criteria": dict(TIER_CRITERIA),
        }
    return questions


#: redundancy を聞く Tier。DETAIL と CONTEXT には聞かない。
#:
#: `policy::decorate` は REDUNDANT な Unit の Tier を `weakened()` で 1 段
#: 落とすだけなので、もともと下にいる CONTEXT / DETAIL は聞いても表示が
#: 変わらない（CONTEXT -> DETAIL は Budget の並び順を少し動かすが、MARKED
#: にも DIM の閾値にも効かない）。聞く相手を絞ると question が実測で
#: 55〜95% 減る（README.md は 113 Unit 中 8 個だけが対象だった）。
REDUNDANCY_TIERS = frozenset({"essential", "supporting"})


#: redundancy の Choice で「該当なし」を表す選択肢のキー。
#:
#: **これが閾値の代わりである。** 以前は Noul（「言い直しか」）を 0.7 で切り、
#: 相手は語の重なりが最大の先行 Unit をローカルに選んでいた。閾値は demo.md の
#: 2 点から置いた暫定値で掃引しておらず、業務議事録では 0.70〜0.71 の Unit が
#: 1 ランだけ閾値の上に乗って揺れ、相手は 7 件中 3 件が誤りだった（見出しだけの
#: Unit を指す、別の節を指す）。Choice は候補を突き合わせて 1 つ返すので、
#: 「どれの言い直しか」は Jev が答え、「どれでもない」はこの選択肢が受ける。
#: 値で倒す場所はどこにも無い。
REDUNDANCY_NONE = "none"

#: 「該当なし」の選択肢の本文。他の選択肢は先行 Unit の本文の引用なので、
#: ここだけが判定基準の文になる。
REDUNDANCY_NONE_TEXT = (
    "該当なし。対象は、これより前のどの箇所の言い直しでもなく、新しい情報を加えている。"
)

#: 言い直しの**元**として選択肢に並べる先行 Unit の Tier。
#:
#: context preservation（[`CONTEXT_TIERS`]）と同じ絞り方で、**DETAIL だけ
#: 落とす**。DETAIL は Jev 自身が「重要でない」と言ったものなので、それを
#: 言い直した Unit が SUPPORTING 以上になることは考えにくい。CONTEXT は
#: 「背景」で、前置きを結論部で言い直す形はありうるので残す。
#:
#: 「SUPPORTING 以上だけ」も測った（`examples/semantic/measurements/redundancy.md`）。
#: 選ぶ側の Tier（[`REDUNDANCY_TIERS`]）とは別の集合なので混ぜないこと。
REDUNDANCY_SOURCE_TIERS = frozenset({"essential", "supporting", "context"})

#: redundancy の Choice の文面。選択肢は前の Unit の本文そのもの（キーは
#: `u:<添字>`）と「該当なし」（[`REDUNDANCY_NONE`]）。
#:
#: **方向は文面で指定する。** 「これより**前**の箇所」であって、対称に
#: 「重複しているか」ではない。対称に聞くと結論まで拾う（Noul 時代の実測で
#: 結論の Unit が 0.71 を出し、方向ありに直すと 0.36 へ落ちた）。設計書が
#: 「**既読内容との** redundancy」と書き、Duggan & Payne の satisficing が
#: 逐次的なモデルであることと整合する。選択肢を `range(target)` に限っているので
#: 文面と構造の両方で後ろ向きになる。
#:
#: 「同じ話題に触れているだけ・関連しているだけの箇所は当てはまらない」は
#: [`CONTEXT_CHOICE`] と同じ一文で、Noul 時代の誤判定（同じ案件の別の文を
#: 0.72〜0.76 で言い直しとした）を狙っている。
REDUNDANCY_CHOICE = (
    "「対象」はこの文書の後ろの方にある次の一続きである。\n\n"
    "――― 対象 ―――\n{target}\n―――――――――\n\n"
    "選択肢は、対象より**前**にある各部分の本文である。対象が**すでに述べられた"
    "内容を言い直しているだけで、新しい情報を加えていない**とき、その言い直しの"
    "元になっている箇所はどれか。\n"
    "同じ話題に触れているだけ・関連しているだけの箇所は当てはまらない。"
    "対象がどの箇所の言い直しでもなく新しい情報を加えているなら"
    "「該当なし」を選ぶこと。"
)


def redundancy_candidates(tiers: list[str], bodies: list[str], target: int) -> list[int]:
    """`target`（0 始まり）より前で、言い直しの元になれる Unit の添字。

    **語彙で絞らない。** 以前の `redundancy_target` は 2 文字 bigram の重なりの
    argmax で相手を選んでいたが、候補の多い前方に構造的に寄り、内容を持たない
    見出しだけの Unit も指した。絞るのは Jev 自身が付けた Tier だけ
    （[`REDUNDANCY_SOURCE_TIERS`]。[`context_candidates`] と同じ線）。
    """
    return [
        position
        for position in range(target)
        if bodies[position].strip() and tiers[position] in REDUNDANCY_SOURCE_TIERS
    ]


def redundancy_choice(
    bodies: list[str], target: int, pool: list[int], budget: RequestBudget
) -> tuple[dict, list[int]]:
    """Choice 1 つ。**予算を超えたら位置が遠い順に落とす。**

    返すのは `(question, 実際に載せた候補)`。[`context_choice`] と同じで、
    個数ではなくトークンで切る。「該当なし」は常に載る。

    遠い順に落とすのは「近い方が言い直しの元になりやすい」という仮定では
    **ない** — 業務議事録の言い直しは文書末尾の決定事項リストが本文節を指す
    ので、遠い相手もある。予算に当たった回数は報告に載せる（`trim`）。
    """
    instructions = REDUNDANCY_CHOICE.format(target=bodies[target])
    kept = list(pool)
    while kept:
        criteria = {REDUNDANCY_NONE: REDUNDANCY_NONE_TEXT}
        criteria.update({f"u:{position}": bodies[position] for position in kept})
        question = {"type": "choice", "instructions": instructions, "criteria": criteria}
        if question_tokens(question) <= budget.pair:
            return question, kept
        kept.pop(0)
    return {}, []


def redundancy_questions(
    atoms: list[dict], units: list[list[int]], tiers: list[str], budget: RequestBudget
) -> tuple[dict, dict]:
    """ラウンド 3 の questions のうち redundancy の分と、絞り込みの記録。

    SUPPORTING 以上の Unit ごとに **Choice 1 つ**。「この部分が言い直している
    既出の箇所はどれか」を、先行 Unit の本文と「該当なし」から選ばせる。
    「該当なし」が選ばれれば冗長ではない — **閾値は無い**（[`REDUNDANCY_NONE`]）。

    `Presupposes`（[`trace_prerequisites`] の段階 2）と同じ形である。あちらも
    対の Noul が依存ではなく「関連」を測って対称に「はい」を出したのを、Choice
    で解決した。Noul は孤立した真偽で比較をしないが、Choice は候補を
    突き合わせて 1 つ返す。

    先頭の Unit には聞かない —「これより前」が存在せず、REDUNDANT_WITH の
    参照先も作れない。候補が 0 件の Unit にも聞かない（言い直せる相手が無い
    ので冗長にはなれない）。
    """
    bodies = [unit_body(atoms, indices) for indices in units]
    questions: dict = {}
    trim = {"asked": 0, "by_budget": 0, "no_candidate": 0}
    for position, tier in enumerate(tiers):
        if position == 0 or tier not in REDUNDANCY_TIERS:
            continue
        pool = redundancy_candidates(tiers, bodies, position)
        if not pool:
            trim["no_candidate"] += 1
            continue
        question, kept = redundancy_choice(bodies, position, pool, budget)
        if not kept:
            trim["no_candidate"] += 1
            continue
        if len(kept) < len(pool):
            trim["by_budget"] += 1
        trim["asked"] += 1
        questions[f"redundant:u{position + 1}"] = question
    return questions, trim


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


def build_units(
    atoms: list[dict],
    units: list[list[int]],
    tiers: list[str],
    answers: dict,
    questions: dict | None = None,
) -> list[dict]:
    """ラウンド 2・3 の答えから、プロトコルの `units` を組む。

    `questions` はラウンド 3 の question（redundancy の Choice の `criteria` を
    答えの検査に使う）。`confidence` は**捨てず**、各 Unit の `jev` フィールドに
    記録する（プロトコルは未知のフィールドを拒否しないので akapen 側は無視
    する）。**閾値で判定を倒す場所はもう無い** — redundancy も Choice になり、
    「該当なし」（[`REDUNDANCY_NONE`]）が閾値の代わりをする。

    redundancy を聞いていない Unit（CONTEXT / DETAIL と先頭、候補が無かった
    もの、予算で送れなかったもの）は、そもそも REDUNDANT になりえないので、
    ここを出る時点では `relations` が空になる。**空のまま終わるとは限らない** —
    context preservation の前提（`PRESUPPOSES`）は核まで決まったあとに
    [`trace_prerequisites`] が同じ配列へ足す。
    """
    questions = questions or {}
    out = []
    for number, indices in enumerate(units, start=1):
        uid = f"u{number}"
        record: dict = {
            "tier_choice": tiers[number - 1],
            "tier_confidence": confidence_of(answers, f"tier:{uid}"),
        }
        relations: list[dict] = []
        question = questions.get(f"redundant:{uid}")
        if question is not None:
            key = f"redundant:{uid}"
            choice = choice_of(answers, key, question["criteria"])
            record["redundancy_choice"] = choice
            record["redundancy_confidence"] = confidence_of(answers, key)
            record["redundancy_candidates"] = len(question["criteria"]) - 1
            probabilities = answer_of(answers, key).get("probabilities")
            if isinstance(probabilities, dict):
                got = probabilities.get(REDUNDANCY_NONE)
                if isinstance(got, (int, float)):
                    record["redundancy_none_probability"] = float(got)
            if choice == REDUNDANCY_NONE:
                record["redundant_with"] = None
            else:
                target = f"u{int(choice.split(':')[1]) + 1}"
                record["redundant_with"] = target
                relations.append({"redundant_with": target})
        out.append(
            {
                "id": uid,
                "atoms": list(indices),
                "reading_tier": tiers[number - 1],
                "relations": relations,
                "jev": record,
            }
        )
    return out


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

    **境界規則 2 の意図を、規則 4 の先まで運ぶための属性である。** 規則 2
    「見出しは直後の内容に付く」は、見出しだけの Unit を作らないことで
    「中身は残っているのに見出しが沈む」を防いでいた。規則 4 で箇条書きを
    項目ごとに割ってから、見出しの Unit は「見出し ＋ せいぜい最初の項目」に
    なり、**2 つ目以降の項目は別 Unit として浮いた** — 境界だけでは意図を
    守れない。そこで節の境界を属性として渡し、`policy::decorate` が
    「節の中の Unit が 1 つでも残るなら見出しも残す」を実行する。

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


def is_redundant(unit: dict) -> bool:
    """この Unit に `REDUNDANT_WITH` が付いているか。

    **`relations` が空でないこと、ではない。** `PRESUPPOSES`（context
    preservation の前提）が同じ配列に並ぶようになったので、空かどうかで
    判定すると**前提を持つ Unit が重複扱いになる** — MARKED から外れ、
    核も捨てられる。context preservation は光らせ続けるための rule なので、
    そこを混ぜると真逆へ倒れる。`crates/semantic-reading/src/unit.rs` の
    `SemanticUnit::redundant_with` が同じ直しを受けている。
    """
    return any("redundant_with" in relation for relation in unit["relations"])


def wants_core(unit: dict) -> bool:
    """この Unit に核を聞く意味があるか。

    聞くのは **MARKED になりうる Unit だけ**である。`policy::decorate` が
    MARKED にするのは「Budget に残った ESSENTIAL かつ非 REDUNDANT」なので、
    ここで Tier と redundancy を見れば足りる（Budget はこのスクリプトから
    見えないし、見る必要もない — 核の選択は Budget に依存しない）。

    絞り込むことで question 数が減る。実測（45.6 KB の文書）では 34 Unit の
    うち 11 が ESSENTIAL かつ非 REDUNDANT で、さらに Atom が 2 つ以上ある
    7 つだけがラウンド 3 の question になった。
    """
    return unit["reading_tier"] == "essential" and not is_redundant(unit)


def core_candidates(atoms: list[dict], unit: dict) -> dict:
    """核の選択肢。**散文の Atom だけ**を候補にする。

    `heading` / `code_block` / `table` は「このまとまりから 1 か所だけ読むなら
    どこか」の答えにならない。見出しは中身を持たないし、コード例や表は要点の
    言い換えではない。種別は構文の話なので、設計書「Jev に判断させないもの:
    **syntax parsing**」のとおりここで落とす — 実測でも、候補に見出しがあると
    Jev は見出しを選んだ（demo.md の `## 結論` / `## 制約`）。

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


def assign_lone_cores(
    atoms: list[dict], units: list[dict], handled: frozenset[int] = frozenset()
) -> None:
    """候補が 1 つしかない Unit の核を、聞かずに決める。

    絞り込んだ結果 1 つになった場合に**聞かないだけ**だと、核が空のまま
    Unit 全体（見出しを含む）が MARKED に戻ってしまう。候補が 1 つなら答えは
    決まっているので、question を使わずにそれを核にする。

    実測ではこれで核の question が 33〜89% 減った（design.md は 19 Unit 中
    17 が聞かずに決まった）。

    `handled` は [`plan_run_cores`] が既に面倒を見た Unit の添字。run キャップ
    （1 本のリストにつき核 1 つ）に入った Unit をここで上書きしないためにある。
    """
    for position, unit in enumerate(units):
        if position in handled or not wants_core(unit):
            continue
        options = core_candidates(atoms, unit)
        if len(options) == 1:
            index = int(next(iter(options)).split(":")[1])
            unit["core_atoms"] = [index]
            unit["jev"]["core_choice"] = f"atom:{index}"
            unit["jev"]["core_by"] = "rule:only_prose_atom"


def core_questions(
    atoms: list[dict],
    units: list[dict],
    budget: RequestBudget,
    handled: frozenset[int] = frozenset(),
) -> dict:
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
    for position, unit in enumerate(units):
        if position in handled or not wants_core(unit):
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


#: run（= 1 本のリスト）をつなぐ境界の理由。[`boundary_rule`] の規則 4 が
#: 「別項目なので NEW」と判断した切れ目だけが、同じリストの中の切れ目である。
RUN_BOUNDARY = "rule:new_list_item"


def unit_runs(units: list[list[int]], plan: list[dict]) -> list[list[int]]:
    """規則 4 由来の境界でつながった Unit の並び ＝ **1 本のリスト**を並べる。

    返すのは Unit の添字の列で、**長さ 2 以上のものだけ**（1 つしかない並びは
    リストとして束ねる意味が無い）。

    見出しや散文で切れた境界はつながない。だから「箇条書き → 段落 → 箇条書き」は
    2 本の別のリストになり、リストの途中に引用やコードブロックが挟まればそこで
    切れる（規則 3 が NEW を返し、理由が `rule:standalone_block` になるため）。
    """
    if not units:
        return []
    by = {entry["after_atom"]: entry["by"] for entry in plan}
    runs, current = [], [0]
    for position in range(len(units) - 1):
        if by.get(units[position][-1]) == RUN_BOUNDARY:
            current.append(position + 1)
        else:
            runs.append(current)
            current = [position + 1]
    runs.append(current)
    return [run for run in runs if len(run) > 1]


def plan_run_cores(
    atoms: list[dict], units: list[dict], runs: list[list[int]], budget: RequestBudget
) -> tuple[dict, dict, dict, frozenset[int]]:
    """**1 本のリストにつき核を 1 つ**に絞る。Tier（沈む側）は触らない。

    返り値は `(questions, fixed, scope, handled)`:

    - `questions` … run ごとの核 question（キーは `core:run:<先頭 Unit の番号>`）
    - `fixed` … 聞かずに決まったぶん `{Unit の添字: [atom] または []}`
    - `scope` … `{question のキー: その答えが核を決める Unit の添字の列}`
    - `handled` … この関数が面倒を見た Unit の添字。残りは従来の Unit ごとの
      経路（[`core_questions`] / [`assign_lone_cores`]）が拾う

    ## なぜ要るか

    規則 4 で箇条書きを項目ごとに割ると、1 本のリストの中に ESSENTIAL な Unit が
    いくつも立ち、**そのすべてが光る**。実測では業務 `CLAUDE.md` の MARKED が
    3.3〜3.7 % から 10.8〜11.2 % へ増えた。**沈む側（Tier）と光る側（核）は
    別のメカニズム**なので、Tier を項目ごとのままにして核だけをリスト単位に
    畳める。

    選に漏れた Unit には `core_atoms` に**空の配列**を入れる。プロトコルは
    「無い」と「空」を区別していて、空は「**核を持たない**」＝ MARKED に
    ならない、を意味する（`crates/semantic-reading/src/protocol.rs`
    「`core_atoms` は 3 値」）。ここを省くと、選に漏れた Unit が丸ごと光る。

    ## 予算を超えた run は面倒を見ない

    run 全体の散文を選択肢にすると 32k 枠を超えることがある。その run は
    `handled` に入れず、**従来の Unit ごとの核へ落とす**。「核が無ければ Unit
    全体が MARKED」を run に当てると、そのリストの**全項目が光って最悪**になる。
    そこへは落とさない。
    """
    questions: dict = {}
    fixed: dict[int, list[int]] = {}
    scope: dict[str, list[int]] = {}
    handled: set[int] = set()
    for run in runs:
        members = [position for position in run if wants_core(units[position])]
        if len(members) < 2:
            # MARKED になりうる Unit が 1 つ以下の run は、畳む相手がいない。
            continue
        options, owner = {}, {}
        for position in members:
            for key, text in core_candidates(atoms, units[position]).items():
                options[key] = text
                owner[key] = position
        if not options:
            # 散文が 1 つも無い run。従来どおり Unit ごとに任せる。
            continue
        if len(options) == 1:
            key = next(iter(options))
            index = int(key.split(":")[1])
            for position in members:
                won = position == owner[key]
                fixed[position] = [index] if won else []
                units[position]["jev"]["core_by"] = (
                    "rule:only_prose_atom_in_run" if won else "rule:run_cap"
                )
            handled.update(members)
            continue
        if not core_fits(options, budget):
            continue
        key = f"core:run:{members[0] + 1}"
        questions[key] = {
            "type": "choice",
            "instructions": CORE_INSTRUCTIONS,
            "criteria": options,
        }
        scope[key] = members
        handled.update(members)
    return questions, fixed, scope, frozenset(handled)


def apply_run_cores(
    units: list[dict], questions: dict, fixed: dict, scope: dict, answers: dict
) -> None:
    """[`plan_run_cores`] の決定を Unit へ書き戻す。

    答えが無い / criteria に無い値だった question は [`choice_of`] が失敗させる。
    他のラウンドと同じで、黙って埋めない。
    """
    for position, core in fixed.items():
        units[position]["core_atoms"] = list(core)
    for key, members in scope.items():
        choice = choice_of(answers, key, questions[key]["criteria"])
        winner = int(choice.split(":")[1])
        for position in members:
            unit = units[position]
            if winner in unit["atoms"]:
                unit["core_atoms"] = [winner]
                unit["jev"]["core_choice"] = choice
                unit["jev"]["core_confidence"] = confidence_of(answers, key)
            else:
                # 同じリストの別項目が核に選ばれた。この Unit は光らせない。
                unit["core_atoms"] = []
                unit["jev"]["core_by"] = "rule:run_cap"


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
# context preservation — 前提を聞く（段階 1〜3 を波で回す）
#
# 設計上の位置は `docs/design/semantic-reading-layer.md`「同じ Tier 内部では
# … `context preservation` などの決定論的な rule をまず利用する」。**表示では
# ない** — 前提は読み手に知らせるのではなく、`policy::decorate` が閉包ごと
# 予算に数えて**一緒に生き残らせる**。
#
# 中身は設計書が定義していないので、実測で決めた
# （`examples/semantic/measurements/context-preservation.md`。採取スクリプトは
# 証拠ディレクトリの `tools/ctxchoice.py`）。**この節の 3 つの文面と 0.5 は
# そこから一字一句移したものである。言い換えると計測が根拠でなくなる。**
#
#     段階 1  seed ごとに Noul 1 つ。「前を読まないと意味が取れないか」
#     段階 2  「はい」の Unit ごとに **Choice 1 つ**。選択肢は自分より前の Unit
#     段階 3  当て木。Noul 1 つ →「はい」なら選んだものを外した Choice を
#             もう 1 回で**止める**。1 Unit の直接前提は最大 2
#
# ## なぜ Choice なのか（閾値の話ではなかった）
#
# 第 1 版は段階 2 を「自分より前の全 Unit と対の Noul」でやり、閾値で切った。
# 0.4 では閉包が文書の 17〜58 %（中央値）に膨らみ、0.5 では手で見つけた穴の
# 片方を取り逃がす。**原因は閾値ではなく primitive の取り違えだった** — 役を
# 入れ替えても「はい」になる対が 0.4 で 7.6〜32 % あり、依存は定義から非対称
# なので、それは依存ではなく**関連**を測っている。
#
# Noul は孤立した真偽で比較をしない。Choice は候補を突き合わせて 1 つ返すので
# **扇が構造的に 1 になる**。差し替えたら閉包は 0.5 の大きさのまま（b1 10.3 % /
# design 2.5 %）、穴は 2 件とも捕まり、**閉包が単独で 30 % の予算を超える
# MARKED は全 32 ランで 0** になった。コストも 2 乗が消えた（design で
# 155 秒 / 4.7M tokens → 15 秒 / 0.27M tokens）。
#
# akapen は既に同じ形を核の選択で使っている（[`core_questions`]）。
# ---------------------------------------------------------------------------


#: 段階 1・3 の Noul の「はい」の境目。**振らない。**
#:
#: 「Noul の『はい』の自然な境目で、恣意的でない点はここしかない」を計測から
#: 引き継いだ。段階 2 には閾値が無い（Choice が返した 1 つを採る。
#: [`apply_core_answers`]「`probabilities` を閾値で切って複数採る案は採らない」
#: と同じ線）。
#:
#: **穴 A の 2 本目はこの境目のすぐ上に乗っている。** 実測で当て木が 0.52
#: だった（4 ランとも）。`docs/design/jev.md` は `confidence` の揺れ ±0.07 を
#: 理由に閾値ガードを不採用にしていて、**同じ幅がここに乗れば裏返る**。
#: 「捕まえた」であって「安定して捕まえる」ではない。
CONTEXT_YES = 0.5

#: 前提の選択肢にする Tier — **DETAIL だけ落とす。**
#:
#: 指示は「SUPPORTING 以上」だったが、字義どおりだと CONTEXT も落ちる。
#: **実測で穴 A の相手は 2 つとも CONTEXT だった**ので、字義を採ると
#: Choice の出来と無関係に捕まらない（b1 の 4 ランとも候補 0 件）。
#: 絞り込みの理由は「DETAIL は Jev 自身が重要でないと言ったものだから」なので、
#: 理由に合わせて DETAIL だけを落とす。
#:
#: **CONTEXT を落とせない理由は構造的である。** CONTEXT は設計上「背景」で、
#: 人物や書名が何者かの説明はまさにそこに落ちる。ESSENTIAL な一文が CONTEXT に
#: 寄りかかるという想定は捨てられない。
CONTEXT_TIERS = frozenset({"essential", "supporting", "context"})

#: 閉包の波の上限。
#:
#: **打ち切りのためではなく、止まらなくなったときの保険である。** 実測では
#: 4 文書 × 4 ラン × 2 条件のすべてで、新しい前提が出なくなって自然に止まった
#: （波は 2〜4）。辺は必ず自分より前の Unit を指すので波の数は Unit 数を
#: 超えられないが、それを当てにせず数で止める。
CONTEXT_MAX_WAVES = 12

#: 段階 1（Noul）。**計測と一字一句同じ。**
CONTEXT_STAGE1 = (
    "次の部分は、これより**前**のどこかを読んでいないと意味が取れない。\n"
    "前の箇所を読まずにここだけを読むと、何を指しているのか・誰の何の話なのかが"
    "決まらず、書かれている内容を取り違える。\n\n"
    "――― 対象 ―――\n{body}\n―――――――――"
)

#: 段階 2（Choice）。選択肢は前の Unit の本文そのもの（キーは `u:<添字>`）。
#: 核の Choice と同じで、`criteria` の説明文が判定基準ではなく本文の引用になる。
#:
#: 対の Noul との違いは「1 つ選べ」と強制するところにある。答えを 1 つへ倒す
#: 問い方にしないと「どれも当てはまる」に戻る（[`CORE_INSTRUCTIONS`]
#: 「『重要な部分はどれか』とは聞かない」と同じ理由）。
CONTEXT_CHOICE = (
    "「対象」はこの文書の後ろの方にある次の一続きである。\n\n"
    "――― 対象 ―――\n{target}\n―――――――――\n\n"
    "選択肢は、対象より**前**にある各部分の本文である。**それを読まずに対象だけを"
    "読むと、対象が何を言っているのかが決まらない**のはどれか。\n"
    "同じ話題に触れているだけ・関連しているだけの箇所は当てはまらない。"
    "**対象の意味がそこに依存している**箇所を選ぶこと。"
)

#: 段階 3（当て木。Noul）。
#:
#: Jev は question を独立・並列に評価するので、「いま選んだもの」という指示語は
#: 届かない（`docs/design/jev.md`）。**選んだ Unit の本文を埋め込む。**
CONTEXT_SPLINT = (
    "「対象」を正しく読むには、下の「既に分かっている前の箇所」だけでは足りない。"
    "**これとは別に、さらに前のどこかをもう 1 つ**読まないと、対象が何を言って"
    "いるのかが決まらない。\n\n"
    "――― 対象 ―――\n{target}\n"
    "――― 既に分かっている前の箇所 ―――\n{known}\n―――――――――"
)


def context_seeds(units: list[dict]) -> list[int]:
    """前提を聞き始める Unit の添字（0 始まり）。

    **`policy::decorate` が MARKED にしうる Unit と同じ条件**である —
    ESSENTIAL かつ非 REDUNDANT かつ核が空でないもの。穴の形が「光っている
    一文が、沈んだ Unit に寄りかかっている」なので、光る側から辿る。

    ここは Budget を見ない。見えないし、見る必要もない — どの Budget で
    沈むかを決めるのは `policy` の仕事で、この層は辺を渡すだけである。
    """
    return [
        position
        for position, unit in enumerate(units)
        if unit["reading_tier"] == "essential"
        and not is_redundant(unit)
        and unit.get("core_atoms") != []
    ]


def context_candidates(
    units: list[dict], bodies: list[str], target: int, exclude: set[int]
) -> list[int]:
    """`target` より前で、選択肢になれる Unit の添字。

    **語彙で絞らない。** 初出の語・人名の有無などで代用すると、擬似見出しを
    句点で捕まえようとした手（`docs/gotchas/semantic-reading.md` で 2 回却下）と
    同じ形になる。絞るのは Jev 自身が付けた Tier だけ（[`CONTEXT_TIERS`]）。
    """
    return [
        position
        for position in range(target)
        if position not in exclude
        and bodies[position].strip()
        and units[position]["reading_tier"] in CONTEXT_TIERS
    ]


def context_choice(
    bodies: list[str], target: int, pool: list[int], budget: RequestBudget
) -> tuple[dict, list[int]]:
    """Choice 1 つ。**予算を超えたら位置が近い順に詰める。**

    返すのは `(question, 実際に載せた候補)`。[`core_fits`] と同じく個数では
    なくトークンで切る — 同じ 10 個でも文書によって桁が違う。

    **実測ではここは 1 度も発火しなかった**（4 文書 × 4 ラン × 2 条件）。
    全 Unit の本文を並べても 4.3〜9.6k tokens で、32k 枠の残り（21〜26k）に
    収まる。もっと大きい文書では当たるはずで、そこは測っていない。
    """
    instructions = CONTEXT_CHOICE.format(target=bodies[target])
    kept = list(pool)
    while kept:
        question = {
            "type": "choice",
            "instructions": instructions,
            "criteria": {f"u:{position}": bodies[position] for position in kept},
        }
        if question_tokens(question) <= budget.pair:
            return question, kept
        # いちばん遠い（＝位置が離れた）候補から落とす。
        kept.pop(0)
    return {}, []


def trace_prerequisites(
    units: list[dict],
    bodies: list[str],
    budget: RequestBudget,
    ask,
) -> tuple[dict[int, list[int]], dict]:
    """段階 1〜3 を波で回し、`{対象の添字: 直接の前提（最大 2）}` を返す。

    `ask(questions, label) -> answers` は [`annotate`] のラウンド送信。
    2 つ目の戻り値は報告用の記録。

    ## 波は回す。扇だけが 1 に変わる

    新しく見つかった前提についても段階 1〜3 を聞き直す。**「2 段で止める」は
    1 Unit あたりの当て木の話であって、閉包の波の話ではない** — 深さを出すには
    推移的に辿るしかない。実測では深さの中央値 0〜2・最大 7 で、閉包は
    文書の 0〜10.3 %（中央値）に収まった。

    ## 辺は必ず後ろ向きに立つ

    選択肢を `range(target)` に限っているので、前提は必ず自分より前の Unit に
    なる。**循環は構造上ありえない**（測って 0 だったのではない）。
    `policy` 側はそれに依存せず訪問済み集合で辿るが、ここで前向きの辺を
    作らないこと自体は保証している。
    """
    picks: dict[int, list[int]] = {}
    trim = {"asked": 0, "by_budget": 0, "forced_single": 0, "no_candidate": 0}
    waves: list[dict] = []

    def ask_some(questions: dict, label: str) -> dict:
        """空の束では 1 往復も使わない。

        段階 1 が全部「いいえ」なら段階 2 は組まれず、当て木が全部
        「いいえ」なら段階 4 も組まれない。**波の後半は空になるのが普通**
        なので、ここで落とさないとラウンドの記録に空の往復が並ぶ。
        """
        return ask(questions, label) if questions else {}

    frontier = context_seeds(units)
    seeds = list(frontier)
    visited: set[int] = set()

    for wave in range(CONTEXT_MAX_WAVES):
        todo = [position for position in frontier if position not in visited]
        if not todo:
            break
        visited.update(todo)
        found: set[int] = set()

        # --- 段階 1: そもそも前を読む必要があるか（Noul）--------------
        stage1 = {
            f"needs:u{position}": {
                "type": "noul",
                "instructions": CONTEXT_STAGE1.format(body=bodies[position]),
            }
            for position in todo
            if bodies[position].strip()
        }
        answers = ask_some(stage1, f"context1.w{wave}")
        # 送れなかった question は「聞かなかった」と同じに倒す — 前提が
        # 付かないだけで、注釈としては従来どおりになる安全側である。
        askers = [
            position
            for position in todo
            if f"needs:u{position}" in answers
            and noul_of(answers, f"needs:u{position}") >= CONTEXT_YES
            and position > 0
        ]

        def choose(targets: list[int], label: str, exclude: dict[int, set[int]]) -> None:
            """段階 2 / 段階 4 に共通の Choice 1 往復。"""
            questions: dict = {}
            pools: dict[int, list[int]] = {}
            for target in targets:
                pool = context_candidates(units, bodies, target, exclude[target])
                if not pool:
                    trim["no_candidate"] += 1
                    continue
                if len(pool) == 1:
                    # 候補が 1 つなら答えは決まっている（[`assign_lone_cores`]
                    # と同じ扱い）。question を使わずに採る。
                    trim["forced_single"] += 1
                    picks.setdefault(target, []).append(pool[0])
                    found.add(pool[0])
                    continue
                question, kept = context_choice(bodies, target, pool, budget)
                if not kept:
                    trim["no_candidate"] += 1
                    continue
                if len(kept) < len(pool):
                    trim["by_budget"] += 1
                trim["asked"] += 1
                questions[f"{label}:u{target}"] = question
                pools[target] = kept
            got = ask_some(questions, f"{label}.w{wave}")
            for target in pools:
                key = f"{label}:u{target}"
                if key not in got:
                    continue
                choice = choice_of(got, key, questions[key]["criteria"])
                won = int(choice.split(":")[1])
                picks.setdefault(target, []).append(won)
                found.add(won)

        # --- 段階 2: どれか（Choice）----------------------------------
        choose(askers, "pick", {target: set() for target in askers})

        # --- 段階 3: ほかにもあるか（当て木の Noul）-------------------
        splint_targets = [target for target in askers if picks.get(target)]
        splint = {
            f"more:u{target}": {
                "type": "noul",
                "instructions": CONTEXT_SPLINT.format(
                    target=bodies[target], known=bodies[picks[target][0]]
                ),
            }
            for target in splint_targets
        }
        answers = ask_some(splint, f"context3.w{wave}")
        second = [
            target
            for target in splint_targets
            if f"more:u{target}" in answers
            and noul_of(answers, f"more:u{target}") >= CONTEXT_YES
        ]

        # --- 段階 4: 2 本目（選んだものを外した Choice）。ここで止める ---
        choose(second, "pick2", {target: set(picks[target]) for target in second})

        waves.append(
            {
                "wave": wave,
                "asked_stage1": len(stage1),
                "askers": len(askers),
                "splint_yes": len(second),
                "new_prereqs": sorted(found - visited),
            }
        )
        frontier = sorted(found - visited)

    return picks, {"seeds": seeds, "waves": waves, "trim": trim}


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


def api_key() -> str:
    """環境変数 `TYPESAFE_API_KEY` **だけ**を見る。値は絶対に出力しない。"""
    key = os.environ.get("TYPESAFE_API_KEY", "").strip()
    if not key:
        raise JevError(
            "TYPESAFE_API_KEY is not set. Run `export TYPESAFE_API_KEY=...` "
            "and start akapen again (examples/semantic/README.md says where "
            "the key lives)."
        )
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


#: 送れなかった Tier question の Unit に入れる Reading Tier。
#:
#: **選んだ形と理由**（2026-09-21）。`state` + その question だけで 32k を
#: 超える Unit には Tier を聞けない。核には「聞かなければ Unit 全体が MARKED」
#: という安全側の落とし先があるが、**Tier には対応するものが無い**ので、
#: ここで決める。
#:
#: 既定値を置く・Unit を落とす・文書全体を失敗させるの 3 つから、**既定値
#: `detail` ＋ `core_atoms: []`** を採った。
#:
#: 1. `core_atoms: []` は「核を持たない」＝ **MARKED にならない**（プロトコルの
#:    3 値。`docs/gotchas.md`）。だから既定の Tier が決めるのは「**いつ沈むか**」
#:    だけで、「読む価値が高い」と嘘をつく経路は最初から無い
#: 2. `detail` を選んだのは `policy::decorate` の打ち切り方のためである。
#:    `keep_order` は Tier を第 1 キーにして長さの**昇順**に並べ、`decorate` は
#:    **最初に予算へ入らなかった Unit で `break` する**。送れないほど巨大な
#:    Unit（この経路に来るには本文だけで 18 KB 前後が要る）を上位 Tier に置くと、
#:    そこで打ち切られて**その下の Unit が丸ごと巻き添えで沈む**。`detail` なら
#:    順序の最後尾に来るので、被害はその Unit 自身に閉じる
#: 3. **Unit を落とす案は却下した。** `decorate` の `total` はすべての Unit の
#:    バイト長の合計なので、Unit を消すと Budget の分母が黙って縮み、
#:    「Budget 50 %」が指す量が文書によって変わる
#: 4. **文書全体を失敗させる案も却下した。** 核の既存の落とし先（絞り込めない
#:    だけで注釈としては壊れない）と作法を揃えた。1 つの巨大な Unit のために
#:    文書全体の注釈を失うほうが損失が大きい
#:
#: **実測ではどの文書でも 0 件だった**（5 文書。
#: `examples/semantic/measurements/request-splitting.md`）。この経路に来るのは
#: 1 つの Unit の本文が 18 KB 前後になる文書だけなので、**動いたところを
#: 見ていない落とし先**である。
UNANSWERED_TIER = "detail"


def annotate(request: dict, model: str, timeout: float) -> dict:
    version = request.get("version")
    if version != VERSION:
        raise JevError(f"unsupported protocol version: {version!r}")
    state = request.get("source") or ""
    atoms = request.get("atoms") or []
    if not atoms:
        return {"version": VERSION, "units": []}

    # 3 ラウンドで共有する予算。`state` のトークン数はここで 1 度だけ決める
    # （大きい文書では 1 リクエスト使って実測する。[`measure_state_tokens`]）。
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

    # --- ラウンド 1: 散文どうしの境界 -> Unit --------------------------
    #
    # 送れなかった境界は **NEW_UNIT** へ倒す。SAME だと巨大な Atom どうしが
    # さらに大きな Unit になり、ラウンド 2 の Tier question も送れなくなる。
    # NEW なら Unit は小さいままなので、後のラウンドが救える側へ倒れる。
    plan = plan_boundaries(atoms, state)
    questions = boundary_questions(atoms, plan)
    if questions:
        answers = ask(questions, "boundary")
        for entry in plan:
            key = f"boundary:{entry['after_atom']}"
            if entry["decision"] is None and key not in answers:
                entry["decision"], entry["by"] = NEW, "rule:question_too_large"
        apply_boundary_answers(plan, answers)
    units = group_units(atoms, plan)

    # --- ラウンド 2: Unit ごとの Tier ----------------------------------
    #
    # **1 つも送れなかったら失敗させる。** [`UNANSWERED_TIER`] は個別の巨大な
    # Unit のための落とし先であって、文書全体の落とし先ではない。全部が既定値に
    # なった注釈は「それらしく見えるが、何も判定していない」ものになる
    # （[`apply_boundary_answers`] が黙って埋めないのと同じ理由）。実測では
    # 83 KB の文書がここに来て、**530 Unit すべてが detail** の応答を exit 0 で
    # 返していた。`state` は 32k に収まっているので probe は通り、**question の
    # ぶんだけが足りない**という、いちばん気づきにくい壊れ方をする。
    questions = unit_questions(atoms, units)
    answers = dict(ask(questions, "tier"))
    unanswered = set(unsent.get("tier", ()))
    if questions and len(unanswered) == len(questions):
        raise JevError(
            "Document too large for Jev — shrink it. Splitting cannot help: "
            f"state alone is {budget.state_tokens} tokens, leaving "
            f"{max(budget.pair, 0)} of the 32k budget, so not one Tier "
            "question fits."
        )
    tiers = [
        UNANSWERED_TIER
        if f"tier:u{n}" in unanswered
        else choice_of(answers, f"tier:u{n}", TIER_CRITERIA)
        for n in range(1, len(units) + 1)
    ]

    # --- ラウンド 3: redundancy（SUPPORTING 以上）と、核 ----------------
    #
    # 2 つを 1 ラウンドにまとめている。核を聞く相手は本来「ESSENTIAL かつ非
    # REDUNDANT」だが、redundancy の答えを待つとラウンドが 4 つになる。
    # 実測では 4 文書とも「ESSENTIAL かつ REDUNDANT」は 0〜1 件しかないので、
    # 捨てることになる核の question はほぼ出ない。
    #
    # ここで送れなかった question は、どちらも**聞かなかった場合と同じ**に
    # 倒れる。redundancy を聞かなければ REDUNDANT にならず、核を聞かなければ
    # Unit 全体が MARKED になる — どちらも既存の安全側の振る舞いである。
    #
    # redundancy の Choice は先行 Unit の本文を全部並べるので、context
    # preservation の段階 2 と同じく文書のどの question よりも大きくなりうる。
    # 予算に当たったら遠い候補から落とす（[`redundancy_choice`]）。
    provisional = [
        {
            "id": f"u{n}",
            "atoms": list(ix),
            "reading_tier": tiers[n - 1],
            "relations": [],
            "jev": {},
        }
        for n, ix in enumerate(units, start=1)
    ]
    # run キャップ（1 本のリストにつき核 1 つ）を先に決める。残りは従来どおり
    # Unit ごとに聞く。
    runs = unit_runs(units, plan)
    run_questions, fixed, scope, handled = plan_run_cores(atoms, provisional, runs, budget)
    questions, redundancy_trim = redundancy_questions(atoms, units, tiers, budget)
    questions.update(run_questions)
    questions.update(core_questions(atoms, provisional, budget, handled))
    if questions:
        third = ask(questions, "core")
        for key in unsent.get("core", ()):
            # 聞けなかった question は「そもそも出さなかった」ことにする。
            questions.pop(key, None)
            scope.pop(key, None)
        answers.update(third)

    built = build_units(atoms, units, tiers, answers, questions)
    # 節の見出しは構造だけで決まる。Jev は出てこない（[`assign_sections`]）。
    assign_sections(atoms, units, built)
    bodies = [unit_body(atoms, indices) for indices in units]
    for unit, source_unit in zip(built, provisional):
        unit["jev"].update(source_unit["jev"])
    for number in range(1, len(units) + 1):
        if f"tier:u{number}" in unanswered:
            built[number - 1]["core_atoms"] = []
            built[number - 1]["jev"]["tier_by"] = "rule:question_too_large"
    apply_run_cores(built, questions, fixed, scope, answers)
    assign_lone_cores(atoms, built, handled)
    apply_core_answers(built, questions, answers)
    # REDUNDANT だった Unit は MARKED にならないので、核は使われない。
    # **`relations` が空でないこと、ではない**（[`is_redundant`]）。
    for unit in built:
        if is_redundant(unit):
            unit.pop("core_atoms", None)
            unit["jev"].pop("core_choice", None)
            unit["jev"].pop("core_by", None)

    # --- ラウンド 4 以降: context preservation の前提 -------------------
    #
    # **ここまでの答えが全部要る。** 聞き始める Unit は「MARKED になりうる
    # Unit」で、それが決まるのは Tier・redundancy・核が出そろったあと、
    # つまり上のエピローグの**後**である（[`context_seeds`]）。前へ動かすと
    # 光らない Unit にも聞くことになり、question が無駄に増える。
    #
    # ラウンド数が固定でないのはここだけである。波ごとに段階 1〜4 の
    # 4 往復で、実測では波は 2〜4 だった。
    prerequisites, context_report = trace_prerequisites(built, bodies, budget, ask)
    for target, sources in prerequisites.items():
        for source in sources:
            built[target]["relations"].append({"presupposes": built[source]["id"]})
        built[target]["jev"]["presupposes"] = [built[s]["id"] for s in sources]

    report = {
        "rounds": rounds,
        "boundaries": plan,
        "budget": budget.record(),
        "redundancy": redundancy_trim,
        "context": context_report,
    }
    if unsent:
        report["unsent"] = unsent
    return {"version": VERSION, "units": built, "jev": report}


def dry_run(request: dict, model: str, state_tokens: int | None = None) -> dict:
    """API を叩かずに、送るリクエストの形を出す。

    後のラウンドは前のラウンドの答えに依存するので、仮定を置いて組む。

    - ラウンド 2: **Jev に聞く境界はすべて NEW_UNIT だった**と仮定する
      （構造ルールで決まった境界はそのまま効く）
    - ラウンド 3: **すべての Unit が ESSENTIAL だった**と仮定する。本番では
      redundancy は SUPPORTING 以上にだけ、核は ESSENTIAL かつ散文の候補が
      2 つ以上ある Unit にだけ聞くので、実際に送る question はこれより少ない
    - ラウンド 4: **context preservation の最初の波だけ**を、段階 1 が
      全部「はい」だったと仮定して組む。本番の波の本数は答え次第なので
      ここには出ない（[`trace_prerequisites`]）

    ラウンド 4 を出すのは、**32k 枠に当たるとしたらここだから**である。
    段階 2 の Choice は自分より前の Unit の本文を全部並べるので、文書の
    どの question よりも大きくなりうる（実測の 4 文書では当たらなかったが、
    もっと大きい文書は測っていない）。**逆に言えば、ここに出る 1 波ぶんが
    本番の下限**である — 波が 2〜4 回るぶんリクエストは増える。

    仮定は戻り値の `assumptions` にも載せる — 形だけ見て「これが本番で送る
    question 数だ」と読まれると困るため。
    """
    atoms = request.get("atoms") or []
    state = request.get("source") or ""
    plan = plan_boundaries(atoms, state)
    first = boundary_questions(atoms, plan)
    for entry in plan:
        if entry["decision"] is None:
            entry["decision"] = NEW
    units = group_units(atoms, plan)
    tiers = ["essential"] * len(units)
    as_essential = [
        {
            "id": f"u{number}",
            "atoms": indices,
            "reading_tier": "essential",
            "relations": [],
            "jev": {},
        }
        for number, indices in enumerate(units, start=1)
    ]
    budget = (
        RequestBudget(state_tokens, measured=True)
        if state_tokens is not None
        else RequestBudget.estimated(state)
    )
    run_questions, _fixed, _scope, handled = plan_run_cores(
        atoms, as_essential, unit_runs(units, plan), budget
    )
    third, _trim = redundancy_questions(atoms, units, tiers, budget)
    third.update(run_questions)
    third.update(core_questions(atoms, as_essential, budget, handled))
    # ラウンド 4（最初の波だけ）。段階 1 は全 seed に、段階 2 は段階 1 が
    # 全部「はい」だったと仮定して組む。
    bodies = [unit_body(atoms, indices) for indices in units]
    fourth = {
        f"needs:u{position}": {
            "type": "noul",
            "instructions": CONTEXT_STAGE1.format(body=bodies[position]),
        }
        for position in context_seeds(as_essential)
        if bodies[position].strip()
    }
    for position in context_seeds(as_essential):
        pool = context_candidates(as_essential, bodies, position, set())
        if len(pool) < 2:
            continue
        question, kept = context_choice(bodies, position, pool, budget)
        if kept:
            fourth[f"pick:u{position}"] = question

    rounds = []
    for number, questions in enumerate(
        (first, unit_questions(atoms, units), third, fourth), start=1
    ):
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
            "round 2 assumes every boundary Jev is asked about is new_unit",
            "round 3 assumes every Unit is essential (production narrows this, "
            "so it sends fewer questions)",
            "round 4 shows only the first context-preservation wave, assuming "
            "stage 1 said yes everywhere; production runs 2-4 waves, so this "
            "is a lower bound on requests and an upper bound on one wave's size",
            "when the state token count is an estimate (0.5 tokens/byte) this "
            "splits into more chunks than production, which measures the count "
            "on large documents (--state-tokens passes a measured value in)",
        ],
        "budget": budget.record(),
        "rounds": rounds,
    }


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
        help="print the shape of the 3 rounds without calling the API "
        "(round 2 assumes every boundary is new_unit, round 3 that every "
        "Unit is essential)",
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
