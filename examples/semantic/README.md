# Semantic Reading Layer のデモ

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo.json
```

view モードで `-` / `+`（`=` も可）が Reading Budget ±1、`<` / `>` が ±10。
現在値はステータス行に `READ 73%` として出る。

`--semantic` を渡さなければ、この 4 つのキーは**束縛されない**。READ の
読み出しも `?` ヘルプの行も出ず、akapen はこの層が無かったときと完全に
同じ動きをする。

見え方の強さは 2 つのフラグで調整できる（既定は実機で選んだ値）。

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo.json \
  --mark-blend 0.22 --dim-blend 0.60
```

- `--mark-blend` — MARKED の背景をページからテキスト色の方へどれだけ
  持ち上げるか。既定 0.22（dark で `rgb(68,70,89)`）
- `--dim-blend` — DIM の前景をページの方へどれだけ寄せるか。既定 0.60
  （dark の本文なら `rgb(205,214,244)` → `rgb(99,103,125)`）

DIM は `Modifier::DIM`（SGR `2`）ではなく**実際の色**である。SGR `2` は
無視する端末が多く、MARKED と見分けがつかなかったため。

## 見どころ

`## 結論` の下の 1 行:

```
採用する方式は差分配信である。詳細は付録にまとめた。
```

これは **1 つの source 行**だが、前半は Unit `u3`（ESSENTIAL）、後半は
Unit `u4`（DETAIL）に属する。

| READ | 前半「採用する方式は差分配信である。」 | 後半「詳細は付録にまとめた。」 |
| ---- | -------------------------------------- | ------------------------------ |
| 100% | MARKED                                 | NORMAL                         |
| 75%  | MARKED                                 | DIM                            |
| 30%  | MARKED                                 | DIM                            |

`## 補足` の行も同じ作りで、`つまり、…` が Unit `u9`
（SUPPORTING かつ `REDUNDANT_WITH(u3)`）、`念のため繰り返しておく。` が
Unit `u10`（DETAIL）。READ 75 % では前半 NORMAL・後半 DIM になる。

Budget を下げていくと落ちていく順は DETAIL → REDUNDANT → CONTEXT →
SUPPORTING → ESSENTIAL。目安:

| READ | DIM になっているもの                                     |
| ---- | -------------------------------------------------------- |
| 100% | なし（ESSENTIAL に薄いマーカーが乗るだけ）               |
| 75%  | DETAIL 4 つ（付録 / 念押し / 数値の目安 / 余談）         |
| 73%  | それに加えて REDUNDANT な「補足」。CONTEXT はまだ全部残る |
| 30%  | ESSENTIAL 3 つと、いちばん短い SUPPORTING 以外すべて     |

## 外部コマンドに判断させる（`--semantic-cmd`）

fixture ではなく、**外部コマンドに意味判断を返させる**経路もある。

```sh
akapen examples/semantic/demo.md \
  --semantic-cmd "python3 examples/semantic/annotate-doc.py"
```

`annotate-doc.py` は Jev を呼ばない**決定論的な参照実装**である。目的は
API キー無しでパイプライン全体を端から端まで動かせることで、判断そのものは

```text
見出し               -> ESSENTIAL
見出し直後の 1 Atom  -> SUPPORTING
それ以外             -> DETAIL
直前の文と語が重なる -> REDUNDANT_WITH
```

という素朴なヒューリスティクスでしかない。実際の Jev（LLM ではなく
System One モデル。`docs/design/jev.md` 参照）への question 設計はここには無い。

### プロトコル

akapen → コマンド（stdin、JSON 1 行）:

```json
{"version": 1,
 "source": "<文書全文>",
 "atoms": [{"index": 0, "kind": "heading", "range": {"start": 0, "end": 12}, "text": "## 見出し"}]}
```

コマンド → akapen（stdout、JSON）:

```json
{"version": 1,
 "units": [{"id": "u1", "atoms": [0], "reading_tier": "essential", "relations": []}]}
```

**コマンドは range を返さない。** 返すのは Atom の index だけで、
`SemanticDocument` は akapen 自身の `atomize()` の出力から組み立てられる。
だから外部コマンドが壊れた位置を返して文書の違う場所を装飾する、という
事故が**原理的に起きない**。`--semantic` 経路で必要だった
`source_sha256` の照合も、この経路では要らない。

返ってきた JSON は全項目を検証し、1 つでも失敗したら**レスポンス全体を
捨てる**（部分適用は何もしないより悪い）。弾かれるのは version 不一致 /
範囲外の atom index / unit id の重複 / 存在しない relation 先 / 未知の
reading_tier / 未知の relation。同じ Atom を複数の Unit が主張した場合は
**先勝ち**で、後の Unit からその index を落とす。

### 非同期

コマンドは**別スレッド**で走る。外部プロセスの起動とネットワーク往復を
挟むので、同期実行すると UI が固まるため。解析中はステータス行が

```text
L1/42 · READ 100% · 解析中…
```

になる。解析中に文書が変わったら、古い方の結果は**世代カウンタで破棄**
される（変わった後の文書に変わる前の判定を当てない）。タイムアウトは
60 秒。異常終了・タイムアウト・JSON 不正はステータス行に理由を出すだけで、
直前の注釈（同じ文書のもの）は保持される。

`--semantic` と `--semantic-cmd` は排他である。

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
（Jev なら `TYPESAFE_API_KEY`）。

## ファイル

| ファイル              | 中身                                                     |
| --------------------- | -------------------------------------------------------- |
| `demo.md`             | 日本語の設計メモ（AI が書きがちな、長く重複する文書の見本） |
| `demo.json`           | それに対する `semantic-reading` の `SemanticDocument`     |
| `build-demo-json.py`  | `demo.json` の生成スクリプト                              |
| `annotate-doc.py`     | `--semantic-cmd` プロトコルの参照実装（Jev を呼ばない）   |

**byte range は手で書かない。** `demo.md` を編集したら必ず

```sh
python3 examples/semantic/build-demo-json.py
```

を走らせ直すこと。`demo.json` は `source_sha256` で `demo.md` の中身を
名指ししているので、再生成を忘れると akapen は fixture を拒否して
「この fixture は別の文書のものです」と言う（`src/semantic.rs` の
`DigestChecked`）。テスト
`semantic::tests::the_demo_fixture_names_the_current_demo_md` でも落ちる。

スクリプトは Reading Policy の keep 順と累積 % の表も出力する。テストが
使う budget の閾値はその表から取っている。

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
TYPESAFE_API_KEY="$(security find-generic-password -s typesafe-jev -w)" \
  akapen examples/semantic/demo.md \
  --semantic-cmd 'python3 examples/semantic/jev-annotate.py'
```

### 鍵は環境変数 `TYPESAFE_API_KEY` だけ

スクリプトはキーチェーンも `op` も見ない。**起動時に 1 回取り出して環境変数で
渡す**のが正しい形である。理由は 2 つ。akapen は再解析のたびにこのコマンドを
起動し直すので毎回 `security` や `op read` を叩くのは無駄（`op` なら生体認証が
毎回出る）。そして akapen は OSS なので、macOS 固有の手段を埋めると他 OS で
動かない。

取り出し方はどれでもよい。

```sh
# macOS キーチェーン
export TYPESAFE_API_KEY="$(security find-generic-password -s typesafe-jev -w)"

# 1Password（akapen のプロセスに限って渡す）
op run --env-file=.env -- akapen doc.md --semantic-cmd '…'

# pass
export TYPESAFE_API_KEY="$(pass show typesafe/api-key)"

# 平文（使い捨ての実験だけ）
export TYPESAFE_API_KEY=sk-…
```

未設定なら非ゼロ終了し、ステータス行に何をすればよいかが 1 行で出る。鍵は
stdout にも stderr にも出さない。`TYPESAFE_BASE_URL` / `TYPESAFE_DEFAULT_MODEL`
も SDK と同じ名前で効く（`--model` / `--timeout` でも指定できる）。

### 3 ラウンド構成

Tier の question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

```text
ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice } → Unit を確定
ラウンド2  state=文書全文, questions={ Unit ごとの Tier(Choice) と redundancy(Noul) }
             → どの Unit が MARKED になるかが確定
ラウンド3  state=文書全文, questions={ MARKED になる Unit の核を Choice }
```

ラウンド 3 も畳めない。Jev は question を**並列・独立に**評価するので、
ラウンド 2 の時点では「どの Unit が ESSENTIAL か」をまだ誰も知らない。

akapen 側のプロトコルは 1 往復（atoms in / units out）のままで、3 ラウンドは
このスクリプトの内部事情である。

### 境界は「構造は聞かない。散文どうしだけ聞く」

設計書「Jevに判断させないもの: **syntax parsing**」のとおり、見出し・コード
ブロック・リスト項目・引用が絡む境界は**パーサが既に知っている**。demo.md の
境界 26 件のうち 17 件がそれで、Jev に聞くと質問を浪費したうえ誤りが増えた。

| 方式 | demo.json と一致 | 質問数 |
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
| 3 | どちらかが `code_block` / `table` | NEW_UNIT（単独の Unit） |
| 4 | どちらも `list_item` | SAME_UNIT |
| 5 | どちらも `sentence` | **Jev に聞く** |
| 6 | それ以外 | NEW_UNIT（既定。引用と散文の間など） |

規則 4 と、規則 2 が規則 3 に勝つこと（`## 見出し` + コードブロックは 1 つの
Unit）と、`block_quote` が規則 6 で単独になることは、**demo.md に現れないので
測っていない**。箇条書きを 1 つにまとめるのは、著者が既にまとまりとして束ねた
構造であり、半分だけ DIM になったリストは読み物として壊れるため。

### redundancy は方向を指定する

「これより**前**の箇所ですでに述べられた内容の言い直しか」と聞く。対称に
「重複しているか」と聞くと結論まで拾う（実測で結論の Unit が 0.71 を出し、方向
ありに直すと 0.36 へ落ちた）。設計書が「**既読内容との** redundancy」と書き、
Duggan & Payne の satisficing が逐次的なモデルであることと整合する。

Noul は「言い直しか」までしか答えないので、`REDUNDANT_WITH` の**参照先**は語の
重なりでローカルに選ぶ。現在の `policy::keep_order` は `is_redundant()` しか見て
いないため、参照先の選び方は表示に効かない。

### confidence は記録するだけ

`conf < 0.5 なら NEW_UNIT に倒す`は**採用していない**。実測で閾値が値の真上に
乗り、実行ごとに答えが揺れた。捨てもせず、返す JSON に載せる。

```json
{"version": 1,
 "units": [{"id": "u3", "atoms": [3, 4], "reading_tier": "essential", "relations": [],
            "jev": {"tier_choice": "essential", "tier_confidence": 1.0,
                    "redundancy_noul": 0.37}}],
 "jev": {"rounds": [{"questions": 9, "elapsed_s": 0.73, "usage": {…}}],
         "boundaries": [{"after_atom": 4, "decision": "new_unit", "by": "jev",
                         "confidence": 0.8}]}}
```

`jev` は akapen のプロトコルに無いフィールドで、**読み飛ばされる**（未知の
フィールドは拒否しない）。使い道は実測してから決める。

### demo.md での実測

| | ラウンド 1 | ラウンド 2 |
| - | --------- | --------- |
| question 数 | 9（境界 26 件のうち散文どうしだけ） | 31（Unit 16 の Tier + 先頭以外の redundancy 15） |
| 所要 | 0.73 秒 | 0.67 秒 |
| input tokens | 3,197 | 6,032 |

合計 9,229 input tokens ＝ **約 $0.0004**（出力トークンは無料）。1 回の解析で
往復は 2 回。Budget の上げ下げでは Jev を呼ばない。

### 大きな文書での実測（2026-09-21）

demo.md は 1,664 バイトしかない。**日常的に開く大きさの文書で測り直した。**
どれも 2 回ずつ（45,650 バイトのものは 7 回）、`--timeout 120` で走らせている。

| 文書 | バイト | Atom | R1 質問 | R1 秒 | R1 tokens | R2 質問 | R2 秒 | R2 tokens | 全体 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `demo.md` | 1,664 | 27 | 9 | 0.69 | 3,197 | 31 | 0.69 | 6,088 | 1.50〜1.55 秒 |
| `design/semantic-reading-layer.md` | 9,857 | 221 | 36 | 0.82 | 12,390 | 301〜305 | 1.45 | 49,609 | 2.33〜2.45 秒 |
| `design/jev.md` | 15,656 | 149 | 71 | 1.03 | 26,067 | 163〜169 | 1.23 | 38,139 | 2.30〜2.48 秒 |
| `gotchas.md` | 24,280 | 181 | 121 | 1.52 | 46,389 | 153〜159 | 1.42 | 46,107 | 2.88〜3.27 秒 |
| 実業務の `CLAUDE.md` | 45,650 | 218 | 21 | 1.00 | 23,935 | 67 | 1.36 | 60,518 | **2.42〜2.63 秒** |

**時間は問題ではない。** 45,650 バイトで 2.5 秒、akapen の `COMMAND_TIMEOUT`
（60 秒）には 18 倍の余裕がある。

### 本当の上限は context window（≒ 65,536 input tokens）

大きな文書で先に当たるのは**時間ではなく大きさ**である。超えると 2 秒台で
HTTP 400 `max_tokens_exceeded` が返る（待っても変わらない）。

| 条件 | input tokens | 結果 |
| --- | ---: | --- |
| 実業務の `CLAUDE.md` そのまま（45,650 B） | 60,518〜63,152 | OK（**天井の 92〜96 %**） |
| 同じ文書を 1.04 倍（47,556 B） | 64,851 | OK |
| 同じ文書を 1.06 倍（48,470 B） | — | **失敗** |
| state 固定で question を増やす二分探索 | 65,033 | OK |
| 同上 | 約 65.5k | **失敗** |

天井は `65,536`（2^16）と読めるが、**TypeSafe の公式値は未確認**である。設計書
`docs/design/jev.md` も「context window は需要に応じて変わりうる」と書いている。
バイト数での上限（この日本語文書では約 2.6 バイト/token）は文書の言語に依存する
ので、**上限は tokens で言うこと**。

ラウンド 2 が重いのは、`state`（文書全文）に加えて**各 Unit の本文を Tier と
redundancy の 2 回引用する**ためで、およそ `3.6 × 文書のトークン数`になる。

### 何が時間を使っているかの切り分け

2 軸を独立に振って測った（どちらも同じ 1 リクエスト）。

| 条件 | question 数 | input tokens | 所要（中央値） |
| --- | ---: | ---: | ---: |
| 往復の床（極小 state・1 question） | 1 | 277 | 0.58 秒 |
| **state 固定**（45,650 B）・question を変える | 1 | 17,750 | 0.91 秒 |
| 〃 | 20 | 21,876 | 1.03 秒 |
| 〃 | 87 | 63,152 | 1.42 秒 |
| **question 固定**（同じ 20 個）・state を変える | 20 | 5,167 | 0.68 秒 |
| 〃 | 20 | 13,201 | 0.81 秒 |
| 〃 | 20 | 21,876 | 1.00 秒 |

question を 1 → 87（87 倍）にしても 0.91 → 1.42 秒にしかならない。**question の
個数そのものは、時間にはほぼ効かない** — 「全 question を並列・独立に評価する」
が実際にそう動いている。両軸を通すと `約 0.55 秒 + 1.3 マイクロ秒/token` で
説明がつき、question 数の単独の寄与は誤差に埋もれる。

ただし **tokens では question 数はタダではない**。question 1 つにつき定型文
（instructions の枕と `criteria`）が約 116 文字乗る。時間ではなく**天井**に
効くのはこちらである。

### Atom を細かくするとどうなるか

`atomize` がリスト項目を文へ割るようになると Atom 数が増える。**同じ文書・同じ
state で、question の粒度だけを変えて測った**（`atomize.rs` は触らず、計測側で
割っている）。

決定変数は **割ったあとの `kind`** である。

| 文書 | 粒度 | Atom | R1 質問 | R2 質問 | R2 引用本文 | R2 定型文 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `CLAUDE.md` | 粗い（現状） | 218 | 21 | 87 | 40,694 | 10,089 |
| 〃 | 細かく割るが `kind` は `list_item` のまま | 404 | 24 | 93 | 41,072 | 10,782 |
| 〃 | 細かく割って `kind` を `sentence` にする | 404 | **278** | **677** | 41,656 | **78,234** |

**引用する本文の総量は粒度でほとんど変わらない**（40,694 → 41,656 文字）。
変わるのは question の個数と、それに比例する定型文である。

- `kind` が `list_item` のままなら、構造ルール 4（`list_item` どうしは
  SAME_UNIT）が効くので **question はほぼ増えない**（21 → 24、87 → 93）
- `kind` が `sentence` になると規則 5 で Jev に聞く境界が激増し、`CLAUDE.md`
  では R1・R2 とも **`max_tokens_exceeded` で失敗した**

時間の方は、粒度が実際に変わって**かつ収まる**文書（`design/jev.md`、Atom
149 → 154）で R1 1.05 → 1.01 秒・R2 1.54 → 1.41 秒と、**横ばい**だった。
「Atom を細かくすると並列が増えて速くなる」は観測されていない。速くも遅くも
ならず、**天井に当たるかどうかだけが変わる**。

表示は、`## 結論` の下の 1 行で**行の途中が切り替わった**。

| READ | 「採用する方式は差分配信である。」 | 「詳細は付録にまとめた。」 |
| ---- | ---------------------------------- | -------------------------- |
| 100% | MARKED | NORMAL |
| 70%  | MARKED | NORMAL |
| 60%  | MARKED | **DIM** |

`demo.json`（人の注釈）では 75 % で切り替わる（このページ上部の表）。Jev の
判定では 60 % だった。一方 `## 補足` の行は、Jev が `つまり、…` と
`念のため繰り返しておく。` を**どちらも DETAIL** と見たため 90 % で両側とも
同時に DIM になり、行の途中では切り替わらなかった。

### MARKED を Unit の核だけに絞る（2026-09-21 の実測）

45.6 KB の実業務文書（`CLAUDE.md`）を開くと、**画面の半分近くが MARKED**
だった。原因は Atom の粒度ではなく **Unit** である。境界の構造ルール 4
（`list_item` どうしは SAME_UNIT）が箇条書き 1 つを丸ごと 1 Unit にまとめ、
`policy::decorate` が Unit の Tier を構成 Atom **全部へ一律に**投影していた。
だから ESSENTIAL な箇条書きは段落ごと光る。直前の改修で Atom を 218 → 372 へ
細かくしても、**比率は 1 % も動かなかった**（判断も表示も Unit のままだったため）。

そこでラウンド 3 を足し、MARKED になる Unit にだけ

> このまとまりから **1 か所だけ**読むとしたら、どこを読めば要点が取れるか

を Choice で聞いて、**その Atom だけを MARKED、同じ Unit の残りを NORMAL** に
した。選択肢は Unit を構成する Atom の本文そのもの（キーは `atom:<index>`）。

**DIM は一律のままである。** Unit が落ちたなら丸ごと沈む — ここを選択的に
すると「なぜこの行の一部だけが沈むのか」を読者に説明できない。

比率（`decorate-report`。分母は Atom のバイト長の合計）:

| 文書 | Budget | MARKED 前 | MARKED 後 | NORMAL 前 → 後 | DIM 前 → 後 |
| --- | ---: | ---: | ---: | --- | --- |
| `CLAUDE.md` (45.6 KB) | 100 % | 46.7 % | **5.0 %** | 53.3 → 95.0 | 0.0 → 0.0 |
| 〃 | 80 % | 46.7 % | **5.0 %** | 22.2 → 63.1 | 31.1 → 31.9 |
| 〃 | 60 % | 46.7 % | **5.0 %** | 8.2 → 49.1 | 45.1 → 45.9 |
| 〃 | 40 % | 22.2 % | **2.9 %** | 0.0 → 19.8 | 77.8 → 77.3 |
| 〃 | 20 % | 8.2 % | **1.5 %** | 0.0 → 7.3 | 91.8 → 91.2 |
| 〃 | 1 % | 0.8 % | **0.5 %** | 0.0 → 0.3 | 99.2 → 99.2 |
| `demo.md` (1.6 KB) | 100 % | 17.7 % | **7.9 %** | 82.3 → 92.1 | 0.0 → 0.0 |
| 〃 | 80 % | 17.7 % | **7.9 %** | 57.0 → 66.7 | 25.3 → 25.3 |
| 〃 | 60 % | 17.7 % | **7.9 %** | 27.5 → 37.3 | 54.8 → 54.8 |
| 〃 | 40 % | 17.7 % | **7.9 %** | 19.4 → 29.2 | 62.9 → 62.9 |
| 〃 | 20 % | 17.7 % | **7.9 %** | 0.0 → 9.8 | 82.3 → 82.3 |
| 〃 | 1 % | 3.3 % | **0.6 %** | 0.0 → 2.8 | 96.7 → 96.7 |

**小さい文書で MARKED が消えはしない。** `demo.md` は 27 Atom のうち 6 →
**3** が MARKED として残った（Atom が 1 つしかない Unit には核を聞かないので、
短い文書ほど絞り込みが効かない）。

ラウンド 3 の値段:

| 文書 | 聞いた Unit | question | 所要 | input tokens |
| --- | ---: | ---: | ---: | ---: |
| `CLAUDE.md` | 34 Unit 中 8（ESSENTIAL 11 のうち Atom が 2 つ以上あるもの） | 8 | 0.99 秒 | 28,354 |
| `demo.md` | 16 Unit 中 2 | 2 | 0.49 秒 | 1,171 |

**context window には余裕が残る。** いちばん重いのは依然ラウンド 2 で、
`CLAUDE.md` の 60,772 tokens（天井 ≒65,536 の 93 %）。ラウンド 3 は state に
核を聞く Unit の本文だけを足すので 28,354 tokens（43 %）に収まった。
プロセス全体は 2.64 秒 → **3.54〜3.57 秒**（2 回）、`demo.md` は 1.33 → 1.88 秒。

**核の選択は 2 回の実行で揺れなかった。** `CLAUDE.md` を 2 回走らせると、
境界（Unit の組）は完全に一致し、両方で聞かれた 7 つの question は
**7 件とも同じ Atom** を核に選んだ。8 つ目の question は片方にしか無い —
ラウンド 2 の Tier がその Unit で揺れて ESSENTIAL から外れたためで、
ラウンド 3 が揺れたのではない。`demo.md` は 2 回とも完全一致。比率で見ても
MARKED は 5.0 % と 4.9 %（修正前も 46.7 % と 46.3 %）で、この差は実行ごとの
揺れの幅に収まっている。

**核に閾値は使っていない。** Choice が返す 1 つをそのまま採り、
`probabilities` を閾値で切って複数採ることはしない（`confidence` の閾値が
実測で不安定だった件は上の「confidence は記録するだけ」）。選ばれた核の
`confidence` は 0.31〜0.96 と幅が広いが、**値で判定を倒していない**。

選ばれた核の `kind` の内訳（`CLAUDE.md` 8 件）は `list_item` 4 / `sentence` 3
/ `heading` 1 だった。見出しが核になった Unit が 1 つある（構造ルール 2 で
見出しは直後の内容と同じ Unit になる）。**これが読み物として良いかは
測っていない。**

### `--dry-run`

API を叩かず、送る 3 ラウンドのリクエスト（`state` / `model` / `questions`）を
そのまま出す。後のラウンドは前のラウンドの答えに依存するので仮定を置く
（`assumptions` にも載る）。

- ラウンド 2: **Jev に聞く境界はすべて `new_unit` だった**と仮定する
- ラウンド 3: **すべての Unit が `essential` かつ非 REDUNDANT だった**と仮定
  する。本番ではここが絞られるので、実際に送る question はこれより少ない

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

見ているのは構造ルール / フィクスチャからの Unit 組み立て / `--dry-run` が送る
リクエストの形 / `TYPESAFE_API_KEY` 未設定時のエラー経路 / HTTP エラーの文面の
5 つで、判定の質は上の実測の表で見る。

### 自分で測る

上の表を測り直すには、akapen を起動せずにアダプタだけを走らせる。要求 JSON は
**akapen 本体と同じ `atomize` 経路**から作る（別実装で真似すると、測っている
ものが本番とずれる）。

```sh
cargo run -p semantic-reading --example dump-request -- doc.md > request.json
TYPESAFE_API_KEY=... python3 examples/semantic/jev-annotate.py --timeout 120 \
  < request.json > answer.json
jq '.jev.rounds' answer.json
cargo run -p semantic-reading --example decorate-report -- doc.md answer.json
```

`jev.rounds` に 1 ラウンドずつの `questions` / `elapsed_s` / `usage` が載る。
`--timeout` を既定の 20 秒より伸ばしておくのは、測っている最中に切られないため。

`decorate-report` は `policy::decorate` そのものを通して、Budget ごとの
MARKED / NORMAL / DIM の比率を出す（**分母は Atom のバイト長の合計**であって
source 全体ではない — Markdown の記号や空行は Atom に入らない）。

### このディレクトリの追加ファイル

| ファイル                  | 中身                                          |
| ------------------------- | --------------------------------------------- |
| `jev-annotate.py`         | Jev アダプタ（本番の判定器）                  |
| `test_jev_annotate.py`    | そのテスト（API を叩かない）                  |

測るための道具は Rust 側にある。

| example | 何を出すか |
| --- | --- |
| `crates/semantic-reading/examples/dump-request.rs` | アダプタへ渡す要求 JSON |
| `crates/semantic-reading/examples/decorate-report.rs` | 返ってきた答えを当てたときの表示状態の比率 |
