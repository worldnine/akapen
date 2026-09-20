# Semantic Reading Layer のデモ

```sh
akapen examples/semantic/demo.md --semantic examples/semantic/demo.json
```

view モードで `-` / `+`（`=` も可）が Reading Budget ±1、`<` / `>` が ±10。
現在値はステータス行に `READ 73%` として出る。

`--semantic` を渡さなければ、この 4 つのキーは**束縛されない**。READ の
読み出しも `?` ヘルプの行も出ず、akapen はこの層が無かったときと完全に
同じ動きをする。

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

## ファイル

| ファイル              | 中身                                                     |
| --------------------- | -------------------------------------------------------- |
| `demo.md`             | 日本語の設計メモ（AI が書きがちな、長く重複する文書の見本） |
| `demo.json`           | それに対する `semantic-reading` の `SemanticDocument`     |
| `build-demo-json.py`  | `demo.json` の生成スクリプト                              |

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
