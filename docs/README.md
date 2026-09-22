# akapen のドキュメント索引

どれを読めばよいかを、**あなたが何をしたいか**で選んでください。

| 読む人 | 文書 | 何が書いてあるか | 現行か |
|---|---|---|---|
| 使う人 | [`../README.md`](../README.md) / [`../README.ja.md`](../README.ja.md) | akapen とは何か、インストール、全キーバインド、フラグ | 現行 |
| 使う人 | [`quickstart.md`](quickstart.md) | 3 つの入口から見た実際のレビューの流れ（日本語） | 現行 |
| 全体像を読む人 | [`internals.md`](internals.md) | いまの実装が守っている設計上の不変条件と、その理由（日本語） | 現行 |
| 触る人 | [`gotchas.md`](gotchas.md) | **知らずに触ると静かに壊れるところ。** 約束と全項目の索引。本体は [`gotchas/`](gotchas/) の下（下表） | 現行・キュレート |
| 測る人 | [`../examples/semantic/README.md`](../examples/semantic/README.md) | Semantic Reading Layer のデモとアダプタの使い方。実測の記録は [`../examples/semantic/measurements/`](../examples/semantic/measurements/) | 現行 |
| 決める人 | [`design/`](design/) | 長寿命の仕様・設計書（下表） | 現行の設計意図 |
| 歴史を掘る人 | [`handoff-archive.md`](handoff-archive.md) | かつての `HANDOFF.md`。**歴史的資料で、個々の記述が現在も真である保証はありません** | 歴史 |

`demo.gif` は README が参照しているデモ動画です。

## `docs/gotchas/` — 地雷の本体

[`gotchas.md`](gotchas.md) は約束と索引だけを持ちます。**1 ファイルに戻さないで
ください** — 戻すと akapen 自身の Semantic Reading Layer が解析できる大きさを
超えます。節を名指しして参照するときは、下のファイル名で書いてください。

| 文書 | 何が書いてあるか |
|---|---|
| [`gotchas/rendering.md`](gotchas/rendering.md) | 描画と attribution |
| [`gotchas/semantic-reading.md`](gotchas/semantic-reading.md) | Semantic Reading Layer（いちばん大きい） |
| [`gotchas/terminal-keys.md`](gotchas/terminal-keys.md) | 端末とキー入力（kitty protocol と押しっぱなし） |
| [`gotchas/public-repo.md`](gotchas/public-repo.md) | 公開リポジトリとしての約束 |
| [`gotchas/external-processes.md`](gotchas/external-processes.md) | 外部プロセス |
| [`gotchas/vendoring-ci.md`](gotchas/vendoring-ci.md) | ベンダリングと CI |
| [`gotchas/open-questions.md`](gotchas/open-questions.md) | 未解決（地雷ではなく設計判断が要るもの） |

## `docs/design/` — 決める人のための設計書

実装より長生きする文書です。「いまどう動いているか」ではなく
**「何をどう決めたか / これからどうするか」**が書いてあります。

| 文書 | 対象 | 状態 |
|---|---|---|
| [`design/semantic-reading-layer.md`](design/semantic-reading-layer.md) | Semantic Reading Layer（Atom / Unit / 問いとスコア / 核 / つまみ）の設計。**この層の正典** | 実装中（MVP のうち incremental reanalysis は未実装。cache は 2026-09-22 に実装 — [`gotchas/open-questions.md`](gotchas/open-questions.md) 参照）。**DIM 版（Reading Tier / Reading Budget）は 2026-09-22 に削除**。**テストの fixture ではなくなった**（読む先は `crates/semantic-reading/tests/fixtures/design-doc-frozen-2026-09-22.md`）ので、大きさの上限は無い |
| [`design/range-attribution-plan.md`](design/range-attribution-plan.md) | 内部位置モデルを source byte range にする計画。**Phase 番号はこの文書が正典** | Phase 1・2 実装済み、Phase 3 以降は未着手 |
| [`design/jev.md`](design/jev.md) | 意味判断を担う判定器 Jev（**LLM ではなく System One モデル**）の primitive と呼び出し方 | 参照資料。akapen からの接続はまだ（`--semantic-cmd` の口まで） |
| [`design/reading-research.md`](design/reading-research.md) | **拾い読みの研究の保管庫。** 合図（signaling）・satisficing・核性・位置の効き方と、先行システム Scim。**採らなかった手とその理由**（TextTiling / 文書レベルの LEAD） | 参照資料。実装の指示ではない |
| [`design/marks-only-and-review-mode.md`](design/marks-only-and-review-mode.md) | DIM を廃止してマーカーだけにする層（0 節）、読み手の判断が要る箇所を光らせる赤入れモード（2 節）、自然語で聞く意味の検索（3 節）、**直すためのマーク = slop の除去（4 節、`R` の校正候補）**。2026-09-22 の費用と 2026-09-23 の候補の実測つき | **0 節（marks）と 4 節の段階 1（Review = 校正候補、`R`）は実装済み**（機構の正典は設計書の側。ここに残るのは実測と判断）。4 節の段階 2（LLM への送信と書き換え）と 1〜3 節は候補。DIM 版は 2026-09-22 に削除 |
| [`design/local-snapshot-spec.md`](design/local-snapshot-spec.md) | LOCAL スナップショットと統合タイムラインの仕様 | 現行仕様（v0.1 の基本機能は実装済み） |
| [`design/git-integration-spec.md`](design/git-integration-spec.md) | 旧 Git 連携仕様 | **akapen に関する記述は `local-snapshot-spec.md` で置換済み**（ashiato 側の 4 章が残っている） |
| [`design/review-badge-design.md`](design/review-badge-design.md) | レビュー未確認件数バッジ（`! N`）のデザインと、別案へ戻すときの切り替え箇所 | 実装済み（文書の自己申告は 2026-08） |

## どこに何を書くか（書く人向け）

- **経緯と判断の理由** → コミットメッセージ。このリポジトリのコミットは
  詳しく、`git log` が最良の判断ログです。文書に書き写さないでください
- **実測の記録** → [`../examples/semantic/measurements/`](../examples/semantic/measurements/)。
  主題ごとに 1 ファイル。索引は
  [`../examples/semantic/README.md`](../examples/semantic/README.md)「実測の索引」
- **次に触る人を刺すもの** → [`gotchas/`](gotchas/) の該当する章。追記ではなく
  **キュレート**します（解消したら消す）。「いつ書いたか」ではなく
  **「何を確認して真だと判断したか」**を添えてください
- **決めたこと・これから決めること** → [`design/`](design/)
- **いま守っている不変条件** → [`internals.md`](internals.md)
- [`handoff-archive.md`](handoff-archive.md) には**追記しないでください。**
  追記専用ファイルは腐ります
