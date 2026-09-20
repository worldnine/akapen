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

### 2 ラウンド構成

Tier の question は Unit について聞くものだが、Unit は境界判定の答えから
生まれる。**1 ラウンドでは原理的に組めない。**

```text
ラウンド1  state=文書全文, questions={ 散文どうしの境界を Choice } → Unit を確定
ラウンド2  state=文書全文, questions={ Unit ごとの Tier(Choice) と redundancy(Noul) }
```

akapen 側のプロトコルは 1 往復（atoms in / units out）のままで、2 ラウンドは
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

### `--dry-run`

API を叩かず、送る 2 ラウンドのリクエスト（`state` / `model` / `questions`）を
そのまま出す。ラウンド 2 は境界の答えに依存するので、**Jev に聞く境界はすべて
`new_unit` だった**と仮定して組む。

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
リクエストの形 / `TYPESAFE_API_KEY` 未設定時のエラー経路の 4 つで、判定の質は
上の実測の表で見る。

### このディレクトリの追加ファイル

| ファイル                  | 中身                                          |
| ------------------------- | --------------------------------------------- |
| `jev-annotate.py`         | Jev アダプタ（本番の判定器）                  |
| `test_jev_annotate.py`    | そのテスト（API を叩かない）                  |
