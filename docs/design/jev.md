# Jev — Semantic Reading Layer が使う判定器

Semantic Reading Layer の設計書（`semantic-reading-layer.md`）は Jev を既知の
ものとして扱い、役割だけを定義している。このドキュメントは Jev 自体が何かを
記録する。出典は <https://docs.typesafe.ai/>。

---

## この層は何のためにあるか

Jev をどう使うかの前に、Jev に何をさせたいのかを置く。出発点は設計書
（[`semantic-reading-layer.md`](semantic-reading-layer.md)）の「研究的背景」で
ある。

> Duggan & Payne の satisficing モデルでは、読み進めることで得られる
> information gain が低下すると、読者は次の場所へ移動する。

> **限られた attention を、意味的な収穫が高い部分へ配分する**

Semantic Reading Layer は要約器ではなく、**限られた attention を文書のどこへ
配るかを決める層**である。配分そのものを決めるのは Reading Policy
（`crates/semantic-reading/src/policy.rs`。根拠はそのモジュールドキュメント）
で、そこでは Jev を呼ばない。Jev が担うのは、配分に必要な意味判断だけである。

この位置づけは question の設計に直接効く。設計書が挙げる判断の例は、
**すべて関係的・損失ベースの疑問文**になっている。

```text
ここを飛ばすと要点を失う？        ← 飛ばした場合の損失
これは主要な主張を支えている？    ← 他との関係
これは主に背景説明？
これは前に出た内容と実質同じ？    ← 既読との関係
```

「この段落は重要か」という**絶対的な問いが 1 つも無い**。attention の配分を
決めるのだから、聞くべきは「ここに attention を使うと何が得られるか / 使わない
と何を失うか」であって、Unit 単体の重要度ではない。question の文面を書くときは
ここへ戻ること。

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

実測でもそうなった（下の「大きな文書での実測」）。45,650 バイトの実業務文書の
解析が**プロセス全体で 2.5 秒**で、60 秒には 18 倍の余裕がある。**大きな文書で
先に当たるのはタイムアウトではなく context window である。**

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
Unit の核（MARKED を絞る先）                → Choice（Unit 内の Atom の数だけ）
```

最後の 1 つだけは設計書に直接の記述が無い。設計書が MARKED を **Atom** に
対して定義していること（「読む価値の高い**Atom**」）と、判断単位が Unit で
あることの差を埋めるための question である（下の「Unit の核」）。

設計書が挙げる問いの例（上の「この層は何のためにあるか」）は、そのまま
question の文面の出発点になる。ただし**疑問文であることと Noul であることは
別**で、primitive は 1 つではない。「これは前に出た内容と実質同じ？」は Noul
だが、残り 3 つは Tier の段階（ESSENTIAL / SUPPORTING / CONTEXT）に対応して
いて、Choice の criteria になる（文面は
`examples/semantic/jev-annotate.py` の `TIER_CRITERIA`）。

**state は文書全文**。設計書の
「単一ファイルかつ現実的なサイズである限り、全文を Jev の context へ渡す」
はこれを指す。

### redundancy の question は方向を持つ

Noul の文面を対称に書いてはならない。「他の箇所で既に述べられた内容と実質
同じか」と対称に聞くと、**結論まで 0.71 を出す** — 結論は文書中で何度も触れ
られるので「重複」に見えるためである。「これより**前**の箇所を言い直している
だけか」と方向を入れると、同じ結論は 0.36 へ落ち、本当の言い直しが 0.92 へ
上がった（実測。下の「実測」と同じラウンド）。

これは偶然ではなく、satisficing の逐次性から出てくる。読み進める過程のモデル
なのだから、redundancy は文書が単体で持つ性質ではなく**既読との相対**で決まる。
設計書も「**既読内容との** redundancy」と書いている。`REDUNDANT_WITH` が方向を
持つ関係なのも同じ理由で、Reading Policy 側の根拠は
`crates/semantic-reading/src/policy.rs` のモジュールドキュメントにある。

現在の文面は `examples/semantic/jev-annotate.py` の `unit_questions`。

### Unit の核 — 選択肢が本文そのものになる Choice

MARKED を Unit 全体ではなく「Unit の核となる Atom」に絞るために、ラウンド 3 で
こう聞いている（文面は `examples/semantic/jev-annotate.py` の
`CORE_INSTRUCTIONS`）。

> 次の選択肢は、この文書の中の連続した 1 つのまとまりを構成する各部分の本文で
> ある。このまとまりから **1 か所だけ**読むとしたら、どこを読めば要点が取れるか。

他の question と違い、**`criteria` の説明文が判定基準ではなく本文の引用**に
なる（キーは `atom:<index>`）。Choice は「どの選択肢か」を返す primitive なので
この使い方ができる。1 question あたり 255 選択肢まで扱えるが、実測でいちばん
大きい Unit でも Atom は 96 個だった。

設計と実測の詰め方は 3 点。

- **instructions に Unit の本文を書かない。** 選択肢の総和がその Unit の全文に
  なるので、instructions にも入れると同じテキストを 2 回送ることになる。
  効くのは時間ではなく context window である
- **聞く対象を絞る。** MARKED になりうる Unit（ESSENTIAL かつ非 REDUNDANT）で、
  かつ Atom が 2 つ以上あるものだけ。実測では 34 Unit のうち 8 つ
- **「重要な部分はどれか」と聞かない。** それだと「どれも重要」と答えられて
  しまい、Unit を丸ごと光らせていた元の状態に戻る。「1 か所だけ読むなら」で
  強制的に 1 つへ倒す

核は **1 Unit につき 1 つ**で、`probabilities` を閾値で切って複数採ることは
していない（下の「`confidence` の閾値ガードは不採用」と同じ理由）。実測では
選ばれた核の `confidence` は 0.31〜0.96 と幅があるが、**値では何も倒していない**。

比率・時間・tokens・安定性の実測は `examples/semantic/README.md`
「MARKED を Unit の核だけに絞る」にある。

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

もし現在の順序が実文書で不十分だと分かったら、Score の question を足す前に
`confidence` を見る余地がある。Tier 判定は Choice なので `confidence` は
聞かなくても返ってくる（追加コストゼロ）。

ただし **`confidence` を閾値で判定に使うのは実測で駄目だった**（下の「実測」）。
閾値が値の真上に乗り、実行ごとに答えが揺れる。閾値を持たない使い方
（同一 Tier 内の tie-break など）ならこの問題は起きないが、まだ試していない。

そして**現時点でどちらもやる理由は無い**。実文書で現在の順序を見て「これは違う」
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

## 実測（`demo.md` / 2026-09-20）

すべて `examples/semantic/demo.md`（624 文字）に対する実測で、推測ではない。
アダプタは `examples/semantic/jev-annotate.py`、詳しい表は
`examples/semantic/README.md` にある。

### 速さと値段

| | question 数 | 所要 | input tokens | 概算 |
|---|---|---|---|---|
| プローブ（1 ラウンド） | 52 | 0.84 秒 | 11,043 | $0.00046 |
| アダプタ本番（2 ラウンド） | 9 + 31 | 1.4 秒 | 9,229 | $0.0004 |

出力トークンは無料なので、値段は state（文書全文）を何回送るかでほぼ決まる。
52 question を 1 リクエストで 0.84 秒という数字が、「多数の小さな問いを
1 リクエストで並列評価する」が実際にそう動くことの確認である。

### 安定性

同一文書・同一 question を 3 回実行した。Choice の `choice` は**全件同一**で、
揺れたのは `confidence` だけ、幅は ±0.07 だった。

### `confidence` の閾値ガードは不採用

`conf < 0.5 なら NEW_UNIT に倒す`を検討して**採らなかった**。実測で閾値が値の
真上に乗り、実行ごとに答えが裏返ったためである（ある境界が 0.46 ↔ 0.53）。
上の ±0.07 がそのまま判定をひっくり返す幅になる。`confidence` は返す JSON に
記録するが、**判定には使わない**。

### 構造の境界は Jev に聞かない

demo.md の境界 26 件のうち 17 件は見出し・コードブロック・リスト項目・引用が
絡む境界で、**パーサが既に知っている**。全部 Jev に聞くと 20/26、構造ルールを
併用すると 18/22 で、質問数は半分になった。設計書「Jevに判断させないもの:
syntax parsing」の実証である。

> 注意: この 2 つの数字はプローブでの実測で、出荷したスクリプトでの測り直しは
> 21/26 / 質問 9（`examples/semantic/README.md` の表）である。分母が揃って
> いない理由は**まだ突き合わせていない**。

### 手書き fixture は正解ではない

上の一致率は `examples/semantic/demo.json`（人が手で書いた注釈）との一致で
あって、正解との一致ではない。外れた 6 件のうち 3 件は、むしろ Jev の判断の方
が妥当だった（コードブロックを別 Unit として切る、など）。fixture を基準にした
計測は 20〜22/26 で頭打ちになる。**この数字を上げること自体を目標にしない。**

---

## 大きな文書での実測（2026-09-21）

上は 624 文字の 1 ファイルだけの実測だった。日常的に開く大きさで測り直した。
表の全体は `examples/semantic/README.md` にある。

### 時間は問題ではない

| 文書 | バイト | 質問（R1 + R2） | 解析の全体 |
| --- | ---: | ---: | ---: |
| `examples/semantic/demo.md` | 1,664 | 9 + 31 | 1.50〜1.55 秒 |
| `design/semantic-reading-layer.md` | 9,857 | 36 + 305 | 2.33〜2.45 秒 |
| `design/jev.md` | 15,656 | 71 + 169 | 2.30〜2.48 秒 |
| `gotchas.md` | 24,280 | 121 + 153 | 2.88〜3.27 秒 |
| 実業務の `CLAUDE.md` | 45,650 | 21 + 67 | 2.42〜2.63 秒（7 回） |

文書が 27 倍になっても時間は 1.7 倍にしかならない。「全 question を並列・独立に
評価する」は、実際にそう動いている。**question 数は時間にほぼ効かない** —
state を固定して question を 1 → 87（87 倍）にしても 0.91 → 1.42 秒だった。
両軸を通した実測は `約 0.55 秒 + 1.3 マイクロ秒/token` で説明がつく。

### 本当の上限は context window

「rate limit と context window は需要に応じて変わりうる」と原典にはあるが、
**2026-09-21 時点の実測値は入力 65,536 tokens（2^16）と読める**。公式値は
未確認である。超えると 2 秒台で HTTP 400 `max_tokens_exceeded` が返る。

| 条件 | input tokens | 結果 |
| --- | ---: | --- |
| 実業務の `CLAUDE.md`（45,650 B）のラウンド 2 | 60,518〜63,152 | OK（**天井の 92〜96 %**） |
| 同じ文書を 1.04 倍 | 64,851 | OK |
| 同じ文書を 1.06 倍 | — | **失敗** |
| 二分探索の最大成功 | 65,033 | OK |

ラウンド 2 が重いのは、`state`（文書全文）に加えて**各 Unit の本文を Tier と
redundancy で 2 回引用する**ためで、およそ `3.6 × 文書のトークン数`になる。
バイト数での上限は言語に依存する（この日本語文書で約 2.6 バイト/token）ので、
**上限は tokens で言うこと**。

この上限は「ラウンドを分けると state が 2 回課金される」というコストの話とは
別の、**機能するかしないかの線**である。

---

## akapen 側の接続

akapen 本体には HTTP クライアントも async ランタイムも入れない。Jev は
`--semantic-cmd` の外部コマンド経由で呼ぶ（`semantic-cmd-protocol` は
`examples/semantic/README.md`）。アダプタスクリプトが

```
atoms → Jev の question 群 → Jev の typed answer → units
```

を担う。akapen が渡すのは Atom の index だけで、range は一度も外へ出ない。

### 未解決: confidence と probabilities が akapen まで届かない

`--semantic-cmd` プロトコルの `AnalyzeResponse` は `version` と `units` しか
持たないので、Jev の **`confidence` と `probabilities` は akapen 側に届かない**。
アダプタは捨てずに `jev` という追加フィールドへ載せているが、プロトコルが
知らないフィールドなので**読み飛ばされる**（未知のフィールドを拒否しないので、
拡張自体は可能）。

使い道として挙がっていたのは 3 つで、うち 1 つは実測で潰れている。

| 案 | 現状 |
|---|---|
| 境界判定の confidence が低いときは `NEW_UNIT` 側に倒す | **不採用**（上の「実測」。閾値が値の真上に乗って実行ごとに答えが揺れた） |
| Tier の confidence が低い Unit は CONTEXT 扱いにする | 同じ閾値の問題を踏むはず。試していない |
| 同一 Tier 内の順序の tie-break に使う | 閾値を持たないので上の問題は起きない。未検証 |

**境界線は「順位付けか / ガードか」ではない。**「Tier の中か / Tier そのものか」
である。

| 使い方 | 判定 | 理由 |
|---|---|---|
| 同一 Tier 内の順序に使う | **問題なし** | 設計書が挙げる `redundancy / length / document position` がまさに Tier 内の順位付けであり、現実装も `policy::keep_order` でバイト長という連続値を使っている |
| 判定のガードに使う（上の 2 例） | **問題なし** | Tier の離散性を壊さない |
| 離散 Tier を連続値で置き換える | **駄目** | 「0〜100のimportance scoreは使用しない」に反する。Policy の単調性と「粗い Tier × 細かい Budget」が壊れる（上の「Score を今は使っていない」参照） |

プロトコルを拡張するかは実測してから決める。現時点で現在の順序が不十分だと
観測した人はいない。
