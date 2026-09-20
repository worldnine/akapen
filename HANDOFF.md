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

## source view には手を付けていない

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
