# Semantic Reading Layer

`docs/gotchas.md` から分けた 1 章です。この文書の約束と全項目の索引は
[`../gotchas.md`](../gotchas.md)。
**文中に測定対象として出てくる `docs/gotchas.md` /
`examples/semantic/README.md` のバイト数・Unit 数は分割前の値です。**

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

### `core_atoms` は DIM に効く — **2026-09-22 に変わりました**

> **この項目は逆になりました。** 台帳が一段だった頃は「核は DIM に効かない」で、
> 下にその根拠（Budget 4 点で DIM 集合が完全一致）を残してあります。
> **台帳を二段にしたので、いまは効きます。**

`policy::decorate` の**一段目**は、核を持つ Unit（ESSENTIAL かつ非 REDUNDANT
かつ `core_atoms != Some([])`）を **Budget を見ずに**残します。だから
`core_atoms` を `Some([])` にするか否かで、**その Unit が沈むかどうかが
変わります**。

- `None` / `Some([i])` … 一段目。**どの Budget でも沈まない**
- `Some([])` … 二段目。Tier と Budget の取り合いに出る

実測では `design`（173 Unit / 核 38）で、二段にしたとき低い予算で落ちた Unit が
延べ 122 個あり、**その全部が「ESSENTIAL だが `Some([])`」**でした
（`examples/semantic/measurements/context-preservation.md` 第 4 版）。
run キャップが核の選に漏れさせた Unit です。

**切り分け方**: 沈み方が変わったとき、まず**核の 3 値が動いていないか**を見ます。
`Some([])` が増減していれば一段目の顔ぶれが変わっているので、それは Tier の
揺れではありません。3 値が同じなら、従来どおりラウンド 2 の Tier の揺れを疑い
ます（比べたい 2 条件のラウンド 2 の question が同一かを先に見ること）。

**`keep_order` と `cost` は相変わらず核を読みません。** 核が効くのは
「一段目に入るか」だけで、**並び順と値段には効きません**。

**確認したこと**: `decorate` の一段目が `bears_a_core`（`reading_tier` /
`is_redundant` / `has_core`）を読むこと。表示状態の割り当ても同じ関数を読むので、
「一段目で確保したのに MARKED にならない」はずれません。落ちた 122 個の内訳は
`~/.local/share/akapen/evidence/runs/2026-09-22-two-tier-ledger/xcheck.json`。

<details>
<summary>台帳が一段だった頃の記述（2026-09-21 まで）</summary>

`policy::keep_order` と `cost` は核を読みません。**どの Unit が沈むかは Tier と
Budget だけで決まります。** 核が決めるのは「残った Unit の中で MARKED か NORMAL か」
だけです。

だから核の選び方を変えたときに「沈む項目が変わった」と見えたら、それは
**ラウンド 2 の Tier の揺れ**であって核の変更の効果ではありません。実測でも、
同じ応答から `core_atoms` だけを抜いて通すと DIM の集合は Budget
100 / 60 / 40 / 20 % のどれでも完全に一致しました（業務議事録と業務 `CLAUDE.md`）。

</details>

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
[`list_marker_indent`](../../examples/semantic/jev-annotate.py) がそうしています。
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
