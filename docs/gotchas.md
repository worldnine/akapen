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

### 「Unit の判断を Atom へ投影する」は一律コピーではない（MARKED だけ選択的）

設計書は

> Semantic Unit に付与した意味情報を、その Unit を構成する Atom へ投影する

としか書いていません。**「投影 = Unit の Tier を構成 Atom 全部へ配る」は
実装側の解釈**であって設計書の要求ではなく、いまはそこを MARKED についてだけ
狭めています（設計書は変更していません）。

```text
Unit が kept ∧ ESSENTIAL ∧ ¬REDUNDANT
    core_atoms に挙がった Atom -> MARKED
    同じ Unit の残り           -> NORMAL
Unit が kept でその他          -> 全 Atom NORMAL
Unit が落ちた                  -> 全 Atom DIM（**一律**）
```

刺さるのは次の 2 点です。

- **`core_atoms` が「無い」は「核が無い」ではなく「絞り込みを受けていない」**で、
  Unit 全体が MARKED になります。これが既存 fixture と古い判定器の経路です。
  ここを「空なら光らせない」に変えると、`demo.json` も `annotate-doc.py` も
  黙って何も光らなくなります。**空の配列 `[]` は別の意味**（核を持たない =
  MARKED にならない）で、下の「`core_atoms` は 3 値」を見てください
- **DIM を同じように選択的にしないでください。** Unit が落ちたなら丸ごと
  沈むのが正しく、混ぜると「なぜこの行の一部だけ沈むのか」を読者に説明
  できなくなります

**確認したこと**: `crates/semantic-reading/src/policy.rs::decorate` の
`marks` の条件と `unit.is_core(atom)`、テスト
`only_the_core_of_an_essential_unit_is_marked` /
`a_dropped_unit_dims_whole_even_when_it_has_a_core` /
`a_unit_without_a_core_still_marks_all_of_its_atoms` /
`an_essential_unit_with_an_empty_core_is_normal_not_marked`。核を選ぶ question は
`examples/semantic/jev-annotate.py` の `core_questions`（Unit ごと）と
`plan_run_cores`（リスト 1 本ごと）で、どちらもラウンド 3。比率の実測は
`examples/semantic/README.md`「MARKED を Unit の核だけに絞る」と
「run キャップ」。

### 箇条書きのラベルが MARKED になる — 擬似見出しを構文で捕まえる案は却下した

**現象**: `**担当**` のように単独行に立つ強調が、単独の Unit になって
ESSENTIAL 判定を受け、MARKED になります。「これさえ見ればオッケー」の位置に
ラベルだけが光るので、読み物として意味を成しません。実測では業務議事録の
宿題セクションで 2 件、業務 `CLAUDE.md` の節のラベルで 1 件出ました
（**本文はここに引用しません** — 下の「業務文書の本文はこのリポジトリに
書かない」）。

**原因**: pulldown-cmark は `#` の無い行を `Paragraph(Strong(Text))` としか
報告しないので、`atomize` は `Sentence` に分類します。Markdown の慣用としては
見出しですが、**構文上は段落**です。パーサは正しく、`atomize` も正しい。

**試して却下した案**: 「段落の inline 内容がちょうど 1 つの `Strong` /
`Emphasis` span なら `AtomKind::Heading` に倒す」。実装は 20 行ほどで済み、
既存の境界規則（規則 2「heading → SAME」、ラウンド 3 の種別フィルタ）が
そのまま連鎖するので、追加のルールは要りません。

**採らなかった理由は、利得と損失が釣り合わないからです。** 5 文書で測ると
正例 3 件・誤検出 1 件でした。

| | 内容 |
| --- | --- |
| 正例 3 件 | 箇条書きのラベル 3 件 |
| 誤検出 1 件 | `CLAUDE.md` 冒頭の太字リード段落 — **その文書でいちばん重要な一文** |

正例 3 件のうち、**修正前に実際に MARKED になっていたのは 2 件**です
（担当者ラベルの片方は、修正前も光っていませんでした）。

誤検出の側が重い。`Heading` になると
[`PROSE_KINDS`](../examples/semantic/jev-annotate.py) の種別フィルタで核の
候補から外れるので、**文書の要点そのものが MARKED になれなくなります**。
さらに規則 1（次が heading → NEW）でその直前の本物の見出しが単独 Unit に
なり、見出しの行が丸ごと光りました（`CLAUDE.md` で 2/2）。ラベル 3 件を
消す代わりに要点 1 件を失う取引です。

**使えない手がかり。** 誤検出だけを外そうとして次を検討しましたが、どれも
成立しません。**同じ道を戻ってこないでください。**

- **空行** — Markdown では見出しの後に空行が入るのが普通です。実測した該当
  2 件とも `\n\n` を挟んでいました。逆向きも同じで、
  `design/semantic-reading-layer.md` の sentence → list_item の境界 5 件は
  **5 件とも空行があり、しかも 5 件とも導入文**でした
- **句点の有無** — `**注意。ここは変更しないこと。**` のような擬似見出しは
  普通にあります。句点では区別できません
- **末尾の文字全般** — `atomgrain` の頃に「読点で終わる Atom を後続へ付ける」
  を実測して失敗しています（33 件中、Jev も SAME と答えたのは **0 件**）
- **長さの閾値** — **短くて本質的な一文**を silent に落とします。実測の誤検出
  もこの形でした

**同じ形の取りこぼしが他にもあります。** `対象:` / `対象外:` のようなコロン
終わりの導入文や、`方針は次のとおり。` のように後続を
指すだけの文も、直後のリストから切り離されて単独 Unit になります。これらは
そもそも強調が無いので、上の案では最初から捕まりません。

**この現象は未解決のまま残っています。** 同じ日に入れたラウンド 3 の損失
ベースの文面（`examples/semantic/README.md`「核の問いを損失ベースにする」）は、
**これを直しません** — 実測でも、文面だけを変えた条件でラベルは 2/2 で
MARKED のままでした。理由は経路が違うからです。ラベルは**それ自体が 1 Atom
だけの Unit** になるので、核の question（Atom が 2 つ以上ある Unit にしか
聞かない）が最初から適用されません。損失ベースの文面が効くのは
**複数 Atom の Unit の中で核を選ぶとき**で、そこではラベルや導入文ではなく
中身が選ばれるようになります。

直すなら、`atomize` の種別ではなく **1 Atom の Unit をどう扱うか**の側を
見ることになります。ここは測っていません。

**確認したこと**: 5 文書（`demo.md` / `design/semantic-reading-layer.md` /
`examples/semantic/README.md` / 業務議事録 22.7 KB / 業務 `CLAUDE.md`
45.6 KB）に対して `atomize` の前後で Atom 列を突き合わせ、種別が変わった
4 件を 1 件ずつ目で確かめました。表示は `decorate-report` /
`atom-states`（`policy::decorate` そのもの）を各条件 2 回。**空行・末尾の
文字・長さの閾値の 3 点は引き継いだ記録**で、ここで取り直してはいません。

---

### 揺れの幅どうしを比べるときは、ラン数を揃える

Jev の答えはランごとに揺れるので、判定品質は 1 点ではなく**幅**で見ます
（`docs/gotchas.md` 5.1「揺れの床」）。このとき**ラン数が違うと比較が壊れます**
— ランを増やせば観測される最大値は上がるので、4 ランの床と 7 ランの変種を
並べると、**変種の側だけが超過して見えます**。

実際に踏みました。床 4 ラン対 run キャップ 6〜7 ランで読んだときは
`design`・業務議事録・業務 `CLAUDE.md` の 3 文書が「床を 0.4〜1.2 pt 超過」に
見えました。床を同じラン数まで足すと、超過が残ったのは `CLAUDE.md` だけです。

| 文書 | 床（4 ラン時） | 床（ラン数を揃えた後） | 変種 | 判定の変化 |
| --- | --- | --- | --- | --- |
| `design/…` | 22.9〜26.0 % | 22.9〜26.0 %（6 ラン） | 23.0〜26.9 % | 超過 → **重なる** |
| この `README.md` | 2.9 %（2 ラン） | **1.2〜2.9 %**（4 ラン） | 1.5〜2.6 % | 超過 → **床の中** |
| 業務議事録 | 9.6〜10.2 % | 9.6〜10.4 %（6 ラン） | 10.0〜11.4 % | 超過 → **重なる** |
| 業務 `CLAUDE.md` | 3.3〜3.7 % | 3.3〜3.7 %（7 ラン） | 4.1〜4.5 % | 超過 → **超過のまま** |

`README.md` がいちばん露骨で、**2 ランの床は 1 点（2.9 %）にしか見えません**。
3・4 ラン目が 1.2 % を出して初めて 1.7 pt 幅が見えました。

**2 ランの床を床と呼ばないこと。** 幅で判定するなら、床も変種も同じラン数で
測ってから並べてください。

**確認したこと**: 5 文書について変更前と run キャップを同じラン数
（4 / 6 / 4 / 6 / 7）で測り直し、`decorate-report` の Budget 100 % の
MARKED 比率を突き合わせたこと。数字は `examples/semantic/README.md`
「run キャップ」。

### 退行の判定 — 基準の版と同じラン数で、幅が重なるかを見る

**MARKED 比率の単独の値を合否に使わないこと。** 2026-09-21 の run キャップの
採用にあたって、判定の形をこう決めました。

- **基準の版は、run キャップのマージコミットである。**
- **退行の判定は、疑う版と基準の版を同じラン数で走らせ、幅が重なるかで行う。**
  観測の最大値は分布の上限ではない（上の「ラン数を揃える」）。重なれば退行では
  ない。重ならなければ退行である
- **1 本のリストから MARKED が 2 つ以上出たら、それだけで退行。** 比率と違って
  数え上げなので揺れません。採用時の実測は全ランで 0 件

**比率は合否ではなく警報として使います。** 業務 `CLAUDE.md` で 4.1〜4.5 %
（採用時の 7 ラン）を超えたら「退行した」ではなく「**同じラン数で基準を測り直す
合図**」です。この位置に置くと「まだ小さいから良い」という理屈の入り口が
なくなります — その理屈は毎回 +1 pt を通してしまいます。

**採用の判断そのものも、比率の小ささを理由にしていません。** 業務 `CLAUDE.md`
は床 3.3〜3.7 % に対し 4.1〜4.5 % で、**合格条件を満たさないまま採っています**。
理由は増えた分の中身で、増えた 2 件はどちらも**その文書の設計上の制約そのものを
述べた一文**でした（本文は引用しません。下の「業務文書の本文はこのリポジトリに
書かない」）。これは次に判断するときも同じように確かめられます。経緯はマージ
コミットに。

### `core_atoms` は 3 値 — `[]` を「無い」に丸めると選に漏れた Unit が丸ごと光る

`SemanticUnit::core_atoms` は `Option<Vec<AtomIndex>>` で、**「無い」と「空」は
別の意味**です。

| wire 形 | 内部 | 意味 |
| --- | --- | --- |
| フィールドが無い | `None` | **絞り込みを受けていない** → Unit 全体が MARKED |
| `"core_atoms":[]` | `Some([])` | **核を持たない** → この Unit は MARKED にならない |
| `"core_atoms":[i]` | `Some([i])` | `i` だけが MARKED |

`None` は後方互換のための既定で、このフィールドを知らない判定器・fixture を
従来どおり動かします。`Some([])` は判定器が「この Unit に核は要らない」と
**積極的に決めた**場合のためにあり、リスト 1 本につき核を 1 つに絞るときに
選に漏れた Unit がこれを受け取ります。

**刺さるのは丸めたときです。** `[]` を `None` と同一視すると「絞り込み無し」に
なり、選に漏れた Unit が**丸ごと光ります** — 絞ったつもりが元より悪くなる、
という向きに倒れます。だから `skip_serializing_if` は `Vec::is_empty` ではなく
`Option::is_none` です。

**`Some([])` は NORMAL であって DIM ではありません。** 沈めるかどうかは Tier と
Budget が決めることで、核の選に漏れたことは「読まなくてよい」を意味しません。

**版は上げません。** `[]` を知らない古い akapen はそれを空の Vec と読んで
「絞り込み無し」に倒すので、表示が従来どおりに戻るだけで、位置を取り違える
ことはありません。

**確認したこと**: `crates/semantic-reading/src/unit.rs` の `core_atoms` /
`is_core` / `set_core` と、テスト `an_unrefined_unit_treats_every_atom_as_its_core` /
`an_empty_core_means_the_unit_has_no_core_at_all` /
`an_empty_core_survives_the_wire_round_trip`。表示側は
`crates/semantic-reading/src/policy.rs::an_essential_unit_with_an_empty_core_is_normal_not_marked`。
Atom を隣の Unit に取られて核が全部落ちた場合に `Some([])` へ倒す（`None` へ
戻さない）ことは `protocol.rs::overlapping_atoms_go_to_the_first_unit_that_claims_them`。

### `core_atoms` は DIM に効かない — 「沈み方が変わった」は核のせいではない

`policy::keep_order` と `cost` は核を読みません。**どの Unit が沈むかは Tier と
Budget だけで決まります。** 核が決めるのは「残った Unit の中で MARKED か NORMAL か」
だけです。

だから核の選び方を変えたときに「沈む項目が変わった」と見えたら、それは
**ラウンド 2 の Tier の揺れ**であって核の変更の効果ではありません。実測でも、
同じ応答から `core_atoms` だけを抜いて通すと DIM の集合は Budget
100 / 60 / 40 / 20 % のどれでも完全に一致しました（業務議事録と業務 `CLAUDE.md`）。

**切り分け方**: 比べたい 2 条件のラウンド 2 の question が同一かを先に見ること。
Unit の割り方が同じなら Tier の question は同一なので、沈み方の差は全部揺れです。

**確認したこと**: `crates/semantic-reading/src/policy.rs::decorate` の `kept` の
計算が `unit.is_core` を呼ばないこと（呼ぶのは表示状態の割り当てだけ）。
上の DIM 集合の一致は `atom-states` を 2 条件 × 4 Budget で突き合わせて確認。

### `examples/semantic/README.md` は測定対象の文書でもある — 測りながら書くと分母が動く

実測の 5 文書のうち 1 つが **この README 自身**です。`decorate-report` /
`atom-states` は渡されたファイルを**読み直して `atomize` する**ので、測ってから
結果を README に書き足すと、次に測ったときの Atom 列が変わります。

実際に踏みました。run キャップの節を書いた後に測り直すと、`README.md` の
**「変更前」の値が 2.9 % から 2.0 % に変わりました** — 判定器の出力は 1 バイトも
変わっていないのに、です。分母（Atom のバイト長の合計）が動いたためです。

**要求 JSON に `source` が丸ごと入っている**ので、そこから書き出したものを
`decorate-report` に渡してください。

```sh
jq -r .source request.json > frozen.md
cargo run -p semantic-reading --example decorate-report -- frozen.md answer.json
```

同じ理由で `docs/gotchas.md`（この文書）も測定対象にしないほうが無難です。

**確認したこと**: `crates/semantic-reading/examples/decorate-report.rs` と
`atom-states.rs` がどちらも `std::fs::read_to_string(&doc)` してから
`atomize(&source)` していること（応答 JSON の中の位置を信じてはいない）。
5 文書の `source` を書き出して `cmp` すると、`demo.md` と
`design/semantic-reading-layer.md` は現物と一致し、`README.md` だけ不一致。

### 要求 JSON の `range` はバイト位置 — Python の文字列添字で読むと全件ずれる

`AnalyzeRequest` の `range` は **source のバイト位置**です。Python の `str` の
添字は符号位置なので、日本語を含む文書では `source[start:end]` が `text` と
**1 件も一致しません**（実測: 45.6 KB の業務 `CLAUDE.md` で **372/372 件**が
不一致）。

刺さり方が意地悪なのは、**例外が出ないこと**です。返ってくるのは「もっともらしい
別の場所の文字列」で、しかも ASCII だけの文書では正しく動きます。規則 4 の
ネスト判定を最初に書いたときは、行頭を取りに行ったつもりで**別の項目の本文**を
読んでいて、それでも 5 文書すべてで数字が出ました（子項目が 0 件、継続文が
311/314 件という、いま思えばあり得ない内訳でしたが）。

`source.encode()` でバイト列にしてから `rfind(b"\n", 0, start)` してください。
[`list_marker_indent`](../examples/semantic/jev-annotate.py) がそうしています。
`plan_boundaries` が `source` を受け取ってその場で 1 度だけ `encode()` するのも
同じ理由で、境界ごとに `encode()` すると文書の長さ × 境界数になります。

**`text` は使ってよい。** アダプタが source を自分で切り出さずに済むように
`RequestAtom` が `text` を載せているので（`crates/semantic-reading/src/protocol.rs`）、
本文が欲しいだけなら `range` に触る必要はありません。`range` が要るのは
**Atom の外側**（行頭からマーカーまでのインデントなど）を見るときだけです。

**確認したこと**: `crates/semantic-reading/src/protocol.rs` の `RequestAtom` が
`atom.range` をそのまま載せ、`text` を `source.get(atom.range)` で切っていること
（Rust の `str` の添字はバイト）。5 文書の要求 JSON について
`source.encode()[start:end].decode() == text` が全件成立し、
`source[start:end] == text` は日本語を含む 4 文書で全件不成立であること。
テストは `examples/semantic/test_jev_annotate.py` の `ListMarkerIndentTest`
（`atoms_from` が実際の Markdown からバイト位置で Atom を作る）。

### `cargo clippy --workspace --all-targets -- -D warnings` は緑になったことがない

CI は clippy を走らせていません（`.github/workflows/ci.yml` は `cargo build` /
`cargo test --workspace` / `check-vendor-diff.sh` の 3 つだけ）。手で clippy を
当てるときは**呼び方で結果が変わる**ので、「clippy が赤い」を回帰と読み違え
ないでください。

| 呼び方 | 結果 |
| --- | --- |
| `--workspace --all-targets -- -D warnings` | **赤**（14 件） |
| `--workspace -- -D warnings`（`--all-targets` なし） | 緑 |
| `--workspace --all-targets`（`-D warnings` なし） | 緑（warning 14 件） |
| `-p akapen -p semantic-reading --all-targets -- -D warnings` | 緑 |

**14 件はすべて `third_party/tui-markdown` のテストコード**（`#[cfg(test)]`）で、
akapen 本体と `semantic-reading` は 0 件です。内訳は
`unused import: super::*` が 10 件と `single_range_in_vec_init` が 4 件。

**ツールチェーンで件数が変わります。** 同じツリーに当てた実測:

| toolchain | 該当 warning |
| --- | ---: |
| clippy 0.1.90 (2025-09-14) | 4 |
| clippy 0.1.92 (2025-12-08) | 4 |
| clippy 0.1.98 (2026-09-01) | **14** |

`unused import: super::*` の 10 件は新しい rustc で増えたぶんで、
`single_range_in_vec_init` の 4 件は**1.90 の時点から赤**です。つまり
`--workspace --all-targets -- -D warnings` はこのツリーで緑だったことがなく、
「main は clippy 0」という記録は**上の表の下 3 行のいずれかの呼び方**を指します。

**確認したこと**: `git diff main...HEAD -- '*.rs' '*.toml' 'Cargo.lock'` が空
（ブランチ `rule4-list-boundary` の Rust は main と同一）。上の 4 通りの呼び方と
3 つの toolchain を同じシェルで実行。`cargo clippy --message-format short` の
出力がすべて `third_party/tui-markdown/src/renderer/*.rs` を指すこと。

### 箇条書きを項目ごとに割ると MARKED が増える — 規則 4 の変更は条件を満たさなかった

**現象**: 境界の構造ルール 4 を「`list_item` どうしは SAME」から「別項目どうしは
NEW / 同じ項目の中は SAME」に変えると、**MARKED 比率が揺れの床を超えて増えます**。
5 文書のうち 3 文書で増え、業務 `CLAUDE.md` は 3.3〜3.7 % → 10.8〜11.2 % と
**約 3 倍**になりました。

**原因は経路がはっきりしています。** MARKED は「ESSENTIAL かつ非 REDUNDANT な
Unit」ごとに 1 つの核 Atom として付きます（`jev-annotate.py` の `wants_core`）。
Unit が割れれば ESSENTIAL な Unit が増え、**MARKED はそれに比例して増えます**。
箇条書きを細かく割る変更は、必ずこの経路を踏みます。

**それでも要望そのものは叶っています。** 業務議事録の `## 決定事項`（8 項目）は、
変更前は丸ごと 1 Unit で 8 項目が一斉に DIM になりました。変更後は 1・4・7・8 が
沈み、2・3・5・6 が残ります — 「しょうもない決定事項が個別に沈む」は実際に
起きています。数字と定性の評価が**正面から衝突している**のがこの変更の性質で、
どちらかが間違っているのではありません。比較の表は
`examples/semantic/README.md`「箇条書きを項目ごとに割る」にあります。

**次に触る人へ、測る前に知っておくこと。**

- **Unit 数で判定しないこと。** 決定事項が 8 Unit に割れれば最大 8 個 MARKED に
  なりえます。効くのは MARKED 比率です
- **Tier 一致率は使えません。** その床（`docs/gotchas.md` 5.1）は「同じ分割
  どうし」で測った値で、**分割そのものを変える変更には当たりません**
- **天井は超えませんでした。** 事前の見積もりは 1 Unit あたり 262 tokens
  （Tier 181 + redundancy 81）で計算していましたが、redundancy は既にラウンド 3 へ
  移っていて SUPPORTING 以上にしか聞かないので、**ラウンド 2 の固定費は 181 だけ**
  です。`CLAUDE.md` は 62 % → 89 % で収まりました。ただし `README.md` の 90 % と
  並んで余裕はありません（規則 4 と無関係の既存の崖。未解決 5）
- **子項目を割らないこと。** 親項目＋その詳細の束で 1 つの「決定事項」なので、
  そこで割ると「半分だけ DIM のリスト」が親子のあいだで起きます
  （`policy::decorate` の docstring）。実測でも子を割らない判断が `CLAUDE.md` で
  63 Unit を節約しています
- **`is_marker_only` の親は取りこぼします。** 子だけを持つ親項目（`-` と改行だけの
  行）は `atomize` が Atom にしないので、その子は直前の何かに付きます。
  実文書では踏んでいませんが、直すなら `atomize` 側の話になります

**増えたぶんは「1 Atom の Unit」だけに寄ってはいません。** MARKED になる Unit
（ESSENTIAL かつ非 REDUNDANT）の内訳を数えると、業務 `CLAUDE.md` では
11〜12 → 25〜26 に増えるうち、**Jev が核を選んだ Unit が 7〜8 → 16〜17**、
`assign_lone_cores` が聞かずに決めた Unit が 4 → 9 で、**どちらも同じ比率で
増えています**。つまり「1 Atom の Unit の扱いを直せば MARKED の増加だけ消せる」
という逃げ道は**ありません** — 増加は分割そのものから来ています。上の
「箇条書きのラベルが MARKED になる」とは別の経路です。

**効いたのは run キャップです（後日実装）。** 沈む側（Tier）と光る側（核）は
別のメカニズムなので、Tier を項目ごとのままにして**核だけをリスト 1 本につき
1 つ**に畳めます（`jev-annotate.py` の `unit_runs` / `plan_run_cores`）。
`CLAUDE.md` の MARKED は 10.8〜11.2 % → **4.1〜4.5 %** に戻り、1 本のリストから
MARKED が 2 つ以上出るケースは全ランで 0 件になりました。

**それでも床（3.3〜3.7 %）の中には戻りません**（7 ランずつ測って一度も
重なりません。ほかの 4 文書は重なるので、落ちるのは `CLAUDE.md` だけです）。
残差は 2 つに分かれ、**run キャップで消えるのは片方だけ**です:

- **消える**: 1 本のリストの中で複数の項目が光る分
- **消えない**: 丸ごと 1 Unit なら ESSENTIAL にならなかったリストが、割ると
  中に ESSENTIAL な項目を持つ分。`CLAUDE.md` の `list_item` の MARKED は
  3〜4 → 6 で、ここが残差の正体です。**1 本につき 1 つまでという上限を
  守っている以上、原理的に消せません**

次に縮めるなら、見るのは核ではなく**リスト全体の Tier の付け方**になります。
数字は `examples/semantic/README.md`「run キャップ」。

**確認したこと**: 5 文書 × 変更前後を 2〜4 回ずつ、合計 34 ラン。要求 JSON は
`dump-request`、比率は `decorate-report`、Atom ごとの状態は `atom-states`。
変更前のスクリプトを退避して同じ要求を食わせており、`atomize` は変えていないので
Atom の range は前後で同一です。内訳は各応答の `units[].jev.core_by`
（`rule:only_prose_atom` か否か）で数えました。表は
`examples/semantic/README.md`「箇条書きを項目ごとに割る」。

---

## 公開リポジトリとしての約束

### 業務文書の本文はこのリポジトリに書かない

**このリポジトリは public です。** Semantic Reading Layer の判定品質は実文書で
しか測れないので、手元の業務文書（議事録・案件の `CLAUDE.md` など）を測定対象に
使っています。**その本文をここに書いてはいけません。**

書いてよいもの / いけないもの:

| | 例 |
| --- | --- |
| 書いてよい | 件数、比率、Unit 数、tokens、ラン数、構造（indent と kind の並び）、`業務議事録` `業務 CLAUDE.md` のような**総称**、「設計上の制約を述べた一文」のような**種類** |
| 書いてはいけない | 本文の引用、取引先名・個人名・製品名、ファイル名やパス、日付と固有名詞の組、そこからたどれる固有の事実 |

**本文が判断材料として要るときは、リポジトリの外に置いてください。**
`~/.local/share/akapen/evidence/` を使います（`.gitignore` ではなくリポジトリの
外。中に入れると「無視されているだけ」の状態になり、いつか誰かが `git add -f`
します）。リポジトリ側には**件数と、証拠がそこにあること**だけを書きます。

**基準のランの応答 JSON も同じです。** `state` に文書の全文が入るので、
リポジトリには絶対に置けません。

**踏んだ実績があります。** 2026-09-21、規則 4 と run キャップの測定で、取引先名・
個人名・案件の決定事項が `examples/semantic/README.md` と `docs/gotchas.md` に
入りました。push 前に気づいて、未 push の 50 コミットをツリーとコミットメッセージ
ごと書き換えて消しています。**push 済みだったら取り返しがつきませんでした。**

**確認のしかた**: 固有名詞の一覧はリポジトリに置けないので（それ自体が漏洩に
なる）、`~/.local/share/akapen/evidence/fingerprints.txt` に置いてあります。
実文書で測ったあと、その一覧で作業ツリーと未 push のコミットを走査してください。

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

### 5. Jev の context window は **2 つ**の制約で縛られている

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
日本語なら 85 KB 前後と外挿できますが、そこは**測っていません**。

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
