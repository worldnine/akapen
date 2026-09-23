# 描画と attribution

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。

---

### 本文の高さは端末の高さから直に出さない — Review の一覧が下に据わっている

**症状（になるもの）**: `R` の一覧を開いたまま j/k やクリックをすると、
カーソルが一覧の下に潜る・クリックが 1 行ずれる・スクロールバーの端が合わない。

**原因**: `R` の一覧は窓ではなく**本文の下に据え付けてあり**、開いている間は本文の
領域が一覧のぶん低い（`src/review_dock.rs`）。端末の高さ（`terminal::size()` /
`App::terminal_height`）から `- 4` や `- 2` で本文の高さを出すと、一覧のぶんを
数え損なう。本文の高さは **`App::view_viewport_rows` / `App::source_viewport_rows`**
を通すこと（両方が `review_dock::body_rows_taken` を引く）。メッセージ行の y は
`chrome::message_row`、一覧の矩形は `review_dock::split` / `current` が唯一の式。

view では据え付けの題の行が本文の枠の下辺と**同じ行**に乗る（1 行重なる）ので、
本文が失うのは据え付けの全高より 1 行少ない。source は枠が無いので全高を失う。
この差を別の場所で計算し直すと 1 行ずれる。

テストは端末の大きさを `crate::app::TEST_TERMINAL_SIZE` で `TestBackend` と揃える。
`cargo test` は端末の中で走ると本物の大きさを拾うので、揃えないと
`view_viewport_rows` と描いた本文が別の高さを見る。

**確認したこと**: `src/review_dock_tests.rs` の
`the_viewport_is_exactly_the_body_that_is_drawn`（view / source × 高さ 20・24・40・60
で、計算した本文の高さと描いた本文の行数が一致）と
`a_click_on_the_body_lands_on_the_line_drawn_there_and_keeps_the_list`。
`timeline.rs` の `terminal_width` など、**幅だけ**を見ている箇所は据え付けの影響を
受けない（一覧は横幅を取らない）。

### 横幅変更のフレーム比較は新旧バッファの共通領域だけを見る

**症状**: 日本語を含む文書を表示したまま端末を 120 桁から 40 桁へ縮めると、
ratatui のバッファ範囲外参照で panic した。比較処理を直した後も、幅 1 の
View と幅 0 の Source ではスクロールバー座標の減算がオーバーフローした。

**原因**: 全角文字の右半分を消す `clear_wide_char_residue_to` が、前フレームの
領域を走査しながら縮小後のバッファを同じ座標で参照していた。縮小で画面から
消えた列は残像消去の対象ではないため、新旧 `Rect` の共通領域だけを比較する。
スクロールバーの右端は、幅から 1 桁または 2 桁を引けない場合があるので
`saturating_sub` で求める。

**なぜ気づかなかったか**: 既存の残像テストは新旧バッファを同じ大きさで作り、
描画テストも通常幅だけを使っていた。さらに `TestBackend` は毎回バッファ全体を
描くため、実端末の差分描画後に走る残像消去を通らない。異なる幅のバッファを
直接比較するテストと、View / Source を 120 → 40 → 20 → 8 → 120 桁および
0〜10 桁で描くテストを対にして固定する。


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

**この天井は「確定色」の話です。** 2026-09-23 に、演出が**引く線**だけは
これを超えるようになりました（`MARK_FLASH_BLEND` = 0.65、ダークで
`rgb(176,124,16)`）。線は 450 ms で通り過ぎ、後ろも 250 ms で乾くので、
その間だけ字が 2.83:1 まで落ちるのは許容する、という切り分けです。
**確定色をこれで動かしてはいけません** — そこは読み続ける色です。

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

### カーソル行の MARKED は帯に隠れる — 「1 本だけ琥珀が乗らない」に見える

**症状**: `--semantic` で開いた文書で、policy が MARKED と判定した Atom の
うち **1 つだけ**琥珀が乗らない。他は乗る。`marks::mark` を直に呼ぶと
その Atom も `Marked` を返すので、描画側の退行に見える。

**原因**: その行に**カーソルが乗っていた**だけです。カーソル帯は行全体の
背景を塗り、マークより優先します（`src/view.rs` の `span_hl` →
`s.style.bg(selected_bg)` は無条件。「MARKED の天井の理由が変わった」の
末尾「帯とマークが重なったとき」参照）。帯の下では MARKED の句と NORMAL の
句が**同じ背景**になるので、「その行だけマークが無い」と読めます。
確かめたい行へカーソルを動かして見る、という確認のしかた自体が
症状を作ります。文書末尾の Unit ほど「j で下りて見る」ので当たりやすい。

**切り分け**: 状態行の `行/総行数` がその Atom の行を指していないか。
`k` で 1 行離れて琥珀が戻れば帯です。`decorate_row` の交差ルール、
hanging pad、折り返し、`section_of` はどれも無関係でした。

**確認したこと**: 実機の該当行 5 本すべての背景が選択帯 `48;2;88;91;112`
で、状態行のカーソル位置がその source 行だったこと（`herdr pane read
--format ansi`）。TestBackend で同じ文書を幅 80〜236、budget 100 / 31、
すべてのスクロール位置で描き、カーソルを他の行に置けば **17 本すべて**に
琥珀が乗り、カーソルをその行に置くと琥珀 0 セル・帯 74 セルになること。
合成文書での固定は `src/state_tests.rs` の
`a_marked_line_under_the_cursor_shows_the_band_not_the_amber`
（帯の下で MARKED と NORMAL の背景が一致し、離れれば琥珀が戻る）。

**ついでに踏みかけた地雷**: 実機の akapen は**起動時のバイナリ**で動き
続けます。症状を見たプロセスの起動時刻と `target/debug/akapen` の更新時刻
を見比べてから、いまのコードを疑うこと（`ps -o lstart= -p <pid>` と
`ls -l`）。

### テーマの highlight scope 尊重は、一度やって落とした

**これは自然に再発する案です。** 「テーマが自前の highlight 背景を
持っているなら、それを尊重して使うべきだ」は筋が通って聞こえます。
実際に実装されていました。**測って落としました。** 同じ提案を
また書く前に、ここを読んでください。

あった分岐（`src/decoration.rs::mark_background` の先頭ループ）:

```rust
const MARK_SCOPES: &[&str] = &[
    "markup.highlight", "markup.mark", "markup.quote.highlight", "region.yellowish",
];
// この 4 つのどれかに背景を持つテーマなら、その色を使って早期 return
```

two-face の埋め込みテーマを**総なめした結果、32 本中 1 本**だけが
到達しました。

| テーマ | 当たった scope | 背景 |
|---|---|---|
| DarkNeon | `markup.quote.highlight` | `rgb(254,224,156)` |

**その 1 本も、間違った理由で発火して、壊れた結果を出していました。**

1. DarkNeon は `markup.quote.highlight` を定義していません。定義して
   いるのは **`markup.quote`（ただの引用ブロック）** で、syntect の
   前方一致で `markup.quote.highlight` にも当たります。つまり分岐は
   「テーマが highlight 色を持っている」ではなく
   **「テーマが引用ブロックに背景を付けている」**を拾っていました
2. その背景は **アルファ 18/255 の重ね色**です。`scope_style` は
   アルファを捨てるので、ほぼ透明な指定が**べた塗りのクリーム色**に
   なります（下の節）
3. 結果、**紙が黒い DarkNeon の上にほぼ白い帯**が出て、実測
   白 `38;2;255;255;255` on `48;2;254;224;156` = **約 1.3:1**。
   字が読めませんでした

つまり残る価値が無い。**1 本のテーマに、間違った理由で発火して、
壊れた結果を出すものは尊重ではない**、というのが落とした判断です。

落としたことで得たもの:

- **経路が 1 本になり、必ず走る。** 「書かれてから一度も実行されて
  いない分岐」がそもそも問題でした。式が 1 本なら、壊れていれば
  誰でもすぐ気づきます
- `--mark-blend` が**どのテーマでも**効くようになりました。以前は
  テーマ次第でつまみが黙って死んでいました

**戻すなら**、少なくとも (a) scope を前方一致で誤射しない形にする、
(b) アルファを紙へ合成する、(c) 出た色の上で本文が読めるか検査する —
の 3 つが要ります。再発防止のテストは `src/decoration.rs` の
`a_theme_that_styles_a_highlight_scope_gets_the_amber_anyway`
（`.tmTheme` を書いて「見えているのに使っていない」ことを固定）と
`every_embedded_theme_gets_the_amber_formula`（32 本すべてが式どおり）。

### `scope_style` はアルファを捨てる — DarkNeon の引用とインラインコードが読めない

**上の分岐を落としても、この問題は消えません。** アルファを捨てて
いるのは `src/highlight.rs::scope_style` で、そこは
`src/render.rs::MdcommentStyleSheet::from_theme` が**本体の描画**に
使っている一般の経路だからです。

再現:

```sh
./target/debug/akapen <引用とインラインコードを含む .md> --theme DarkNeon
```

実測（別ペインで `herdr pane read --format ansi`）:

| 構造 | 解決する scope | テーマの指定 | 実際に出る | コントラスト |
|---|---|---|---|---|
| 引用ブロック | `markup.quote.markdown` | `rgba(254,224,156,18)` | bg `48;2;254;224;156` / fg `38;2;225;212;185` | **約 1.1:1** |
| インラインコード | `markup.raw.inline.markdown` | `rgba(177,179,186,8)` | bg `48;2;177;179;186` / fg `38;2;87;139;179` | **約 1.7:1** |

紙は黒（`#000000`）なので、アルファ 18/255 を正しく合成すれば
`rgb(18,16,11)` 程度の「ほぼ黒」になるはずのものが、**べた塗りの
クリーム色**として出ています。

**これは MARKED の色とは無関係の、前からあるバグです。**（今回は
触っていません。）影響範囲も測ってあります — 埋め込み 32 本のうち
アルファ付き背景を持つのは `ansi` / `base16` / `base16-256` /
`DarkNeon` / `Monokai Extended` 系ですが、`render.rs` が解決する
markdown の scope に当たるのは **DarkNeon だけ**です
（Monokai の `markup.table` はどこからも引かれていません）。

直すなら `scope_style` で `theme.settings.background` へ合成する
のが素直です。ただし**合成先が「紙」とは限らない**（インライン
コードは本文の上に乗る）点は考えること。
### 演出の立っている 1 枚目には琥珀が無い — テストが「マークが消えた」と言う

**症状**: `TestBackend` で 1 フレーム描き、琥珀の背景 (`mark_bg()`) を
数えると **0 セル**。`app.semantic_decorations` には MARKED が入っていて、
実機では琥珀が見えている。

**原因**: マーカーが引かれる演出（`effects::marks_reveal_effect`）が
立っていました。段 1 は**ページ色から**琥珀の半分へ上げるので、
`alpha = 0` の 1 枚目では琥珀のセルがちょうどページ色で塗られています
（線の色（`mark_flash_bg()`）が乗るのは段 2 からです）。答えが
`semantic_doc` に入った瞬間に立つので（`App::accept_analysis`）、
**fixture 経路でも起動直後の 1 枚目は必ずこれに当たります。**

**対処**: 画面のセルを見るテストは `app.config.fx = false` にし、
`app.marks_fx` / `app.readout_fx` を `None` に落としてから描くこと
（旗を折るだけでは、既に立っている演出が消えません）。静止した絵が
見たいのであって、演出を見たいのではない場合です。

**確認したこと**: `src/marks_tests.rs` の
`focus_changes_what_is_painted_not_just_a_flag`。fx を止める前は
「琥珀が画面に出ている」の前提で落ち、画面の bg の分布は
`Reset` / ページ色 / 選択帯 の 3 つだけでした（琥珀のセルがページ色の
側に混ざっていた）。

### 名前付きの色を塗ったセルは tachyonfx の演出が素通りする

**症状**: 読み出しの座布団（琥珀の地に黒字）に 300 ms のフラッシュを
乗せたのに、**1 フレームも光らない**。矩形は合っていて、演出は立って
いて、`done()` でもない。フレームを途中まで進めて前景を見ると、静止時の
黒のまま 1 度も動いていません。

**原因**: `crate::view::lerp_color` は **RGB 同士でしか混ぜません**。

```rust
match (a, b) {
    (Color::Rgb(..), Color::Rgb(..)) => …,
    _ => b,                       // ← 行き先をそのまま返す
}
```

演出（`effects::readout_flash_effect`）は「琥珀 → **描かれた前景**」へ
戻す形なので、描かれた前景が `Color::Black`（名前付き）だと、どの alpha
でも `Color::Black` が返ります。**演出は走っていて、混ざった結果が動いて
いないだけ**なので、旗を見ても矩形を見ても気づけません。

**対処**: 演出を乗せるセルの色は `Color::Rgb(...)` で塗ること。座布団は
`Color::Rgb(0, 0, 0)` にしてあります（`chrome::draw_footer`）。同じ黒でも、
光らない `FOCUS` バッジの方は `Color::Black` のままでよい — 違いはここに
書いてあります。

**確認したこと**: `src/marks_tests.rs` の
`the_flash_lands_on_the_cushion_and_nowhere_else`。`app.last_draw` を
150 ms 前へずらして 1 枚描くと、演出の途中の絵が見られます（実機では
速すぎて captured frame に写りません — CLI の往復が 300 ms より遅い）。

### `RAPID_BLINK` は波線の印 — 別の用途に使わない

**Review の下線のセルには `Modifier::RAPID_BLINK` が立っています**
（`crate::decoration::CURL_CARRIER`）。ratatui 0.30 のセルには下線の形を
書く場所が無いので、印を借りて描画の出口（`NoBlinkBackend::draw` →
`crate::undercurl::draw`）で `CSI 4:3 m` か普通の下線に読み替えています。
出口は印を**必ず落とす**ので、点滅は 1 度も端末に出ません。

- 何かを本当に点滅させたくて `RAPID_BLINK` を立てても、**出口が黙って
  捨てます**（波線の印と見分けられない）。点滅は使わない前提です
  （`tui-design` の「blink は避ける」とも合う）
- `TestBackend` のテストで Review の下線を探すときは、`UNDERLINED` では
  なく `UNDERLINED | CURL_CARRIER` で数えてください。レベル 1 の見出しも
  `UNDERLINED` を持っています（`src/render.rs` の見出しのスタイル）
- `NoBlinkBackend` を通らない描画（`CrosstermBackend` を直に使う）を
  足すと、印が点滅として漏れます

**確認したこと**: `src/undercurl.rs` の
`a_carried_underline_curls_only_when_the_terminal_can`（`on` / `off`
どちらの出口でも `CSI 5 m` / `CSI 6 m` が出ない）と
`src/review_render_tests.rs` の
`the_painted_underline_leaves_as_a_curl_through_the_exit`（実際に描いた
画面を出口に通す）。

### herdr は下線の色を落とす — 波線は通る

herdr 0.9.1 の中で akapen を開くと、**Review の下線の重さの色が出ません**
（下線は文字色になる）。herdr は `CSI 58`（下線の色）を内部の画面には
持っていますが、外側の端末へ描くときに出しません。波線（`CSI 4:3 m`）は
そのまま外側へ出ます。akapen の不具合ではないので、重さはガターの白抜きの
印（重さの色の地に紙の色で抜いた `E` / `W` / `I`）で読んでください（字と地の
色は herdr を通る）。下線の色はパレット番号（`58;5;N`）で出していますが、
番号でも RGB でも herdr が落とすのは同じです。

**確認したこと**: 2026-09-23、別名のセッション（`herdr --session
<name>`）を tmux の中で立て、その pane で `printf` と akapen を描いて
tmux の `capture-pane -e` で herdr の**外側への出力**を読んだ。`4:3` は
あり、`58;2;…` / `58:5:…` は無い（外側の `TERM` を `tmux-256color`・
`xterm-ghostty` のどちらにしても同じ）。同じ pane を
`herdr pane read --format ansi` で読むと `58;2;210;57;73` がある。
`--undercurl auto` は herdr に専用の枝を持たず、herdr を立てた端末の
`TERM_PROGRAM` で決まる（`src/undercurl.rs` のモジュールの文書）。
