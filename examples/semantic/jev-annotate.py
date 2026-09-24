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

## ラウンドは 1 つ — 散文の Atom ごとに Noul 1 問

    state=文書全文, questions={ 散文の Atom ごとに、いまの問いへの Noul }
      → スコアが確定する。光るのはスコアを付けた Atom そのもの

**Unit = 散文の Atom 1 つ**（[`PROSE_KINDS`]: 文・リスト項目・引用・表の
データ行）。見出し・コードブロック・表のヘッダ行には **Unit を作らない** —
スコアを聞かず、光ることもない。応答がすべての Atom を覆わなくてよいことは
akapen 側が約束している（`crates/semantic-reading/src/document.rs` の
`validate`。どの Unit にも属さない Atom は NORMAL のまま表示される）。

各 Unit の `core_atoms` は `[その Atom]` と**明示する**。省いても「絞り込み
無し ＝ Unit 全体が核」で光り方は同じだが、`[]`（核を持たない ＝ 光らない）と
取り違えないよう、3 値のどれを言っているかを字面に出す
（`crates/semantic-reading/src/unit.rs` の `has_core` / `is_core`）。

Jev は question を**並列・独立に**評価するので、全部を 1 リクエストで聞ける
（大きな文書は [`send_in_chunks`] が分ける）。**1 段である**（設計書「Jev への
問いは 1 段に保つ」）— ラウンドが 1 つなので、前の答えを次の問いの前提に
差し込む連鎖は原理的に無い。

**問いの文面は akapen が送ってくる。** 正本は akapen 側の
`assets/marks-questions.json` 1 か所で、このスクリプトは枠（[`MARKS_FRAME`]）と
本文を足すだけの汎用の器である。2 か所に置くとずれる。要求の question に
入っている `core_floor`（核を聞く Unit の足切り）は**もう使わない**。akapen は
まだ送ってくるが、読まずに捨てる。

## なぜ文ごとか — Unit の境界も核も Jev に決めさせない（2026-09-24）

2026-09-24 までは 3 ラウンドだった。散文どうしの境界を Jev に Choice で聞いて
Unit を組み（答えはキャッシュし、同時に走るプロセスは flock で 1 本に絞る）、
Unit ごとに Noul を聞き、足切りを越えた Unit の中で光らせる一文（核）を
Choice で選ぶ。Unit も境界の問いも DIM 版の都合で作ったもので、marks だけに
なってから 2 本の実測で測り直した
（`examples/semantic/measurements/unit-granularity.md` / `core-question.md`、
切り替えたあとの通しと既定のつまみは `unit-per-atom.md`）:

- 文ごと（B）は、核の問いを直した 3 ラウンド（A'）より正解ラベルの取りこぼしが
  少ない（`showcase`、つまみ 100 % で本数を揃えて 0 件対 8 件）。A' の 8 件の
  うち 7 件は「1 Unit から光るのは 1 文だけ」の制限で、核の問いをどう直しても
  減らない
- 実物（業務議事録・web 記事）で A' と B が食い違った印を人が判定すると、
  **どちらか片方だけが光らせた印も全部「要る」**だった。精度は同じ
- 費用は 14 % 安く（境界と核のラウンドで state を送り直さない）、ランごとの
  揺れも小さい（境界の揺れも核の Choice の揺れも無い）
- 弱点: 見出しの無い文書で、元は見出しだった行が光る率は A' 7.3 % 対 B 12.8 %

**Unit を Jev に決めさせる形へ戻すと、次のものが一緒に戻ってくる:**

- 1 Unit から光るのは 1 文だけになる。問いに答えている文が同じ Unit に 2 つ
  あると片方は光らない（`showcase` の取りこぼしの大半がこれだった）
- 光らせる一文を選ぶ問い（核の Choice）が要る。その文面がいまの問いを
  見ていないと、numbers で光った Unit でも「要点」の文が光る（2026-09-24 の
  朝まで実際にそうだった）
- 境界の問いは、見出しの無い文書では境界の 7〜9 割に及ぶ。問いを変えるたびに
  聞き直さないためのキャッシュと、Review のルールが同時に起こすプロセスどうしの
  排他（flock）が要る（`docs/gotchas/semantic-reading.md` に過去の地雷として
  残してある）
- つまみの既定が合わなくなる（`crates/semantic-reading/src/marks.rs` の
  `DEFAULT_SHARE` は、Unit = 散文の Atom を分母にして決め直した値）

**focus は 2026-09-24 に決め直した。** 「光った文と見出しのほかを沈め、
琥珀の線を消す」（読み手）で、1 文の Unit のままで成り立つ。それまでの
「光った Unit の核でない文を沈めない」は 1 文の Unit では空振りしていた
（設計書の focus の節）。

## context window は 2 つの制約で縛られている

公式値は「**64k tokens per request; 32k tokens for `state` plus the longest
question**」（`docs.typesafe.ai/models.md`）。**後者を見落とすと、合計が 64k に
収まっているのに 400 で落ちる** — 実測でも state 30k + question 3k（合計 33k）
が失敗した。いまの question は文 1 つの Noul なので、ここに当たるのは state
だけで 32k 枠の大半を使う文書である。上限は [`RequestBudget`] が state の
大きさから毎回計算する。

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
import json
import os
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

#: 名乗る User-Agent。**urllib の既定（`Python-urllib/3.x`）は Cloudflare に
#: error 1010（署名で拒否）の 403 で弾かれる**（2026-09-23 に踏んだ。curl や
#: 独自の名前なら通る）。空にも既定にも戻さないこと。
USER_AGENT = f"akapen-jev-annotate/{VERSION}"

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
#:   削除した DIM 版の実測である**。いまは probe と marks の 2 種で、probe は
#:   大きな文書でしか飛ばない。2026-09-24 に境界と核のラウンドも外した）
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
#: 実測（2026-09-21、4 文書の state）は 0.34〜0.39 tokens/byte、核 question
#: （2026-09-24 に外した）の選択肢本文 ＝ 文の本文は 0.496 tokens/byte だった。
#: 上限の判定に使うので、**足りないより多く見積もる方が安全**である（見積もりが
#: 小さすぎると上限を超えた question を送って 400 で落ちる）。日本語と英語が
#: 混ざる文書で最も高かった 0.496 を丸めた。
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

#: リクエスト全体の予算から引く安全マージン。内訳は [`PAIR_MARGIN`]
#: と同じ（見積もり誤差と、公式値と実測の切れ目のずれ）。
REQUEST_MARGIN = 2_048

#: `state` + question 1 つの予算（32k の側）から引く安全マージン。内訳:
#:
#: - question の定型文と criteria のキー名（実測で 1 question あたり 68 tokens）
#: - [`TOKENS_PER_BYTE`] の見積もり誤差。state が大きいほど絶対値で効く
#: - 公式値 32k と実測の切れ目のずれ（state 20k + question 12k = 32,305 は
#:   通り、合計 35k は落ちた。境界は 32,768 付近だが厳密には詰めていない）
#:
#: 2026-09-24 まではいちばん大きい question が核の Choice（Unit の全散文を
#: 選択肢に並べる。45.6 KB の実文書で選択肢 82 個・実測 5,469 tokens）で、
#: 名前も `CORE_QUESTION_MARGIN` だった。いまの question は文 1 つの Noul
#: なので、ここに当たるのは state だけで 32k 枠の大半を使う文書である。
PAIR_MARGIN = 2_048

#: state のトークン数を**実測しに行く**閾値（見積もりがこれを超えたら測る）。
#:
#: 32k 枠の半分。ここを下回っていれば、[`TOKENS_PER_BYTE`] が実測の 1.4 倍まで
#: 過大評価していても真値は 16k を超えず、1 つの question に 14k 以上が残る。
#: いまの question は文 1 つの Noul（問いの文面 67〜194 字 + その文）で、
#: 14k に届く文は無い。
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

#: **Unit を作る** Atom の種別 — **散文だけ**（[`prose_units`]）。
#:
#: 見出し・コードブロック・表のヘッダ行（`table`）には Unit を作らない —
#: スコアを聞かず、光らない。見出しは中身を持たず、コード例は要点の言い換えでは
#: なく、ヘッダ行は列の名前を挙げても中身を言ったことにならない。種別は構文の
#: 話なので、設計書「Jev に判断させないもの: **syntax parsing**」のとおりここで
#: 落とす。3 ラウンドの頃もこれらは核の候補に入れておらず、文ごとに聞いた測定
#: （`unit-granularity.md` の B）でも一度も光っていない — 外しても光り方は
#: 変わらず、問いの数だけが減る。
#:
#: **引用（`block_quote`）は散文なので入る。** 2026-09-22 まで外れていて、
#: 引用だらけの文書では一度も光らなかった。**表のデータ行（`table_row`）も
#: 入る** — 表の中身は行単位で光る。
PROSE_KINDS = frozenset({"sentence", "list_item", "block_quote", "table_row"})


class JevError(Exception):
    """akapen のステータス行に 1 行で出したい失敗。"""


# ---------------------------------------------------------------------------
# Unit — 散文の Atom 1 つ
# ---------------------------------------------------------------------------


def atom_text(atom: dict) -> str:
    """Atom の本文。前後の空白は落とす（末尾の改行を含む Atom がある）。"""
    return (atom.get("text") or "").strip()


def prose_units(atoms: list[dict]) -> list[int]:
    """Unit にする Atom の添字（文書順）。**Atom 1 つが Unit 1 つになる。**

    [`PROSE_KINDS`] に入り、本文が空でない Atom だけ。本文が空の Atom は
    Jev に見せても判断材料が無く、光らせる中身も無い（実測の 6 文書には
    1 つも無かった）。

    **境界は聞かない**（冒頭の「なぜ文ごとか」）。構造のルールも Jev も
    使わず、種別だけで決まる。
    """
    return [
        index
        for index, atom in enumerate(atoms)
        if atom.get("kind") in PROSE_KINDS and atom_text(atom)
    ]


# ---------------------------------------------------------------------------
# 答えの取り出し（緩めない）
# ---------------------------------------------------------------------------


def answer_of(answers: dict, key: str) -> dict:
    answer = answers.get(key)
    if not isinstance(answer, dict):
        raise JevError(f"Jev answered nothing for {key}")
    return answer


def noul_of(answers: dict, key: str) -> float:
    got = answer_of(answers, key).get("noul")
    if not isinstance(got, (int, float)):
        raise JevError(f"Jev returned no noul for {key}: {got!r}")
    return float(got)


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
#: 1 ラン（大きな文書では数本のリクエスト）のあいだ `security` を叩き続ける。
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
            "User-Agent": USER_AGENT,
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
# いまのラウンドはスコアの 1 本だけだが、ここはラウンドの事情を知らない
# 汎用の道具として置いてある。問いを足すときも同じ 2 つの崖（64k と 32k）を
# 持つので、足したラウンドもここを通すこと。
# ---------------------------------------------------------------------------


def estimate_tokens(text: str) -> int:
    """バイト数からトークン数を見積もる（[`TOKENS_PER_BYTE`]）。"""
    return int(len(text.encode()) * TOKENS_PER_BYTE)


def question_tokens(question: dict) -> int:
    """question 1 つのトークン数の見積もり。

    器（[`QUESTION_OVERHEAD`]）＋ instructions と criteria のテキスト。
    criteria の**キーも数える** — Choice の選択肢はその個数だけキーが並ぶ
    ので、選択肢の多い question では無視できない（いまの Noul は criteria を
    持たないが、器としてはどちらも同じに数える）。

    **個数で切らずにこれで切る。** 同じ 10 個の question でも、短い文と
    表の長い行では何倍も違う。`docs/gotchas/open-questions.md`「核の
    question は個数ではなくトークンで切る」と同じ理由で、固定の個数は
    どの文書でも正しくない。
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
        self.pair = STATE_PLUS_QUESTION_LIMIT - PAIR_MARGIN - state_tokens

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
    question のキーは Unit の番号を持っていて、順序が変わると
    デバッグのとき突き合わせられなくなる。答えはキーで戻すので、分け方が
    答えを変えることはない。

    **`pair` を超える question は誰にも送れない。** 分割はリクエストの数を
    増やすだけで `state` を小さくしないので、`state + その question` が 32k を
    超える question は、チャンクを 1 つにしても救えない。呼び手がそれぞれの
    落とし先を決める（[`marks_annotate`] を見ること）。
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
    答えが足りないほうは [`noul_of`] が捕まえる（黙って埋めない）。
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


def marks_questions(atoms: list[dict], units: list[int], text: str) -> dict:
    """スコアのラウンド — **散文の Atom ごとに Noul 1 問**。

    `units` は [`prose_units`] が返す Atom の添字。キーの `u<n>` は Unit の
    通し番号（1 始まり、文書順）で、応答の Unit の `id` と同じである。

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
            "instructions": text + MARKS_FRAME.format(body=atom_text(atoms[index])),
        }
        for number, index in enumerate(units, start=1)
    }


def marks_annotate(request: dict, question: dict, model: str, timeout: float) -> dict:
    """解析の本体 — 散文の Atom ごとに Noul 1 問の 1 ラウンド。**唯一の入口である。**

    **run キャップは掛けない。** 1 本のリストの項目が全部光るのは「答えが
    全部光る」で正しい — **問いが既に選んでいる**ためである。量はつまみが
    受け持つ（`measurements/marks-mode.md`）。

    **送れなかった question の Unit はスコアを持たない**（光らない）。
    Unit そのものは残す — 応答の Unit の並びは、送れたかどうかに
    かかわらず散文の Atom の並びと同じである。
    """
    state = request.get("source") or ""
    atoms = request.get("atoms") or []
    text = question.get("text")
    if not isinstance(text, str) or not text.strip():
        raise JevError("the request carries a question with no text")
    # `question["core_floor"]` は読まない。核のラウンドはもう無い（冒頭の
    # docstring）。akapen がまだ送ってくるので、あっても無くても同じに動く。

    budget = measure_state_tokens(state, model, timeout)
    rounds = [budget.probe] if budget.probe else []
    units = prose_units(atoms)

    # **1 つも送れなかったら失敗させる。** 全部が「スコア無し」の応答は、
    # akapen 側では 0 本と区別が付かない（`marks::has_scores`）。そこは
    # 「答えている箇所が無い」という意味を持つ場所なので、送れなかったことを
    # そこへ混ぜてはならない。
    questions = marks_questions(atoms, units, text)
    answers, records, dropped = send_in_chunks(state, questions, budget, model, timeout)
    for record in records:
        record["round"] = "marks"
    rounds.extend(records)
    unanswered = set(dropped)
    if questions and len(unanswered) == len(questions):
        raise JevError(
            "Document too large for Jev — shrink it. Splitting cannot help: "
            f"state alone is {budget.state_tokens} tokens, leaving "
            f"{max(budget.pair, 0)} of the 32k budget, so not one question fits."
        )

    built = []
    for number, index in enumerate(units, start=1):
        key = f"marks:u{number}"
        out = {"id": f"u{number}", "atoms": [index], "core_atoms": [index]}
        if key not in unanswered:
            out["score"] = noul_of(answers, key)
        built.append(out)

    report = {
        "rounds": rounds,
        "budget": budget.record(),
        "question": question.get("id"),
    }
    if dropped:
        report["unsent"] = {"marks": dropped}
    return {
        "version": VERSION,
        "question": question.get("id"),
        "units": built,
        "jev": report,
    }


def dry_run(request: dict, model: str, state_tokens: int | None = None) -> dict:
    """API を叩かずに、送るリクエストの形を出す。

    ラウンドはスコアの 1 本だけで、前の答えに依存する問いが無いので、
    **答えについての仮定は置かない**。本番と違いうるのは state のトークン数の
    出どころだけで、それは戻り値の `assumptions` に書く（形だけ見て
    「これが本番で送るチャンク数だ」と読まれると困るため）。

    **問いが要る。** 問いを持たない要求は本番と同じく断る — 形だけ見たい
    場合でも、問いの文面が question の大きさをそのまま決めるので、載せずに
    出した数字は本番の予測にならない。
    """
    atoms = request.get("atoms") or []
    state = request.get("source") or ""
    question = request.get("question")
    if not isinstance(question, dict) or not isinstance(question.get("text"), str):
        raise JevError("--dry-run needs a request that carries a question")

    budget = (
        RequestBudget(state_tokens, measured=True)
        if state_tokens is not None
        else RequestBudget.estimated(state)
    )
    questions = marks_questions(atoms, prose_units(atoms), question["text"])
    chunks, dropped = plan_chunks(questions, budget)
    return {
        "assumptions": [
            "when the state token count is an estimate (0.5 tokens/byte) this "
            "splits into more chunks than production, which measures the count "
            "on large documents (--state-tokens passes a measured value in)",
        ],
        "budget": budget.record(),
        "rounds": [
            {
                "round": "marks",
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
        ],
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
        help="print the shape of the request without calling the API "
        "(one Noul per prose Atom, the same as production)",
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
