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
- **1 ファイルには収めません。** 章ごとに [`gotchas/`](gotchas/) の下へ分けて
  あります。この文書は**約束と索引**で、地雷の本体はリンク先にあります。
  分けた理由は大きさです — 1 ファイルのままだと akapen 自身の Semantic
  Reading Layer が解析できる大きさを超えます

---

## 地雷の一覧

節を名指しして参照するときは、**リンク先のファイル名**で書いてください
（`docs/gotchas.md`「◯◯」ではなく `docs/gotchas/semantic-reading.md`「◯◯」）。

### [描画と attribution](gotchas/rendering.md)

- `ViewState` に行を差し込むときは `row_attrs` も対で差し込む
- 折返しの hanging pad にも装飾の背景が乗る
- タブを含む行は、fragment の途中で終わる装飾が効かない
- `Dim` は前景色を書く — 帯の下では装飾する前に落とす
- `decorate_row` のコストは O(可視 span 数 × 装飾数) / フレーム
- 上位集合 attribution の range の「広さ」は当たり判定そのもの
- exact を増やすときは `line_of` が変わらないことを確認する
- MARKED の天井の理由が変わった — 帯との混同ではなく、字が読めるか
- テーマの highlight scope 尊重は、一度やって落とした
- `scope_style` はアルファを捨てる — DarkNeon の引用とインラインコードが読めない

### [Semantic Reading Layer](gotchas/semantic-reading.md)

- `FixtureProvider` は渡された source を見ない → `source_sha256` の照合が要る
- タイムマシンで過去 revision を見ると fixture が拒否され、BEL が毎回鳴る
- 非同期で解析させるなら、答えには世代番号を載せる
- 「Unit の判断を Atom へ投影する」は一律コピーではない（MARKED だけ選択的）
- 箇条書きのラベルが MARKED になる — 擬似見出しを構文で捕まえる案は却下した
- 揺れの幅どうしを比べるときは、ラン数を揃える
- 退行の判定 — 基準の版と同じラン数で、幅が重なるかを見る
- `core_atoms` は 3 値 — `[]` を「無い」に丸めると選に漏れた Unit が丸ごと光る
- `core_atoms` は DIM に**効く**（2026-09-22 に逆転）— 核を持つ Unit は一段目で沈まない
- `examples/semantic/README.md` は測定対象の文書でもある — 測りながら書くと分母が動く
- `state` が大きい文書は境界を 1 つも聞けない — 比率より先に `jev.unsent` を見る
- `boundary_rule` は段落の切れ目を見ていない — 空行を跨ぐ SAME は規則 2 だけが出す
- 要求 JSON の `range` はバイト位置 — Python の文字列添字で読むと全件ずれる
- `cargo clippy --workspace --all-targets -- -D warnings` は緑になったことがない
- 箇条書きを項目ごとに割ると MARKED が増える — 規則 4 の変更は条件を満たさなかった
- 中身が残っているのに見出しが沈む — 規則 2 の意図は境界だけでは守れない
- `docs/design/semantic-reading-layer.md` も test の fixture — 書き足すと 1 リクエストから溢れる
- Budget の下限は「数字の約束」であって「集合の約束」ではない — `decorate(floor)` と `decorate(floor - 1)` は同じとは限らない

### [公開リポジトリとしての約束](gotchas/public-repo.md)

- 業務文書の本文はこのリポジトリに書かない

### [外部プロセス](gotchas/external-processes.md)

- TUI が生きているあいだの子プロセスは `export::run_child` を通す

### [ベンダリングと CI](gotchas/vendoring-ci.md)

- 上流のバージョンとフォーク自身のバージョンは別物
- シェルスクリプトの日本語メッセージ内の変数展開は `${VAR}` で括る

### [未解決（地雷ではなく、設計判断が要るもの）](gotchas/open-questions.md)

- 1. `cache` と `incremental reanalysis` は、実測して作らないと決めた
- 2. Phase 番号が 2 つの意味で使われている（アーカイブ側）
- 3. `--semantic-cmd` が `confidence` / `probabilities` を運ばない
- 4. 同一 Tier 内の rule の逐次性 — `context preservation`（**解決済み。2026-09-21**）
- 5. Jev の context window は **2 つ**の制約で縛られている
  - 5.1 Tier 一致率の「揺れの床」を先に測ること
  - 5.2 4 文書 × 方式の実測（2026-09-21）
  - 5.3 分割後の天井は `state` が 32k に収まるか（2026-09-21）
