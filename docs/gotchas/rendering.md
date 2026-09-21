# 描画と attribution

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。

---


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
先頭の `"  "` span の bg が `mark_style().bg`（測定時のダークテーマで
`Rgb(68, 70, 89)`。**いまは amber の `Rgb(90, 69, 33)`** —
下の「MARKED の天井の理由が変わった」参照）になりました。pad が exact を
名乗らないことは `src/render.rs::a_hanging_pad_is_never_exact` が
固定しています。

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

### MARKED の天井の理由が変わった — 帯との混同ではなく、字が読めるか

`MARK_BG_BLEND_CEILING` は **0.25 → 0.40**、理由ごと差し替わりました。
数字だけ見て古い理屈を引き継がないでください。

| | 以前 | いま |
|---|---|---|
| ブレンド先 | テーマの**前景色** | `MARK_TINT`（amber `#ffb000`）|
| 既定値 | 0.22 | **0.27** |
| ダークで出る色 | `rgb(68,70,89)` 灰 | `rgb(90,69,33)` |
| ライトで出る色 | `rgb(219,218,205)` | `rgb(253,227,165)` |
| 天井 | 0.25 | **0.40** |
| 天井の理由 | 濃くすると**選択帯 `rgb(88,91,112)` と見分けがつかない** | 濃くすると**マークの上の字が読めない** |

**古い理由はもう成り立ちません。** 灰色だった頃は明るさだけが違いで、
濃くするほど帯に近づきました。いまは色相で分かれます — `rgb(90,69,33)` と
`rgb(88,91,112)` は**明るさがほぼ同じでも混ざりません**（暖色 vs 寒色）。
実機の ANSI で 4 色を同一画面に出して確認済みです:

| 役割 | 実測 SGR |
|---|---|
| MARKED（dark） | `48;2;90;69;33` |
| MARKED（light） | `48;2;253;227;165` |
| 選択 / カーソル帯 | `48;2;88;91;112`（light `210;210;220`）|
| diff 緑 / 赤 | `48;2;35;61;47` / `48;2;61;35;35` |

代わりに効くのはコントラストです。ブレンドを上げると amber は明るく
なり、上に乗る文字は動かないので、ダークの本文 `#cdd6f4` は既定で
約 6:1、0.40 で約 4.5:1、0.55 で 4.5:1 を割ります。天井はそこから
決めました。テストは `src/decoration.rs` の
`the_ceiling_is_where_the_text_on_the_mark_stays_readable` と
`the_mark_background_stays_clear_of_the_selection_band`（距離ではなく
**寒暖**を見る形に書き換えてあります）。

**帯とマークが重なったとき**: 帯が勝ってセル全体を塗るので、amber の
穴は空きません（`src/view.rs` の `s.style.bg(selected_bg)` は無条件）。
ただし**行が複数の source 行から折り畳まれている場合**、選択されて
いない側の phrase segment は amber のまま残ります。これは以前から
そうでしたが、**灰色どうしだったので見えていませんでした。**
いまは継ぎ目がはっきり見えます — バグではなく、選択の粒度が
行であることが可視化されただけです。

**確認したこと**: `src/decoration.rs::mark_background` の blend 先が
`MARK_TINT`。実機（別ペインで `herdr pane read --format ansi`）で
ダーク / `--light` / `--theme DarkNeon` / source モードの diff 帯を確認。

### `MARK_SCOPES` に背景を持つ埋め込みテーマは **DarkNeon 1 本だけ**

「テーマが自前の highlight 背景を持つならそれが勝つ」という分岐
（`src/decoration.rs::mark_background` の先頭ループ）は、**書かれてから
一度も実機で走っていませんでした。** 次に触る人が試すテーマが
分からない、というのがその原因です。総なめした結果を置いておきます。

two-face の埋め込みテーマ **32 本中 1 本**だけが到達します:

```sh
./target/debug/akapen <file> --theme DarkNeon --decorations '[{"range":[0,20],"kind":"mark"}]'
```

| テーマ | scope | 背景 |
|---|---|---|
| DarkNeon | `markup.quote.highlight` | `rgb(254,224,156)` |

**ただし、この到達の仕方は事故に近いものです。** 3 点:

1. DarkNeon は `markup.quote.highlight` を定義していません。定義して
   いるのは **`markup.quote`（ただの引用ブロック）** で、syntect の
   前方一致で `markup.quote.highlight` にも当たります。つまりこの分岐は
   「テーマが highlight 色を持っている」ではなく
   **「テーマが引用ブロックに背景を付けている」**を拾っています
2. その背景は **アルファ 18/255 の重ね色**です。`Highlighter::scope_style`
   はアルファを捨てるので、本来ほぼ透明な指定が**べた塗りのクリーム色**
   になります
3. 結果、**紙が黒い DarkNeon の上にほぼ白い帯**が出ます。本文 span は
   前景色を持たず端末既定（ダーク端末では白に近い）で描かれるため、
   実測で白 `38;2;255;255;255` on `48;2;254;224;156` = **約 1.3:1** —
   字が読めません

この分岐を残すか落とすか、`MARK_SCOPES` から `markup.quote.highlight` を
外すか、`scope_style` でアルファを合成するかは**設計の判断**なので、
ここでは何も変えていません。テーマ優先そのものは
`a_theme_that_styles_a_highlight_scope_wins_over_the_amber`
（`.tmTheme` を書いて固定）で保証されています。顔ぶれが変わったら
`the_only_embedded_theme_that_reaches_the_scope_branch` が落ちるので、
**そのときはこの表も直してください。**
