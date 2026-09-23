# Semantic Reading Layer のデモ

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo-marks.json
```

view モードで `m` が問いの popup（`M` は逆回り）、`/` で自由入力。`-` / `+`（`=` も可）が
つまみ ±1、`<` / `>` が ±10 で、現在値はステータス行に `MARK 20% · 3 · Essential`
として出る。

判定器を繋いで試すなら、**架空の議事録** `showcase.md` が向いている（決定事項・
要確認・宿題・継続議題・費用と日程の表・引用・コードブロックを 1 本に入れてある。
登場する団体・人物・数字はすべて作り物）:

```sh
akapen examples/semantic/showcase.md   # AKAPEN_SEMANTIC_CMD を設定してあれば、これだけ
```

`Settled` で末尾の決定事項が、`Unsettled` で要確認・継続議題が、`Numbers` で表の行が、
`/` に「費用」「日程」と打てばそれぞれの箇所が光る。

`f` を押すと、光っていない箇所が沈む（もう一度押すと戻る）。`]m` / `[m` で
次・前のマーク行へ飛ぶ。

**スコアの無い注釈を開くと光らない。** そのとき読み出しは
`no scores in this answer — the analyser returned none` と**誰のせいか**を
言う。「この問いに答えている箇所が無い（0 本）」とは別のことなので、黙った
空白にはしない。

`--semantic` を渡さなければ、これらのキーは**束縛されない**。読み出しも
`?` ヘルプの行も出ず、akapen はこの層が無かったときと完全に同じ動きをする。

見え方の強さは 2 つのフラグで調整できる（既定は実機で選んだ値）。

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo-marks.json \
  --mark-blend 0.22 --dim-blend 0.60
```

- `--mark-blend` — MARKED の背景をページからテキスト色の方へどれだけ
  持ち上げるか。既定 0.22（dark で `rgb(68,70,89)`）
- `--dim-blend` — **フォーカス（`f`）で沈む**前景をページの方へどれだけ
  寄せるか。既定 0.60（dark の本文なら `rgb(205,214,244)` →
  `rgb(99,103,125)`）

沈みは `Modifier::DIM`（SGR `2`）ではなく**実際の色**である。SGR `2` は
無視する端末が多く、MARKED と見分けがつかなかったため。

## 見どころ

`## 結論` の下の 1 行:

```
採用する方式は差分配信である。詳細は付録にまとめた。
```

これは **1 つの source 行**だが、前半は Unit `u3` の**核**、後半は別の
Unit `u4` に属する。

| つまみ | 前半「採用する方式は差分配信である。」 | 後半「詳細は付録にまとめた。」 |
| ---- | -------------------------------------- | ------------------------------ |
| 20%  | MARKED                                 | NORMAL                         |
| 50%  | MARKED                                 | MARKED                         |
| `f`  | MARKED                                 | 沈む（20 % のとき）            |

**行の途中で表示状態が切り替わる**のがこの層の看板である。

つまみを上げると光る集合は**入れ子で広がる**（一度光ったものは消えない）。
`demo-marks.json` の目安:

| つまみ | 光る本数 |
| ---- | ---: |
| 1%   | 1 |
| 20%  | 3 |
| 50%  | 7 |
| 100% | 7（足切りが上限を作る） |

13 Unit のうち足切り 0.20 を越えるのが 7 本なので、**つまみを 100 % まで
上げても 7 本で止まる**。「N % を上げれば全部光る」ではない。

## 外部コマンドに判断させる（`--semantic-cmd`）

fixture ではなく、**外部コマンドに意味判断を返させる**経路もある。

```sh
akapen examples/semantic/demo.md \
  --semantic-cmd "python3 examples/semantic/annotate-doc.py"
```

`annotate-doc.py` は Jev を呼ばない**決定論的な参照実装**である。目的は
API キー無しでパイプライン全体を端から端まで動かせることで、判断そのものは

```text
見出し               -> 0.90
見出し直後の 1 Atom  -> 0.70
code block / table   -> 0.05
それ以外             -> 0.30
直前の文と語が重なる -> スコアを半分に
```

という素朴なヒューリスティクスでしかない。**問いの文面は読まない** —
どの問いで呼ばれても同じスコアを返す。問いの無い要求は明確なエラーで断る。実際の Jev（LLM ではなく
System One モデル。`docs/design/jev.md` 参照）への question 設計はここには無い。

### プロトコル

akapen → コマンド（stdin、JSON 1 行）:

```json
{"version": 1,
 "source": "<文書全文>",
 "question": {"id": "essential", "text": "<問いの文面>", "core_floor": 0.2},
 "atoms": [{"index": 0, "kind": "heading", "range": {"start": 0, "end": 12}, "text": "## 見出し"}]}
```

**`question` は必ず載る。** 文面は akapen が持って送るので（正本は
`assets/marks-questions.json`）、コマンドは文面を知らない汎用の器でよい。
問いの無い要求を受け取ったコマンドは、黙って別のものを返さず**明確な
エラーで終わる**こと。

コマンド → akapen（stdout、JSON）:

```json
{"version": 1, "question": "essential",
 "units": [{"id": "u1", "atoms": [0], "score": 0.94, "core_atoms": [0]}]}
```

`score` は「いまの問いにどれだけ答えているか」（0.0〜1.0）、`core_atoms` は
「この Unit のどこだけ読めば要点が取れるか」。`question` は要求の id の echo で、
キャッシュの読み戻しの照合に使う。

**コマンドは range を返さない。** 返すのは Atom の index だけで、
`SemanticDocument` は akapen 自身の `atomize()` の出力から組み立てられる。
だから外部コマンドが壊れた位置を返して文書の違う場所を装飾する、という
事故が**原理的に起きない**。`--semantic` 経路で必要だった
`source_sha256` の照合も、この経路では要らない。

返ってきた JSON は全項目を検証し、1 つでも失敗したら**レスポンス全体を
捨てる**（部分適用は何もしないより悪い）。弾かれるのは version 不一致 /
範囲外の atom index / unit id の重複 / その Unit の Atom でない核。
同じ Atom を複数の Unit が主張した場合は**先勝ち**で、後の Unit からその
index を落とす。

### 非同期

コマンドは**別スレッド**で走る。外部プロセスの起動とネットワーク往復を
挟むので、同期実行すると UI が固まるため。解析中はステータス行が

```text
Essential · analyzing…
```

（フッタの右端。タイトル行は path とファイル位置だけである）

になる。解析中に文書が変わったら、古い方の結果は**世代カウンタで破棄**
される（変わった後の文書に変わる前の判定を当てない）。タイムアウトは
60 秒。異常終了・タイムアウト・JSON 不正はステータス行に理由を出すだけで、
直前の注釈（同じ文書のもの）は保持される。

`--semantic` と `--semantic-cmd` は排他である。

### 毎回打たない（`AKAPEN_SEMANTIC_CMD`）

`--semantic-cmd` を省いたときは、環境変数 `AKAPEN_SEMANTIC_CMD` が既定になる。

```sh
# ~/.zshrc に 1 行（パスは絶対で）
export AKAPEN_SEMANTIC_CMD='python3 /abs/path/to/akapen/examples/semantic/jev-annotate.py'
```

これで `akapen foo.md` が**どこからでも** marks で開く。フラグを書けばフラグが
勝つ。`--semantic <fixture>` と同時に指定すると今までどおり排他のエラーになり、
そのときは `(--semantic-cmd from AKAPEN_SEMANTIC_CMD)` と出どころが添えられる。
空にすれば（`export AKAPEN_SEMANTIC_CMD=`）一時的に外せる。

**キャッシュはコマンド行ごとに分かれる。** 引き当てに使うのは
`sha256(コマンド行そのもの)` なので、相対パスで測ったときのキャッシュは絶対パス
では当たらない（`akapen --semantic-cache-clear` で消せる）。

### 自分のコマンドを書く

stdin から 1 つの JSON を読み、stdout へ 1 つの JSON を書くだけでよい。

```sh
akapen doc.md --semantic-cmd 'python3 ./jev-annotate.py'
akapen doc.md --semantic-cmd './my-annotator.ts'
```

Jev を繋ぐなら、このコマンドが**アダプタ**になる。担うのは

```text
atoms → Jev の question 群 → Jev の typed answer → units
```

で、Jev は TypeSafe の System One モデル（**LLM ではない** — typed な
question を state に対して並列評価して構造化された値を返す）である。
primitive と呼び出し方は `docs/design/jev.md` を参照。

API キーの管理は akapen の責務ではない — コマンドが自分の環境で解決する
（Jev なら `TYPESAFE_API_KEY`、無ければ macOS のキーチェーン）。

## ファイル

| ファイル              | 中身                                                     |
| --------------------- | -------------------------------------------------------- |
| `demo.md`             | 日本語の設計メモ（AI が書きがちな、長く重複する文書の見本） |
| `demo-marks.json`     | それに対する `semantic-reading` の `SemanticDocument`     |
| `build-demo-marks-json.py` | `demo-marks.json` の生成スクリプト                   |
| `annotate-doc.py`     | `--semantic-cmd` プロトコルの参照実装（Jev を呼ばない）   |

**byte range は手で書かない。** `demo.md` を編集したら必ず

```sh
python3 examples/semantic/build-demo-marks-json.py
```

を走らせ直すこと。`demo-marks.json` は `source_sha256` で `demo.md` の中身を
名指ししているので、再生成を忘れると akapen は fixture を拒否して
`this fixture belongs to a different document` と言う（`src/semantic.rs` の
`DigestChecked`）。テスト
`semantic::tests::the_demo_fixture_names_the_current_demo_md` でも落ちる。

スクリプトはつまみごとの本数も出力する。**スコアは手で置いた値で、実測では
ない** — fixture が測っているのは投影の側（つまみ・足切り・核の絞り込み）で
あって、判定の質ではない。

## crate のテスト fixture との違い

`crates/semantic-reading/tests/fixtures/sample.{md,json}` は crate 単体の
テスト用で、**1 行の中で表示状態が切り替わる箇所を持たない**（同じ行に
並ぶ 2 つの Atom が同じ Unit に属しているため、常に同じ状態になる）。
こちらの demo はそれを実証するために別に用意したもので、crate 側の
fixture とは独立している。

## Jev に判定させる（`jev-annotate.py`）

`annotate-doc.py` が判断をしない参照実装なのに対し、`jev-annotate.py` は
**本番の判定器**である。同じ `--semantic-cmd` プロトコルを喋り、判断は Jev
（TypeSafe の System One モデル。**LLM ではない** — `docs/jev.md`）へ委譲する。

```sh
akapen examples/semantic/demo.md \
  --semantic-cmd 'python3 examples/semantic/jev-annotate.py'
```

### 鍵は `TYPESAFE_API_KEY`、無ければ macOS のキーチェーン

macOS なら**一度保存するだけ**でよい。

```sh
security add-generic-password -a "$USER" -s typesafe-jev -w
```

（`-w` で鍵を聞かれる。履歴に残さないため、引数には書かない。）

以後スクリプトは `TYPESAFE_API_KEY` を見て、無ければこの項目を読む。1 プロセス
のあいだ鍵は 1 回だけ取り出す。

**環境変数が先**で、そこが普遍の逃げ道である。mac 固有の読み方を埋めてよいのは
そのためで、他 OS の人は今までどおり export すれば済む。

```sh
# 1Password（akapen のプロセスに限って渡す）
op run --env-file=.env -- akapen doc.md

# pass
export TYPESAFE_API_KEY="$(pass show typesafe/api-key)"

# 平文（使い捨ての実験だけ）
export TYPESAFE_API_KEY=sk-…
```

どちらも無ければ非ゼロ終了し、ステータス行に何をすればよいかが 1 行で出る
（保存のコマンドもその 1 行に入る）。鍵は stdout にも stderr にも出さない。`TYPESAFE_BASE_URL` / `TYPESAFE_DEFAULT_MODEL`
も SDK と同じ名前で効く（`--model` / `--timeout` でも指定できる）。

### 3 ラウンド構成

スコアの question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

```text
ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice } → Unit を確定
             （**キャッシュに当たれば 0 問**）
ラウンド2  state=文書全文, questions={ Unit ごとに、いまの問いへの Noul }
             → 誰の核を聞くかが確定
ラウンド3  state=文書全文, questions={ 足切りを超えた Unit の核(Choice) }
             （**狭い問いではこのラウンドごと消える**）
```

ラウンド 3 は畳めない。Jev は question を**並列・独立に**評価するので、
ラウンド 2 の時点では「どの Unit が足切りを超えるか」をまだ誰も知らない。

**どのラウンドも 1 段である**（設計書「Jev への問いは 1 段に保つ」）。前の
答えを次の問いの前提に差し込む連鎖は無く、ラウンド 3 が前の答えを使うのは
「どの Unit に聞くか」の絞り込みだけである。

**境界は問いをまたいでキャッシュする。** akapen 側のキャッシュは
(コマンド行, 文書, 問い) で引くので、問いを変えればこのスクリプトがもう一度
起きる。そのとき境界のラウンドまで回し直すと「境界を 1 回取る」が成り立たない
ので、判定器は境界の答えを `~/.cache/akapen/semantic/boundaries/v1/` に別に
持つ（**本文は 1 バイトも入らない**。0600/0700）。同じ文書を同時に解析する
プロセスどうし（TUI の `R` は Review のルールを同時に起こす）は、隣の
`<digest>.lock` の `flock` で順番に境界を見るので、聞くのは 1 本だけである。

akapen 側のプロトコルは 1 往復（atoms in / units out）のままで、ラウンドの
数はこのスクリプトの内部事情である。

### 境界は「構造は聞かない。散文どうしだけ聞く」

設計書「Jevに判断させないもの: **syntax parsing**」のとおり、見出し・コード
ブロック・リスト項目・引用が絡む境界は**パーサが既に知っている**。demo.md の
境界 26 件のうち 17 件がそれで、Jev に聞くと質問を浪費したうえ誤りが増えた。

| 方式 | 人が手で書いた注釈と一致 | 質問数 |
| ---- | ---------------- | ------ |
| 全部 Jev に聞く（文面 v1「話題が同じか」） | 20/26 | 26 |
| 全部 Jev に聞く（文面 v2「一緒に読む必要があるか」） | 17/26 | 26 |
| 構造ルール + 散文だけ v2 | **21/26** | **9** |

（上 2 行はプローブでの実測、最下行はこのスクリプトでの実測。内訳は構造ルール
15/17・Jev 6/9。外したのは文書冒頭のタイトル、コードブロックを含む段落、
散文どうし 3 件。）

ローカルの構造ルールは上から順に当てる。

| # | 条件 | 判定 |
| - | ---- | ---- |
| 1 | 次が `heading` | NEW_UNIT |
| 2 | 現在が `heading` | SAME_UNIT（見出しは直後の内容に付く） |
| 3 | どちらかが `code_block` / `table` | NEW_UNIT（単独の Unit）。ただし**次が `table_row` なら SAME_UNIT** — 表は行へ割れても 1 Unit |
| 4 | どちらも `list_item` | SAME_UNIT |
| 5 | どちらも `sentence` | **Jev に聞く** |
| 6 | それ以外 | NEW_UNIT（既定。引用と散文の間など） |

規則 4 と、規則 2 が規則 3 に勝つこと（`## 見出し` + コードブロックは 1 つの
Unit）と、`block_quote` が規則 6 で単独になることは、**demo.md に現れないので
測っていない**。箇条書きを 1 つにまとめるのは、著者が既にまとまりとして束ねた
構造であり、半分だけ DIM になったリストは読み物として壊れるため。

### confidence は記録するだけ

`conf < 0.5 なら NEW_UNIT に倒す`は**採用していない**。実測で閾値が値の真上に
乗り、実行ごとに答えが揺れた。捨てもせず、返す JSON に載せる。

```json
{"version": 1, "question": "essential",
 "units": [{"id": "u3", "atoms": [3, 4], "score": 0.82, "core_atoms": [4],
            "jev": {"score": 0.82, "core_choice": "atom:4",
                    "core_confidence": 0.9}}],
 "jev": {"rounds": [{"questions": 9, "elapsed_s": 0.73, "usage": {…}}],
         "boundaries": [{"after_atom": 4, "decision": "new_unit", "by": "jev",
                         "confidence": 0.8}],
         "boundaries_cached": false, "core_floor": 0.2}}
```

`jev` は akapen のプロトコルに無いフィールドで、**読み飛ばされる**（未知の
フィールドは拒否しない）。使い道は実測してから決める。
ここから下にあった**実測の記録は [`measurements/`](measurements/) へ移した**。
何を測ったかは下の「実測の索引」にある。**DIM 版（Reading Budget）の実測も
そのまま残してある** — 2026-09-22 に削除した機能の記録で、各ファイルの冒頭に
その旨を書いてある。

### `--dry-run`

API を叩かず、送る 3 ラウンドのリクエスト（`state` / `model` / `questions`）を
そのまま出す。後のラウンドは前のラウンドの答えに依存するので仮定を置く
（`assumptions` にも載る）。

- ラウンド 1: **キャッシュは見ない**（当たれば 0 問になるが、形として知りたい
  のは「当たらなかったとき何を送るか」である）
- ラウンド 2: **Jev に聞く境界はすべて `new_unit` だった**と仮定する
- ラウンド 3: **すべての Unit が足切りを越えた**と仮定する。本番では越えた
  Unit にしか聞かないので、実際に送る question はこれより少ない

**要求に問いが載っていないと断る。** 問いの文面が question の大きさをそのまま
決めるので、載せずに出した数字は本番の予測にならない。

akapen から使うものではなく、要求の JSON を自分で流し込んで見る。

```sh
python3 examples/semantic/jev-annotate.py --dry-run < request.json | jq '.rounds[].questions | length'
```

鍵は要らない。テストもこの経路でリクエストの形を検証している。

### テスト

`cargo test` には載せられないので Python の `unittest` で、スクリプトの隣に置く
（`plugins/akp/scripts/test_akp.py` と同じ置き方）。**API は叩かない。**

```sh
python3 -m unittest discover -s examples/semantic -p 'test_*.py'
```

見ているのは構造ルール / 核の選び方 / `--dry-run` が送るリクエストの形 /
リクエスト分割 / 鍵の取り出し（環境変数 → キーチェーン → 停止）/ HTTP エラーの
文面の 6 つで、判定の質は [`measurements/`](measurements/) の実測の表で見る。

### 自分で測る

[`measurements/`](measurements/) の表を測り直すには、akapen を起動せずに
アダプタだけを走らせる。要求 JSON は
**akapen 本体と同じ `atomize` 経路**から作る（別実装で真似すると、測っている
ものが本番とずれる）。

```sh
cargo run -p semantic-reading --example dump-request -- doc.md > request.json
# dump-request は問いを載せないので、自分で足す（文面は
# assets/marks-questions.json の逐語を使うこと）。
jq --argjson q "$(jq '{id, text} + {core_floor: 0.2}' <(jq '.presets[0]' assets/marks-questions.json))" \
   '. + {question: $q}' request.json > asked.json
python3 examples/semantic/jev-annotate.py --timeout 120 \
  < asked.json > answer.json
jq '.jev.rounds' answer.json
cargo run -p semantic-reading --example marks-report -- doc.md answer.json
```

`jev.rounds` に 1 ラウンドずつの `questions` / `elapsed_s` / `usage` が載る。
`--timeout` を既定の 20 秒より伸ばしておくのは、測っている最中に切られないため。

`marks-report` は `marks::mark` そのものを通して、つまみごとの本数と
MARKED の比率を出す（**分母は Atom のバイト長の合計**であって source 全体では
ない — Markdown の記号や空行は Atom に入らない）。

### このディレクトリの追加ファイル

| ファイル                  | 中身                                          |
| ------------------------- | --------------------------------------------- |
| `jev-annotate.py`         | Jev アダプタ（本番の判定器）                  |
| `test_jev_annotate.py`    | そのテスト（API を叩かない）                  |
| `measurements/`           | 実測の記録（下の「実測の索引」から飛べる）    |

測るための道具は Rust 側にある。

| example | 何を出すか |
| --- | --- |
| `crates/semantic-reading/examples/dump-request.rs` | アダプタへ渡す要求 JSON |
| `crates/semantic-reading/examples/marks-report.rs` | 返ってきた答えを当てたときの、つまみごとの本数と比率 |
| `crates/semantic-reading/examples/atom-states.rs` | Atom 1 つずつの表示状態（行の途中の切り替わりを見る） |

## 実測の索引

実測の記録は [`measurements/`](measurements/) に、**主題ごと**に置いてある。
1 ファイルに戻さないこと — 測るたびに書き足す文書なので、まとめると
akapen 自身が解析できない大きさへすぐ戻る。

| 何を測ったか | どこ |
| --- | --- |
| `demo.md`（624 文字）で 3 ラウンドが何を聞き、何を返すか | [`measurements/speed-and-limits.md`](measurements/speed-and-limits.md) |
| 日常的に開く大きさ（9.8〜45.6 KB）での question 数・時間・tokens | [`measurements/speed-and-limits.md`](measurements/speed-and-limits.md) |
| 本当の上限は時間ではなく context window（≒ 65,536 input tokens）であること | [`measurements/speed-and-limits.md`](measurements/speed-and-limits.md) |
| 時間の内訳 — question 数と state を独立に振ったときの効き方 | [`measurements/speed-and-limits.md`](measurements/speed-and-limits.md) |
| リクエスト分割で 64k の側を外す — 5 文書の before / after、コスト、意味的な中立性、送れなかった question の落とし先 | [`measurements/request-splitting.md`](measurements/request-splitting.md) |
| Atom を細かくするとどうなるか（文単位まで割ったときの見え方。第 2 版は**表を行ごとに割った**とき — Atom の増分、DIM 版の Unit / Tier / 下限、marks で表が光るか） | [`measurements/atom-granularity.md`](measurements/atom-granularity.md) |
| MARKED を Unit の核だけに絞ったときの比率 | [`measurements/core-selection.md`](measurements/core-selection.md) |
| 核の問いを損失ベースの文面にしたときの比率 | [`measurements/core-selection.md`](measurements/core-selection.md) |
| 箇条書きを項目ごとに割る（規則 4 の変更）と MARKED がどれだけ増えるか | [`measurements/lists-and-run-cap.md`](measurements/lists-and-run-cap.md) |
| run キャップ（リスト 1 本につき核は 1 つ）でどこまで戻るか | [`measurements/lists-and-run-cap.md`](measurements/lists-and-run-cap.md) |
| 見出しが無い文書はどれだけ壊れるか（対照 3 対 + 実記事 3 本、各 4 ラン） | [`measurements/headless-documents.md`](measurements/headless-documents.md) |
| MARKED が文書の中でどう散らばるか — 偶然との比較、光らない区間の中身、redundancy の位置 | [`measurements/mark-distribution.md`](measurements/mark-distribution.md) |
| 節に中身が残るなら見出しも残す（規則 2 の言い直し）の前後 — 4 文書 18 ラン × READ 1 / 5 / 30 / 100 %、入れ子の連鎖と予算への影響 | [`measurements/section-heads.md`](measurements/section-heads.md) |
| `context preservation` の依存を聞いたとき、根まで辿った閉包がどれだけ大きくなるか（4 文書 × 4 ラン。第 1 版は対の Noul を閾値 0.4〜0.7 で、第 2 版は Choice で `probabilities` に閾値を置かずに）。**第 3 版で実装し**、参照実装との一致 288 件と実機で穴が塞がったことまで。**第 4 版で台帳を二段にした**（READ 1 % でも核が出る。前半への偏りは戻らない） | [`measurements/context-preservation.md`](measurements/context-preservation.md) |
| ESSENTIAL の例示を「未決の論点や宿題」まで広げたとき、議事録の未決の節がDETAIL を脱するか / 他の 4 文書で ESSENTIAL が縮まないか（5 文書 × 前後 × 4 ラン）。**READ 30 % では直っていない**ことまで | [`measurements/essential-unsettled.md`](measurements/essential-unsettled.md) |
| `REDUNDANT_WITH` の相手を Jev に Choice で選ばせたとき — 議事録の相手の正否、他の 4 文書で冗長が**増えた**こと（第 1 版）と、**選ばれた対に Noul を 1 つ足して閾値を 0.7 から 0.5 へ置き換えた第 2 版**（5 文書 × 4 ラン） | [`measurements/redundancy.md`](measurements/redundancy.md) |
| 冗長な対の負けを位置ではなく Tier と長さで決め、核は奪わないようにしたときの前後（5 文書 × 4 ラン × 前後 × 9 予算）。対ごとの勝敗、MARKED と下限の変化、(b) と (c) が打ち消し合う範囲まで | [`measurements/redundancy-loser.md`](measurements/redundancy-loser.md) |
| 定型プロンプト 8 本（要点 / 判断が要る / 決まったこと / 決まっていないこと / 数字と日付 ＋ 自由入力 3）を Unit ごとの Noul で聞いたとき（5 文書 × 8 問 × 2 ラン、コードは変えていない）。妥当性・スコアの分布・既存 ESSENTIAL との一致・費用・正規表現との重なり・ラン間の揺れ | [`measurements/marks-presets.md`](measurements/marks-presets.md) |
| `PROSE_KINDS` に `block_quote` を足したとき（核のラウンドだけ前後 2 ラン、5 文書、marks と DIM 版の両方）。引用を含む Unit の核、question が増えないこと、揺れの床、実機で光った引用の DarkNeon でのコントラスト | [`measurements/core-selection.md`](measurements/core-selection.md)「引用（`block_quote`）を核の候補に入れる」 |
| **marks モードを実装して**実機の議事録で測ったとき（定型 4 本 ＋ 自由入力 1 本）。節ごとの妥当性が段 1 と小数第 2 位まで並ぶこと、つまみの本数、1 問 0.31 円・2.2 秒、境界のキャッシュが question を 30 % 減らすこと、DIM 版が従来どおりであること | [`measurements/marks-mode.md`](measurements/marks-mode.md) |
| **marks に Unit は要るか** — Jev の境界と核をやめて文ごと（Unit = Atom）に聞く案 B と現行 A を比べた（6 文書 × 5 問 × 2 案 × 2 ラン、コードは変えていない）。ラベルに対する AUC・精度・取りこぼし、光る場所の重なりと差の内訳、業務議事録の節の中央値、見出しの無い文書、費用、focus の沈めない範囲の見積り | [`measurements/unit-granularity.md`](measurements/unit-granularity.md) |
| **Review の 1 周**（段階 2、`showcase-slop.md`）— 選別から `s`・書き換え・差分の受け入れまで。キー数と往復時間、書き換えの前後での数字・語・`[要: …]` 印の出入り、指示の範囲の外の変化、ラベルとの照合 | [`measurements/review-roundtrip.md`](measurements/review-roundtrip.md) |

**バイト数と Unit 数は分割前の値。** この README と `docs/gotchas.md` は
どちらも測定対象なので、分割でどちらも小さくなっている
（`docs/gotchas/semantic-reading.md`「`examples/semantic/README.md` は
測定対象の文書でもある」）。
