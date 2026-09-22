# 未解決（地雷ではなく、設計判断が要るもの）

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。
**文中に測定対象として出てくる `docs/gotchas.md` /
`examples/semantic/README.md` のバイト数・Unit 数は分割前の値です。**

---


刺しにくるものではありませんが、**設計書と実装の差**として残っています。
どれも「誤記」ではないので、直すには判断が要ります。

> **DIM 版（Reading Budget）は 2026-09-22 に削除しました。** 4 と、5 の中で
> Tier・Budget・二段台帳・前提の閉包に触れている部分は、**当時の記録**です —
> そこに書かれた「未解決」はコードごと消えました。なぜ削ったかは
> [`semantic-reading.md`](semantic-reading.md) の冒頭にあります。

### 1. `incremental reanalysis` は無い（`cache` は 2026-09-22 に作った）

設計書 [`design/semantic-reading-layer.md`](../design/semantic-reading-layer.md)
の MVP は `cache` と `incremental reanalysis` を挙げていました。
**`cache` は作りました**（`src/semantic_cache.rs`）。`incremental reanalysis`
—— 文書の変わった部分だけを再解析する —— は**ありません**。文書が 1 バイトでも
変われば全文を解析し直します。

**作った理由は速度ではなく費用です。** かつてここには「1 文書の再解析は
1.4 秒 / 約 $0.0004 だから要らない」と書いてありました。その数字は
`examples/semantic/demo.md`（1.6 KB）のもので、**実業務の大きさで測り直したら
桁が違いました**（2026-09-22、`docs/design/marks-only-and-review-mode.md` の
1 節）:

| 文書 | 1 回の解析 |
| --- | ---: |
| `examples/semantic/demo.md`（1.6 KB） | 0.1 ¢ |
| 業務議事録（22.7 KB） | 3.0〜3.3 ¢（≈ 5 円） |
| `docs/gotchas/semantic-reading.md` | 5.3〜5.6 ¢（≈ 8 円） |

**費用の 9 割は問いの中身ではなく、`state`（文書の全文）を 30 回近く送って
いることにあります。** 開き直し・再起動・つまみの上げ下げでそれを毎回払うのは、
判定の質と何の関係もない出費でした。読み手の言葉は「思ったより高い」。

いま入っているのは 2 つです。**どちらも判定には触っていません** —— 走る回数が
変わるだけで、走ったときの答えは同じです。

1. **sha キャッシュ**: キーは `sha256(source)` ＋ `sha256(--semantic-cmd の
   文字列)` ＋ `sha256(問いの文面)`。同じ文書に同じ問いは二度解析しません。
   費用が発生するのは文書か問いが変わったときだけです
2. **解析の遅延**: `--semantic-cmd` は開いただけでは走らず、**問いを決めた
   最初の 1 打**（`m` / `M` / `/`）で始まります。素で読むだけの文書に
   1 回分を払いません

`incremental reanalysis` を作らない判断は**維持されています**。理由は変わって
いません —— Jev は全 question を 1 リクエストで並列評価するので、文書が 27 倍に
なっても時間は 1.7 倍にしかならず（1.6 KB で 1.5 秒、45.6 KB で 2.4〜2.6 秒）、
「大きくなったら差分更新が要る」という成長経路は**その前に別の壁に当たります**
（下の項目 5 の context window）。そして費用の方は、上の 2 つが「変わって
いない文書には払わない」で先に片付けています。差分更新が効くのは「少しだけ
変わった文書」で、その効き目はまだ測っていません。

**確認したこと**: `src/app.rs::reanalyze_semantics` が毎回
`provider.analyze(&self.source.content)` に文書全文を渡すこと（差分ではない）。
キャッシュは `src/semantic_cache.rs` にあり、`CommandProvider::analyze` の
内側で引かれること（`src/semantic.rs`）。実機で業務議事録を 2 回開き、
外部コマンドが 1 回しか起きないこと —— 1 回目 25 リクエスト / 809k input
tokens / 27 秒、2 回目 0 リクエスト / 0.3 秒未満（2026-09-22）。
上の費用の表の出どころは `examples/semantic/measurements/redundancy.md` の
コストの節。1.5 秒と 2.4〜2.6 秒は [`design/jev.md`](../design/jev.md) の
「大きな文書での実測」。

### 2. Phase 番号が 2 つの意味で使われている（アーカイブ側）

[`design/range-attribution-plan.md`](../design/range-attribution-plan.md) の
Phase 3 は **Render Mapping 強化**です。一方
[`handoff-archive.md`](../handoff-archive.md) には
「Semantic Reading Layer を akapen へ配線（**Phase 3** / Reading Budget）」
というエントリがあり、別物を指しています。アーカイブを読んで Phase 番号を
信じると食い違います。**設計書の番号が正**です。

**確認したこと**: `docs/design/range-attribution-plan.md` の見出しが
Phase 1 Range Attribution / 2 Range Decoration / 3 Render Mapping 強化 /
4 Range Selection / 5 Range Comment であること。`src/` 内の
「Phase N」への言及（`decoration.rs` / `main.rs` / `render.rs` /
`view.rs` / `state_tests.rs` / `atomize.rs`）はすべて設計書の番号と
一致していること — 衝突はアーカイブ側にだけ残っています。

### 3. `--semantic-cmd` が `confidence` / `probabilities` を運ばない

判定器の Choice / Score primitive は `probabilities` と `confidence` を
返しますが（[`design/jev.md`](../design/jev.md)）、現行プロトコルは `units`
しか受け取りません。アダプタ（`examples/semantic/jev-annotate.py`）は値を
捨てておらず `jev` という追加フィールドに載せていますが、プロトコルが
知らないフィールドなので**読み飛ばされます**。プロトコルは未知のフィールドを
拒否していないので**拡張は可能**で、「出力を全部使う」と書いた箇所も無いので
**矛盾してはいません**。

使い道として挙がっていた「境界判定の confidence が低いときは `NEW_UNIT` に
倒す」は、**実測で不採用になりました**（閾値が値の真上に乗り、実行ごとに答えが
裏返る。[`design/jev.md`](../design/jev.md) の「実測」）。残っているのは閾値を
持たない使い方だけです。

**この線引きは 2026-09-22 に書き換わりました。** Reading Tier が消え、Jev が
返すのは問いへの Noul（連続値）そのものになったので、「離散 Tier を連続値で
置き換えるな」という禁止は対象を失いました。設計書が禁じているのは
**文書の側の属性としての 0〜100 importance score** であって、
**問いとの距離**はそれではありません（設計書「問いとスコア」）。
残っているのは `confidence` / `probabilities` の使い道だけで、閾値を持つ
使い方は実測で不採用のままです。

**確認したこと**: `crates/semantic-reading/src/protocol.rs` の
`AnalyzeResponse` が `version` と `units` しか持たないこと
（`confidence` / `probabilities` というフィールドはこの crate のどこにも
無い）。`examples/semantic/jev-annotate.py` が各 Unit の `jev` フィールドへ
`score` / `core_confidence` を書いていること。
`docs/design/jev.md` に未解決として記録済みであること。

### 4. 同一 Tier 内の rule の逐次性 — `context preservation`（**2026-09-22 に機能ごと削除**）

> この項目は 2026-09-21 に「解決済み」として閉じ、翌 2026-09-22 に
> **機能ごと削除**しました（`Presupposes`・閉包・二段台帳・`policy::floor`）。
> 以下は当時の記録で、**ここに出てくるコードはもうありません。**

> **2026-09-21 に実装しました。** 以下は「何が未解決だったか」と「何を決めて
> 解決したか」の記録です。**まだ残っている部分**は末尾にあります。

設計書は同一 Tier 内の rule として
`redundancy / length / document position / context preservation` の 4 つを
挙げ、`policy::keep_order` は最初の 3 つしか使っていませんでした。これは
**「4 つのうち 1 つを落とした」ではありません。**

最初の 3 つは Unit 単体の属性から決まる静的な値です（重複しているか・
何バイトか・文書のどこにあるか）。4 つ目だけが
**「残った Unit を順に読んだとき文脈が繋がるか」**という、選択の結果に依存する
性質を指しています。satisficing は逐次的なモデルなので、4 つ目はこの層で
**逐次性を担う唯一の項目**でした。

#### 何を決めたか

**設計書は `context preservation` の中身を定義していません**（語が出てくるのは
rule の列挙 1 箇所だけ）。**設計書は列挙のままにして、定義は実測と
`crates/semantic-reading/src/policy.rs` に置く**ことにしました。勝手に設計書へ
書き足すのではなく、「測って選ばれた形を実装した」という形にしてあります。

材料は
[`examples/semantic/measurements/context-preservation.md`](../../examples/semantic/measurements/context-preservation.md)
（4 文書 × 4 ラン × 2 版）。

| | 決めたこと |
| --- | --- |
| 依存の判定 | 判定器（`jev-annotate.py`）。段階 1 Noul →段階 2 **Choice** →当て木 Noul。直接の前提は最大 2、**閾値はどこにも置かない** |
| 選択肢 | **DETAIL だけ落とす。** 字義の「SUPPORTING 以上」だと、実測で穴の相手（2 つとも CONTEXT）が構造的に死ぬ |
| 運搬 | `Relation::Presupposes`。**`RedundantWith` とは向きは同じで効き方が逆**（あちらは持ち主を弱め、こちらは指した先を引き上げる） |
| 閉包と予算 | `policy::decorate`。**台帳は二段**で、一段目が核とその閉包を Budget を見ずに確保し、二段目が残りを `keep_order` の prefix で取り合う。**根まで辿り、前提も予算に数える**（数えないと「30 % と言って 45 % 出る」）。2026-09-22 に一段から二段へ直した（下の「台帳の単位」） |
| 表示 | **変えない。** 前提は読み手に知らせるものではなく、一緒に生き残らせるもの |
| 版 | 上げない（理由は `protocol.rs`） |

**第 1 版の「0.4 なら閉包が爆発、0.5 なら穴を取り逃がす」という二択は、
閾値の問題ではなく primitive の取り違えでした。** 対の Noul は依存ではなく
**関連**を測っていて（役を入れ替えても「はい」になる対が 0.4 で 7.6〜32 %）、
関連は沢山あるから扇が広がります。Choice は突き合わせて 1 つ返すので扇が
構造的に 1 になり、**閉包は 0.5 の大きさのまま 0.4 の再現率**になりました。

#### 台帳の単位 — 2026-09-22 に直しました

台帳が一段だった頃、**READ を下げると「最低限これを読め」が消えました**
（READ 20 % で MARKED 3 個 → READ 1 % で 1 個）。**予算は Unit で数え、
マーカーは Atom に置いている**というずれが原因で、Unit が予算に入らなければ
その中の核の一文も一緒に沈んでいました。

二段にして直しました。一段目が核（MARKED になりうる Unit）とその閉包を
**Budget を見ずに**確保し、二段目が残りを取り合います。**これは
「閉包を払うのは MARKED の分だけ」を含みます** — 二段目の Unit は自分の
前提を連れてこなくなりました。

- **逆転は消えました。** 26 Unit の小さい文書で MARKED 5 つのうち 2 つが
  落ちていた形は、実測で 5 / 5 になりました。**拾い直して直したのでは
  ありません**（それは best-fit で単調性を壊します）。核をそもそも取り合いに
  出さないので落ちなくなりました
- **MARKED が Budget の関数でなくなりました。** 「MARKED は Budget に依存
  しない」という `policy.rs` と設計書の主張は、一段の頃は「生き残った Unit に
  ついては真、落ちた Unit については偽」でした。いまは文字どおり成り立ちます
- **一段目が `budget` を超えることがあります。** 実測の記事では一段目だけで
  文書の 28.3 % を占めます。これは「最低限これを読め」は予算より先にある、
  という宣言です。2026-09-22 にその大きさを `policy::floor`（Budget の下限。
  一段目 ÷ 全体の切り上げ）として返すようにし、akapen は Budget を下限より
  下へ回さず、ステータス行に `READ 43% (floor)` と出します。**「READ 1 %」
  と表示しながら 4 割を出す状態は無くなりました。** 下限は測定値で、記事
  29 % / b1 30 % / b3 46 % / 設計書 43 %。**Tier の揺れと一緒に動きます** —
  業務議事録の 4 ランで 43〜56 %（幅 13 ポイント）。予算を下限に置いていると
  再解析で数字が引き上がることがあり、**黙って引き上げます**（2026-09-22 に
  決定。数字は常に正直で `(floor)` の印から状態が読めるので、toast は
  「何が起きたか」を足すだけで「何をすればいいか」を持たない — ステータス行
  の警告を却下したのと同じ理由。証拠は repo 外 `runs/2026-09-22-read-floor/`）

#### 単調性は壊れていません

支えているのは「**一段目が Budget に依存しない**」「`keep_order` が Budget に
依存しない」「打ち切りが `break` である」の 3 点だけで、**どれも辺の構造を
使っていません**。二段目は固定の下駄を履いて始まり、各 rank で払う額は rank
だけの関数なので、累積額の列は Budget に依存しない固定列になり、Budget が
決めるのは「どこで初めて越えるか」だけです。詳細は `policy.rs`。

#### まだ残っていること

- **前半への偏りは残っています。** 前提を数えると、残る部分の重心が実測で
  46.1 % から 28.1 % へ前へ寄ります。**二段にしても戻りませんでした**
  （同じ 28.1 %）。原因は台帳の単位ではなく、`PRESUPPOSES` が後ろ向きにしか
  立たないこと（前提は必ず自分より前にある）です。詳細は
  `examples/semantic/measurements/context-preservation.md` 第 4 版
- **`REDUNDANT_WITH` の側は相対になっていません。** `keep_order` の
  `redundant` は `SemanticUnit::is_redundant()` であって、参照先がその Budget で
  残っているかを見ていません。u0 を参照して弱められた Unit は、u0 自身が DIM に
  なる Budget でも弱められたままです。**今回入れたのは `PRESUPPOSES` の側だけ**
- **節点が小さい文書。** 1 Unit 平均 54 バイトの文書では、採った辺の 8 本に
  1 本が逆向きにも立ちます。primitive を替えても残りました（むしろ悪化）。
  **question の形ではなく節点の大きさの問題**で、そこは測っていません
- **当て木が境目すれすれ。** 実測で穴 A の 2 本目は Noul 0.52（境目 0.5 の
  0.02 上）でした。`docs/design/jev.md` が挙げる `confidence` の揺れ ±0.07 が
  ここに乗れば裏返ります。「捕まえた」であって「安定して捕まえる」ではない

**確認したこと**: `policy::decorate` の一段目が核と閉包を Budget を見ずに
確保し、二段目が `break` で打ち切ること。参照実装 `two_tier_kept` と
`decorate` の残る集合が、4 文書 × 2 条件 × 4 ラン × 9 予算 = 288 件で
完全に一致したこと（突き合わせは repo 外で実施）。34 fixture × 予算
1..=100 = 3,400 点で単調性違反が 0 件、MARKED が動いた点が 0 件だったこと。

### 5. Jev の context window は **2 つ**の制約で縛られている

> **2026-09-21 に片方が外れました。** 64k の側は
> [`send_in_chunks`](../../examples/semantic/jev-annotate.py) がリクエストを分割
> して外します。**残っているのは 32k の側だけ**で、そちらが分割後の本当の
> 天井です（下の「5.3 分割後の天井」）。以下の「失敗」は**分割前**の記録です。

**`--semantic-cmd` で Jev を繋いだとき、大きな文書の制約はタイムアウトでは
ありません。** 先に当たるのは Jev の **context window** です。超えると**約 1〜2 秒で** HTTP 400
`max_tokens_exceeded` が返ります — 待っても変わりません。

公式値は `https://docs.typesafe.ai/models.md` の Jev 1.13 の表にあります。逐語:

> Context length | 64k tokens per request; **32k tokens for `state` plus the
> longest question**

**制約は 2 つあります。合計だけを見ていると足をすくわれます。**

| 制約 | 値 | 何に効くか |
| --- | ---: | --- |
| 1 リクエスト全体 | 64k tokens | question を並べすぎると当たる |
| `state` + **最長の** question | 32k tokens | 1 つの question が大きいと当たる |

2 つ目は**合計が 64k に収まっていても落ちます**。実測（2026-09-21）:

| `state` | 最長 question | 合計 | 結果 |
| ---: | ---: | ---: | --- |
| 20k | 12k | 32,305 | OK |
| 20k | 14k | — | **失敗** |
| 25k | 10k | — | **失敗** |
| 30k | 1k | 31,281 | OK |
| 30k | 3k | — | **失敗** |

64k の側の切れ目も二分探索しました（`state` 固定・ダミー question を増やす）:
**`usage.input_tokens` が 65,771 で成功・65,874 で失敗**。公式の「64k」
＝ 65,536 より約 235 上で切れるので、**budget は `usage` とは別の数え方を
している**ようです（そこは詰めていません）。分母には公式値 65,536 を
使ってください。

各文書の `state`（実測）:

| 文書 | バイト | `state` tokens | 32k 枠に対して |
| --- | ---: | ---: | ---: |
| `examples/semantic/demo.md` | 1,664 | 852 | 3 % |
| `docs/design/semantic-reading-layer.md` | 9,857 | 3,626 | 11 % |
| `examples/semantic/README.md` | 28,172 | 11,180 | 34 % |
| 実業務の `CLAUDE.md` | 45,650 | 17,561 | 54 % |

**`state` だけで 32k を使い切る文書では、question を 1 つも出せません。**

> **「日本語なら 85 KB 前後」という外挿がここにありました。** 当たっては
> いましたが、**1 つの係数で外挿するのが間違い**でした。tokens/byte は
> 実測で 0.341〜0.399 と文書ごとに 17 % 違い、同じ 32k 枠が 82 KB にも
> 96 KB にもなります（5.3）。

#### 核の question は個数ではなくトークンで切る

ラウンド 3（核）は Unit の全散文 Atom を選択肢として引用するので、Atom を
多く持つ Unit は**単独で巨大な question**になります。だから
`MAX_CORE_CHOICES = 255` のような固定値は**どの文書でも正しくありません** —
上限は `state` の大きさに依存するからです。

実測: 45.6 KB の `CLAUDE.md` の最大 Unit は 96 Atom（散文 82 個）で、
question は 5,469 tokens、`state` 込みで 23,030 tokens ＝ **32k 枠の 70 %**。
同じ形で選択肢が 255 個なら約 17k になり、`state` 17,561 と合わせて 32k を
**超えます**。

`jev-annotate.py` は [`core_budget`] で `32,768 − マージン − state の見積もり`
から毎回計算します。見積もりは **0.5 tokens/byte**（実測は state が
0.34〜0.39、核の選択肢本文が 0.496。**多めに出る側へ倒してあります**）。
超える Unit には核を聞かず、Unit 全体が MARKED になります — 絞り込めない
だけで注釈としては壊れない、安全側の振る舞いです。

#### 固定費は Unit 1 つあたり 262 tokens（**ラウンド 2 に乗るのは 181 だけ**）

**262 で見積もると外します。** redundancy は SUPPORTING 以上の Unit にしか
聞かず、しかもラウンド 3 に置いてあるので、**ラウンド 2 に乗る固定費は Tier の
181 だけ**です。2026-09-21 に 262 で「規則 4 を入れると context window を
超える」と予測して外しました（実測は 62 % → 89 % で収まった）。

本文を除いた question 1 つあたりの実測（2026-09-21）:

| 部品 | tokens |
| --- | ---: |
| Tier question 全体 | **181** |
| ├ 器（型と criteria のキー名） | 68 |
| ├ criteria の説明文 4 つ | 65 |
| └ instructions の枠組み文 | 65 |
| redundancy question 全体（**2026-09-22 まで**。下記） | **81** |
| **Unit 1 つあたり合計** | **262** |

**器の 68 tokens は削れません。** `README.md`（113 Unit）のラウンド 2 は
`state` 11,180 + 本文の 2 度引き 21,818 + 固定費 29,525 = 62,523 tokens で、
**固定費が全体の 47 %** を占めていました。

だから効く手は「1 Unit あたりの question を減らす」ことです。redundancy を
SUPPORTING 以上にだけ聞くようにして（[`redundancy_questions`]）、
`README.md` のラウンド 2 は 62,523 → **50,895 tokens（天井の 78 %）**に
下がり、**通るようになりました**。

**redundancy の 81 tokens はもう固定費ではありません**（2026-09-22）。Choice に
替えたので選択肢が自分より前の Unit の本文全部になり、**Unit 数の 2 乗**で
効きます。ラウンド 3 は実測で 2.7〜17 倍、大きい文書で 1 → 8〜13 リクエスト
（`examples/semantic/measurements/redundancy.md` の 5 節）。ラウンド 2 側の
話は変わりません。

#### 採らなかった手と、その理由

| 案 | なぜ採らなかったか |
| --- | --- |
| Tier と redundancy を 1 つの Choice に畳む（「既出の言い直し」を 5 つ目の選択肢に） | **設計書が別軸と定めている**（`reading_tier = SUPPORTING` と `redundant_with = u3` が同居する）。実測でも軸が消えた — `demo.md` の u9 / u10 は 4 つの Tier の probability が**すべて 0.0** になり、Tier は tie-break 次第（同じ question で `detail` と `context` の両方が出た） |
| question の問い文を短くする | **判定が壊れる。** Tier 一致率が README で 51〜54 %、design で 42〜46 % まで落ち、ESSENTIAL の比率が README で 3.5 % → 44.2 % に膨らんだ。「次の部分は」が消えると、引用した本文が対象だと分からなくなるらしい |
| `――― 対象 ―――` の枠を外す | **文書によって結果が割れた。** 枠だけを外し問い文はそのままにした版（mode b で Unit の組を揃え、3 回ずつ）は `README.md` で 94.7〜98.2 %（床 96.5〜97.3 % の中）だが、`design` では 82.4〜84.8 %（床 92.6〜95.9 %）で明確に下回る。減るトークンは 1 Unit あたり 19〜38 しかなく、`design` の劣化に見合わない |
| 段落 = Unit にして境界 question を出さない（方式 B） | **誤分割が直せていない。** `design.md` で Unit が 150 → 165 に増える。「空行が Atom と Atom の**間**にあるときだけ段落境界と見なす」という修正は**実測で no-op**（4 文書のどの Atom も range に末尾改行を含まず、境界判定が 1 件も変わらない）。「読点で終わる Atom を後続へ付ける」案は**悪化**（33 件のうち Jev も SAME と答えたのは 0 件） |

### 5.1 Tier 一致率の「揺れの床」を先に測ること

**判定品質を一致率で測るなら、まず「何も変えずに 2 回走らせたときの一致率」を
測ってください。** Jev の答えは実行ごとに揺れるので、床を知らずに閾値を置くと
**「変更なし」すら棄却する**基準になります。

実測（`current` モード、6 ラン、Unit は構成 Atom の組で対応づけ）:

| 文書 | 現行どうしの Tier 一致率 |
| --- | --- |
| `demo.md`（16 Unit） | 100 % |
| `design/semantic-reading-layer.md`（約 150 Unit） | **92.6〜95.9 %** |
| `README.md`（113 Unit、mode b） | 96.5〜97.3 % |
| 実業務の `CLAUDE.md`（26 Unit、mode b） | 92.3〜96.2 % |

この計測の前に「一致率 95 % 以上」という条件を置いていましたが、
**design では床が 95 % を割るので到達不能**でした。変種の一致率は
**床と並べて**読んでください。床の中に収まっていれば「劣化していない」、
床を明確に下回っていれば「変種のせい」と切り分けられます。

### 5.2 4 文書 × 方式の実測（2026-09-21）

ラウンド 2 の input tokens と、天井 65,536 に対する割合。3 回ずつ。

| 文書 | 現行（redundancy を全 Unit に） | **採用（SUPPORTING 以上だけ）** |
| --- | --- | --- |
| `demo.md`（1.6 KB） | 6,088 (9 %) | 4,329 (7 %) |
| `design/…`（9.8 KB） | 49,349 (75 %) | 34,028〜34,389 (52 %) |
| `README.md`（28.2 KB） | **失敗**（推定 76k、116 %） | **50,895〜51,438 (78 %)** |
| 実業務の `CLAUDE.md`（45.6 KB） | 60,772〜61,034 (93 %) | 40,911〜41,092 (62 %) |

所要はどれも 2.2〜3.7 秒でした（**2 ラウンドの頃の数字**です。9 種になった
あとの実測は `examples/semantic/measurements/speed-and-limits.md`「第 2 版」に
あり、35 KB の文書で 47 秒です。akapen 側の見切り方も壁時計の
`COMMAND_TIMEOUT` から `COMMAND_IDLE_TIMEOUT`（無音 30 秒）＋
`COMMAND_BACKSTOP`（600 秒）に変わっています）。

**確認したこと**: `examples/semantic/jev-annotate.py` を akapen 抜きで単体実行し、
5 文書（1,664 / 9,857 / 15,656 / 24,280 / 45,650 バイト）を 2 回ずつ、45,650
バイトのものは 7 回走らせたこと。同じ文書を 1.02 / 1.04 / 1.06 / 1.10 / 1.25 /
1.50 倍に伸ばし、1.06 倍から `max_tokens_exceeded` になること。`state` を固定して
question 数を二分探索し、input 65,771 tokens が成功・65,874 が失敗すること。
`state` と最長 question を独立に振って 32k 側の制約を確認したこと。
要求 JSON は `cargo run -p semantic-reading --example dump-request` で akapen 本体と
同じ `atomize` 経路から作っていること。4 文書とも akapen 本体で実際に開き、
`README.md` が通ること・`demo.md` の `## 結論` の行が MARKED / DIM に
分かれることを画面で確かめたこと。

**再現しなかったこと**: 「45.6 KB の文書の解析に 30〜60 秒かかる」という報告は
**再現していません**。同じ文書・同じアダプタで 7 回測って 2.42〜2.63 秒でした。
`App::accept_analysis` は失敗時にも `semantic_inflight` を落とすので、「解析中」
表示が残り続けることもありません。**原因は特定できていません。**

### 5.3 分割後の天井は `state` が 32k に収まるか（2026-09-21）

**64k の側は外れました。** [`send_in_chunks`] が question をトークンで詰めて
複数のリクエストへ分け、`state` を毎回丸ごと付け直します。**残るのは 32k の
側だけ**で、`state` はどのチャンクにも乗るので分割では小さくなりません。

天井は二分探索で `state` **32,609 tokens が成功・32,768 tokens 相当で失敗**。
**公式値 32,768 にそのまま載っています** — 64k 側で見えた「公式値より約 235
上」というずれは、こちらにはありません。

**ただし実用の天井はそれより低い。** `state` が収まっても、**question を
置く余地が残らなければ 1 つも聞けません**。しかも Tier question の固定費は
ほぼ一定（実測 181）なので、**崖であって坂ではありません** — 予算がそれを
下回った瞬間に全 question が同時に落ちます。

| 天井 | `state` tokens | 合成文書でのバイト数 |
| --- | ---: | ---: |
| **実用（この実装）** | ~29,700 | **約 80 KB** |
| API が受け付ける限界 | ~31,000 | 約 83 KB |
| `state` だけが収まる限界 | 32,609 | 約 89 KB |

**1 つも聞けないときは失敗させること。** 塞ぐ前は 83 KB の文書が exit 0 で
**530 Unit すべて `detail`** を返していました。`state` は 32k に収まるので
probe は通り、足りないのは question のぶんだけ — いちばん気づきにくい
壊れ方です。`UNANSWERED_TIER` は個別の巨大な Unit のための落とし先であって、
文書全体の落とし先ではありません。

**バイト数で言わないこと。** 実測の tokens/byte は文書ごとに違います。

| 文書 | バイト | `state` tokens | tokens/byte |
| --- | ---: | ---: | ---: |
| `examples/semantic/demo.md` | 1,664 | 583 | 0.350 |
| `docs/design/semantic-reading-layer.md` | 9,857 | 3,357 | **0.341** |
| `examples/semantic/README.md` | 58,032 | 23,147 | **0.399** |
| `docs/gotchas.md`（分割前） | 65,225 | 24,169 | 0.371 |
| `docs/handoff-archive.md` | 106,518 | — | **32k 超過** |

同じ 32k 枠が、`design` の混ざり方なら約 96 KB、`README.md` の混ざり方なら
約 82 KB です（**この 2 つは外挿**。実測したのは `handoff-archive.md` の
先頭を切り詰めた合成文書での 89 KB だけ）。

以前この節に**古い `state` の値**が残っていたことにも注意してください
（`README.md` は 28,172 B / 11,180 tokens と書いてありますが、いまは
58,032 B / 23,147 tokens です）。**文書は育ちます。天井比は測り直すこと。**

#### 見積もりでは 32k 側を判定できない

`0.5 tokens/byte` は 65 KB の `docs/gotchas.md` の `state` を **32,612** と
見積もります。真値は 24,169 なので、見積もりを信じると「question を 1 つも
送れない文書」に見え、**分割で救えるはずの文書を落とします**。

だから [`measure_state_tokens`] は、見積もりが 16,384（32k 枠の半分）を超える
文書についてだけ、最小の question を 1 つ付けた**リクエストを 1 本だけ**投げて
`usage.input_tokens` を読みます。小さい文書は測りません。

**0.5 は question の本文には残してあります。** そちらは多めに倒すのが安全側
だからで、外すべきだったのは `state` の側だけです。

#### 分割は意味的に中立だった（床と重なる）

| 文書 | Unit | ラン | 床 | 変種 | 判定 |
| --- | ---: | ---: | --- | --- | --- |
| `demo.md` | 17 | 8 | 100.0 % | 100.0 % | 床の中（**幅ゼロ**） |
| `design/…` | 172 | 4 | 91.3〜95.9 % | 90.7〜95.9 % | 重なる |
| `examples/semantic/README.md` | 306 | 4 | 97.4〜98.4 % | 96.7〜99.0 % | 重なる |
| `gotchas.md`（分割前） | 244 | 4 | 93.9〜95.1 % | 92.2〜96.3 % | 重なる |

判定は「退行の判定」の規則（**同じラン数で幅が重なるか**）に従っています。
床は 6 組（C(4,2)）、変種は 16 組（4 × 4）で**組の数が違う**ので、変種の幅は
両側に広く出ます — 下限だけを取り出して劣化と読まないでください。

**`demo.md` の床は幅ゼロなので判定に使えません。** 4 ランで測った別のバッチ
では床 100 %・変種 94.1〜100 % で、同じ規則なら「床を下回る」と読めました
（8 ランでは再現せず）。`docs/gotchas/semantic-reading.md`「2 ランの床を床と
呼ばない」と同じ罠が、**ゼロ幅の
床**でも起きます。

`README.md` と `gotchas.md` で比べているのは「分割したかどうか」ではなく
**「切れ目の位置を変えたかどうか」**です（非分割がそもそも通らないので）。

詳しい表と手順は `examples/semantic/measurements/request-splitting.md`。

**確認したこと**: 凍結した 5 文書のコピー（`shasum -a 256` を控えた）に対して、
分割前のコミット `2203d93` と分割後で各 2〜3 回。`state` の tokens は 5 文書 +
二分探索 6 点を `usage.input_tokens` から（空 `state` の floor 282 を引いた値）。
Tier 一致率は 4 文書 × 床と変種 × 4〜8 ラン、ラウンド 1 を 1 度だけ走らせて
Unit の組を固定。表示は `decorate-report` を 65 KB / 248 Unit の
`docs/gotchas.md` に当てた。**していないこと**: akapen 本体の TUI では開いて
いません。並列化は測っていません。

### 6. 取れたラウンドまでを使うには「劣化の印」が要る（2026-09-22 に評価だけした）

`--semantic-cmd` が見切られると、読み手は**約 8 円払って何も得ません**。
子が殺されるので stdout は空（アダプタは JSON を終了時に 1 回だけ書く）、
`SemanticCache` にも入らないので開き直せばもう一度払います。

**「取れたラウンドまでの結果を使う」案は、プロトコルの上では成立します。**
`relations` / `core_atoms` / `section_of` はどれも省略可なので
（`crates/semantic-reading/src/protocol.rs` のテスト
`relations_may_be_omitted`）、**boundary ＋ tier だけの応答も妥当な
`SemanticDocument`** です。得られるものと失うものは:

| | boundary + tier だけのとき |
| --- | --- |
| DIM | **正しく出る**（Tier と Budget だけで決まる） |
| MARKED | **粗くなる** — `core_atoms` が無いので Unit 丸ごと（核を入れる前の挙動） |
| 冗長の沈み | 出ない（`relations` が空） |
| 前提の波 | 出ない（同上） |
| 見出しの追随 | **効く**（`section_of` は構文由来で Jev の判定ではない） |

費用も安いです。35 KB の文書で **probe + boundary + tier = 5 リクエスト /
5.4 秒 / 約 0.8 円** — 全体 47 秒・8.3 円の 11 % と 10 % です。

**それでも実装していません。2 つ足りないからです。**

1. **キャッシュの汚染。** `SemanticCache` のキーは
   `sha256(source)` ＋ `sha256(--semantic-cmd の文字列)` だけなので、劣化した
   文書も同じキーで `put` され、**一度掴むと二度と良くなりません**。
   「劣化した結果はキャッシュしない」か「劣化の印を持つ」かのどちらかが
   必須で、後者は**プロトコル変更**です（応答に新しいフィールドが要る）
2. **読み手への表示が無い。** いまの akapen は劣化した文書と完全な文書を
   区別できません。何の表示もないまま marks が粗くなるのは、
   「動いているのに壊れて見える」の別の形です。ステータス行は
   読み出し（`Essential · 12 · 20%`）と `analyzing…` しか持っていません

**確認したこと**: `protocol.rs` の `relations_may_be_omitted` と
「`core_atoms` は 3 値」の表。`src/semantic.rs::CommandProvider::analyze` が
`cache.put` を成功時にだけ呼ぶこと（失敗時は書かない）。
`src/app.rs::accept_analysis` が `Err` を `flash_err` へ渡すこと、
`src/chrome.rs` の `read` クロージャが持つ状態が
`analyzing…` / `READ n%` / 無表示の 3 つだけであること。
ラウンドごとの内訳は
`~/.local/share/akapen/evidence/runs/2026-09-22-command-timeout/README.md`。
