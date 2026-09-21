# akapen のドキュメント索引

どれを読めばよいかを、**あなたが何をしたいか**で選んでください。

| 読む人 | 文書 | 何が書いてあるか | 現行か |
|---|---|---|---|
| 使う人 | [`../README.md`](../README.md) / [`../README.ja.md`](../README.ja.md) | akapen とは何か、インストール、全キーバインド、フラグ | 現行 |
| 使う人 | [`quickstart.md`](quickstart.md) | 3 つの入口から見た実際のレビューの流れ（日本語） | 現行 |
| 全体像を読む人 | [`internals.md`](internals.md) | いまの実装が守っている設計上の不変条件と、その理由（日本語） | 現行 |
| 触る人 | [`gotchas.md`](gotchas.md) | **知らずに触ると静かに壊れるところ。** 各項目に「何を確認して真だと判断したか」付き | 現行・キュレート |
| 決める人 | [`design/`](design/) | 長寿命の仕様・設計書（下表） | 現行の設計意図 |
| 歴史を掘る人 | [`handoff-archive.md`](handoff-archive.md) | かつての `HANDOFF.md`。**歴史的資料で、個々の記述が現在も真である保証はありません** | 歴史 |

`demo.gif` は README が参照しているデモ動画です。

## `docs/design/` — 決める人のための設計書

実装より長生きする文書です。「いまどう動いているか」ではなく
**「何をどう決めたか / これからどうするか」**が書いてあります。

| 文書 | 対象 | 状態 |
|---|---|---|
| [`design/semantic-reading-layer.md`](design/semantic-reading-layer.md) | Semantic Reading Layer（Atom / Unit / ReadingTier / Reading Budget）の設計 | 実装中（MVP のうち cache と incremental reanalysis は未実装 — [`gotchas.md`](gotchas.md) 参照） |
| [`design/range-attribution-plan.md`](design/range-attribution-plan.md) | 内部位置モデルを source byte range にする計画。**Phase 番号はこの文書が正典** | Phase 1・2 実装済み、Phase 3 以降は未着手 |
| [`design/jev.md`](design/jev.md) | 意味判断を担う判定器 Jev（**LLM ではなく System One モデル**）の primitive と呼び出し方 | 参照資料。akapen からの接続はまだ（`--semantic-cmd` の口まで） |
| [`design/reading-research.md`](design/reading-research.md) | **拾い読みの研究の保管庫。** 合図（signaling）・satisficing・核性・位置の効き方と、先行システム Scim。**採らなかった手とその理由**（TextTiling / 文書レベルの LEAD） | 参照資料。実装の指示ではない |
| [`design/local-snapshot-spec.md`](design/local-snapshot-spec.md) | LOCAL スナップショットと統合タイムラインの仕様 | 現行仕様（v0.1 の基本機能は実装済み） |
| [`design/git-integration-spec.md`](design/git-integration-spec.md) | 旧 Git 連携仕様 | **akapen に関する記述は `local-snapshot-spec.md` で置換済み**（ashiato 側の 4 章が残っている） |
| [`design/review-badge-design.md`](design/review-badge-design.md) | レビュー未確認件数バッジ（`! N`）のデザインと、別案へ戻すときの切り替え箇所 | 実装済み（文書の自己申告は 2026-08） |

## どこに何を書くか（書く人向け）

- **経緯と判断の理由** → コミットメッセージ。このリポジトリのコミットは
  詳しく、`git log` が最良の判断ログです。文書に書き写さないでください
- **次に触る人を刺すもの** → [`gotchas.md`](gotchas.md)。追記ではなく
  **キュレート**します（解消したら消す）。「いつ書いたか」ではなく
  **「何を確認して真だと判断したか」**を添えてください
- **決めたこと・これから決めること** → [`design/`](design/)
- **いま守っている不変条件** → [`internals.md`](internals.md)
- [`handoff-archive.md`](handoff-archive.md) には**追記しないでください。**
  追記専用ファイルは腐ります
