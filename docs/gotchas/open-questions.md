# 未解決（地雷ではなく、設計判断が要るもの）

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。
**文中に測定対象として出てくる `docs/gotchas.md` /
`examples/semantic/README.md` のバイト数・Unit 数は分割前の値です。**

---


刺しにくるものではありませんが、**設計書と実装の差**として残っています。
どれも「誤記」ではないので、直すには判断が要ります。

### 1. `cache` と `incremental reanalysis` は、実測して作らないと決めた

設計書 [`design/semantic-reading-layer.md`](../design/semantic-reading-layer.md)
の MVP は `cache` と `incremental reanalysis` を挙げていますが、どちらも
ありません。文書が入れ替わるたびに**全文を再解析**します。
`RELOAD_DEBOUNCE`（300ms）はファイル変更の debounce であって解析結果の
キャッシュではありません。

**これは未実装の取り残しではなく、実測に基づく判断です。**
`examples/semantic/demo.md`（624 文字）に対する Jev の全文再解析は
**2 ラウンドで 1.4 秒 / 約 $0.0004**。しかも `--semantic-cmd` 経路の解析は
別スレッドで走り、その前にファイル変更の debounce が 300ms 入ります。

**2026-09-21 に大きな文書で測り直し、この判断は維持されました。** かつてここには
「測ったのは 624 文字の 1 ファイルだけ」という但し書きがありました。実業務の
45,650 バイトの文書（`docs/design/jev.md` の「大きな文書での実測」）でも
**2.42〜2.63 秒 / 約 $0.0035**で、1,664 バイトの demo.md の 1.5 秒から
1.7 倍にしかなりません。文書が 27 倍になっても時間はほぼ伸びない —
Jev が全 question を 1 リクエストで並列評価するためです。

**しかも、これより大きい文書はキャッシュの有無に関係なく解析できません**
（下の項目 5）。つまり「大きくなったらキャッシュが要る」という想定していた
成長経路自体が、途中で別の壁に当たります。`Provider` の doc が「キャッシュや
debounce、rate limit は実装側が内部に持てばよい」と委譲しているので、
**置き場は空けたまま**にしてあります。

**確認したこと**: `src/app.rs::reanalyze_semantics` が毎回
`provider.analyze(&self.source.content)` に文書全文を渡していること。
`SemanticSource::Command`（Jev を繋ぐ経路）の腕が `std::thread::spawn` で
別スレッドへ投げ、その場で待たないこと。
`App` に解析結果のキャッシュ用フィールドが無いこと
（`semantic_decorations` は doc × budget の投影であって解析のキャッシュでは
ない）。`RELOAD_DEBOUNCE`（300ms）の定義は `src/app.rs`。
1.4 秒 / $0.0004 と 624 文字の出どころは
[`design/jev.md`](../design/jev.md) の「実測」節と
`examples/semantic/README.md`（`wc -m examples/semantic/demo.md` が 624）。
大きな文書の数字は同じ 2 つの「大きな文書での実測」節（5 文書 × 2 回、
45,650 バイトのものは 7 回）。

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

線引きは「順位付けか / ガードか」ではなく **「Tier の中か / Tier そのものか」**
です。同一 Tier 内の順序に使うのは設計どおり（`policy::keep_order` は
バイト長という連続値を既に使っています）。駄目なのは離散 Tier を連続値で
置き換えることで、そこは設計書の「0〜100 の importance score は使用しない」に
直接反します。

**確認したこと**: `crates/semantic-reading/src/protocol.rs` の
`AnalyzeResponse` が `version` と `units` しか持たないこと
（`confidence` / `probabilities` というフィールドはこの crate のどこにも
無い）。`examples/semantic/jev-annotate.py` が各 Unit の `jev` フィールドへ
`tier_confidence` / `redundancy_noul` を書いていること。
`docs/design/jev.md` に未解決として記録済みであること。

### 4. 同一 Tier 内の rule の逐次性 — `context preservation`（**解決済み。2026-09-21**）

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
| 閉包と予算 | `policy::decorate`。`keep_order` の順に、その Unit と未払いの閉包を一緒に払う。**根まで辿り、前提も予算に数える**（数えないと「30 % と言って 45 % 出る」） |
| 表示 | **変えない。** 前提は読み手に知らせるものではなく、一緒に生き残らせるもの |
| 版 | 上げない（理由は `protocol.rs`） |

**第 1 版の「0.4 なら閉包が爆発、0.5 なら穴を取り逃がす」という二択は、
閾値の問題ではなく primitive の取り違えでした。** 対の Noul は依存ではなく
**関連**を測っていて（役を入れ替えても「はい」になる対が 0.4 で 7.6〜32 %）、
関連は沢山あるから扇が広がります。Choice は突き合わせて 1 つ返すので扇が
構造的に 1 になり、**閉包は 0.5 の大きさのまま 0.4 の再現率**になりました。

#### 単調性は壊れていません

支えているのは「`keep_order` が Budget に依存しない」と「打ち切りが `break`
である」の 2 点だけで、**どちらも辺の構造を使っていません**。各 rank で払う額は
rank だけの関数なので、累積額の列は Budget に依存しない固定列になり、Budget が
決めるのは「どこで初めて越えるか」だけです。詳細は `policy.rs`。

#### まだ残っていること

- **逆転。** 前提を予算に数えると、長い系譜を持つ ESSENTIAL が払えずに落ち、
  系譜の無い格下が繰り上がります。実測で 26 Unit の小さい文書は MARKED
  5 つのうち 2 つが落ちたまま（第 1 版と同じ）。**これは「前提を数える」と
  決めた時点で決まる帰結**であって、拾い直すと best-fit になり単調性が壊れます
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

**確認したこと**: `policy::decorate` の打ち切りループが
`need = {i} ∪ closure(i)` を払って `break` すること。実測の集計スクリプト
`charged_kept` と `decorate` の残る集合が、4 文書 × 2 条件 × 4 ラン ×
9 予算 = 288 件で完全に一致したこと（突き合わせは repo 外で実施）。
実機 1 文書の予算 1..=100 で単調性違反が 0 件だったこと。

### 5. Jev の context window は **2 つ**の制約で縛られている

> **2026-09-21 に片方が外れました。** 64k の側は
> [`send_in_chunks`](../../examples/semantic/jev-annotate.py) がリクエストを分割
> して外します。**残っているのは 32k の側だけ**で、そちらが分割後の本当の
> 天井です（下の「5.3 分割後の天井」）。以下の「失敗」は**分割前**の記録です。

**`--semantic-cmd` で Jev を繋いだとき、大きな文書の制約はタイムアウトでは
ありません。** `COMMAND_TIMEOUT`（60 秒）には 15 倍以上の余裕があり、先に
当たるのは Jev の **context window** です。超えると**約 1〜2 秒で** HTTP 400
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
| redundancy question 全体 | **81** |
| **Unit 1 つあたり合計** | **262** |

**器の 68 tokens は削れません。** `README.md`（113 Unit）のラウンド 2 は
`state` 11,180 + 本文の 2 度引き 21,818 + 固定費 29,525 = 62,523 tokens で、
**固定費が全体の 47 %** を占めていました。

だから効く手は「1 Unit あたりの question を減らす」ことです。redundancy を
SUPPORTING 以上にだけ聞くようにして（[`redundancy_questions`]）、
`README.md` のラウンド 2 は 62,523 → **50,895 tokens（天井の 78 %）**に
下がり、**通るようになりました**。

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

所要はどれも 2.2〜3.7 秒で、`COMMAND_TIMEOUT` には遠く届きません。

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
