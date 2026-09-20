# 地雷（gotchas）— 次に触る人を刺すもの

ここに書いてあるのは「知らずに触ると静かに壊れるところ」だけです。
**何をしたかの物語は書きません**（それは `git log` の領分）。
解決済みの話も書きません（消してください）。

## この文書の約束

- **キュレートします。追記専用にはしません。** 項目が解消したらその場で
  削除する。増える一方の文書は読まれなくなり、腐った記述が残ります
- 各項目には **「何を確認して、いま真だと判断したか」** を付けます。
  次の人が同じ手順で再検証できるように、ファイル名・関数名・テスト名で
  書きます（行番号は動くので主にしません）
- **正典はコードとテストです。** ここと食い違ったらコードが正しい。
  食い違いを見つけたらこの文書を直してください
- 関連: 仕様と設計は [`design/`](design/)、現在の設計不変条件は
  [`internals.md`](internals.md)、過去の引き継ぎメモは
  [`handoff-archive.md`](handoff-archive.md)（歴史的資料）

---

## 描画と attribution

### `ViewState` に行を差し込むときは `row_attrs` も対で差し込む

`ViewState` は `rows` / `row_segments` / `row_attrs` / `card_rows` が
**行単位で並行**な構造です。どれか 1 つに `insert` して他を忘れると、
**その行より下の attribution が 1 行ぶん静かにズレます。** 落ちないし、
見た目も「ただの誤判定」にしか見えません。

差し込んでいるのは 2 箇所（`src/main.rs`）:

- `insert_cards` — インラインのコメントカード行
- `insert_history_ghosts` — 履歴 ghost 行

どちらも合成行なので、attribution は `vec![None; spans.len()]` です
（`vec![None]` ではありません。カード行も ghost 行も複数 span を持ちます）。
3 つ目の差し込み箇所を書くときは、4 本すべてに入れてください。

**確認したこと**: `src/main.rs` の `insert_cards` / `insert_history_ghosts`
に `rows` / `card_rows` / `row_segments` / `row_attrs` の 4 本が並んでいる
こと。`src/render.rs` の `insert_missing_blank_rows` も同じ形で 3 本
（`rows` / `segments` / `attrs`）を揃えていること。防御として
`ViewState::visible_text_with_glow` の入口に
`debug_assert!(decorations.is_empty() || row_attrs.len() == rows.len())`、
`decoration::decorate_row` の入口に `debug_assert_eq!(row.len(), attrs.len())`
があること。テストは `src/state_tests.rs` の
`a_comment_card_keeps_row_attrs_parallel_to_the_rows` と
`a_decoration_below_a_comment_card_still_lands_on_its_own_row`。

### 折返しの hanging pad にも装飾の背景が乗る

折返し行の頭に入るぶら下げ空白（hanging pad）は、元 span の range を
**そのまま引き継いだ非 exact な attribution** を持ちます。したがって
段落や項目を丸ごと MARKED にすると、pad にも背景色が乗ります。
交差ルールどおりの動作なので直す対象ではありませんが、
**「本文だけに色を付けたつもりが左端の空白まで塗られる」** という
見え方になる点は覚えておいてください。

**確認したこと**: `src/highlight.rs::wrap_spans_tagged` で pad の
attribution が `attr.as_ref().map(Attr::demoted)`（range は維持、exact は
落とす）であること。使い捨てのテストで実測: 幅 20 で折り返した
`- これは折り返すほど長いリスト項目の…` を文書全体 mark すると、継続行
先頭の `"  "` span の bg が `mark_style().bg`（既定ダークテーマで
`Rgb(68, 70, 89)`）になりました。pad が exact を名乗らないことは
`src/render.rs::a_hanging_pad_is_never_exact` が固定しています。

### タブを含む行は、fragment の途中で終わる装飾が効かない

`wrap_spans` はタブを空白へ展開します。展開した fragment はもう source の
verbatim ではないので、range は保ったまま `exact` を落とします。すると
`decorate_row` の**上位集合の腕**に回り:

- fragment を丸ごと覆う装飾 → 効く
- fragment の**途中で終わる**装飾 → その fragment には何も乗らない

source view でこれが起きるのは**タブ行だけ**です（それ以外は全 fragment が
exact）。「タブのある行でだけ装飾が消える」という症状を見たらこれです。

**確認したこと**: `src/highlight.rs` の
`a_tab_expanded_fragment_keeps_its_range_and_loses_exactness`（fragment が
exact ⟺ そのテキストが今も `source[range]`、という一般形で固定）と
`source_wrapping_reaches_the_superset_branch_only_through_tabs`
（testdata 4 ファイル × 幅 8/17/40/80 を掃いて、タブ行以外は全 fragment が
exact であることを確認している）。

### `Dim` は前景色を書く — 帯の下では装飾する前に落とす

`Dim` は `Modifier::DIM`（SGR 2）ではなく**実際の前景色**を書きます
（SGR 2 を無視する端末が多く、装飾の目的を果たさなかったため）。
その結果、**背景の帯では打ち消せません** — 背景は前景を元に戻せない
ので、選択したのに文字が沈んだままになります。

対処は「帯が乗る行では `Dim` の装飾を当てない」です。新しい帯（背景を
行全体に塗るもの）を足したら、その判定にも足してください。忘れると
「カーソルを乗せた行の文字が霞む」というバグに戻ります。

**view と source で「帯」の定義が違います**:

| | 帯に数えるもの |
|---|---|
| view（`src/view.rs`） | selection / カーソル帯 / history glow（`let banded = glowing_row \|\| gutter_hl`） |
| source（`src/main.rs::build_rows`） | それに加えて **changed（緑）帯**（`if cursor_bg \|\| changed_bg`） |

source モードは changed 帯も行全体の背景を塗るためです（view は gutter の
マーカーだけ）。意図的な差です。

**確認したこと**: `src/view.rs` の `banded` による `DecorationKind::Dim`
除外と、`src/main.rs::build_rows` の `cursor_bg || changed_bg` による
`undimmed` の選択。テストは `src/view.rs` の
`the_selection_band_suppresses_dim_entirely`（帯の下は「装飾なしの行と
完全一致」）/ `the_cursor_band_suppresses_dim_as_well` /
`the_band_suppresses_only_dim_not_the_mark`（Mark は帯の下でも当たる）。

### `decorate_row` のコストは O(可視 span 数 × 装飾数) / フレーム

span ごとに装飾リストを線形走査します。手で数個〜 demo の Atom 数十個なら
測るまでもありませんが、**判定器が数千個の装飾を持ち込むと効きます。**
そのときは装飾を `range.start` でソートして二分探索へ切り替える余地が
あります（**先回りで実装しないこと。** いまは不要な複雑さです）。

**確認したこと**: `src/decoration.rs::decorate_row` と `push_exact` が
装飾リストをそのまま線形に走査していること（ソートも二分探索も無い）。

### 上位集合 attribution の range の「広さ」は当たり判定そのもの

`src/decoration.rs` の交差ルールは「上位集合 span は、装飾 range が
**完全に覆うときだけ**装飾する」です。にじみ出しを防ぐために意図的に
厳格にしてあります。その裏返しとして、**attribution の range が 1 バイト
でも広すぎると、その span は永久に装飾されません。**

実際に起きた例: リストマーカーの span を `Start(Item)` の event range
（pulldown-cmark は**末尾の改行込み**で返す）に紐づけ、一方 `atomize` は
Atom に末尾改行を含めない。`0..26` は `0..27` を覆えないので、DIM にした
項目のマーカーだけ明るく残りました。3 つのコンポーネント（atomize /
renderer / decoration.rs）はそれぞれ単独では正しく、噛み合わせだけが
1 バイトずれていた、という形です。

いまは renderer がマーカーをマーカー自身のバイトに紐づけるので直って
います（`a_list_marker_is_attributed_to_the_marker_alone`）。**注意が要る
のは一般則のほう**です:

- 合成 span を新しく attribution するときは、**その span が本当に由来する
  いちばん狭い source** を指すこと。`event_attr()`（event の range 丸ごと）
  を既定にすると、ブロック要素では広すぎます
- 装飾を出す側（Atom / 検索ヒット / diff）と attribution を付ける側は
  別のファイルにいます。**片方だけを見て「直った」と判断しないこと。**
  実機か TestBackend のセル比較で、装飾が実際に乗ることを確かめる

**確認したこと**: `src/decoration.rs::decorate_row` の
`d.range.start <= attr.range.start && d.range.end >= attr.range.end`
（上位集合の枝）と、`third_party/tui-markdown/src/renderer/list.rs::start_item`
がマーカーに付ける range。セル比較は
`src/state_tests.rs::a_dimmed_list_item_dims_its_marker_too`
（マーカーの前景色が本文と同じ dim 色まで沈むこと）。

### exact を増やすときは `line_of` が変わらないことを確認する

attribution の `exact` は「span のテキストが `source[range]` そのもの」と
いう強い主張で、byte でスライスしてよい印です。ただし exact を増やすと
**その span が指す行が変わることがあります**。たとえば link の URL span を
exact にすると、reference link では URL が文書の別の場所にある ref-def 行
から borrow されているため、`line_of` が別の行へ飛びます。HTML block の
各行を上位集合のままにしてあるのも同じ理由です。

判断基準は **「exact 化は `line_of` が構造上変わらない場所だけ」**。

**確認したこと**: `src/render.rs::line_of` が `Attr` → 行番号の唯一の
橋渡しであること（renderer 自身の `line_at` を使う）。構造別の
exact / 上位集合の一覧は
`third_party/tui-markdown/src/renderer/mod.rs::exactness_by_markdown_construct`
が正典で、link の URL が意図的に非 exact であること（理由のコメント付き）も
そこに書かれています。

---

## Semantic Reading Layer

### `FixtureProvider` は渡された source を見ない → `source_sha256` の照合が要る

`FixtureProvider::analyze(&self, _source: &str)` は JSON をそのまま返す
だけで、**開いている文書を一切見ません。** 別の文書の fixture を渡しても
「もっともらしい」range が返ってきます。`decoration::sanitize` は panic を
防ぐだけで、文字境界に載ってしまう嘘の range は通ります。

だから `SemanticDocument` の任意フィールド `source_sha256` を
`DigestChecked<P>` が `analyze` の中で照合し、不一致なら注釈を捨てて
`flash_err` します。**この照合を外すと、別の文書の判断が黙って現在の
文書に当たります。**

**確認したこと**: `crates/semantic-reading/src/provider.rs` の
`impl Provider for FixtureProvider`（引数が `_source`）と、その
テスト `fixture_provider_reads_json_and_ignores_the_source`。
`src/semantic.rs` の `DigestChecked::analyze` が `source_digest(source)` と
照合していること。なお `--semantic-cmd` 経路では range が akapen 自身の
`atomize()` 由来なので照合は不要です（`src/semantic.rs` の
`a_command_cannot_move_a_range_even_if_it_tries`）。

### タイムマシンで過去 revision を見ると fixture が拒否され、BEL が毎回鳴る

`--semantic` の fixture は NOW の文書に紐づいています。タイムマシンで
過去 revision を表示すると文書が入れ替わり、再解析が走り、digest 不一致で
拒否されます（装飾が消えるのは正しい。嘘の位置を装飾するよりよい）。

問題は**音**です。拒否メッセージには実 digest の先頭 12 桁が入るため、
**revision ごとに文言が変わります。** `is_repeat_error` は**同一文言**の
ときしか BEL を抑制しないので、抑制が効きません。**表示した revision ごとに
ビープが鳴ります**（矢印 1 押しごとではありません — 履歴描画は
`HISTORY_RENDER_DEBOUNCE` = 300ms で debounce されているので、スクラブ中は
表示が落ち着いたところで鳴ります）。

直すなら、digest をメッセージから外すか、拒否した digest を覚えて 1 回だけ
言う形にしてください（この項目だけで閉じる話です。解析結果のキャッシュは
**この規模では作らない**ので、それと一緒に入れることはできません — 未解決 1）。

**確認したこと**: `src/semantic.rs::DigestChecked::analyze` の
`SemanticError::Invalid` メッセージが `&expected[..12]` / `&actual[..12]` を
埋め込んでいること。`src/app.rs::is_repeat_error` が
`prev == msg && at.elapsed() < STATUS_SECS` でしか抑制しないこと。
拒否 → 音の経路は `App::accept_analysis` の `Err` 腕が `flash_err` を呼び、
`flash_err` が「新しいエラーのときだけ」BEL（`\x07`）を書いて flush する
こと。過去 revision の表示が `src/main.rs` の履歴描画から
`App::reanalyze_semantics` を呼び、その描画自体が
`HISTORY_RENDER_DEBOUNCE`（300ms、`src/main.rs`）で debounce されていること。

### 非同期で解析させるなら、答えには世代番号を載せる

`--semantic-cmd` は別スレッドで走ります。解析中にファイルが変わって再解析が
始まると、**古い方の答えは「もう画面に無いテキスト」の byte 位置を指します。**
当てても range は文字境界に載るので panic せず、見た目は「ただの誤判定」です。

`App::semantic_generation` を再解析のたびに +1 し、**世代はチャネルではなく
メッセージに載せて**、受け取り時に現在の世代と違えば捨てます（複数の解析が
同時に飛びうるし、先に聞いた方が先に答えるとは限りません）。
供給源をもう 1 つ足すときも同じ作法にしてください。

**確認したこと**: `src/app.rs::reanalyze_semantics` が
`semantic_generation += 1` してから `AnalysisMessage { generation, .. }` を
送っていること。テストは `src/state_tests.rs` の
`an_answer_from_an_older_generation_is_thrown_away`（決定論的）と
`a_slow_analysis_started_first_never_overwrites_a_newer_one`（実スレッド）。

---

## 外部プロセス

### TUI が生きているあいだの子プロセスは `export::run_child` を通す

`Command::spawn` + `wait` を素朴に書くと、子プロセスを噛ませるスクリプトなら
何であれ踏む 4 つの事故が返ってきます:

- stdin を読まない子（パイプバッファが埋まって書き込みが止まる）
- 子の exit 後もパイプを掴んでいる孫プロセス
- パイプバッファを超える出力の洪水（相互にブロックする）
- 返ってこない子（デッドラインで kill する必要がある）

4 つとも `src/export.rs` の `run_child` で解決済みです。イベントループは
単一スレッドなので、ここで詰まると**キーが全部死にます。**
出力を**解析する**呼び出し側は `run_capturing` を使い、`truncated` を
エラーにしてください（切れた JSON は「壊れた応答」に見えて原因を隠します）。

例外は TUI を畳んだ後の起動だけです（`src/main.rs` の `--callback` は
`TerminalGuard` を drop した後に fire-and-forget で spawn しています）。

**確認したこと**: `src/export.rs` の `run_child`（非ブロッキング stdin、
stdout/stderr の逐次 drain、`try_wait` + デッドラインでの `child.kill()`）と
`run_capturing`（`out.truncated` で `bail!`）。テストは同ファイルの
`pipe_and_wait_succeeds_when_the_child_reads_stdin` /
`pipe_and_wait_succeeds_when_a_grandchild_keeps_stdin_open` /
`pipe_and_wait_does_not_deadlock_on_a_flood_of_output` /
`pipe_and_wait_times_out_and_kills_the_child`。

---

## ベンダリングと CI

### 上流のバージョンとフォーク自身のバージョンは別物

`third_party/tui-markdown` は tui-markdown のフォークです。Cargo.toml に
2 つのバージョンがあります:

| フィールド | 意味 | 使われ方 |
|---|---|---|
| `[package.metadata] vendored-from` | **ベンダリング元の上流バージョン**（いま `0.3.9`） | 上流ソースの解決（ダウンロード URL / registry キャッシュ）とマニフェストヘッダの照合 |
| `[package] version` | **フォーク自身のバージョン**（いま `0.4.0`） | 表示のみ |

`scripts/check-vendor-diff.sh` は前者を `VER`、後者を `FORK_VER` として
**別々に**読みます。ここを混同すると CI が 2 通りに壊れます — 存在しない
上流 crate を取りに行って 403、あるいはマニフェストヘッダと不一致で契約1 が
落ちる。**フォークの API を変えて `[package] version` を上げるだけなら、
CI はグリーンのままが正しい挙動です。**

上流を本当に上げ直したときは `vendored-from` を書き換えて
`./scripts/check-vendor-diff.sh --update` でマニフェストを再生成してください。

**確認したこと**: `third_party/tui-markdown/Cargo.toml` に
`version = "0.4.0"` と `[package.metadata] vendored-from = "0.3.9"` が
両方あること。`scripts/check-vendor-diff.sh` の `VER` / `FORK_VER` が
別の sed で読まれていること。
`TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh` が
「上流 0.3.9 / フォーク 0.4.0、28 ファイル、1880 変更行」でグリーンに
なることを実行して確認。

### シェルスクリプトの日本語メッセージ内の変数展開は `${VAR}` で括る

`"… $FORK_VER）です"` と書くと、**直後の全角括弧まで変数名として読まれて**
`set -u` の下で `unbound variable` になります。日本語メッセージの中で
変数を展開するときは必ず `${FORK_VER}` と括ってください。

**確認したこと**: `bash -c 'set -u; FORK_VER=0.4.0; echo "フォーク $FORK_VER）です"'`
が `FORK_VER<全角括弧の先頭バイト>: unbound variable` で exit 127 になる
ことを、macOS の bash 3.2 と bash 5.2.37 の両方で実測。`scripts/check-vendor-diff.sh` の
該当メッセージは `${FORK_VER}` になっています。

---

## 未解決（地雷ではなく、設計判断が要るもの）

刺しにくるものではありませんが、**設計書と実装の差**として残っています。
どれも「誤記」ではないので、直すには判断が要ります。

### 1. `cache` と `incremental reanalysis` は、実測して作らないと決めた

設計書 [`design/semantic-reading-layer.md`](design/semantic-reading-layer.md)
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
[`design/jev.md`](design/jev.md) の「実測」節と
`examples/semantic/README.md`（`wc -m examples/semantic/demo.md` が 624）。
大きな文書の数字は同じ 2 つの「大きな文書での実測」節（5 文書 × 2 回、
45,650 バイトのものは 7 回）。

### 2. Phase 番号が 2 つの意味で使われている（アーカイブ側）

[`design/range-attribution-plan.md`](design/range-attribution-plan.md) の
Phase 3 は **Render Mapping 強化**です。一方
[`handoff-archive.md`](handoff-archive.md) には
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
返しますが（[`design/jev.md`](design/jev.md)）、現行プロトコルは `units`
しか受け取りません。アダプタ（`examples/semantic/jev-annotate.py`）は値を
捨てておらず `jev` という追加フィールドに載せていますが、プロトコルが
知らないフィールドなので**読み飛ばされます**。プロトコルは未知のフィールドを
拒否していないので**拡張は可能**で、「出力を全部使う」と書いた箇所も無いので
**矛盾してはいません**。

使い道として挙がっていた「境界判定の confidence が低いときは `NEW_UNIT` に
倒す」は、**実測で不採用になりました**（閾値が値の真上に乗り、実行ごとに答えが
裏返る。[`design/jev.md`](design/jev.md) の「実測」）。残っているのは閾値を
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

### 4. 同一 Tier 内の rule に逐次性が無い（`context preservation`）

設計書は同一 Tier 内の rule として
`redundancy / length / document position / context preservation` の 4 つを
挙げ、`policy::keep_order` は最初の 3 つしか使っていません。これは
**「4 つのうち 1 つを落とした」ではありません。**

最初の 3 つは Unit 単体の属性から決まる静的な値です（重複しているか・
何バイトか・文書のどこにあるか）。4 つ目だけが
**「残った Unit を順に読んだとき文脈が繋がるか」**という、選択の結果に依存する
性質を指しています。そして satisficing は「読み進めて information gain が
落ちたら次へ移る」という**逐次的**なモデルなので、4 つ目はこの層で
**逐次性を担う唯一の項目**でした。`decorate` がしているのは「集合を選ぶ」
ことまでで、**選んだ集合が読む経路として成立しているかは誰も見ていません。**

**設計書は `context preservation` の中身を定義していません。** 語が出てくるのは
rule の列挙 1 箇所だけです。直すには設計判断が要り、ここで定義を足すのは
「設計書に無い設計」を足すことになります。

**確認したこと**: `policy::keep_order` の並び替え鍵
`(実効 Tier, redundant か, バイト長, 先頭バイト位置, 添字)` が、すべて
`doc.units[index]` 1 つから計算されていること。とくに `redundant` は
`SemanticUnit::is_redundant()` であって、`REDUNDANT_WITH` の参照先がその
Budget で残っているかを見ていないこと（`policy::decorate` の `kept` は
`keep_order` を呼んだ**後**に作られ、順序の計算には戻りません）。
`grep -n "context preservation" docs/design/semantic-reading-layer.md` の
ヒットが rule 列挙の 1 行だけであること。同じ話は
`crates/semantic-reading/src/policy.rs` のモジュールドキュメントにあります。

### 5. 大きな文書の上限はタイムアウトではなく Jev の context window

**`--semantic-cmd` で Jev を繋いだとき、大きな文書の制約はタイムアウトでは
ありません。** `COMMAND_TIMEOUT`（60 秒）には 18 倍の余裕があり、先に当たるのは
Jev の **context window** です。超えると**約 2 秒で** HTTP 400
`max_tokens_exceeded` が返ります — 待っても変わりません。

実測（2026-09-21）では入力の上限は **65,536 tokens（2^16）**と読めますが、
**TypeSafe の公式値は未確認**です。原典も「context window は需要に応じて
変わりうる」と書いています。

アダプタのラウンド 2 は、`state`（文書全文）に加えて**各 Unit の本文を Tier と
redundancy で 2 回引用する**ので、入力はおよそ `3.6 × 文書のトークン数`に
なります。だから文書そのものが 18,000 tokens ほどで天井に当たります。

**実業務の 45,650 バイトの `CLAUDE.md` は、その 92〜96 % を使っています。**
同じ文書を 1.04 倍にしても通りましたが、**1.06 倍で失敗しました。**
この文書に 2 KB 足すと解析できなくなる、ということです。

バイト数での上限は言語に依存します（この日本語文書で約 2.6 バイト/token）。
**上限は tokens で言ってください。**

**しかも文書サイズだけでは決まりません。** ラウンド 2 の重さは Unit 数に
比例し、Unit 数はラウンド 1 で Jev が `SAME_UNIT` を何回返すかで決まります。
この `gotchas.md`（24,280 バイト）は実走では通りました（Unit 77〜80、
ラウンド 2 は 153〜159 question / 46,107 tokens）が、**境界がすべて
`NEW_UNIT` だった場合**（Unit 154、307 question）は天井を超えます。
**同じ文書が、境界判定の出方によって成功したり 400 になったりしうる**
ということです。

直すには設計判断が要るので、ここでは**分かるエラー文を出すところまで**に
してあります（`examples/semantic/jev-annotate.py` の `http_error_message`）。
選択肢と、それぞれが何を犠牲にするか:

| 案 | 効果 | 代償 |
| --- | --- | --- |
| ラウンド 2 を K 分割する | 天井が K 倍に伸びる | `state` が K 回課金される（この文書で 1 分割あたり +17,500 tokens ≒ +$0.0007）。時間は 1 分割あたり +0.55 秒の床 |
| question で Unit 本文を引用せず参照で聞く | 入力が `state` + 定型文だけになり、150 KB 級まで入る | **判定品質が未測定**。Jev が参照から対象を特定できるかは測っていない |
| Atom を粗くする | question 数が減り定型文が減る | 引用本文は減らないので効きは限定的。Unit が粗くなると表示の粒度が落ちる |
| 大きな文書では解析しない | 明示的に諦める | 45 KB は日常的な大きさなので、機能が使えない文書が増える |

**確認したこと**: `examples/semantic/jev-annotate.py` を akapen 抜きで単体実行し、
5 文書（1,664 / 9,857 / 15,656 / 24,280 / 45,650 バイト）を 2 回ずつ、45,650
バイトのものは 7 回走らせたこと。同じ文書を 1.02 / 1.04 / 1.06 / 1.10 / 1.25 /
1.50 倍に伸ばし、1.06 倍から `max_tokens_exceeded` になること。`state` を固定して
question 数を二分探索し、input 65,033 tokens が成功・約 65.5k が失敗すること。
要求 JSON は `cargo run -p semantic-reading --example dump-request` で akapen 本体と
同じ `atomize` 経路から作っていること。akapen 側の子プロセス経路
（`src/export.rs` の `run_child`）は 512 KiB の stdin を 37 ミリ秒で流すので、
所要には効いていないこと。

**再現しなかったこと**: 「45.6 KB の文書の解析に 30〜60 秒かかる」という報告は
**再現していません**。同じ文書・同じアダプタで 7 回測って 2.42〜2.63 秒でした。
`App::accept_analysis` は失敗時にも `semantic_inflight` を落とすので、「解析中」
表示が残り続けることもありません。**原因は特定できていません。**
