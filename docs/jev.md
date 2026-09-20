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
  - HTTP API `POST /v1/systemone`（bearer token）
  - Python SDK（`TYPESAFE_API_KEY` を自動で読む）
- レスポンスには token usage が含まれる

SDK の定数（<https://docs.typesafe.ai/sdk/python/api/constants.md> で確認）:

| 定数 | 値 |
|---|---|
| `API_KEY_ENV` | `TYPESAFE_API_KEY` |
| `BASE_URL_ENV` | `TYPESAFE_BASE_URL` |
| `DEFAULT_MODEL_ENV` | `TYPESAFE_DEFAULT_MODEL` |
| `DEFAULT_BASE_URL` | `https://api.typesafe.ai` |
| `DEFAULT_MODEL` | `jev-latest` |
| **`DEFAULT_TIMEOUT`** | **`10.0`** |

モデルは `jev-1.13.0` が現行で、`jev-latest` と `jev-preview` がどちらもそこを
指す。全モデルが同一エンドポイント `POST /v1/systemone` を使い、`model`
フィールドで振り分ける。

**`DEFAULT_TIMEOUT` が 10 秒である**ことは、akapen 側のタイムアウト設計の根拠に
なる。LLM を前提にした「10〜30 秒かかる」という想定は誤りで、Jev 自身の SDK は
10 秒で切る。akapen の既定 60 秒はアダプタスクリプトの起動コストまで含めた余裕
であって、Jev の応答時間の想定ではない。

料金は入力 `$42 / 十億トークン`（= `$0.042 / 百万トークン`）で、**出力トークンは
無料**。rate limit と context window は需要に応じて変わりうる、とされている。

> 未確認: pip のパッケージ名。`typesafe-sdk` と読めたが原典で確定できていない。

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

### Score を今は使っていない（禁止ではない）

Jev は Score primitive を持っているが、現状どこにも使っていない。設計書の
関連記述は 2 つあり、**強さが違う**ので分けて扱う。

**1. Tier を 0〜100 に置き換えない — 構造的な制約。守る。**

> 0〜100のimportance scoreは使用しない。

Reading Policy 全体がここに乗っている。「粗い Tier × 細かい Budget」という
分割、単調性の保証（Budget を下げて一度 DIM になった range が復活しない）、
そして「43%・42%・41% で表示が変わらなくても問題ない」という割り切りは、
いずれも Tier が離散だから成立する。ここに連続値を入れると Policy は作り直しになる。

**2. 同一 Tier 内の順序 — 暫定。こだわる必要は薄い。**

> 同じTier内部では、redundancy / length / document position /
> context preservation などの決定論的なruleを**まず**利用する。
> Jevに精密な順位スコアを出させない。

「まず」とある通り出発点の指定であって、禁止ではない。そもそも現在の
`policy::keep_order` は `実効Tier → redundant か → バイト長 → 先頭バイト位置`
で並べており、**バイト長の時点で既に連続値**である。「決定論的か否か」の線は
もう引かれていない。

もし現在の順序が実文書で不十分だと分かったら、**Score の question を足す前に
`confidence` を試すべき**である。Tier 判定は Choice なので `confidence` は
聞かなくても返ってくる（追加コストゼロ）。順位付けに使うのが設計書の意図から
外れると感じるなら、「confidence が低い Unit は CONTEXT 側に倒す」という
判定のガード用途もある。

ただし**現時点でどちらもやる理由は無い**。実文書で現在の順序を見て「これは違う」
と観測した人がまだいないため。観測されていない問題を解くと、代わりに
「なぜこの段落が先に消えたか」を説明できる性質を失う。

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
