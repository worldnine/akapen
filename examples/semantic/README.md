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
