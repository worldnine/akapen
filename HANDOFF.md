<<<<<<< HEAD
# HANDOFF: 外部コマンド委譲の Provider（`--semantic-cmd`）

## 何を作ったか

```sh
akapen doc.md --semantic-cmd 'python3 examples/semantic/annotate-doc.py'
```

意味判断だけを外部プロセスへ委譲する経路。**将来 Jev（LLM）を繋ぐ口**で、
このタスクでは LLM は呼んでいない。

akapen に HTTP クライアントも async ランタイムも**入れていない**。
reqwest / tokio を足すと、この機能を使わない全ユーザーにコンパイル時間・
バイナリサイズ・依存監査のコストが乗る。代わりに `--send-cmd` と同じ作法で
外部コマンドへ渡す — API キー管理が akapen の責務から外れ、ユーザーが
既に持っている CLI（`claude -p`、`llm`、自作スクリプト、ローカル LLM）が
そのまま使える。

## 1. プロトコル — Atom を渡して Unit を受け取る

`crates/semantic-reading/src/protocol.rs`（新規）。

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

**Atom 生成は akapen 側でやる**（設計書「Jev に判断させないもの: Atom生成 /
source position管理」）。コマンドが返すのは Atom の **index だけ**で、
`SemanticDocument` は akapen 自身の `atomize()` の出力と、返ってきた Unit の
構造から組み立てられる。

### これが効いている理由: 不正な range が原理的に生まれない

位置を一度も外へ渡して受け取り直していないので、外部コマンドが壊れた位置を
返して文書の違う場所を装飾する事故が**起きえない**。`--semantic`（fixture）
経路で必要だった `source_sha256` の照合も、この経路では**不要**である。

`a_command_cannot_move_a_range_even_if_it_tries` で固定した — コマンドが
`"range": {"start": 9999, ...}` を返しても、文書の位置は atomize の出力の
ままになる（プロトコルに無いフィールドとして読まれない）。

### ワイヤは 1 往復にした

設計書は Jev への問いを 2 段階（境界判定 → Tier 付け）に分けているが、
**ワイヤプロトコルは一括（atoms in, units out）**にした。外部コマンドが
内部で LLM を 2 回呼ぶのは自由で、akapen のプロトコルが LLM 側の段取りを
規定すべきではない。将来キャッシュのために段階を分ける必要が出たら
`stage` を足せる（未知のフィールドは拒否していない）。

### 検証 — 全項目、1 つでも駄目ならレスポンス全体を捨てる

部分適用は何もしないより悪い（「半分だけ意味が付いた文書」は誤読を誘う）。

| 弾く条件 | どこで |
| -------- | ------ |
| `version` 不一致 | `AnalyzeResponse::into_document` |
| 未知の `reading_tier` | serde |
| 未知の `relation` | serde |
| atom index が範囲外 | `SemanticDocument::validate` |
| unit id の重複 | 同上 |
| relation の参照先が存在しない / 自分自身 | 同上 |
| stdout が 16 MiB 超 | `export::run_capturing` |

**同じ Atom を複数の Unit が主張したら先勝ち。** 後の Unit からその index を
落とす（`policy` の attention コストが二重計上にならない／文書順の最初の判断が
残る）。全部落とされて空になった Unit は**消さない** — 他の Unit の
`redundant_with` の参照先として生きている可能性があり、消すと参照が宙に浮いて
レスポンス全体が捨てられることになる。

## 2. 非同期実行 — 世代カウンタ

イベントループは `event::poll(Duration::from_millis(tick))` のポーリングなので、
10〜30 秒かかる呼び出しを同期実行すると UI が固まる。

```text
reanalyze_semantics
   ├ Inline(fixture)   -> その場で analyze（従来どおり）
   └ Command(cmd)      -> thread::spawn + mpsc::Sender
                          世代 N を付けて送る
イベントループ毎 tick -> App::poll_semantic_analysis（try_recv）
                          世代 N == 現在の世代 なら適用、違えば捨てる
```

- `App::semantic_generation` — `reanalyze_semantics` のたびに +1
- `App::semantic_inflight: Option<u64>` — 走っている解析の世代（`解析中…` 表示）
- `App::semantic_results: Option<AnalysisChannel>` — 送受信端をセッション中
  持ち続ける。世代は**チャネルではなくメッセージに載せている**: 複数の解析が
  同時に飛びうるし、先に聞いた方が先に答えるとは限らない

**世代を入れていないと静かに壊れる。** 解析中に `reload.rs` がファイル変更を
検知して再解析を始めたとき、古い方の答えは「もう画面に無いテキスト」の
バイト位置を指している。当てても range は文字境界に載るので panic もせず、
見た目も「ただの誤判定」に見える。だから番号で落とす。テストは
`an_answer_from_an_older_generation_is_thrown_away`（決定論的・スレッド無し）と
`a_slow_analysis_started_first_never_overwrites_a_newer_one`（実スレッド）の 2 本。

### 古い注釈をいつ捨てるか

タスクの要求は「失敗時は直前の注釈を保持」だが、**文書が変わっていたら保持は
誤り**である（変わった後の文書に変わる前の判定を当てることになる）。

そこで `App::drop_stale_annotation` を再解析の入口に置いた。注釈が名乗っている
`source_sha256` が画面の文書と違えば、答えを待つ前に落とす。同じ文書なら残す。
これで両方が成り立つ:

- 異常終了 / タイムアウト / JSON 不正 → 理由を `flash_err`、**同じ文書の注釈は保持**
- 文書が入れ替わった → 古い注釈は**即座に落とす**（解析中も当てない）

`CommandProvider::analyze` が結果に `source_sha256` を**書き込んで**いるのは
この判断のためで、照合（別文書の拒否）のためではない。この経路では range が
akapen 自身のものなので「別の文書のもの」がありえない。

### タイムアウト

`semantic::COMMAND_TIMEOUT = 60s`。**設定値はここ 1 箇所**。寛容にしたのは、
この先に繋がるのが LLM を呼ぶスクリプトだから — 短く切ると「動いているのに
切られる」になり、ユーザーには「壊れている」と区別がつかない。UI が固まらない
ことはタイムアウトではなく別スレッドが担保している。

## 3. 子プロセスの実行は `export.rs` を再利用した

`pipe_and_wait` を `run_child`（stdout を返す）へ一般化し、`run_capturing` を
足した。2 つ目の実装を書かなかったのは、LLM ラッパースクリプトが普通にやる
4 つの事故が**すでにそこで解決済み**だから:

- stdin を読まない子（非ブロッキング書き込み）
- 子の exit 後もパイプを掴んでいる孫プロセス
- パイプバッファを超える出力の洪水
- 返ってこない子（デッドライン + kill）

`Capture` に上限と `truncated` を足し、tail だけ要る既存経路は 64 KiB、
応答を**解析する** `--semantic-cmd` は 16 MiB を渡して、切り詰めが起きたら
エラーにする（切れた JSON を「壊れた応答」と報告するより直せる）。

## 4. 参照実装（LLM 不使用）

`examples/semantic/annotate-doc.py`。

```text
見出し               -> ESSENTIAL
見出し直後の 1 Atom  -> SUPPORTING
それ以外             -> DETAIL
直前の文と語が重なる -> REDUNDANT_WITH
```

目的は**パイプライン全体を API キー無しで端から端まで動かせること**で、
判断の質ではない。実際の Jev のプロンプト設計は別タスクなのでここには無い。
統合テストはこのスクリプトを**実際に起動**している。

## 型の形について: なぜ `SemanticSource` という enum か

「遅いかもしれない」を `Provider` の中に隠すことはできない — `analyze` の
戻り値が `Result<SemanticDocument>` であって「あとで」を表現できないため。
App は呼び方（同期 / スレッド）を変えねばならず、その分岐は型に出るのが正しい。

```rust
pub(crate) enum SemanticSource {
    Inline(Box<dyn Provider>),   // --semantic: その場で答える
    Command(CommandProvider),    // --semantic-cmd: 別スレッド
}
```

`App::semantic_provider` は `App::semantic_source` になった。供給源を増やすのは
`semantic::source_from_config` の `match` に腕を 1 本足す作業のまま。

## 既知の制限

- **終了時に走っている子プロセスは kill されない。** App が `Child` を持たない
  設計（`export::run_child` がスレッド内で完結する）なので、`q` 直後に
  `claude -p` が最大 60 秒だけ孤児として残りうる。実害は小さいが、気になるなら
  `Child` をチャネル越しに App へ預けて終了時に殺す形にする
- **失敗したときの理由は stderr の「最後の」非空行**。タスクは「1 行目」と
  書いていたが、`export.rs` の既存の作法（`Capture::tail`）に合わせた。
  python の traceback は末尾が理由で、先頭は `Traceback (most recent call last):`
  でしかないため
- **UI 文字列 `解析中…` だけが日本語**（他は英語）。タスク本文の字面に合わせた。
  英語に寄せるなら `chrome.rs` の 1 行

## テスト

全緑。akapen 532（+18） / tui-markdown 173 / semantic-reading 71+4+3+10（+16） /
doc 7+3（+1）。

| 何を | どこに |
|------|--------|
| プロトコルの検証 15 本（先勝ち・空 unit・未知フィールド含む） | `protocol.rs` |
| `Error::Provider` が「文書が不正」と言わない | `error.rs` |
| 参照実装を起動するラウンドトリップ | `semantic.rs` |
| range を動かせないこと / 素性の stamp | 同上 |
| 異常終了・起動不能・タイムアウト・不正 JSON・version 不一致 | 同上 |
| `--semantic` / `--semantic-cmd` の排他とパース | `config.rs` |
| 非同期一周（`解析中…` 表示込み） | `state_tests.rs` |
| 世代カウンタ（決定論 + 実スレッド） | 同上 |
| 失敗で注釈を保持 / 文書が変われば落とす | 同上 |
| 解析中の Budget キーは「解析中」と言う | 同上 |
=======
# HANDOFF: source view に range decoration を通す（Phase 1+2 の source 側）

## 何を作ったか

rendered view だけが持っていた range decoration を、**source view（生の
Markdown を見るモード）にも通した**。同一行の途中で MARKED / NORMAL / DIM が
切り替わり、rendered view と**同じ source byte range**が装飾される。

前任者の「source view には手を付けていない」節（下）が挙げた 3 段を、
その順にやった。

設計書の Phase 番号でいうと、これは **Phase 1（Range Attribution）と
Phase 2（Range Decoration）の source view 側**。設計書の Phase 3 は
「Render Mapping 強化」で、別の話（rendered 側の attribution を
table / code まで精密にする）。

## 1. range を作る — syntect region → 文書の byte range

`Highlighter::highlight_with` が返す型を `Vec<Vec<Span>>` から
`Vec<TaggedLine>`（`spans` と `attrs` の並行 vec）に変えた。

syntect の `highlight_line` は、渡した行の**部分スライスを順番に隙間なく**
返す。だから走る offset 1 本で足りる:

```text
行の文書内オフセット（line_start）
  + その行で既に span にしたバイト数（consumed）
```

`line_start` は `LinesWithEndings` を回しながら `line.len()` を足すだけ。
span のテキストは末尾の `\n` を落としているので、**range の方も落とす** —
span の range は自分のテキストであって、その後ろの改行ではない。

**source view は原理的に全部 exact。** 表示しているのが source そのもので、
syntect はテキストを書き換えないので、`content[range]` は必ず span の
テキストになる。`Attr::exact` だけを作る。rendered 側のように強調記号が
消えたり `&amp;` が `&` になったりする段差が無い。

呼び出し側（`reload.rs` / `main.rs` / 各テストの 11 箇所）は
`app.spans = …highlight_with(…)` という代入だけなので、**1 行も触っていない**。
`app.spans` を読むのは `App::rebuild_base_rows` と `build_rows` の 2 箇所だけ
だった。

## 2. `wrap_spans` の tagged 化 — 二重実装を作らずに

`wrap_spans` と `wrap_spans_tagged` は**元から同じ折返しアルゴリズムの
2 つの写し**だった。新しく書き足すのではなく、**片方を消した**:

```rust
pub fn wrap_spans(spans: &[Span], width: usize) -> Vec<Vec<Span>> {
    let attrs = vec![None; spans.len()];
    wrap_spans_tagged(spans, &attrs, width, 0).into_iter().map(|(row, _)| row).collect()
}
```

`hang = 0` のとき `pad_due` が立たないので、tagged 側の全分岐が旧
`wrap_spans` と 1 行ずつ一致する（narrow-pane の retry も、行が埋まった
ときの flush も）。**折返しのロジックは今 1 つしか無い。**
`plain_wrap_equals_the_tagged_wrap_it_delegates_to` が、幅 1〜80 ×
（ASCII / 日本語 / 絵文字 / 全角 / タブ / 空入力）でこの同一性を固定している。

折返し挙動は壊していない。既存の折返しテスト 14 本（`wrap_*` 9 本 /
`hanging_wrap_*` 5 本）が無改造で緑のまま。

## 3. source 側の描画経路に attribution を通す

`build_rows`（source モードの実描画）で:

- `wrap_spans(&app.spans[idx], width)` → `wrap_spans_tagged(&line.spans,
  &line.attrs, width, 0)`
- 各表示行に `decoration::decorate_row` を掛けてから、既存の
  cursor / selection / changed 帯を乗せる

**装飾ロジックは 1 行も書いていない。** `decorate_row` と交差ルールを
そのまま呼ぶだけ。decoration は span を切るがテキストは変えないので、
行数も帯の埋め幅も変わらない（`base_rows` は `wrap_spans` のままでよい）。

decoration リストは `App::active_decorations` に括り出し、**draw_view と
build_rows の両方が同じものを呼ぶ**。`--decorations` と semantic の連結も
`sanitize` も 1 箇所になったので、2 つのモードが「どのバイトが MARKED か」で
食い違えない。

`DecorationStyles` は `App` に持たせた（`App::new` で 1 回解決）。
`app.view.decoration_styles` は使えない — **view モードで一度も表示して
いないファイルは `ViewState::default()` を持っており**、その styles は
ダークテーマの決め打ちだから、light テーマの source モードで色が狂う。

## タブ展開の扱い — ここだけ exact ではなくなる

`wrap_spans` はタブを空白に展開する。**展開した span はもう source の
verbatim ではない。** rendered 側の作法（`Attr::slice(.., still_verbatim)`）
にそのまま乗った: **range は保ったまま `exact: false` に落ちる。**

結果、その fragment は `decorate_row` の**上位集合の腕**へ回る:

- fragment を丸ごと覆う decoration → 効く
- fragment の**途中で終わる** decoration → その fragment には何も乗らない

タブ入りの行だけの割り切りで、代わりに得られるのは「書き換えたテキストを
source だと偽らない」こと。偽ると隣のテキストへ装飾がにじむ。
`a_tab_expanded_fragment_keeps_its_range_and_loses_exactness` が
**「fragment が exact ⟺ そのテキストが今も `source[range]`」**という一般形で
固定してある。

### 「上位集合の腕は実質使われない」は、タブを除けば本当だった

タスクの想定どおりで、確認して**テストに書いた**
（`source_wrapping_reaches_the_superset_branch_only_through_tabs`）。
testdata 4 ファイル × 幅 8/17/40/80 を掃いて、**タブを含む行以外は全 fragment が
exact**。タブ行だけを例外として明示的に除外している（`full.md` に実際に
タブ行が 7 本あるので、この例外は空回りしていない）。rendered 側は強調・entity・softbreak・
table cell が全部 superset になるので、ここが両モードの attribution の
実質的な違い。

## byte offset と terminal column を混同しない

設計書が名指しで警告している点。decoration は**バイト**で切り、折返しは
**カラム**で測る。`a_decoration_lands_on_multibyte_characters` が
「日本語と🎉とＡＢＣ」に対し幅 4 / 7 / 40 で、装飾されたセルを連結したものが
`source[start..end]` と一致することを固定している。どちらの空間で 1 ずれても
落ちる。

## budget キーを source モードにも束ねた

前任者が view のみにした理由（「source は装飾が出ないので、押すたびに
READ % だけ動くのは嘘になる」）は**この改修で消えた**ので、`-` `+` `=`
`<` `>` を `on_source_key` にも足した。ガードも `modifiers.is_empty()` を
付けない方針も view 側と同一。`--semantic` の無いセッションで何も起きない
（toast すら出さない）ことも、source 側で改めて固定した。

`--help` の「in the view」→「in both views」。`?` ヘルプの `read` 行は
元からモード非依存なので変更なし。

## DIM と帯 — source 側だけ `changed_bg` も「帯」に数える

`Dim` は前景を書くので背景の帯では打ち消せない（`decoration.rs` の
モジュールドキュメント）。view.rs と同じく、**帯の乗る行では `Dim` を
decorate する前に落とす**。

ただし **source モードは changed（緑）帯も行全体の背景を塗る**（view は
gutter のマーカーだけ）。なので source では `cursor_bg || changed_bg` を
「帯」とした。view との意図的な差で、理由は上と同じ — 背景は前景を戻せない。

## 恒等性の機械確認（前任者と同じ手法）

改修前 HEAD（`a0bcdec`）を別 worktree に展開し、両方に同じダンプテストを
差し込んで

- testdata 4 ファイル × 幅 20 / 40 / 80
- × カーソル 4 位置 × 選択あり/なし × スクロール offset 3 種

の `build_rows` 出力（**行ごと・span ごとの text と Style**、composer cursor
込み）をダンプして比較 → **34127 行、バイト単位で完全一致**（`cmp`）。

decoration が空なら source view の出力は 1 バイトも変わっていない。
リポジトリ側にも `no_decorations_leaves_the_source_rows_alone` として、
「空の decoration」と「sanitize が丸ごと捨てる decoration」の両方で
行が一致することを残してある。

## 実機 pty での確認

`examples/semantic/demo.md` を 80 桁で開き、Tab で source モードへ。
`demo.md` 7 行目「採用する方式は差分配信である。詳細は付録にまとめた。」の
SGR を、ANSI を画面に再生して読んだ。

| | source view | rendered view |
|---|---|---|
| READ 100 % 前半 | bg `rgb(68,70,89)` = MARKED | bg `rgb(68,70,89)` |
| READ 100 % 後半 | 装飾なし = NORMAL | 装飾なし |
| READ 30 % 前半 | bg `rgb(68,70,89)` のまま | 同じ |
| READ 30 % 後半 | fg `rgb(99,103,125)` = DIM | fg `rgb(99,103,125)` |

**両モードで同じ範囲が同じ色になる。** 30 % へは source モードのまま `<` を
7 回押して到達している（キーが効いていることの実機確認でもある）。

DIM 行にカーソル帯を乗せる（`j` で 7 行目へ）と、行全体が帯の背景
`rgb(88,91,112)` になり、**前景は明るい `rgb(205,214,244)` に戻る** —
霞まない。rendered 側で塞いだ穴が source 側でも塞がっている。

## 既知の割り切り

- **タブ入りの行は、fragment の途中で終わる decoration が効かない**（上記）。
  直すなら `highlight_with` の段階で `\t` の連なりで region を割り、タブの
  断片だけを demote すればよいが、タブ行の span 数が変わる。今回はやって
  いない
- `decorate_row` のコストは rendered 側と同じく O(可視 span 数 × decoration 数)
  / フレーム。source モードは rendered より span が細かいので係数は少し重い
  が、可視行ぶんだけ。**二分探索へ行くのはまだ早い**（Phase 2 の判断のまま）
- source モードの `changed`（緑）帯も `Dim` を落とす（上記）。view には
  対応する挙動が無い

## 検証

- `cargo test --locked --workspace`: **akapen 531 / tui-markdown 173（+1 ignored）/
  semantic-reading 55+4+3+10 / doc 7+2**、全緑
  （ベースライン 514 / 173 / 55+4+3+10 / 7+2 — **+17**）
  - 内訳: `highlight.rs` +7（attribution の exact / verbatim、範囲の前進、
    空行、折返し後の verbatim、タブの demote、上位集合の腕の掃き出し、
    `wrap_spans` 委譲の同一性）、`main.rs` の
    `source_decoration_tests` +8（3 style の同一行、syntax highlight の生存、
    折返し跨ぎ、日本語/絵文字/全角、タブの all-or-nothing、恒等性、
    カーソル帯の打ち消し、選択帯の打ち消し）、`state_tests.rs` +2
    （実キーハンドラ経由の source モード end-to-end、`--semantic` 無しで
    キーが死んでいること）
- `cargo test --locked --release`: 532 passed（`debug_assert` 無効でも緑）
- `cargo clippy --locked --all-targets`: **警告 0**
- `TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh`: **OK**
  （上流 0.3.9 / フォーク 0.4.0、28 ファイル、1880 変更行 — `third_party/` は未変更）
- 恒等性ダンプ: 34127 行完全一致（上記）
- 実機 pty: 上記の表
- 変更ファイル: `src/highlight.rs` / `src/app.rs` / `src/main.rs` /
  `src/state_tests.rs` / `HANDOFF.md`。**`reload.rs` は未変更**（`highlight_with`
  の呼び出しが代入だけだったため）

補足: このリポジトリは rustfmt を掛けていない。既存ファイルと同程度に
揃えてあるだけで、自分の触ったファイルだけを整形するようなことはしていない。
>>>>>>> feat/source-view

---

# HANDOFF: MARKED と DIM を実機で見分けられるようにする（見た目の調整）

## 何が起きていたか

実機でユーザーに見てもらったら、**MARKED と DIM の区別がつかなかった**。

原因は 2 つ。

1. **`Modifier::DIM`（SGR `2`）を無視する端末がある。** 実測でほぼ効いて
   いなかった。これが主因
2. MARK の背景持ち上げ 14 % が控えめすぎた

5 案を実機で見比べた結果、**MARK 22 % / DIM は前景を背景方向へ 60 %
ブレンド**が採用になった。

## 1. DIM は modifier ではなく実色になった

Phase 2 は `Dim` を `Modifier::DIM` だけにして、「DIM は前景不可侵」を
不変条件として固定していた（理由: テーマ由来のグレーにすると dim 範囲の色が
全部同じに潰れる）。**この不変条件を意図的に破った。**

グレーの話は正しかったが、modifier の話が間違っていた。SGR `2` は端末依存が
激しく、効かない環境では DIM が「何も起きていない」になる。これは装飾の
目的そのものを失う。

新しい `Dim` は **span の実効前景色をテーマ背景の方向へ `DIM_BLEND` だけ
ブレンドした色**を前景に置く。

- 実効前景色 = span 自身の `fg`、無ければテーマの既定前景
  （`theme.settings.foreground`）。`None` のまま放置すると**その範囲だけ
  明るいまま残る** — 元のバグと同じ形なので、テストで固定してある
  （`a_span_without_a_foreground_still_dims`）
- **`Modifier::DIM` は付けない。** 解釈する端末で二重に暗くなる
- 潰れの心配は当たらない: 各 span は自分の色をページ方向へ動かすだけなので
  色相が残る。dim した見出しは「dim した見出し」のまま見える。テストで
  「2 色が 1 色に潰れないこと」と「チャネルの大小関係が保たれること」を
  固定した
- RGB でない前景（端末パレットの名前付き色）は補間できないので、テーマ既定
  前景にフォールバックして dim する。色相は失うが「退く」は果たす。描画本文の
  span はテーマ由来の RGB なので実際には通らない経路

`decoration.rs` のモジュールドキュメントに、破った理由込みで書いてある。

## 2. MARK は 22 %、ただし天井つき

`MARK_BG_BLEND` を 0.14 → **0.22**。テーマが `MARK_SCOPES` のいずれかに
背景を持つ場合はそちらが勝つ経路はそのまま。

**制約: selection 帯 `rgb(88,91,112)` と明確に区別できること。**

| | dark (Catppuccin Mocha) | light (Solarized) |
|---|---|---|
| MARK 背景 | `rgb(68,70,89)` | `rgb(219,218,205)` |
| DIM 前景（本文） | `rgb(99,103,125)` | `rgb(192,196,188)` |
| selection 帯 | `rgb(88,91,112)` | — |

MARK と帯のマンハッタン距離は **64**（20+21+23）。0.30 だと
`rgb(82,85,105)` で距離 16 まで縮んで危険域に入る。

そこで `MARK_BG_BLEND_CEILING = 0.25` を置き、

```rust
const _: () = assert!(MARK_BG_BLEND <= MARK_BG_BLEND_CEILING);
```

で**コンパイル時に**押さえた（テストではなくビルドが止まる）。天井は
**出荷する既定値**に対するもので、`--mark-blend` は縛っていない — 見比べる
ための knob を黙って clamp したら knob ではなくなる。

## 3. selection との相互作用（ここが地雷だった）

Phase 2 は「選択は DIM を打ち消す」を `remove_modifier(Modifier::DIM)` で
表現していた。**DIM が前景色になった時点でこれは意味を失う** — 背景の帯は
前景を元に戻せないので、選択しても文字が沈んだままになる（「選択したのに
文字が霞んでいる」）。

対処: **帯が乗る行では、装飾する前に `Dim` を落とす。**

`view.rs` の `visible_text_with_glow`、`decorate_row` を呼ぶ直前:

```rust
let banded = glowing_row || gutter_hl;   // gutter_hl = cursor_row || in_sel_row
let effective = if banded { Dim を除いたもの } else { decorations };
```

- 帯は **selection / カーソル帯 / history glow** の 3 つ。rendered view に
  コメントフォーカスの帯は無い（`focused_deletion` は source モード専用、
  コメントカバー行は marker 列の `▌` だけで背景を持たない）
- `remove_modifier(Modifier::DIM)` は 2 箇所とも**削除した**。もう誰も
  その modifier を立てないので、残しても Phase 2 が書いていた
  「選択帯の span に `sub_modifier: DIM` が載る」という副作用が残るだけ
- **粒度が行単位になった。** 旧方式は span 単位だったので、soft line break で
  複数 source 行が 1 行に繋がっているとき、片方だけを選択すると選択部分だけが
  un-dim されていた。いまは行ごと un-dim される。帯は一時的な状態でユーザーが
  いま見ている場所なので、丸ごと勝たせた
- `Mark` は帯の下でも当たる（span も割れる）。帯と同じチャネル（背景）を
  争うが、帯が後から上書きするので優先順位どおりになる。テストで
  `the_band_suppresses_only_dim_not_the_mark` として固定

### 実機 pty で確認済み

READ 30 %、`## 結論` の行（`examples/semantic/demo.md` 7 行目）:

```
帯なし:
\x1b[10;4H\x1b[39;48;2;68;70;89m採用する方式は差分配信である。   ← MARKED
\x1b[10;34H\x1b[38;2;99;103;125;49m詳細は付録にまとめた。         ← DIM（実色。SGR 2 は出ない）

カーソル帯を乗せる（j×3）/ さらに v で選択:
\x1b[39;48;2;88;91;112m 採用する方式は差分配信である。詳細は付録にまとめた。
                        ^ 行が分割すらされず、前景は既定色（39）のまま
```

**帯の下で文字が霞まないことを実機で確認した。**

## 4. CLI フラグ

```
--mark-blend <0.0..1.0>   既定 0.22
--dim-blend  <0.0..1.0>   既定 0.60
```

範囲外・数値でない値は**明示的なエラー**（`--decorations` と同じ扱い）。
黙って clamp した 1.5 は動いている 1.0 と見分けがつかず、次にユーザーが
出す結論は「このフラグは効かない」になる。`NaN` も比較が全部 false に
なるので同じ経路で弾かれる。

### 既定値の置き場（将来の設定ファイル用）

akapen には設定ファイルの仕組みが無い。今回は CLI まで。将来入れるときに
`CLI > 設定ファイル > 既定値` の層になるよう、

```rust
pub struct DecorationBlend { pub mark: f32, pub dim: f32 }
impl Default for DecorationBlend { /* MARK_BG_BLEND / DIM_BLEND */ }
```

を `decoration.rs` に置き、`Config.decoration_blend` は `Option` ではなく
**値**にした。各層が `Option` を持ち回るのではなく、下の層の値を上の層が
上書きする形。設定ファイルは `Config::parse` の前に既定を差し替えるだけで
入る。

## 配線（`DecorationStyles` の作り直し）

```rust
pub struct DecorationStyles {
    mark: Style,       // 背景だけ
    dim_target: Color, // テーマ背景（DIM の行き先）
    default_fg: Color, // fg を持たない span の実効前景
    dim_blend: f32,
}
pub fn from_theme(highlighter: &Highlighter, blend: DecorationBlend) -> Self;
pub fn mark_style(&self) -> Style;              // 旧 of(SemanticMark)
pub fn dim_fg(&self, base: Option<Color>) -> Color;
pub fn patch(&self, base: Style, kind) -> Style;
```

- `of(kind) -> Style` は**廃止**した。`Dim` はもう span に依存しない
  static な patch では表せない（`base.fg` を読んでから書く）ので、
  「kind ごとの Style」という形自体が嘘になる
- `Eq` derive を外した（f32 を持つため）。`PartialEq` は残っている
- `patch` は両 kind を**どちらの順で当てても同じ結果**になる（mark は
  背景しか書かず、dim は `base.fg` を読んでから前景を書く）。テストで固定。
  ただし**同じ `Dim` を 2 回当てると 2 回暗くなる**。producer は Atom ごとに
  1 つしか出さず、`--decorations` は開発用フラグなので許容

### blend をどう通したか

`ViewState::render` に `blend: DecorationBlend` を足した（`render_view_with_cards`
にも）。**入口を 1 つに保つため**で、テスト側の 40 数箇所は
`Default::default()`（インポート不要）で埋めてある。本番の 4 箇所:

| 場所 | 渡すもの |
|---|---|
| `main.rs` run() 起動時 | `config.decoration_blend` |
| `main.rs` `render_current_view` | `app.config.decoration_blend` |
| `reload.rs` `render_view_pending` | `app.config.decoration_blend` |
| `main.rs` の ghost 用 render × 2 | 既定（`.rows` しか使わない。装飾は載らない） |

## テスト

ベースライン akapen 506 → **514**（+8）。内訳:

- `decoration.rs` +4 / 更新 4
  - `each_kind_owns_one_channel_and_no_modifier`（旧 `no_kind_touches_the_foreground` を作り直し。
    mark=背景 / dim=前景 / modifier を触らない / 色相が残る / 合成の可換性）
  - `a_span_without_a_foreground_still_dims`（テーマ既定前景を解決してからブレンド）
  - `the_default_blends_produce_the_colors_that_were_chosen`（**実際の RGB を固定**）
  - `the_mark_background_stays_clear_of_the_selection_band`（帯との距離、天井の実効性）
  - `the_blend_factors_move_the_colors`（0 と 1 の振り切り）
  - 更新: `marked_normal_dim_on_one_rendered_line` / `a_list_marker_follows_the_whole_item_not_its_text` /
    `two_kinds_over_the_same_range_compose` / `no_kind_touches_the_foreground`
- `view.rs` +1 / 更新 2
  - `the_selection_band_suppresses_dim_entirely`（帯の下は**装飾なしの行と
    バイト単位で同一**。「打ち消す」ではなく「当たらない」になった）
  - `the_cursor_band_suppresses_dim_as_well`
  - `the_band_suppresses_only_dim_not_the_mark`（新規）
- `config.rs` +3（既定値が定数と一致 / 2 つが独立にパース / 範囲外は 7 種類とも拒否）
- `state_tests.rs` 更新 2（`decorations_paint_three_regions_on_one_terminal_line` と
  `the_reading_budget_splits_one_terminal_line_into_two_styles` を、modifier では
  なく**前景色の実際の値**で検査するように）
- 恒等性テスト（装飾ゼロなら出力不変）は変更なしで緑（名前は「検証」節）

## 既知の割り切り

- **`--dim-blend 1.0` は文字が消える。** `lerp(fg, bg, 1.0)` はページ背景
  そのものなので、合法な値で不可視になる。エラーにはしていない（0.0..1.0 は
  仕様どおりで、端点が「振り切り」を意味するのは knob として自然）。
  `the_blend_factors_move_the_colors` がその端点を意図として固定している。
  同様に `--mark-blend 1.0` は本文色べた塗りの帯になる
- **選択帯の `Style` から `sub_modifier: DIM` が消えた。** Phase 2 は
  `remove_modifier(Modifier::DIM)` の副作用として「装飾が空でも選択帯の
  `Style` 構造体に `sub_modifier: DIM` が載る（描画セルは不変）」を
  申し送っていた。その `remove_modifier` を撤去したので副作用ごと消えた。
  Phase 2 より素の状態に戻っただけで、描画は変わらない
- **同じ `Dim` を 2 回当てると 2 回暗くなる**（modifier は冪等だった）。
  producer は Atom ごとに 1 つしか出さない
- **帯の un-dim が行単位になった**（旧: span 単位）。上記「3.」のとおり

## 検証

- `cargo test --locked --workspace`: **akapen 514 / tui-markdown 173（+1 ignored）/
  semantic-reading 51 / doc 7**、全緑（ベースライン 506 / 173 / 51 / 7）
- 恒等性（装飾ゼロなら出力不変）は名指しでも確認:
  `decoration::tests::no_decorations_is_the_identity` /
  `no_decorations_preserves_the_attributions_too` /
  `view::decoration_tests::painting_with_no_decorations_changes_nothing`
- `cargo test --locked --release`: 515 passed
- `cargo clippy --locked --all-targets`: **警告 0**
- `TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh`: **OK**
- 実機 pty: 上記の SGR 列（帯なし / カーソル帯 / 選択帯）
- 変更ファイル: `src/decoration.rs` / `src/view.rs` / `src/config.rs` /
  `src/main.rs` / `src/reload.rs` / `src/chrome.rs` / `src/state_tests.rs` /
  `examples/semantic/README.md` / `HANDOFF.md`

---

# HANDOFF: Semantic Reading Layer を akapen へ配線（Phase 3 / Reading Budget）

## 何を作ったか

`semantic-reading` crate（Atom / SemanticUnit / ReadingTier / FixtureProvider /
`policy::decorate`）と、Phase 1・2 で出来た byte range attribution + range
decoration を**繋いだだけ**。どちらの中身も作り直していない。

マイルストーンは実機 pty で確認済み:

```
akapen examples/semantic/demo.md --semantic examples/semantic/demo.json
READ 100% → 30%
同じ source 行の途中で MARKED / NORMAL → MARKED / DIM に切り替わる
```

## 継ぎ目は `src/semantic.rs` と `App` の 4 フィールド

```rust
App {
    semantic_provider: Option<Box<dyn Provider>>,  // ← Jev はここに刺さる
    semantic_doc: Option<SemanticDocument>,
    reading_budget: u8,                            // 1..=100、既定 100
    semantic_decorations: Vec<Decoration>,         // doc × budget のキャッシュ
}
```

`src/semantic.rs` が akapen 側の変換層で、置いてあるのは 5 つだけ:

| もの | 役割 |
|---|---|
| `provider_from_config(&Config)` | **provider の選択はここだけ**。`--semantic-cmd` を足すときはこの match に腕を 1 本 |
| `DigestChecked<P>` | `source_sha256` を照合してから通す Provider ラッパー |
| `decoration_kind(DisplayState)` | `Marked -> SemanticMark` / `Dim -> Dim` / `Normal -> None` |
| `decorations_for(&doc, budget)` | `policy::decorate` 1 本。**ここから analyze へ到達する経路が無い** |
| `source_digest(&str)` | sha2 で hex 64 桁 |

依存の向きは akapen → crate の一方向のみ。ルート `Cargo.toml` に
`semantic-reading = { path = "crates/semantic-reading" }`（`Cargo.lock` も更新済み。
依存を足した直後は `--locked` が落ちるので、`cargo build` を 1 回挟んでからコミット）。

## 「Budget 変更では Jev を呼ばない」を構造で示した

Phase 2 が「decoration は `render::render` に到達しない」を構造で証明したのと
同じ水準にしてある。

- **App が provider へ `analyze` を投げるのは 1 箇所**（`App::reanalyze_semantics`）。
  確認は `grep -rn 'semantic_provider' src/` — フィールドを `.analyze` で触るのは
  その 1 行だけで、残りは宣言・初期化・代入と `semantic_enabled()` の `is_some()`。
  （`DigestChecked::analyze` も inner へ委譲するが、それは Provider チェーンの
  内側であって App からは 1 回の呼び出しに見える。`grep 'analyze('` だと
  ラッパーとテストの分まで拾うので、確認にはフィールド名の方を使うこと。）
  呼ぶのは**文書が入れ替わる 4 箇所**だけ:
  - `main.rs` run()（`activate_first_file` の直後）
  - `reload.rs` `reload_source`（`app.source = new_source` の直後）
  - `main.rs` `render_pending_history`（タイムマシン。既に debounce 済みの経路）
  - `app.rs` `switch_to_file`（ファイル切替）
- Budget キーの経路は `adjust_reading_budget` → `App::nudge_reading_budget` →
  `refresh_semantic_decorations` → `semantic::decorations_for` で終わり。
  clamp と `policy::decorate` しか無い
- テスト `moving_the_budget_calls_neither_the_provider_nor_the_renderer`:
  `analyze` の回数を数える provider を挿し、`app.view.rows[0]` に `SENTINEL` を
  置いてから budget キーを 50 回叩く。**回数は 1 のまま / SENTINEL は生きたまま /
  それでいて `semantic_decorations` は変わる**
- 逆向きも固定した（`the_provider_is_re_asked_when_the_document_itself_changes`）。
  これが無いと「そもそも繋がっていない」でも緑になる

### ミューテーションで検証済み

| 壊した箇所 | 落ちるテスト |
|---|---|
| キーの `if app.semantic_enabled()` ガードを外す | `without_semantic_the_budget_keys_are_not_bound_at_all` |
| `refresh_semantic_decorations` を no-op に | `moving_the_budget_...` と `the_reading_budget_splits_...` の両方 |
| `reload.rs` の `reanalyze_semantics()` を消す | `the_provider_is_re_asked_...` |

## `--semantic` が無ければ、この改修の前と完全に同一

追加要件（API キーを持たない人への配慮）。すべて `App::semantic_enabled()`
（= `semantic_provider.is_some()`）1 つにぶら下げてある。

- **キーを束縛しない。** `-` / `+` / `=` / `<` / `>` の match 腕にガードを付けて
  あるので、provider が無ければ腕ごと無かったことになり末尾の `_ => {}` に落ちる。
  「使えません」の toast すら出さない
- ステータス行に `READ %` を出さない（こちらは `semantic_doc.is_some()` 基準 —
  annotation が無ければ数値に意味が無いため）
- `?` ヘルプに `read` の行を出さない

判定を **provider** 基準にしたのは意図的で、`--semantic` を渡したのに fixture が
その文書のものでなかった場合はキーが生きたまま理由を言う（無言で死なない）。

## キーバインド（既存キーマップを監査して決めた）

view / source の両方の `match` と `?` ヘルプを洗った結果、記号キーで使われて
いたのは `]` `[` `?` だけだった。

| キー | 動き |
|---|---|
| `-` | READ −1 |
| `+` / `=` | READ +1（`=` は shift 無しの別名。ズーム系の慣習） |
| `<` | READ −10 |
| `>` | READ +10 |

`modifiers.is_empty()` ガードは**付けていない**。既存の `J` / `K` / `N` と同じ
理由で、端末によっては shift 付き文字に SHIFT フラグが立ち、ガードが黙って
握り潰すため。

**view モードのみ**にバインドした。source view は担当範囲外（触っていない）で、
装飾も出ないので、押すたびに READ % だけ動いて何も変わらないのは嘘になる。
読み出し自体は source モードでも出る（文書の性質であってモードの性質ではない）。

> **追補**: source view にも装飾が通ったので、この理由は消えた。
> 同じ 4 本が `on_source_key` にも束ねてある。

## `source_sha256`（手順3の落とし穴）

`FixtureProvider::analyze` は渡された source を見ない。`decoration::sanitize` は
panic を防ぐが、**文字境界に載ってしまう嘘の range は通ってしまう**ので内容の
ズレは防げない。

crate 側 `SemanticDocument` に**任意フィールド** `source_sha256` を足した
（`#[serde(default, skip_serializing_if = "Option::is_none")]` なので既存 fixture は
そのまま読め、書き出しの形も変わらない）。`validate` が見るのは**形だけ**
（hex 64 桁）で、ハッシュ実装は crate に入れていない。

照合は akapen 側の `DigestChecked<P>` が `analyze` の中で行う。**Provider の
内側**に置いたのは、拒否が継ぎ目で起きる形にするため — ダイジェストを名乗ら
ない provider（source を実際に見る Jev）は同じ契約のまま素通りする。

不一致のとき: `semantic_doc = None`（装飾ゼロ・READ 表示なし）＋ `flash_err`。
拒否と警告の両方をやっている。

## demo fixture（`examples/semantic/`）

`crates/semantic-reading/tests/fixtures/sample.json` ではマイルストーンを実証
できない（指摘どおり、line 4 の atom 2/3 は同じ Unit `u3` なので常に同じ状態）。
crate のテスト fixture は**一切触っていない**。

- `demo.md` — 日本語の設計メモ 1664 バイト、Atom 27 / Unit 13
- `demo.json` — その annotation（`source_sha256` 入り）
- `build-demo-json.py` — 生成スクリプト。**byte range は手書きしていない**
- `README.md` — 使い方と見どころ

### 行内で切り替わるのはここ

`## 結論` の下、`demo.md` の **7 行目**:

```
採用する方式は差分配信である。詳細は付録にまとめた。
```

前半が Unit `u3`（ESSENTIAL）、後半が `u4`（DETAIL）。attribution は
`224..302 exact=true` の **1 span** なので、decoration が span を byte 分割する
経路（Phase 2 の本命）をそのまま通る。

`## 補足` の 29 行目も同じ作りで、`u9`（SUPPORTING + `REDUNDANT_WITH(u3)`）と
`u10`（DETAIL）。READ 75 % で NORMAL / DIM になる。

### Budget と DIM の対応（スクリプトが出す累積表から取った。勘で選んでいない）

| READ | DIM |
|---|---|
| 100 % | なし（ESSENTIAL 3 Unit に MARKED が乗るだけ） |
| 75 % | DETAIL 4 つ（u4 付録 / u10 念押し / u12 数値 / u13 余談） |
| 73 % | それに加えて REDUNDANT な u9。CONTEXT（u6/u8/u11）はまだ全部残る |
| 30 % | ESSENTIAL 3 つといちばん短い SUPPORTING（u5）以外すべて |

REDUNDANT な u9 は元 Tier が SUPPORTING でも実効 CONTEXT へ 1 段落ち、さらに
同じ実効 Tier の非 REDUNDANT の後ろに回るので、**CONTEXT 3 つより先に DIM に
なる**。「REDUNDANT は元 Tier にかかわらず優先的に DIM 候補」がそのまま見える。

### Atom の作り方で踏んだところ（renderer の attribution を実測して決めた）

`ViewState::render` の `row_attrs` をダンプして確かめた結果:

- **見出しは `## ` を含めない。** renderer の attribution は marker を除いた
  見出しテキストそのもの（`## 結論` → `216..222` exact）
- **リスト項目も本文だけ。** `- ` の span は項目全体（`716..769` 非 exact）に
  紐づくので、本文だけを指せば本文が装飾され、marker は明るいまま残る
  （Phase 2 が固定した MVP の割り切り）
- **段落内の soft line break は改行 1 バイトまで Atom に含める。** renderer は
  行を半角スペース 1 個で繋ぎ、その合成 span の attribution は改行 1 バイト
  （非 exact）。覆わないと MARKED の帯に 1 セルの穴が空く
- **fenced code block は中身全体（末尾改行込み）を 1 つの Atom に。** 4 つの
  span が同じ非 exact attribution（`1440..1462`）を共有しているので、完全に
  覆わないと何も装飾されない
- **blockquote は引用テキストだけ。** `>` と空白の span は attribution を持たない

生成スクリプトの「末尾に足すバイト数」がこの 2 つ目・3 つ目のためのもの。

## 描画経路

`main.rs` の draw_view で `--decorations` と連結してから `sanitize`:

```rust
let decorations = if app.semantic_decorations.is_empty() {
    sanitize(&app.config.decorations, &app.source.content)   // 従来どおり
} else {
    let mut both = app.config.decorations.clone();
    both.extend_from_slice(&app.semantic_decorations);
    sanitize(&both, &app.source.content)
};
```

semantic 側を後ろに置いてあるので、同じ range に両方が当たれば patch の後勝ちで
semantic が勝つ。semantic が空のときは以前と 1 バイトも変わらない。

`policy::decorate` はフレームごとには走らない（`semantic_decorations` が
doc × budget のキャッシュ）。走るのは budget が動いたときと再解析のときだけ。

## 実機 pty での確認

`examples/semantic/demo.md` を 80 桁で開き、`## 結論` の行の SGR を読んだ。

READ 100 %:

```
\x1b[10;4H\x1b[39;48;2;54;55;73m採用する方式は差分配信である。   ← MARKED（bg のみ）
\x1b[10;34H\x1b[39;49m詳細は付録にまとめた。                     ← NORMAL
```

READ 30 %（`<` を 7 回）:

```
\x1b[10;4H\x1b[39;48;2;54;55;73m採用する方式は差分配信である。   ← MARKED のまま
\x1b[10;34H\x1b[2m\x1b[39;49m詳細は付録にまとめた。             ← DIM
```

READ 75 % では `## 補足` の行が col 54 で `\x1b[2m` に切り替わり（NORMAL → DIM）、
背景（CONTEXT）は `\x1b[39;49m` のまま、数値の目安（DETAIL）は `\x1b[2m`。

## 範囲外にしたもの

- **source view には手を付けていない。** `view.rs` の source モード経路は未変更
- **Jev の接続。** provider の継ぎ目まで。`CommandProvider`（`--semantic-cmd` で
  外部コマンドに委譲。文書を stdin、`SemanticDocument` JSON を stdout）は
  `provider_from_config` の match に腕を 1 本足すだけで刺さる。akapen に
  reqwest / tokio は入れない方針

## 既知の割り切り

- Budget は App 単位（読み方の好みなので、ファイルを切り替えても保つ）。
  annotation の方は文書ごとなので切替時に再解析する
- タイムマシンで過去を見ると、NOW に紐づいた fixture は `source_sha256` で
  拒否され、その revision のあいだ装飾が消える（毎回 `flash_err` が出る）。
  嘘の位置を装飾するよりは正しいが、**拒否メッセージに実 digest の先頭 12 桁が
  入るので revision ごとに文言が変わり、`is_repeat_error` の抑制が効かず BEL が
  毎回鳴る**。`demo.md` は revision が 1 つしか無いので今は踏まない。Jev が
  繋がると「過去を見るたびに解析」にもなるので、そのときは revision 単位の
  キャッシュと、拒否トーストの抑制（digest を messageから外すか、拒否した
  digest を覚えて 1 回だけ言う）をまとめて入れること
- `decorate_row` のコストは Phase 2 の HANDOFF どおり O(可視 span 数 ×
  decoration 数) / フレーム。demo は Atom 27 個なので効かないが、Jev が
  数千個持ち込んだら二分探索へ（**今はやらないこと**）
- source モードでは `READ %` は出るがキーは効かない（上記の理由）
  — **解消済み**。source view に装飾が通ったので、budget キーは
  両モードに束ねてある

## 検証

- `cargo test --locked --workspace`: **akapen 506 / tui-markdown 173（+1 ignored）/
  semantic-reading 51 / doc 7**、全緑（ベースライン 488 / 173 / 49 / 7）
  - 内訳: akapen +18（`semantic.rs` 11 / `state_tests.rs` 6 / `config.rs` 1）、
    semantic-reading +2（`source_sha256` の後方互換と形の検査）
- `cargo test --locked --release`: 507 passed（`debug_assert` 無効でも緑）
- `cargo clippy --locked --all-targets`: **警告 0**
- `TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh`: **OK**（上流 0.3.9 /
  フォーク 0.4.0、28 ファイル、1880 変更行 — `third_party/` は未変更）
- 実機 pty: 上記の SGR 列
- 変更ファイル: `src/semantic.rs`（新規）/ `src/app.rs` / `src/main.rs` /
  `src/chrome.rs` / `src/config.rs` / `src/overlay.rs` / `src/reload.rs` /
  `src/state_tests.rs` / `Cargo.toml` / `Cargo.lock` /
  `crates/semantic-reading/src/document.rs` /
  `crates/semantic-reading/tests/fixtures/README.md` /
  `examples/semantic/*`（新規 4 点）/ `HANDOFF.md`

補足: Phase 2 の HANDOFF と同じく、このリポジトリは rustfmt を掛けていない。
`semantic.rs` も既存ファイルと同程度に揃えてあるだけで、自分のファイルだけを
整形するようなことはしていない。

---

# HANDOFF: Range Decoration 層 — 任意の source byte range に Style を当てる（Phase 2）

## 何を作ったか

`src/decoration.rs`（新規）。Phase 1 が rendered span ごとに付けた
`Option<Attr>`（source byte range + exact フラグ）を intersect して、
**同一行の途中で Style を切り替える**層。Semantic Reading Layer の
最初の足場だが、**`semantic-reading` crate には依存していない**
（設計書「Semantic Reading Layer へ依存しない」）。`DisplayState →
DecorationKind` の対応付けは次フェーズの仕事。

公開 API は 3 つだけ:

```rust
pub struct Decoration { pub range: Range<usize>, pub kind: DecorationKind }

#[non_exhaustive]
pub enum DecorationKind { SemanticMark, Dim }

pub struct DecorationStyles { /* テーマ由来 */ }
impl DecorationStyles { pub fn from_theme(&Highlighter) -> Self; pub fn of(kind) -> Style; pub fn patch(base, kind) -> Style }

pub fn decorate_row(row: &[Span], attrs: &[Option<Attr>], decorations: &[Decoration], styles: &DecorationStyles)
    -> (Vec<Span>, Vec<Option<Attr>>);

pub fn sanitize(decorations: &[Decoration], source: &str) -> Vec<Decoration>;
```

文書全体の `Rendered -> Vec<Vec<Span>>` 変換は `decorate_row` を
`rows.iter().zip(&row_attrs)` に畳むだけなので、**専用関数は置いていない**
（置くと本体から呼ばれない dead code になり、Phase 1 で剥がしたばかりの
`#[allow(dead_code)]` を新設することになる）。

## 交差ルール（実装どおり）

| attribution | 規則 |
|---|---|
| `Some(attr)` かつ `attr.exact` | decoration の端で **span を byte 分割**し、交差部分だけ装飾。端が span 内部に落ちるものだけを cut 点にし、`debug_assert!(text.is_char_boundary(off))` |
| `Some(attr)` かつ `!attr.exact` | **`decoration.range ⊇ attr.range` のときだけ** span 全体を装飾。部分的な重なりは**何もしない** |
| `None` | 合成 span（table の枠・パディング、引用 prefix）。装飾しない |

分割の作り方は「decoration の端を全部 cut 点に入れてから、各片を
**完全に覆う** decoration だけを適用する」。端がすべて cut 点になっている
ので、どの片も各 decoration に対して「完全に内側」か「完全に外側」のどちらか
にしかならず、中途半端な片が残らない。

空レンジ・逆転レンジ（`8..4`）は両分岐の先頭で `range.is_empty()` により
捨てる。捨てないと逆転レンジが cut 点計算に端を入れ替えたまま到達する。

複数 decoration は **スライス順に patch を重ねる**（後勝ち）。`SemanticMark`
と `Dim` はそれぞれ bg と modifier しか触らないので、同一レンジに両方当てると
素直に合成される。

### 確認できた「にじみ出し」防止の実例

- `[ラベル](https://example.com)`: `ラベル` は exact（5..14）、rendered の
  ` (` / URL / `)` は **リンク全体 4..36 の上位集合**。`ラベル` だけを装飾すると
  URL 側は「4..36 ⊆ 5..14」が成り立たず装飾されない。逆にリンク全体 4..36 を
  装飾すると URL まで届く（どちらもテスト済み）
- table cell も上位集合。`cell one` の前半だけを指す decoration は**何も装飾
  しない**。`cell one` 全体を指せばセル全体が装飾される

## Style は置換せず patch する

`base.patch(kind_style)` のみ。`kind_style` が持つフィールドしか上書きされない
ので syntax highlight（fg・BOLD/ITALIC）が生き残る。テストで
「`**重要**` は BOLD と fg を保ったまま bg だけ増える」ことを固定した。

- **`Dim`** → `add_modifier(Modifier::DIM)` だけ。fg には触らない
  （テーマ由来のグレーにすると、dim 範囲の色が全部同じに潰れる）
- **`SemanticMark`** → **background だけ**。fg には触らない

### SemanticMark の背景をどこから取ったか（要注意）

`MdcommentStyleSheet::from_theme` と同じ `highlighter.scope_style(..)` 経路で
`markup.highlight` / `markup.mark` / `markup.quote.highlight` /
`region.yellowish` を順に引く。

**既定テーマは 2 つとも、このどれにも background を持っていない**
（Catppuccin Mocha / Solarized (light) の両方を実測）。したがって実際に
出荷されるのは fallback 側:

```
テーマの settings.background を settings.foreground へ 14% blend
```

- dark: `rgb(30,30,46)` → **`rgb(54,55,73)`**
- light: `rgb(253,246,227)` → `rgb(231,229,213)`

選択帯（dark `rgb(88,91,112)`）より明らかに弱い＝優先順位どおりの見え方になる。
テストで「ページ背景より本文色より**ページ側に近い**」ことを不等式で固定してある。

`invalid` スコープは Solarized (light) で赤背景を持っているが、意味が
「エラー」なので候補リストから**意図的に外した**。

テーマに `settings.background` すら無い場合のみ、本文色の明るさで
`MARK_BG_DARK` / `MARK_BG_LIGHT` の固定値に落ちる。

## 優先順位と selection × DIM

`selection > comment focus > diff > semantic decoration > syntax highlight`。
decoration は `visible_text_with_glow` の中で**最初**に当たり、selection /
カーソル帯 / glow はその**後**に当たるので自然に勝つ。

**問題があったのは DIM の打ち消しで、selection は打ち消していなかった。**
`s.style.bg(selected_bg)` は modifier に触らないので、DIM な行を選択すると
選択帯の中だけ沈んで「帯に穴が空いた」ように見える。selection 分岐と glow 分岐の
両方に `.remove_modifier(Modifier::DIM)` を足した。

- 副作用: decoration が空でも、**選択帯の span の `Style` 構造体に
  `sub_modifier: DIM` が載る**（`remove_modifier` は `sub_modifier` に union する
  実装）。DIM を持たない style に対しては描画セルが完全に同一なので、
  **端末出力は 1 セルも変わらない**。実機の pty でも確認済み（下記）
- 実機確認: カーソル帯をその行に乗せると ` 後` が `ESC[22m`（bold/dim off）
  で出力され、`ESC[2m` は**出ない**

## Rendered はキャッシュのまま、装飾は純粋なパス

- `render::render` / `Rendered` は**装飾を一切知らない**。decoration は
  `visible_text_with_glow` の**引数**で、`ViewState::render` にも
  `render::render` にも届かない
- したがって **decoration が変わっても Markdown の再パースは構造的に起きない**
  （Budget 37% → 36% の要件はここで満たされる）
- 適用は**可視行だけ**（`self.offset..end` のループ内）。view.rs の selection の
  当て方と同じ。decoration が空なら `Cow::Borrowed` で 1 バイトもコピーしない
- span 分割しても**行テキストの連結結果は変わらない**ので、`row_segments` は
  作り直していない（`span_hl(range)` が使う `off` オフセットもそのまま有効）

## row_attrs を ViewState へ移した — Phase 1 の地雷の処理

`Rendered.row_attrs` の `#[allow(dead_code)]` を外し、`ViewState.row_attrs` として
持たせた。あわせて `DecorationStyles` も `ViewState` に持たせている
（テーマは render 時に決まるので render と一緒に解決するのが素直。decoration の
**リスト**の方は selection と同じくフレーム引数）。

Phase 1 が警告していた 2 箇所に `row_attrs.insert` を並べた。**`vec![None]` では
なく `vec![None; spans.len()]`**（カード行も ghost 行も複数 span を持つ。row を
move する前に `len()` を取ること）:

- `main.rs` `insert_cards`（コメントカード行）
- `main.rs` `insert_history_ghosts`（履歴 ghost 行）

`row_segments` を触る箇所を正規表現で全部洗ったが、挿入はこの 2 箇所だけで、
削除は無い（ghost の撤去は行削除ではなく `rebuild_view_preserving_cursor` に
よる作り直し）。

**ミューテーションで検証済み**: `insert_cards` の `row_attrs.insert` を外すと
新規テスト 2 本（`a_comment_card_keeps_row_attrs_parallel_to_the_rows` /
`a_decoration_below_a_comment_card_still_lands_on_its_own_row`）が落ちる。
外さなければ落ちない。

保険として `visible_text_with_glow` 入口に
`debug_assert!(decorations.is_empty() || row_attrs.len() == rows.len())` を、
`decorate_row` 入口に `debug_assert_eq!(row.len(), attrs.len())` を置いた
（decoration 未使用時は発火しない。手組み `ViewState` を使う既存テストを
壊さないため）。

## 外から来るレンジは境界で濾す（実機で見つかった穴）

`--decorations` を pty で実際に走らせたら `a decoration edge must fall on a
UTF-8 boundary` で **panic した**（テストハーネス側が Python の文字インデックスを
渡していたのが直接の原因だが、外部入力が debug build で TUI を落とせる時点で穴）。

`decoration::sanitize(&[Decoration], source) -> Vec<Decoration>` を足し、
**描画直前に `app.source.content` に対して**濾している（`main.rs` の draw_view）。
`App::new` ではなく描画時にしたのは、reload・タイムマシン・ファイル切替で本文が
入れ替わってオフセットが陳腐化するため。`app.source` と `app.view` が必ず一緒に
差し替わることは確認済み（`render_pending_history` は同じ関数内で
`app.source = new_source` してから view を組み直す。ファイル切替も
`app.rs` の `source` / `view` を同じ `mem::take` の並びで入れ替える）ので、
「画面に出ている本文」で濾せている。decoration が空のときは `Vec::new()` なので
アロケーションゼロ。

これで「akapen 内部が作るレンジは整形式」という前提が本物になり、
`debug_assert` は attribution 層のバグ検出装置として意味を保つ。

## `--decorations <json>`（隠しフラグ）

`main.rs` の引数解析に素直に入ったので実装した。

```
akapen doc.md --decorations '[{"range":[23,29],"kind":"mark"},{"range":[31,35],"kind":"dim"}]'
```

`range` は**バイト**オフセット。`kind` は `mark`（= `semantic-mark`）/ `dim`。
JSON 不正・未知の kind・逆転レンジは **起動前に loud に失敗**する（隠しフラグでも
黙って何も描かないのは「層が壊れている」と読まれるので）。serde の
`Deserialize` は `config.rs` 側のローカル struct に置き、`Decoration` 自体は
serde free のままにしてある。

**実機 pty で目視確認済み**。`前 **重要** 後` に対する出力:

```
\x1b[39;49m前          ← NORMAL
\x1b[1m\x1b[39;48;2;54;55;73m重要   ← MARKED（BOLD 維持、fg そのまま、bg だけ追加）
\x1b[22m\x1b[2m\x1b[39;49m 後     ← DIM
```

## テスト（+29 本、459 → 488）

- `decoration.rs` 18 本
  - **マイルストーン**: `marked_normal_dim_on_one_rendered_line` —
    `前 **重要** 後` に対し同一行に 3 つの異なる style。装飾なし baseline と
    比較して「NORMAL は完全一致 / MARKED は bg だけ増える / DIM は modifier
    だけ増える」まで固定
  - **span 分割**: `前重要後`（1 span）→ 3 片。spec の `**重要**` は `**` で
    既に span が分かれているので**分割経路を通らない**。1 span のケースを別に
    用意してある
  - soft wrap 越え / link のにじみ（両方向）/ table cell の部分重なり /
    合成 span / 日本語・絵文字・全角 / 複数 kind の合成 / 範囲外レンジ /
    list marker の MVP 割り切り / fg 不可侵 / テーマ由来 bg / sanitize
  - **恒等性**: testdata 4 ファイル × 幅 40 / 80 で `decorate_row(.., &[])` が
    rows と一致
- `view.rs` 6 本: 描画経路への到達、selection が mark に勝つ、
  **selection 帯が DIM を打ち消す**、カーソル帯も同様、装飾なし描画の恒等性、
  `row_attrs` の並行性
- `state_tests.rs` 3 本: コメントカード下の decoration 位置（地雷）、
  カード挿入後の並行性、**TestBackend で Config → App → draw → セルまで**
  通した 3 領域の検証
- `config.rs` 2 本: フラグのパースと不正値の拒否
- 既存の ghost テストに `row_attrs` 並行性のアサートを追加

### 恒等性の機械確認（Phase 1 と同じ手法）

改修前 HEAD（`708d8ce`）を別 worktree に展開し、両方に同じダンプモジュールを
差し込んで testdata 全 4 ファイル × 幅 40 / 80 の

- `rows` の **span ごとの text + Style**
- `row_attrs` / `row_segments` / `source_starts` / `ghost`

をダンプして比較 → **9991 行、バイト単位で完全一致**（`cmp` で確認）。

## source view には手を付けていない（→ 実装済み）

> **追補**: この節はもう現状ではない。ここに書いた 3 段はその
> ままの形で実装され、source view にも range decoration が通っている。
> 冒頭の「HANDOFF: source view に range decoration を通す（Phase 1+2 の
> source 側）」を
> 参照。以下は当時の調査結果として残す（**地図としては正確だった** —
> 3 段の見立ても、`app.spans` / `line_rows` が `ViewState` とは別構造だと
> いう指摘も、そのとおりだった）。

タスクの指示どおり rendered view を先に完成させ、source view は止めて報告する。
理由（調査済み）:

- source mode の span は `Highlighter::highlight_with` が syntect の region から
  作っており、**byte range を一切持たない**（`Vec<Vec<Span>>` のみ）
- 折返しは `wrap_spans`（tagged でない方）で、attribution のチャネルが無い
- さらに `app.spans` / `line_rows` / source 側の draw は `ViewState` とは別構造で、
  `row_attrs` に相当するものを新設して通す必要がある

つまり「region の累積オフセット + line_starts で range を作る」→
「`wrap_spans` を tagged 化する」→「source 側の描画経路に attribution を通す」の
3 段で、rendered view と同じ規模の改修になる。**rendered view だけで価値がある**
という判断で、別コミットにもしていない。

## 既知の割り切り

- **DIM にしたリスト項目の `- ` マーカーは明るいまま**。marker の attribution は
  項目全体（`- 項目\n`）の上位集合なので、本文だけを指す decoration では
  覆えない。項目の source range 全体を指せばマーカーも dim になる。
  マーカーを本文と一緒に dim するには renderer 側で marker を exact 化する
  必要があり、それは Phase 3 の領分。テストで現状を固定してある
- hanging pad（折返しのぶら下げ空白）は `demoted()` の上位集合なので、段落全体を
  mark すると pad にも bg が乗る。ルールどおりなので許容
- 選択帯の `Style` 構造体に `sub_modifier: DIM` が載る（描画は不変、上記）
- `decorate_row` は span ごとに decorations を線形走査するので、コストは
  **O(可視 span 数 × decoration 数) / フレーム**。いまの用途（手で数個）では
  測るまでもないが、Semantic Reading Layer が Atom 単位で数千個持ち込むと効く。
  そのときは decorations を `range.start` でソートして二分探索に切り替える
  余地がある（**今は実装しないこと**。不要な複雑さになる）

## 検証

- `cargo test --locked --workspace`: **488 / 173（+1 ignored）/ doc 7**、全緑
  （ベースライン 459 + 29）
- `cargo test --locked --release`: 489 passed（`debug_assert` 無効でも緑。
  release でしか走らない「文字境界でない端は panic せず捨てる」テストが 1 本増える）
- `cargo clippy --locked --all-targets`: **警告 0**
- `TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh`: **OK**
  （`third_party/` は未変更なので `--update` 不要。上流 0.3.9 / フォーク 0.4.0、
  28 ファイル、1880 変更行で以前と同一）
- 実機 pty: `--decorations` で MARKED / NORMAL / DIM の 3 領域と、
  カーソル帯下での DIM 打ち消しを SGR 列で確認
- 変更ファイル: `src/decoration.rs`（新規）/ `src/render.rs` / `src/view.rs` /
  `src/main.rs` / `src/config.rs` / `src/state_tests.rs` / `src/chrome.rs` /
  `src/reload.rs` / `HANDOFF.md`

補足: このリポジトリは rustfmt を掛けていない（`cargo fmt -p akapen -- --check`
は改修前から全 19 ファイル 280 箇所で差分が出る。CI にも fmt ジョブは無い）。
`decoration.rs` の 10 箇所は既存ファイルと同程度なので、揃えるために自分の
ファイルだけ整形するようなことはしていない。

---

# HANDOFF: 追補 — vendor-diff CI の修復（上流バージョンとフォークのバージョンを分離）

（直前の「Phase 1: Range Attribution」エントリの追補）

## 問題

Phase 1 で `third_party/tui-markdown` を 0.3.9 → 0.4.0 に上げたところ、
CI の `vendor-diff` ジョブ（`./scripts/check-vendor-diff.sh`）が壊れた。

スクリプトは **フォークの `[package] version` を「ベンダリング元の上流
バージョン」として使っていた**。

```sh
VER="$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' "$VENDORED/Cargo.toml" | head -1)"
URL="https://static.crates.io/crates/tui-markdown/tui-markdown-$VER.crate"
```

結果、2 通りに壊れた（どちらもブランチ上で再現済み）:

- 存在しない上流 `tui-markdown-0.4.0.crate` を取りに行って 403 → exit 2
- `TUI_MARKDOWN_SRC` を渡した場合は、マニフェストのヘッダ（`# tui-markdown 0.3.9`）
  と `version`（0.4.0）が食い違い、契約1 が落ちて exit 1

両者が等しい前提は 0.3.9 のあいだ偶然成立していただけで、フォークの API が
変わるたびに再発する。

## 直し方: 上流バージョンをフォークの version から独立させた

`third_party/tui-markdown/Cargo.toml` に上流を明示するフィールドを足した。

```toml
[package.metadata]
vendored-from = "0.3.9"
```

`check-vendor-diff.sh` は 2 つを別々に読む:

- `VER`（= `vendored-from`）… 上流ソースの解決（ダウンロード URL / registry
  キャッシュ探索）と、マニフェストヘッダの照合。**契約1 はこちらで維持**
  （再ベンダリング時の取り違え検知として価値があるため、外していない）
- `FORK_VER`（= `[package] version`）… フォーク自身のバージョン。表示のみ

あわせて直したもの:

- スクリプト冒頭の「検証する3つの契約」「上流ソースの解決順」のコメントに、
  2 つのバージョンが別物である旨を明記
- `vendored-from` が無いときのエラーメッセージ（何を書けばよいか示す）
- 契約1 不一致時のヒント（「フォーク自身のバージョンは無関係」と明示）
- `--update` が書き出すマニフェストヘッダ。上流バージョンを指すことを明記。
  **フォークのバージョンは焼き込まない** — `--update` を挟まずにフォークだけ
  上がると、黙って古い値が残るため
- Cargo.toml の「Deviations from upstream」に byte range attribution 化を
  既存3項目と同じ粒度で追記（`Attr { range, exact }` と exactness contract）

### 途中で踏んだバグ

書き直したメッセージの `$FORK_VER）` が、直後の全角括弧まで変数名として
解釈されて `unbound variable` で落ちた（`--update` を実際に走らせて発覚）。
日本語メッセージ内の変数展開は `${FORK_VER}` と括ること。同種の箇所が他に
無いことを正規表現で確認済み。

## マニフェストの再生成

`--update` で再生成。変更行数の合計は **1355 → 1880（+525）**。増分は
Phase 1 で触った 6 ファイルにきれいに収まっている:

| ファイル | 旧 | 新 | 差 |
|---|---:|---:|---:|
| `src/lib.rs` | 13 | 14 | +1 |
| `src/renderer/code.rs` | 14 | 25 | +11 |
| `src/renderer/image.rs` | 49 | 53 | +4 |
| `src/renderer/math.rs` | 16 | 17 | +1 |
| `src/renderer/mod.rs` | 182 | 683 | +501 |
| `src/renderer/table.rs` | 886 | 893 | +7 |

内訳（`<` = 上流の行を置き換えた数、`>` = フォーク独自の行）:

- `<` は **全体で +2 だけ**。Phase 1 が新たに書き換えた上流行はこの 2 行のみ:
  - `code.rs`: `self.push_span(Span::styled(code, style));`（inline code の exact 化）
  - `image.rs`: `use super::TextWriter;` → `use super::{Attr, TextWriter};`
- `>` は +523。うち `mod.rs` が +501 で、その内訳は
  `Attr` 型 + doc + impl 78 行 / attribution ヘルパー 5 本 96 行 /
  `run_tagged` の exactness 全域検査 16 行 / `exactness_by_markdown_construct`
  テスト 304 行 = 494 行。残り ~58 行は `LineAttrs` と
  `from_str_with_options_tagged` の doc 書き換え、フィールド doc、呼び出し側の
  差し替え。

`list.rs`（`out_lines` → `out_attrs` の改名）は、もともとフォーク独自行だった
ものを書き換えただけなので変更行数は動かない（133 のまま）。**説明のつかない
増減は無い。**

## 検証

- `TUI_MARKDOWN_SRC=… ./scripts/check-vendor-diff.sh`: **OK**
  （上流 0.3.9 / フォーク 0.4.0、28 ファイル、1880 変更行）
- **CI が実際に通る経路も確認**: `TUI_MARKDOWN_SRC` 無し・registry キャッシュ
  無しで走らせ、`static.crates.io` から `tui-markdown-0.3.9.crate` を取得して
  グリーンになることを確認（以前の 403 は 0.4.0 という存在しない URL が原因で、
  ネットワーク制限ではなかった）。
- 退行しないことの確認:
  - マニフェストヘッダを 0.3.8 に細工 → 契約1 が期待どおり exit 1
  - `vendored-from` を削除 → 何を書けばよいか示して exit 2
  - フォークの version だけ 0.5.0 に上げる → **グリーンのまま**（今回の根本原因が
    消えていることの直接確認）
- `cargo test --locked --workspace`: 459 / 173 / doc 7、全緑
- `cargo clippy --locked --all-targets`: 警告 0
- 変更ファイル: `scripts/check-vendor-diff.sh` / `scripts/vendor-expected.tsv` /
  `third_party/tui-markdown/Cargo.toml` / `HANDOFF.md`

---

# HANDOFF: 内部位置モデルを source line から source byte range へ（Phase 1: Range Attribution）

## 問題

akapen の内部位置は「source line」が一次情報だった。vendored tui-markdown の
`LineAttrs = Vec<Vec<Option<usize>>>` は、rendered span ごとに **行番号だけ** を
持つ。これでは同一行内の一部だけを別スタイルにできない。

```text
line 42
[この部分だけ重要][この部分は通常][ここは重複]
```

上に載る予定の Semantic Reading Layer（`docs/semantic-reading-layer.md`）は、
Atom 単位の MARKED / NORMAL / DIM を「同じ行の途中で切り替える」ことを前提に
設計されている。行単位の attribution では静かに実現できない。

renderer は `Parser::into_offset_iter()` を使っており、内部には `Event + Range<usize>`
がすでに存在する。**新しく位置情報を取る必要はなく、すでにある byte range を
途中で捨てずに rendered output まで運ぶ** のが本改修（`docs/range-attribution-plan.md`
の Phase 1）。

## exactness contract（この改修の中核）

素朴に「event の range」を使うと `**重要**` の range が `**` ごと全体を指す。
Phase 2 の range decoration がこれを使うと、同じ行の隣接テキストへ装飾がにじみ出て
目的が達成できない。そこで range に **exact かどうか** を持たせた。

```rust
pub struct Attr {
    pub range: std::ops::Range<usize>,
    pub exact: bool,
}
pub type LineAttrs = Vec<Vec<Option<Attr>>>;
```

- `exact == true` … `span.text == input[range]`（verbatim スライス）。
  したがって `span.text.len() == range.len()` で、**byte オフセットで部分スライス
  できる**。`**重要**` の `重要` span は `重要` だけを指す。
- `exact == false` … **正しい上位集合**。span はその range から生成されたが、
  コピーではない（`&amp;` → `&`、softbreak → 空白、syntect が再分割した
  code、table cell）。位置の特定には使えるが、スライスしてはいけない。
- `None` … source に対応しない合成 span（table の枠・パディング、引用/リスト
  の prefix、段落区切り）。従来の `None` の意味をそのまま引き継ぐ。

exact 判定は pulldown-cmark が `CowStr::Borrowed` で input のスライスを返すことを
利用する（`TextWriter::exact_attr`）。ポインタ差でオフセットを取り、さらに
`source[offset..offset+len] == text` を実際に照合する二段ガードなので、
Owned / Boxed / Inlined を exact と誤認することはない。空文字列はポインタが
どこを指すか保証がないため、明示的に除外している。

**構造別の exact / fallback 一覧は `renderer/mod.rs` の
`exactness_by_markdown_construct` テストが正典。** 後続フェーズはこの表を前提に
設計してよい（テストは全ケースの全 span について `exact ⇒ verbatim` も検証する）。

## 実装内容

### third_party/tui-markdown（0.3.9 → 0.4.0）

- `Attr` / `LineAttrs` を上記のとおり定義し、`Attr::{exact, inexact, demoted, slice}`
  を用意。`line_starts` / `line_at` を `pub` にした（akapen 側が **同じ関数** で
  range → 行を導出するため。再実装すると末尾改行まわりで静かにズレる）。
- `TextWriter` の `current_line` / `current_end_line` を廃止し、attribution は
  4 つのヘルパーに集約:
  - `exact_attr` / `exact_subslice_attr` … borrow 由来の exact な range
  - `event_attr` … event 自身の range（上位集合）。合成 span の既定
  - `event_end_attr` … End event 由来の描画（front matter の閉じ `---`）
  - `nth_line_attr(k)` … 複数行 event の k 行目 = event range ∩ その行の範囲。
    syntect 再分割 code、`$$…$$`、書き換えられた text event 用
- `out_lines` → `out_attrs` に改名。`push_span_with_line` → `push_span_with_attr`。
- `run_tagged` に **exactness の全域チェック**を `debug_assert` で追加。exact を
  名乗る span が実際に `input[range]` でなければ落ちる。task-list marker の
  `to_mut()` や table cell の再マージのような「attribution 後の書き換え」を
  そのまま捕まえる。
- `table.rs`: cell の attribution は構造上つねに上位集合（折返しと空白畳み込みで
  テキストが書き換わるため）。`TableCell::push` で `demoted()` を強制。
  `CellChar` から冗長な `line` を落とし（`spans[ch.span].1` と同値）、`Copy` を維持。
- `image.rs` / `code.rs` / `math.rs` も `Option<Attr>` へ。

### akapen 本体

- `highlight.rs::wrap_spans_tagged`: exact な span は **byte オフセットで部分
  スライス**（fragment k 文字目 n バイト → `range.start+k .. range.start+k+n`）。
  上位集合の span は全 fragment が元 range 全体を保持。タブ展開でテキストが
  書き換わった fragment は range を保ったまま exact を落とす。
  不変条件「fragment の range ⊆ 元 span の range」を `debug_assert` で表現。
  hanging pad は span の位置を継ぐが `demoted()`（空白は source text ではない）。
- `render.rs`: `Rendered` に `row_attrs: Vec<Vec<Option<Attr>>>`（rows と並行）を
  追加。Phase 2 の range decoration はここを intersect する。
  `Segment` に `source: Range<usize>`（その phrase の source 上の hull）を追加。
- **`line_of(starts, attr) -> usize` を唯一の橋渡しにした。** wrap 直後に
  `row_attrs` から `row_lines: Vec<Vec<Option<usize>>>` を導出し、
  `build_starts_from_tags` / `build_row_segments_from_tags` / ghost 判定 /
  `insert_missing_blank_rows` は従来どおり行番号だけを消費する。view 側
  （`view.rs` / `main.rs` / `yank.rs`）は一切変更なし。

### 既存 UX は 1 ピクセルも変えていない（機械的に確認）

リファクタ前の HEAD を別ディレクトリへ展開し、testdata の全 `.md`（full.md /
a-readme.md / b-design.md / c-impl.rs）について

- renderer の per-span 行 attribution
- 幅 40 / 80 での `source_starts`
- 幅 40 / 80 での `row_segments`（line / start / end）
- `ghost`

をダンプして改修後と `diff` した。**差分 0 行**。`nth_line_attr` を締めた後にも
再取得して 0 行を再確認している。

## 検証

- `cargo test --locked`: **459 passed; 0 failed**（改修前のベースラインは 449。
  新規 10 本の内訳は下記。タスク記載の「422 本」は古い数値だった）。
- `cargo test --locked -p akapen-tui-markdown`: 173 passed / 1 ignored、doc-test 7 passed。
- `cargo test --locked --release`: 459 passed（`debug_assert` が無効な状態でも緑）。
- `cargo clippy --locked --all-targets`: **警告 0**。改修前から残っていた 4 件
  （`main.rs` の collapsible_match、`state_tests.rs` の unnecessary_cast × 3）も
  ついでに解消した。
- 新規テストが実際に効くことを mutation で確認: `wrap_spans_tagged` の部分
  スライスを外すと 2 本、`text()` の exact 判定を外すと 7 本が落ちる。

新規テスト（10 本）:

- `renderer/mod.rs::exactness_by_markdown_construct` … 構造別 exact / fallback 一覧
- `render.rs::range_attribution::` … `exact_ranges_are_verbatim_source_slices`
  （full.md を幅 20/40/80/200 で走査、release でも効く素の `assert!`）、
  ASCII / 日本語 / 絵文字・全角、`**strong**`、`[link](url)`、`` `inline code` ``、
  リスト項目、blockquote、狭い幅の soft wrap での部分スライス、hanging pad、
  `Segment.source` と `line` の一致

## 注意点

- **`Attr` は `Copy` ではない**（`Range<usize>` を持つため）。連鎖して
  `render::Segment` も `Copy` を失った。既存の利用箇所はすべて参照経由だった
  ので影響は無かったが、新規コードでは `.clone()` が要る。
- **exact を増やすときは `line_of` が変わらないことを必ず確認すること。**
  たとえば link の URL span を exact にすると、reference link では URL が
  ref-def 行から borrow されているため `line_of` が別の行へ飛ぶ。同じ理由で
  HTML block の各行も意図的に上位集合のままにしてある。この phase の判断基準は
  「exact 化は `line_of` が構造上変わらない場所だけ」。
- byte offset と terminal column を混同しないこと。日本語 1 文字 = 3 bytes =
  2 columns。`Segment.start` / `.end` は **行内テキストの byte** オフセット
  （従来どおり）、`Segment.source` は **source 全体の byte** レンジで、別物。
- `Rendered.row_attrs` は現状 view から読まれていないので `#[allow(dead_code)]`
  を付けている。Phase 2 で消せる。
- **Phase 2 で `row_attrs` を `ViewState` へ移すときの罠**: インラインコメント
  カードの行を差し込む `main.rs:1207` と `main.rs:2139` が
  `view.row_segments.insert(insert_at + i, Vec::new())` をしている。いまは
  `row_attrs` が `ViewState` に無いので壊れないが、移した瞬間に
  `row_attrs.insert(..., vec![None])` を並べて入れないと、カードより下の
  attribution が 1 行ぶん静かにズレる。
- `akapen-tui-markdown` は crates.io 未公開の path 依存なので、`LineAttrs` の
  破壊的変更は外部影響なし。衛生上 0.4.0 へ上げ、ルート `Cargo.toml` と
  `Cargo.lock` も揃えた（lock の差分はこの 1 エントリのみ）。
- vendored crate のテストに元からある `unused import: super::*` 警告 10 件は
  手つかず（改修前と同数・同箇所）。完了条件の `cargo clippy --locked
  --all-targets`（ルートパッケージのみ）の対象外。
- 変更ファイル: `third_party/tui-markdown/`（Cargo.toml / lib.rs / renderer の
  mod・code・math・image・table）/ `src/highlight.rs` / `src/render.rs` /
  `src/view.rs` / `src/main.rs` / `src/state_tests.rs` / `Cargo.toml` /
  `Cargo.lock` / `HANDOFF.md`

---

# HANDOFF: `e` → $EDITOR 復帰後にターミナルを再初期化する（マウスでゴミが出るバグ）

## 問題

akapen で `e` を押して `$EDITOR`（micro 等）を開き、エディタを終了して戻ると
ターミナルが壊れた状態になる。マウスを動かすと SGR マウスシーケンスの生バイト
（`^[[<35;10;8M` 等）が画面に文字として流れ、キー入力も効かなくなる。
ashiato（prefix+m）→ akapen → micro → Ctrl+q の実機フローで確認。

## 原因

`open_editor` はエディタ起動前に `DisableMouseCapture` + `ratatui::restore()` で
TUI を完全にサスペンドする（raw mode OFF・メイン画面へ）。ところが復帰後の処理は
コメント上は「no-blink init でターミナルを作り直す」となっているのに、実際には
`NoBlinkBackend::init()` を呼んでおらず、`EnableMouseCapture, Hide` の送出だけだった。
結果、復帰後の akapen は **raw mode OFF（canonical + echo）のままマウスキャプチャだけ ON**
という不正状態になり:

- マウス移動のたびに tty が SGR シーケンスの生バイトを echo して画面にゴミが出る
- キー入力は行バッファリングされて akapen に届かず、操作不能になる

pty ドライバでの最小再現で確定（修正前は `GARBAGE_ECHOED: True`）。

## 実装内容（src/reload.rs — `open_editor`）

- エディタ終了後、コメントの主張どおり `crate::NoBlinkBackend::init()` を呼んで
  raw mode + alternate screen を再進入し、`*terminal` を新しい Terminal に差し替える。
- `EnableMouseCapture` は init の**後**に送る（run() の起動順と同一。canonical 状態で
  capture を有効化しない）。
- init 失敗時は `flash_err` で通知し続行（ベストエフォート）。
- TestBackend 経路では検出できないクラスのバグなので、リグレッションテストは無し
 （pty 実機確認を検証手順として下記に記録）。

## 検証

- pty 再現スクリプト（python3 + pty.fork, EDITOR=micro, `e` → Ctrl+q → SGR マウス注入）:
  修正前 = ゴミエコーあり / 修正後 = ゴミ無し・`j` `q` とも正常動作・終了時に
  DisableMouseCapture + LeaveAlternateScreen が正しく出る。
- herdr 実ペインでも同フローを再現し、復帰後のマウス移動で `[<35` のゴミが
  スクロールバックに出ないこと（0 件）を `herdr pane read` で確認。
- `cargo test --locked`: 422 passed; 0 failed。`cargo clippy --locked --all-targets`: 警告 0。
- 変更ファイル: `src/reload.rs` / `HANDOFF.md`

---

# HANDOFF: composer のカーソルをブロックキャレット化し、幅文字の残像を抹消（2件）

## 問題1: カーソルが「半角スペース」のように見える

コメント入力中、カーソルを戻していくと半角スペースのようなものが前に現れずれる。
実機のレンダリング（`テストコ▏メント。`）で確定: カーソルグリフ `▏`（U+258F LEFT
ONE EIGHTH BLOCK）はセル左端 1/8 幅の極細縦線で、多くの端末フォントではほぼ透明に
しか描かれない → 「挿入点に半角スペースが入った」ように読める（コードの幅計算・
IME アンカーは正しかった。ユニットテストで確認済み）。

## 修正1: ブロックキャレット化（src/main.rs）

- `cursor_caret_line`（旧 `insert_cursor_glyph` を置換）: 挿入点の**文字の上に
  fg Black / bg Cyan のブロック**を重ねる。文末（文字が無い箇所）は**シアン下線**の
  キャレット（塗りつぶしブロックは「全角っぽくてダサい」という指摘に応えて変更）。
  グリフを一切使わないので、どの端末でも「スペースのようなもの」は出ない。
- `composer_body_rows` — 本文をグリフなしで折り返し（カーソル位置でリフローしない）。
- `composer_cursor_pos` — 挿入点を `text[..cursor]` の折り返しで追従。
- `composer_body_with_caret` — 本文行＋キャレットセル（幅境界で満杯終端の場合は
  キャレット専用行を追加）。`composer_lines` と `composer_line_count` が同一経路。

## 問題2: バックスペースで残像が残る（原因は ratatui の差分描画）

幅文字（CJK）を消すと右半分が画面に残る。最小再現で確定: ratatui-core 0.1.2（最新）の
diff は、幅文字が無くなった時**左半分だけ空白を書き、右半分セルはスキップする**
（ratatui の内部バッファでは右半分は元々「空白」扱いで前後一致だから）。実端末には
幅文字の右半分が表示されたまま = 残像。akapen のバグではない（pyte 実端末エミュレータ
でも確認。TestBackend は毎フレーム全描画するため検出できない）。

## 修正2: 描画後の残像パス（src/main.rs / src/app.rs / src/reload.rs）

- `draw()` 末尾で完成フレームを `app.last_frame` に保存。
- セッションの描画は `draw_frame(terminal, app)` に統一（run ループ 2 箇所 +
  `open_editor` 復帰）。terminal.draw の後に `clear_wide_char_residue` を実行:
  前フレームで幅文字だった列（記録済み `prior_frame`）が、新フレームで幅文字でなく
  かつ次のセルが空白（= diff がスキップした）なら、その右半分セルへ crossterm で
  明示的にスペースを書いて端末上の半グリフを消す。
- `clear_wide_char_residue_to(prev, curr, out)` は writer 注入可能（テストでバイト検証）。

## テスト

- `cursor_movement_does_not_reflow_the_composer` — カーソル全位置で本文行が安定。
- `composer_caret_follows_the_cursor` / `composer_tabs_expand...` — キャレット位置。
- `wide_char_residue_is_blanked_by_the_afterimage_pass` — 消された幅文字の右半分へ
  スペース書き込みが出ること、幅文字→幅文字では出ないこと。
- render 系テストはキャレット検出を「bg Cyan / シアン下線」判定に更新。
  IME アンカーがキャレット上に乗ることは維持。

## 検証

- `cargo test --locked`: 416 passed; 0 failed。clippy 警告 0。
- 変更ファイル: `src/main.rs` / `src/app.rs` / `src/reload.rs` / `src/state_tests.rs` /
  `HANDOFF.md`

---

# HANDOFF: ハードウェアカーソルの高速点滅を抹消（show_cursor を no-op 化）

## 問題

composer 中、挿入点付近のカーソルが速く点滅する。原因: ratatui の `terminal.draw` は
**位置が公開されるフレームごとに `show_cursor` を送る**（render.rs: フレームに
`cursor_position` があると `show_cursor() → set_cursor_position`）。akapen は composer 中
毎フレーム位置を公開する（macOS IME の変換窓アンカー）ため、毎フレーム Show → 直後の
Hide が繰り返され、ターミナルカーソルが赤rawレートで点滅する。

## 実装内容

- `NoBlinkBackend<W>`（src/main.rs）: `CrosstermBackend` のラッパーで
  **`show_cursor` だけを no-op** にする。位置の公開（`set_cursor_position`）はそのまま
  通す — カーソルの「表示状態」と「位置」は独立なので、IME アンカーは失わず、
  カーソルだけが永遠に出ない。
- `NoBlinkBackend::init()`: raw mode + alternate screen + Hide して返す
  （`ratatui::init()` の代替。パニックフックと Drop 復元は既存の TerminalGuard が担う）。
- `AppTerminal` = `Terminal<NoBlinkBackend<Stdout>>` をセッションの型にし、main.rs /
  reload.rs の全 `DefaultTerminal` 使用箇所を置換。`open_editor` 復帰の再初期化も
  `NoBlinkBackend::init()` に。
- run ループの post-draw `hide_cursor()` は防御として残す（実質不要だが無害）。

## テスト

既存の render 系テストは TestBackend 使用で無影響（416 all green、clippy 0）。

---

# HANDOFF: LOCAL の parent が内容重複 dedup で落ちても正しい血統スロットへ（タイムライン順序の修正）

## 問題

統合タイムライン（`DocumentHistory::assemble_timeline`）で、LOCAL スナップショットを
Git 世代の「観測時点 HEAD oid（parent）」に基づいて配置していた。しかし parent の
lookup を**内容重複除去済みの表示用 gits** に対して行っていたため、次のケースで
誤順序が起きていた:

- commit D と B が同じ内容 → 表示用 gits は newest（D）だけ残し B を除去
- B を parent とする LOCAL は B を解決できず **orphan 化し、タイムライン先頭（最深部
  の直後）に浮く**

```text
コミット: A → B → M → D（D と B は同内容 → B は表示されない）
LOCAL1:  B の上で編集 / LOCAL2: D の上で編集

理想:   now → LOCAL2 → D → M → LOCAL1 → A
従来:   now → LOCAL1 → LOCAL2 → D → M → A   ← 最古の LOCAL1 が一番新しい位置に
```

相対順序まで壊れる（LOCAL2 の方が新しいのに LOCAL1 が上）。
これは spec の「既知の劣化」（parent が解決できない orphan を先頭に置く fallback）では
なく、**解決できるはずの parent が dedup で消えたために起きる実バグ**だった。

## 実装内容（src/history.rs — `assemble_timeline`）

- 表示用に重複除去するとき、各表示コミットの**重複除去前の完全列での index** も
  保持する（`displayed: Vec<(full_index, Revision)>`）。
- LOCAL の parent lookup を**完全列**（`git_revisions`）に対して行う。parent が見つかれば
  「parent より新しい表示 commit の直後」= `insert_at = 1 + count_newer` に配置する。
  `count_newer` は `displayed` のうち `full_index < parent_full_index` の個数。
  displayed は newest-first 順なので、parent より新しい表示 commit は先頭から
  `count_newer` 個に一致する。
- 表示から落ちた parent を持つ LOCAL も正しいスロットに入り、orphan へ逃げない。
- 同一 parent を共有する LOCAL 同士は、従来どおり snapshots を oldest-first で処理し
  同じ slot へ insert するので newest が前（NOW 寄り）に並ぶ（挙動不変）。
- 真の orphan（非 git・rebase・別 ancestry・旧キャッシュ由来で完全列にも parent が
  無い）は従来どおり先頭へ観測順で入る（spec の既知の劣化のまま）。

## 付随テスト（fixture 2 追加、計 8 シナリオ）

1. `local_parent_deduped_out_of_gits_stays_in_its_lineage_slot` — 本バグの回帰。
   B を parent とする LOCAL が M と A の間に正しく収まること + LOCAL 同士の
   相対順序（LOCAL2 が LOCAL1 より新しい側）を固定。
2. `pre_commit_locals_with_null_parent_pile_above_git` — 既知の劣化（parent 無しの
   LOCAL が Git より新しい側へ観測順で積む）の契約を明文化。

## 検証

- `cargo test --locked`: 410 passed; 0 failed（fixture シナリオ 6→8）
- `cargo clippy --locked --all-targets`: 警告 0 件
- 変更ファイル: `src/history.rs` / `testdata/timeline-order-fixtures.json` / `HANDOFF.md`

---

# HANDOFF: `l` コメント一覧から過去リビジョンのコメントへ正しく遷移（feat/lkey）

## 問題

`l` で開くコメント一覧には、タイムライン（過去リビジョン）で付けたコメントも混在する。
従来は Enter で選択しても**リビジョンを復元せず**、現在のツリーの行番号へそのまま
ジャンプしていた。過去コメントの行番号は当時の文書に対するものなので、

- 選択範囲が現在の文書の無関係な行に落ちる
- カードは `visible_cards` が `c.revision == current_revision_context()` で
  フィルタするため表示されない（「コメントにきちんと遷移しない」）

## 実装内容

### src/overlay.rs — `activate_overlay_selection`（Comments 分岐）

Enter / ダブルクリックでコメントを選択したとき、ジャンプ前に**そのコメントが
書かれたリビジョンを復元**する:

- `revision: Some(rev)` のコメント → 対象ファイルの履歴から
  `Revision::context()` が一致する世代を探し `history.position` をそこへ移動。
  見つからなければ従来どおり現在の文書へプレーンジャンプ（fallback）。
- `revision: None`（NOW のコメント）→ 過去閲覧中に選択した場合は NOW
  （position 0）へ戻す。通常の NOW→NOW ジャンプは position が既に 0 なので無変化。
- 復元後、`history.position != rendered_position` のときだけ
  `render_pending_history(app, false)` を**同期実行**（`jump_review_mark` と同じ
  ガード）。オーバーレイは閉じた直後なので即座に描画され、その後に選択範囲を設定
  するため「描画が選択を消す」競合がない。共通パス（NOW で NOW コメント選択）は
  描画をスキップし、ランディングパルス（250ms フレームフラッシュ）も出さない。
- スクラブ中の保留描画（`history_render_due` が残っている）があっても、
  同期描画がフラグを消費するか、同一 content の early-return が選択を保持する
  ため、後から選択が消えることはない。

### src/history.rs — `anchored_line` の空 old ガード（付随修正）

`old` が空（空ファイルの起動直後に履歴をブラウズする等）だと
`old[0]` で index out of bounds パニックする潜在バグを発見。
`new.is_empty()` ガードと対称に `old.is_empty()` なら 0 を返すようにした。

## 追加テスト（5 件、全 green）

1. `comments_overlay_enter_restores_a_historical_revision` — 過去コメント選択で
   position が復元され、当該リビジョンが描画され、選択範囲・カード表示が正しい
2. `comments_overlay_enter_restores_a_revision_on_another_file` — 別ファイルの
   過去コメント選択で対象ファイルの履歴が復元される（複数ファイル）
3. `comments_overlay_enter_falls_back_when_the_revision_is_stale` — 履歴に
   存在しないリビジョン（pruned 等）はプレーンジャンプにフォールバック
4. `comments_overlay_enter_returns_to_now_for_a_live_comment` — 過去閲覧中に
   NOW コメントを選ぶと NOW へ戻ってジャンプ
5. `anchored_line_survives_an_empty_old_document` — 空 old でパニックしない

## 検証結果

- `cargo test --locked`: **382 passed; 0 failed**（ベースライン 377 + 追加 5）
- `cargo clippy --locked --all-targets`: **警告 0 件**
- 変更ファイル: `src/overlay.rs` / `src/history.rs` / `src/state_tests.rs` /
  `README.ja.md`（オーバーレイ共通操作の説明にリビジョン復元を追記）

## 動作確認の手順

1. git 履歴のある markdown で `akapen doc.md` を起動
2. `←` で過去へ遡り、その世代で `c` → コメント作成
3. `l` でコメント一覧 → 過去コメントにカーソルを合わせ Enter
4. タイムラインがそのリビジョンまで戻り、コメントの行が選択され、カードが表示される
