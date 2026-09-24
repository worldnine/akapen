# Semantic Reading Layer

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。
**文中に測定対象として出てくる `docs/gotchas.md` /
`examples/semantic/README.md` のバイト数・Unit 数は分割前の値です。**

---

### DIM 版（Reading Budget）は 2026-09-22 に削除しました

**この文書には削除前の記述が残っています。** Reading Tier の 4 段、
Reading Budget と `READ n%`、二段台帳、下限、冗長の負け、前提の閉包、
見出し復元に触れている項目は、**当時の記録として読んでください** — その
コードはもうありません。キャッシュ・鍵・分割の項目はそのまま生きています。

**2026-09-24 に境界と核のラウンドも外しました。** Unit は散文の Atom 1 つで、
判定器が聞くのはスコアの 1 ラウンドだけです（理由は
[`../design/semantic-reading-layer.md`](../design/semantic-reading-layer.md) の
「Semantic Unit」と判定器の冒頭）。**境界の規則・境界キャッシュ・核の question に
触れている項目も当時の記録です** — `boundary_rule` / `plan_boundaries` /
`list_marker_indent` / `core_questions` / 境界キャッシュと `flock` のコードは
もうありません。各項目の頭に「過去の話」と書いてあります。

削った理由は 1 つです。**System 1 らしさが無かった。** 判決（DIM）を
正しくするために、System 1 的な部品を何段も重ねて System 2 的な出力を
得ようとしていました（Tier → 冗長の Choice → 対の Noul → 前提の波 ×N）。
そのうえ判決そのものが据わらない — 何が重要かは読み手の目的で逆転するのに
（Pichert & Anderson 1977）、Reading Tier は 1 軸で読み手を知りません。
**逆転するものに判決を下すのが無理**でした。

いま残っているのは、**問いを当てて、答えている箇所を光らせる**という
1 段の機構だけです（[`../design/semantic-reading-layer.md`](../design/semantic-reading-layer.md)）。
コードの資産は惜しいが、機能としては削ってよい、が読み手の判断です。
戻したくなったら `git log` にあります。

---


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
  ここを「空なら光らせない」に変えると、核を選ばない判定器の答えが
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
`examples/semantic/measurements/core-selection.md`「MARKED を Unit の核だけに
絞る」と `examples/semantic/measurements/lists-and-run-cap.md`「run キャップ」。

### 箇条書きのラベルが MARKED になる — 擬似見出しを構文で捕まえる案は却下した

**現象**: `**担当**` のように単独行に立つ強調が、単独の Unit になって
ESSENTIAL 判定を受け、MARKED になります。「これさえ見ればオッケー」の位置に
ラベルだけが光るので、読み物として意味を成しません。実測では業務議事録の
宿題セクションで 2 件、業務 `CLAUDE.md` の節のラベルで 1 件出ました
（**本文はここに引用しません** — `docs/gotchas/public-repo.md`「業務文書の本文はこのリポジトリに
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
[`PROSE_KINDS`](../../examples/semantic/jev-annotate.py) の種別フィルタで核の
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
ベースの文面（`examples/semantic/measurements/core-selection.md`「核の問いを
損失ベースにする」）は、
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
（`docs/gotchas/open-questions.md` 5.1「揺れの床」）。このとき**ラン数が違うと比較が壊れます**
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
MARKED 比率を突き合わせたこと。数字は
`examples/semantic/measurements/lists-and-run-cap.md`「run キャップ」。

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
述べた一文**でした（本文は引用しません。`docs/gotchas/public-repo.md`「業務文書の本文はこのリポジトリに
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

**`Some([])` は NORMAL です。** 核を持たないことは「読まなくてよい」を
意味しません — 光らないだけです。

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

同じ理由で `docs/gotchas.md` とその分割先（この文書を含む）も測定対象に
しないほうが無難です。

**確認したこと**: `crates/semantic-reading/examples/decorate-report.rs` と
`atom-states.rs` がどちらも `std::fs::read_to_string(&doc)` してから
`atomize(&source)` していること（応答 JSON の中の位置を信じてはいない）。
5 文書の `source` を書き出して `cmp` すると、`demo.md` と
`design/semantic-reading-layer.md` は現物と一致し、`README.md` だけ不一致。

### `state` が大きい文書は境界を 1 つも聞けない — 比率より先に `jev.unsent` を見る

> **過去の話（境界の問いは 2026-09-24 に無くなった）。** 崖そのものは残って
> います — いまの question（文 1 つの Noul）も同じ 32k の予算に乗り、`state` が
> 32k 枠を使い切る文書では送れなかった文がスコアを持たずに光らなくなります。
> **比率を読む前に `jev.unsent` を見る**作法はそのままで、いまの形は
> `{"marks": ["marks:u12", …]}` です（全部送れなかったときは判定器が止まる）。
> 下の `CORE_QUESTION_MARGIN` はいま `PAIR_MARGIN` という名前です。

1 つの question の大きさを縛るのは `state + その question <= 32k`
（`STATE_PLUS_QUESTION_LIMIT`）で、`jev-annotate.py` が 1 question に割く予算は
**`32,768 − state − 2,048`** です。`state` が 30k を超えると、この予算は
**数百 tokens まで潰れます**。

境界 question は Atom を 2 つ引用するので、そこに真っ先に当たります。落とし先は
決めてあるので**失敗せず、黙って全境界が NEW になります**。

2026-09-21 の実測（`examples/semantic/README.md`、75.5 KB）:

| | `state` tokens | 1 question の予算 | 送れなかった境界 | 送れなかった Tier |
| --- | --- | --- | --- | --- |
| 現物 | 30,395 | **325** | **339 / 339** | 200 / 537 |
| 見出しを剥がした版 | 30,273 | **447** | 103 / 439 | 32 / 594 |

**`state` が 122 tokens 違うだけで、送れる境界が 0 件と 336 件に分かれます。**
崖であって坂ではないので、**近い 2 つの文書を比べているつもりで、実際には
予算の崖の左右を比べていることがあります**。

送れなかった Tier は `detail` + `core_atoms: []` になるので、**この帯に入った
文書は DETAIL がバイトの 96 % を占めます**。比率だけ見ると「判定が壊れた」に
見えますが、壊れているのは判定ではなく送信です。

**比率を読む前に `jev.unsent` を見てください。** `{"boundary": n, "tier": m}` が
入っています。

**確認したこと**: `examples/semantic/jev-annotate.py` の `RequestBudget.__init__`
が `self.pair = STATE_PLUS_QUESTION_LIMIT - CORE_QUESTION_MARGIN - state_tokens`
と書いていること（`whole` のほうは `REQUEST_LIMIT - REQUEST_MARGIN - state_tokens`。
**マージンは両方 2,048 なので、数字だけでは取り違えます**）。
`README.md` の見出しあり / なしを 4 ランずつ走らせ、8 ラン全部で上の件数が
再現したこと（`~/.local/share/akapen/evidence/runs/2026-09-21-headless/ans/`）。
`examples/semantic/measurements/request-splitting.md`「送れなかった question の
落とし先」は
**5 文書とも 0 件と書いていますが、それは `README.md` が 55.6 KB だったとき**
（run キャップのマージ `afd1614`）の話です。

### `boundary_rule` は段落の切れ目を見ていない — 空行を跨ぐ SAME は規則 2 だけが出す

> **過去の話（2026-09-24 に `boundary_rule` ごと消した）。** 段落の切れ目を
> 使う場所はいまありません（focus も 2026-09-24 に「光った文だけ明るく残す」に
> 決め直し、段落で沈めない範囲を取る案は要らなくなった。設計書の DIM の節）。

`plan_boundaries` に渡る情報には**空行が入っています**（`range` の隙間の `\n` の
数）。しかし `boundary_rule` はそれを 1 度も読みません。

その結果、**見出しを剥がした文書では「空行を跨いで SAME」が 1 件も出なくなります**。
2026-09-21 の実測で、段落の切れ目に立つ SAME の件数は規則 2
（`rule:current_is_heading`）の発火数と 4 文書とも**完全に一致**しました
（`demo` 7 / `design` 35 / `README.md` 56 / 実サンプル 2）。ほかに経路が無いためです。

**「段落は残っているから大丈夫」と考えないこと。** 段落は要求 JSON に残って
いますが、いまの実装はそれを使っていません。空行の合図は全部 Jev への question に
なります（実サンプル 3 本で、Jev に聞く境界の 49〜54 % が段落の切れ目）。

**確認したこと**: `examples/semantic/jev-annotate.py` の `boundary_rule` の引数が
`current_kind` / `next_kind` / `next_indent` の 3 つだけで、`source` も `range` も
受け取らないこと（`plan_boundaries` が `source` を使うのは規則 4 のインデントの
ためだけ）。10 条件 × 4 ランの `jev.boundaries` を集計した数字は
`examples/semantic/measurements/headless-documents.md`。

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
2026-09-24 に消した判定器の `list_marker_indent` がそうしていました。そこで
`plan_boundaries` が `source` を 1 度だけ `encode()` していたのも同じ理由で、
境界ごとに `encode()` すると文書の長さ × 境界数になります。

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
`examples/semantic/measurements/lists-and-run-cap.md`「箇条書きを項目ごとに
割る」にあります。

**次に触る人へ、測る前に知っておくこと。**

- **Unit 数で判定しないこと。** 決定事項が 8 Unit に割れれば最大 8 個 MARKED に
  なりえます。効くのは MARKED 比率です
- **Tier 一致率は使えません。** その床（`docs/gotchas/open-questions.md` 5.1）は「同じ分割
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
数字は `examples/semantic/measurements/lists-and-run-cap.md`「run キャップ」。

**確認したこと**: 5 文書 × 変更前後を 2〜4 回ずつ、合計 34 ラン。要求 JSON は
`dump-request`、比率は `decorate-report`、Atom ごとの状態は `atom-states`。
変更前のスクリプトを退避して同じ要求を食わせており、`atomize` は変えていないので
Atom の range は前後で同一です。内訳は各応答の `units[].jev.core_by`
（`rule:only_prose_atom` か否か）で数えました。表は
`examples/semantic/measurements/lists-and-run-cap.md`「箇条書きを項目ごとに
割る」。

---

### 中身が残っているのに見出しが沈む — 規則 2 の意図は境界だけでは守れない

**現象**: 業務議事録を READ 30 % で表示すると、節の中身は残っているのに
**その節が何なのかを言う見出しだけが DIM** になります。ラベル無しで中身が
並ぶので、何の一覧なのかが読めません。報告されたのは 2 件で、残っている中身は
17 atom 中 8 / 13 atom 中 2 でした。**同じ文書を既存の応答 6 ランで数え直すと
4〜5 件**になります（その応答は `PRESUPPOSES` より前のもので、残る集合が
違うため）。下の表はすべて後者の数え方です。

**原因**: 同じ日に入れた境界規則 4（箇条書きを項目ごとに割る）の退行です。
割ったことで見出しの Unit は「見出し ＋ せいぜい最初の項目」になり、
**2 つ目以降の項目が別 Unit として浮きました**。見出しの Unit は短いので
予算の取り合いでは有利ですが、**Tier は見出しと同居している内容を合わせて
判定される**ので、その内容が些末なら見出しごと沈みます。実測では、沈んだ
見出し Unit 317 件（4 文書 18 ラン × READ 1 / 5 / 30 %、延べ）の内訳が
DETAIL 159 / CONTEXT 71 / SUPPORTING 63 / **ESSENTIAL 24** で、
**ESSENTIAL でも二段目の取り合いに負けて沈みます**。

これは境界規則 2「見出しは直後の内容に付く」がまさに防いでいた状態です
（docstring に「見出しだけの Unit は単独では Tier を判定しづらい」とあり
ます）。**規則 4 でその意図が効かなくなりました。**

**直し方**: 新しい原理を足すのではなく、規則 2 の意図を「中身が複数 Unit に
なった場合」について言い直します。

> 節の中の Unit が 1 つでも残るなら、その節の見出しも残す。

判定器が各 Unit に `section_of`（属する節の見出し Unit）を書き、
`policy::decorate` が予算の取り合いのあとで見出しの行を戻します。構造だけで
決まるので **Jev には聞きません**（設計書「Jev に判断させないもの: syntax
parsing」）。**似た規則が 2 つあると読まないこと** — `boundary_rule` の規則 2
と `policy` の「見出しは中身に付いてくる」は 1 つのことです。

**低予算の見え方は変わります。** 入れ子の見出しは連鎖するので（`###` が
戻れば `##` が戻り、それが `#` を戻す）、READ 1 % で文書の背骨が立ちます。
前後はこうなります。

| 文書 | 節 | READ | 立つ見出し 前 → 後 | 見出しだけ沈む節 前 → 後 |
| --- | ---: | ---: | ---: | ---: |
| `demo.md` | 7 | 1 % | 1〜2 → 3 | 1〜2 → **0** |
| | | 5 % | 1〜2 → 3 | 1〜2 → **0** |
| `docs/design/semantic-reading-layer.md` | 36 | 1 % | 5〜8 → 16〜18 | 9〜11 → **0** |
| | | 5 % | 5〜8 → 16〜18 | 9〜11 → **0** |
| `examples/semantic/README.md` | 25 | 1 % | 2 → 6〜8 | 4〜5 → **0** |
| | | 5 % | 3 → 9〜11 | 5〜8 → **0** |
| 業務議事録 22.7 KB | 13 | 1 % | 3〜5 → 7〜9 | 4 → **0** |
| | | 5 % | 3〜5 → 7〜9 | 4 → **0** |

`README.md` の READ 1 % の内訳は `##×1 ###×1` → `#×1 ##×2 ###×3` で、
**3 階層が下から順に戻っています**。議事録では表題の `#` が、前は READ 1 %
で 1 度も立たず、後では 6 ラン中 6 本とも立ちます。

**背骨が立つのは正しい見え方だと判断しました**が、低予算の絵が変わったことは
事実なので数字を残します。おかしいと思ったらここから戻ってください。表と
測り方は
[`examples/semantic/measurements/section-heads.md`](../../examples/semantic/measurements/section-heads.md)。

**知っておくべき 4 点。**

- **戻すのは Atom で、Unit ではありません。** 規則 2 のせいで見出しの Unit に
  は実質的な第 1 文が同居していることがあり、Unit ごと戻すとそれも戻ります。
  `policy::decorate` の「DIM は一律」に対する**唯一の例外**で、戻るのは
  見出しの行そのものです
- **戻した見出しは NORMAL で、MARKED にはしません。** 予算の取り合いに負けた
  見出しが光ると、上の「箇条書きのラベルが MARKED になる」と同じ絵になります
- **予算には数えません。** 実測で Atom バイトの 0.68〜3.26 % です
  （`PRESUPPOSES` の閉包は 2.5〜10.3 % で、あちらは数えています）。数えると
  「見出しが戻ったせいで本文が沈む」が起き、規則 2 の意図の逆になります
- **逆向きは直しません。** 節が部分木ごと沈むときは見出しも沈んだままで、
  実測 4 文書 18 ラン × 4 予算のすべてで取りこぼしは 0 件でした

**確認したこと**: `section_of` は構文だけで決まるので既存の応答に後から
書き足せます。「前」＝もとの応答、「後」＝同じ応答 ＋ `section_of` で、Jev の
判定は前後で同一です（規則 4 のときと同じ対照の取り方）。基準は run キャップ
版の 18 ラン、表示状態は `atom-states`。生ログは証拠ディレクトリの
`runs/2026-09-22-heading-follows-content/`。業務 `CLAUDE.md` 45.6 KB は
凍結した本文が残っておらず Atom を作り直せないので測っていません。

---

### 設計書の凍結コピーは、ラウンド 2 の天井に近い場所で凍っている

`crates/semantic-reading/tests/fixtures/design-doc-frozen-2026-09-22.md` は
2 つのテストが読む fixture です（`tests/atomize.rs` と
`examples/semantic/test_jev_annotate.py::test_small_documents_still_go_in_one_request`）。
後者は「ラウンド 1〜3 が 1 チャンクに収まる」を要求し、**その余地は実測で
残り 2,282 tokens**（`whole` 58,161 − question 55,879）です。
**書き足すなら測り直してください** —— ラウンド 2 の question はほぼ Unit ごと
なので、**Atom が 1 つ増えると約 300 tokens 増えます**（実測 55,879 ÷ 188）。

**2026-09-22 まで、この fixture は設計書の正典そのものでした。** そのため
`docs/design/semantic-reading-layer.md` へ節を 1 つ足しただけで（+1.1 KB）
ラウンド 2 が 2 チャンクに割れ、Rust を 1 行も触っていなくても
`python3 -m pytest examples/semantic` が赤になりました。**テストが設計書の
大きさを決めていて**、残りは 30〜45 バイトでした —— 同じ日に入った規則
「核は奪わない」が正典に載せられず `policy.rs` の docstring にしか無い、
という状態がそれで起きていました。いまは凍結コピーへ切り離したので
**正典の側に上限はなく**、その規則も正典に載っています
（`docs/design/semantic-reading-layer.md`「Reading Policy」）。

**確認したこと**: 切り離し前の正典で `dry_run` の `rounds[1]` を読み、
残りが 21 tokens だったこと。コピーは注記を足し「Role」節と「最初のデモ」節
（計 579 バイト）を落として 2,282 tokens にしたこと。正典に「核は奪わない」
（4 行）と「遅延」節（6 行）を足してもゲートが 4 本とも緑であること。

---

### Choice に「該当なし」を置いても、Jev は「無い」と言わない — redundancy を Choice 単独にすると冗長が 0〜1 件から数十件になる

`REDUNDANT_WITH` の相手を Jev に選ばせるために、redundancy を「先行 Unit の
本文＋**該当なし**」の Choice に替えました（2026-09-22。`jev-annotate.py` の
`redundancy_questions` / `REDUNDANCY_NONE`）。相手の正否は直ります — 業務議事録
で 7 件中 3 件誤っていた相手が、Choice では文書末尾の決定事項リストが本文節を
指す形で正しく出ます。

**しかし「該当なし」は閾値の代わりになりませんでした。** 5 文書 × 4 ランで、
冗長は demo 0 → 2〜3、記事 0 → 6〜8、設計書 0〜1 → 24〜30、議事録 4〜5 →
22〜24 件。設計書と記事で増えたぶんは中身を見るとほぼ「同じ話題」で、言い直し
ではありません。`context-preservation.md` 第 2 版が「**Choice は『無い』と言え
ない**」と書いたのと同じ性質で、選択肢に「無い」を文字で置いても変わりません —
選択肢が多いほど「該当なし」は個々の候補との一騎打ちになり、どれか 1 つの
「関連する箇所」に負けます（偽陽性の `p_none` は 0.2〜0.45、真の言い直しは
0.01〜0.15）。

**閾値を消したいなら primitive を替えるだけでは足りず、「無い」を Noul に
聞かせる段が要ります。** context preservation が段階 1 の Noul を残している
のはそのためです。**そうしました**（第 2 版。ラウンド 4 の
`redundancy_gate_questions` / `REDUNDANCY_YES`）。選ばれた対に「前を読んだ人に
とって後は新しい情報を加えないか」を 1 つ聞き、0.5 以上のときだけ
`REDUNDANT_WITH` を付けます。

**だから「閾値を無くした」とは書けません。** 0.7（`demo.md` の 2 点から取った
暫定値）が 0.5（Noul の「はい」の自然な境目）に**置き換わった**だけです。
定数は 1 つになり、値の出どころは恣意的でなくなりましたが、**0.5 の真上に
乗る対は残っています** — `docs/design/jev.md`「`confidence` の閾値ガードは
不採用」と同じ危うさです。次に触る人へ: **ここを「閾値が無い」と読まないで
ください。**

**確認したこと**: `~/.local/share/akapen/evidence/runs/2026-09-22-redundancy-choice/`
の `ans/*.after.*.json`（Choice 単独）と `ans/*.gate.*.json`（第 2 版）で
`jev.redundancy_choice` / `redundancy_none_probability` /
`redundancy_pair_noul` を数えたこと。`docs/design/jev.md`「redundancy の
question は方向を持つ」。数字は
`examples/semantic/measurements/redundancy.md`。

### 判定器のプロンプトだけ変えると、古いキャッシュが当たる

2026-09-22 から、`--semantic-cmd` の答えは
`~/.cache/akapen/semantic/` に残ります（`src/semantic_cache.rs`）。
**キーは文書の `sha256` と、`--semantic-cmd` に渡した文字列の `sha256`**
です。文書が変われば自動で外れますが、**同じコマンド行のまま
`jev-annotate.py` の `TIER_CRITERIA` や question の文面を書き換えると、
古い答えが当たります。** 「変えたのに結果が変わらない」は、まずこれを
疑ってください。

```sh
akapen --semantic-cache-clear   # 全部消す。逃げ道はこれ 1 つ
```

**判定器に「自分の識別子」を応答に載せさせる案は採りませんでした。**
識別子は走らせないと分からないので、**引き当ての判断に使えません**
（照合できるのは払った後だけです）。走らせる前に聞く probe 往復も却下
しました — アダプタを更新していない人の環境では、probe が
**全文解析（議事録で約 5 円）で返ってきます**。コマンド行に含まれるパスの
mtime を見る案も、シェル文字列の中からパスを推測することになるので
採りませんでした。

**測るときは踏みません。** 実測は `dump-request` でアダプタを直接叩くので
（`examples/semantic/README.md`）、TUI のキャッシュを通りません。刺さるのは
**akapen で開いて確かめている**ときだけです。

**marks モードの「問い」だけは踏みません**（2026-09-22）。
問いつきの項目は `<sha(source)>.q<sha(問いの文面)>.json` と
いう名前なので、**定型の文面を直せば自動で外れます**
（`assets/marks-questions.json`、`SemanticCache::entry_path`）。
外れないのは判定器の中の文面（いまは枠の `MARKS_FRAME` だけ。2026-09-24 までは
核の Choice も）の方で、そちらはどちらのモードでも上の逃げ道が要ります。

**判定器の作りを変えたときも同じです**（2026-09-24、文ごとへの切り替え）。
既に開いたことのある (文書, 問い) は、`--semantic-cache-clear` を打つまで
**古い Unit（Jev が束ねた段落ほど）と古い核のまま**光ります。つまみの既定
（15 %）は akapen 側なのですぐ効くので、古い項目に新しい既定が掛かり、光る本数が
前より少なく見えます。

**確認したこと**: `src/semantic_cache.rs` の `analyzer_dir`（キーが
`sha256(コマンド行)` のディレクトリであること）と、テスト
`a_different_analyser_does_not_hit_the_cache` /
`the_same_document_is_analysed_once` /
`editing_a_preset_misses_the_cache_on_its_own`。逃げ道は `src/config.rs` の
`Action::ClearSemanticCache`。

### `atomize` を変えたら境界キャッシュ — あちらは Atom の**添字**で持っている

> **過去の話（2026-09-24 に判定器の境界キャッシュを消した）。** 判定器は
> もう `~/.cache/akapen/semantic/boundaries/` を読みも書きもしません。古い
> 項目が残っていても害は無く、`akapen --semantic-cache-clear` で一緒に消えます。
> 後半の「akapen 側の項目は `atomize` の変更で壊れないが当たり続ける」は
> いまも生きています。

2026-09-22、表を行ごとの Atom へ割るときに気づきました。キャッシュは 2 か所に
あり、**`atomize` の変更に強いのは片方だけ**です。

| | 中身 | `atomize` が変わると |
| --- | --- | --- |
| akapen 側（`~/.cache/akapen/semantic/…json`） | `SemanticDocument`（**byte range** と Atom 列を持つ） | 自己完結しているので**影響なし**。`src/semantic_cache.rs` の冒頭がその理由を書いています |
| 判定器側（`~/.cache/akapen/semantic/boundaries/v1/…json`） | 境界の判定を **Atom の添字の列**で持つ | 添字が全部ずれる。**黙って当たると節の切れ目がずれます** |

ずれた境界は「それらしく見えて間違った注釈」です。注釈が付かないのとは違って、
**間違っていることが画面から分かりません**。

読み込みは元々「件数が合わなければ外れ」でしたが、**件数は合うことがあります**。
表の行割りでも、データ行が 1 行の表は 1 Atom → 1 Atom のままです。そこで
`(kind, range)` の列の sha256 を項目に書き、合わなければ外すようにしました
（`atoms_fingerprint` / `load_boundaries`）。指紋を持たない古い項目も外れます。

**版のディレクトリ（`v1`）は上げていません。** 上げる方式にすると、次に
`atomize` を触る人が上げ忘れた日に同じ事故が戻ります。指紋なら誰も憶えて
いなくても自動で外れます。

**手で消すなら 2 か所とも消えます** — `akapen --semantic-cache-clear` は
`~/.cache/akapen/semantic/` を丸ごと消すので、`boundaries/` も一緒に消えます。

**そして、消すまでは新しい割り方が画面に出ません。** akapen 側の項目は
`atomize` の変更で**壊れません**が、当たり続けます。鍵は（コマンド行 × 文書 ×
問い）で、`atomize` はどこにも入っていないからです。中の `SemanticDocument` は
**古い割り方の Atom 列をそのまま持っている**ので、`get` はそれを返します。
表の行割りで言えば、既に開いたことのある文書は**変更後も表が光りません**。
`akapen --semantic-cache-clear` を 1 回打つまでです。

**これは「壊れている」とは別の話です。** 古い項目は自己完結していて位置も
正しく、出てくるのは**変更前の正しい表示**です。上の「判定器のプロンプトだけ
変えると、古いキャッシュが当たる」と同じ形で、逃げ道も同じ 1 つです。

**確認したこと**: `examples/semantic/jev-annotate.py` の `atoms_fingerprint` と、
テスト `test_a_different_atom_split_misses_the_boundary_cache` /
`test_a_boundary_cache_entry_without_a_fingerprint_misses`。

### Noul の主張は `instructions` に置く — `claim` だと HTTP 400 で落ちる

2026-09-22、marks モードの実装で踏みました。Jev の Noul question は
**主張を `instructions` に置きます**（`PROBE_QUESTION` /
`REDUNDANCY_PAIR` / `CONTEXT_STAGE1` がどれもそう書いています）。
`{"type": "noul", "claim": …}` のように別の名前で送ると、

```text
Jev returned HTTP 400: {"detail":"Noul question must have criteria or
instructions: marks:u1"}
```

で**ラウンドごと落ちます**。Choice の `criteria` と対になる名前として
`claim` を選びたくなりますが、ありません。

**気づきにくいのは、落ちるのが 2 ラウンド目だから**です（当時は境界のラウンドが
先にありました。2026-09-24 からはスコアのラウンドしか無いので、いまは 1 本目で
落ちます）。境界（Choice）は通るので、進捗行は「48 questions in 1.40s」と出てから 400 になります。
**新しい question の型を足したら、まず 1 問だけ投げて形を確かめること。**

**確認したこと**: `examples/semantic/jev-annotate.py` の `marks_questions`
と、テスト `test_the_question_text_reaches_jev_verbatim`。
