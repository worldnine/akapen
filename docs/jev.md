# Jev — Semantic Reading Layer が使う判定器

Semantic Reading Layer の設計書（`semantic-reading-layer.md`）は Jev を既知の
ものとして扱い、役割だけを定義している。このドキュメントは Jev 自体が何かを
記録する。出典は <https://docs.typesafe.ai/>。

---

## Jev は LLM ではない

最初に、これを間違えると設計の前提が丸ごとずれる。

Jev は TypeSafe の flagship model で、**the first System One model** と
位置づけられている。

> System One models are a class of AI models built to make fast, structured
> decisions that software can use directly.

> System One models do not write replies, produce code, or generate
> explanations of their reasoning.

LLM が「人間が読むテキストを生成する」のに対し、Jev は
**typed な question を state に対して評価し、構造化された結果を直接返す**。

> No text generation, no parsing. You get typed values and probability
> distributions that your code can branch on, sort by, and route with.

名前は Kahneman『ファスト&スロー』の System 1（速く直観的）に由来する。
設計書が Jev を **「System 1 的センサー」** と書いているのは比喩ではなく、
この製品カテゴリそのものを指している。

---

## 3 つの primitive

| primitive | 問い | 返り値 |
|---|---|---|
| **Choice** | 選択肢から 1 つ選ぶ | `choice` / `probabilities` / `confidence` |
| **Score** | rubric に沿って採点する | `score` / `probabilities` / `confidence` |
| **Noul** | この主張は真か | `noul`（0〜1） |

決定的に重要な性質:

> All three *question* types can be mixed in a single API call. Every
> *question* is evaluated in parallel and in isolation against the same
> *state* in one go.

**多数の小さな問いを 1 リクエストで並列評価する**のが Jev の想定される
使い方であり、コスト上の妥協ではない。LLM を前提にした
「呼び出し回数を減らすためにまとめて聞く」という発想は当てはまらない。

確率は校正されている。

> System One models are trained for calibrated decisions: their
> probabilities are optimized against outcomes to reflect uncertainty

---

## 入出力と呼び出し

- 入力は**テキストのみ**。文字列・JSON オブジェクト・テキストの配列を評価する。
  画像・音声・動画は未対応
- 呼び出しは 3 通り
  - Playground（<https://console.typesafe.ai/playground>）
  - HTTP API `POST https://api.typesafe.ai/v1/systemone`（bearer token）
  - Python SDK `pip install typesafe-sdk`（`TYPESAFE_API_KEY` を自動で読む）
- 既定モデルは `jev-latest`
- レスポンスには token usage が含まれる

---

## Semantic Reading Layer との対応

設計書「Jevに判断させるもの」は、そのまま primitive に写る。

```
Atom 間の意味境界（SAME_UNIT / NEW_UNIT）  → Choice（2 択）
Reading Tier（ESSENTIAL / SUPPORTING /
              CONTEXT / DETAIL）            → Choice（4 択）
semantic redundancy                          → Noul
```

設計書が挙げる問いの例が、すべて疑問文になっているのは偶然ではない。
これらは Noul の question そのものである。

```
ここを飛ばすと要点を失う？
これは主要な主張を支えている？
これは主に背景説明？
これは前に出た内容と実質同じ？
```

**state は文書全文**。設計書の
「単一ファイルかつ現実的なサイズである限り、全文を Jev の context へ渡す」
はこれを指す。

### Score を使わない理由

Jev は Score primitive を持っているが、設計書は
**「Jev に精密な順位スコアを出させない」**
と明示している。これは能力の話ではなく設計判断で、0〜100 の importance score を
排して粗い Reading Tier（Choice）に倒す、という方針の一部である。
同一 Tier 内の順序は redundancy / length / document position などの
**決定論的なローカル rule** で決める。

### Jev が触らないもの

設計書「Jevに判断させないもの」はすべてローカルコードの責務。

```
syntax parsing / Atom 生成 / source position 管理 / ファイル変更検知
debounce / cache / rate limit / Reading Budget / Reading Policy
表示状態への変換 / renderer
```

とくに **Budget 変更では Jev を呼ばない**。`37% → 36%` は
`policy::decorate` だけで完結する。

---

## akapen 側の接続

akapen 本体には HTTP クライアントも async ランタイムも入れない。Jev は
`--semantic-cmd` の外部コマンド経由で呼ぶ（`semantic-cmd-protocol` は
`examples/semantic/README.md`）。アダプタスクリプトが

```
atoms → Jev の question 群 → Jev の typed answer → units
```

を担う。akapen が渡すのは Atom の index だけで、range は一度も外へ出ない。

### 未解決: confidence と probabilities を捨てている

現在の `--semantic-cmd` プロトコルは `units` しか受け取らず、Jev の
**`confidence` と `probabilities` を捨てている**。これは Jev の最大の特徴を
使っていないということで、たとえば

- 境界判定の confidence が低いときは `NEW_UNIT` 側に倒す
- Tier の confidence が低い Unit は CONTEXT 扱いにする

といった使い道がありうる。ただし設計書の「精密な順位スコアを出させない」
との線引きが必要で、**順位付けに使うのではなく判定のガードに使う**なら
方針と矛盾しない。プロトコルを拡張するかは実測してから決める。
