# Semantic Reading Layer のデモ

**既定は marks モード**（2026-09-22。それまでは DIM 版）。

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo-marks.json
```

view モードで `m` / `M` が問いを巡り、`/` で自由入力。`-` / `+`（`=` も可）が
つまみ ±1、`<` / `>` が ±10 で、現在値はステータス行に `MARK 20% · 3本 · 要点`
として出る。

DIM 版（Reading Budget と Tier）はフラグで残っている。

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo.json \
  --semantic-mode budget
```

こちらは `-` / `+` / `<` / `>` が Reading Budget を動かし、`READ 73%` として
出る。

**スコアの無い注釈を既定のまま開くと光らない。** `demo.json` は DIM 版の
fixture なのでスコアを持たず、marks では 0 本になる。黙って budget へ落とす
ことはしない（「marks のつもりで DIM を見ている」を画面で見分けられない）
ので、ステータス行が
`MARK · no scores in this answer (try --semantic-mode budget)` と理由と
逃げ道を言う。

`--semantic` を渡さなければ、これらのキーは**束縛されない**。読み出しも
`?` ヘルプの行も出ず、akapen はこの層が無かったときと完全に同じ動きをする。

見え方の強さは 2 つのフラグで調整できる（既定は実機で選んだ値）。

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo.json \
  --semantic-mode budget --mark-blend 0.22 --dim-blend 0.60
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
L1/42 · READ 100% · analyzing…
```

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
| `demo.json`           | それに対する `semantic-reading` の `SemanticDocument`     |
| `build-demo-json.py`  | `demo.json` の生成スクリプト                              |
| `annotate-doc.py`     | `--semantic-cmd` プロトコルの参照実装（Jev を呼ばない）   |

**byte range は手で書かない。** `demo.md` を編集したら必ず

```sh
python3 examples/semantic/build-demo-json.py
```

を走らせ直すこと。`demo.json` は `source_sha256` で `demo.md` の中身を
名指ししているので、再生成を忘れると akapen は fixture を拒否して
`this fixture belongs to a different document` と言う（`src/semantic.rs` の
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

Tier の question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

```text
ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice } → Unit を確定
ラウンド2  state=文書全文, questions={ Unit ごとの Tier(Choice) }
             → 誰に redundancy を聞くか / 誰の核を聞くかが確定
ラウンド3  state=文書全文, questions={ SUPPORTING 以上の redundancy(Choice) と、
                                        ESSENTIAL な Unit の核(Choice) }
ラウンド4  state=文書全文, questions={ 相手が決まった対の Noul }
```

ラウンド 3 も 4 も畳めない。Jev は question を**並列・独立に**評価するので、
ラウンド 2 の時点では「どの Unit が ESSENTIAL か」をまだ誰も知らないし、
ラウンド 3 の答えが出るまで「どの対を確かめるか」も決まらない。

**redundancy はラウンド 2 ではなく 3 で聞く。** 全 Unit ではなく、Tier が
SUPPORTING 以上の Unit だけに聞く — `policy::decorate` は REDUNDANT な Unit の
Tier を `weakened()` で 1 段落とすだけなので、もともと下にいる CONTEXT /
DETAIL は聞いても表示が変わらない。これで question が実測 55〜95 % 減り、
28.2 KB のこの README 自身が context window に入るようになった
（ラウンド 2 が 62,523 → 50,895 tokens、天井の 95 % → 78 %）。

akapen 側のプロトコルは 1 往復（atoms in / units out）のままで、ラウンドの
数はこのスクリプトの内部事情である。

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
| 3 | どちらかが `code_block` / `table` | NEW_UNIT（単独の Unit）。ただし**次が `table_row` なら SAME_UNIT** — 表は行へ割れても 1 Unit |
| 4 | どちらも `list_item` | SAME_UNIT |
| 5 | どちらも `sentence` | **Jev に聞く** |
| 6 | それ以外 | NEW_UNIT（既定。引用と散文の間など） |

規則 4 と、規則 2 が規則 3 に勝つこと（`## 見出し` + コードブロックは 1 つの
Unit）と、`block_quote` が規則 6 で単独になることは、**demo.md に現れないので
測っていない**。箇条書きを 1 つにまとめるのは、著者が既にまとまりとして束ねた
構造であり、半分だけ DIM になったリストは読み物として壊れるため。

### 規則 2 の続きは `section_of` にある

規則 2「見出しは直後の内容に付く」が防いでいるのは、見出しが中身から切り離
されて単独では読めない Unit になることである。**中身が複数 Unit に割れると、
境界だけではこれを守れない** — 見出しの Unit は「見出し ＋ せいぜい最初の
項目」になり、残りが別 Unit として浮く。

そこで各 Unit に `section_of`（属する節の見出し Unit）を付けて返し、
`policy::decorate` が「節の中の Unit が 1 つでも残るなら見出しの行を戻す」を
実行する。構造だけで決まるので **Jev には聞かない**。見出し Unit 自身の
`section_of` は親の節を指すので、入れ子はこのフィールドだけで連鎖する。

**規則 2 とこれは 1 つのこと**である。似た規則が 2 つあると読まないこと。
前後の数字は [`measurements/section-heads.md`](measurements/section-heads.md)。

### redundancy は方向を指定する — 相手は Jev に選ばせ、対で確かめる（2026-09-22）

**2 段である。**

1. SUPPORTING 以上の Unit ごとに **Choice 1 つ**（ラウンド 3）。選択肢は
   「対象より**前**にある Unit の本文」（DETAIL を除く）と「**該当なし**」で、
   「対象が言い直している元はどれか」を聞く
2. 相手が決まった対に **Noul 1 つ**（ラウンド 4）。「前の部分を読んだ人に
   とって、後の部分は新しい情報を加えていない」が 0.5 以上なら
   `REDUNDANT_WITH` を付ける（`REDUNDANCY_YES`）

**閾値は消えなかった。** 1 だけで出す案は実測で駄目で、他の 4 文書の冗長が
0〜1 件から 2〜41 件に増えた。**Choice は「無い」と言えない**ためで、
「該当なし」を選択肢に置いても足りない。だから旧 `REDUNDANCY_THRESHOLD` の
0.7（`demo.md` の 2 点から取った値）が 0.5（Noul の自然な境目）に
**置き換わった**。構造は context preservation の段階 1 と同じである。

方向は文面と構造の両方で入れる。対称に「重複しているか」と聞くと結論まで拾う
（Noul 時代の実測で結論の Unit が 0.71 を出し、方向ありに直すと 0.36 へ落ちた）。
設計書が「**既読内容との** redundancy」と書き、Duggan & Payne の satisficing が
逐次的なモデルであることと整合する。

以前は Noul（「言い直しか」）を 0.7 で切り、参照先は語の重なりの argmax で
ローカルに選んでいた。業務議事録で相手が 7 件中 3 件誤り（見出しだけの Unit を
指す等）だったので、`Presupposes` と同じく Choice に替えた。**ただし Choice 単独
では冗長が大きく増える** — 「該当なし」を選択肢に置いても、Jev は「同じ話題」の
先行 Unit を選ぶ。前後の実測と、対の Noul を 1 つ足す案の測定は
[`measurements/redundancy.md`](measurements/redundancy.md)。

### confidence は記録するだけ

`conf < 0.5 なら NEW_UNIT に倒す`は**採用していない**。実測で閾値が値の真上に
乗り、実行ごとに答えが揺れた。捨てもせず、返す JSON に載せる。

```json
{"version": 1,
 "units": [{"id": "u3", "atoms": [3, 4], "reading_tier": "essential", "relations": [],
            "jev": {"tier_choice": "essential", "tier_confidence": 1.0,
                    "redundancy_choice": "u:1", "redundancy_confidence": 0.9,
                    "redundancy_none_probability": 0.05, "redundancy_target": "u2",
                    "redundancy_pair_noul": 0.71, "redundant_with": "u2"}}],
 "jev": {"rounds": [{"questions": 9, "elapsed_s": 0.73, "usage": {…}}],
         "boundaries": [{"after_atom": 4, "decision": "new_unit", "by": "jev",
                         "confidence": 0.8}]}}
```

`jev` は akapen のプロトコルに無いフィールドで、**読み飛ばされる**（未知の
フィールドは拒否しない）。使い道は実測してから決める。
ここから下にあった**実測の記録は [`measurements/`](measurements/) へ移した**。
何を測ったかは下の「実測の索引」にある。

### `--dry-run`

API を叩かず、送る 3 ラウンドのリクエスト（`state` / `model` / `questions`）を
そのまま出す。後のラウンドは前のラウンドの答えに依存するので仮定を置く
（`assumptions` にも載る）。

- ラウンド 2: **Jev に聞く境界はすべて `new_unit` だった**と仮定する
- ラウンド 3: **すべての Unit が `essential` だった**と仮定する。本番では
  redundancy は SUPPORTING 以上にだけ、核は ESSENTIAL（冗長かどうかは見ない）
  にだけ聞くので、実際に送る question はこれより少ない
- ラウンド 4: **redundancy の Choice が全部、直前の候補を選んだ**と仮定する。
  本番では「該当なし」に倒れるぶんだけ少ない

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
リクエストの形 / 鍵の取り出し（環境変数 → キーチェーン → 停止）/ HTTP エラーの
文面の 5 つで、判定の質は [`measurements/`](measurements/) の実測の表で見る。

### 自分で測る

[`measurements/`](measurements/) の表を測り直すには、akapen を起動せずに
アダプタだけを走らせる。要求 JSON は
**akapen 本体と同じ `atomize` 経路**から作る（別実装で真似すると、測っている
ものが本番とずれる）。

```sh
cargo run -p semantic-reading --example dump-request -- doc.md > request.json
python3 examples/semantic/jev-annotate.py --timeout 120 \
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
| `measurements/`           | 実測の記録（下の「実測の索引」から飛べる）    |

測るための道具は Rust 側にある。

| example | 何を出すか |
| --- | --- |
| `crates/semantic-reading/examples/dump-request.rs` | アダプタへ渡す要求 JSON |
| `crates/semantic-reading/examples/decorate-report.rs` | 返ってきた答えを当てたときの表示状態の比率 |

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

**バイト数と Unit 数は分割前の値。** この README と `docs/gotchas.md` は
どちらも測定対象なので、分割でどちらも小さくなっている
（`docs/gotchas/semantic-reading.md`「`examples/semantic/README.md` は
測定対象の文書でもある」）。
