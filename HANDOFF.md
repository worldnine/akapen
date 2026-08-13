# HANDOFF: snapshot.rs 側 parent 保存・復元 API（feat/snapshot-parent）

## 実装内容（src/snapshot.rs のみ）

契約どおりの公開 API を追加した。代入の意味論はタスク仕様に厳密に一致。

- `SnapshotCache::record_with_parent(&self, path, content, parent: Option<String>) -> Result<CachedFile>`
  - 既存 `record` の本体を移設。新規スナップショットの `parent` に引数の値を記録する。
  - 再観測（既知 content を最新位置へ移動する既存ロジック）では `captured_ms = now_ms()` と並べて `snapshot.parent = parent` を実行。観測のたびに「その観測時点の HEAD」で上書きされ、古い parent の持ち越しによる血統順の嘘を防ぐ。
- `SnapshotCache::open_with_parent(&self, path, content, parent: Option<String>) -> Result<CachedFile>`
  - 既存 `open` の本体を移設し、内部の record 呼び出しを `record_with_parent(path, content, parent)` に変更しただけ。pin 解放処理は既存のまま。
- `record` / `open` は互換ラッパとして残し、それぞれ `record_with_parent(..., None)` / `open_with_parent(..., None)` に委譲。既存呼び出し元（`history.rs` の load_cached/open_cached、`acknowledge` 内部の record 等）は無変更。
- `pin` / `set_baseline` / `acknowledge` / FORMAT_VERSION / materialize は無変更（契約コミット 77e2b07 のフィールド・転記をそのまま使用）。
- 旧キャッシュ（parent フィールドが無い index JSON）は `#[serde(default)]` により None として読める。FORMAT_VERSION は 1 のまま。

## 追加テスト（src/snapshot.rs tests モジュール、6 件）

1. `record_with_parent_stores_the_parent_anchor_across_reload` — parent 付き record → load（ディスク往復）で parent が復元される
2. `reobserving_known_content_updates_the_parent_anchor` — 再観測で parent が新しい値に更新される。None で再観測すれば古い parent はクリアされる
3. `open_with_parent_records_the_parent_and_releases_pins` — open が観測として parent を記録し、pin 解放処理も従来どおり動く
4. `pin_never_sets_a_parent_anchor` — pin による新規追加は parent None、既存スナップショットへの pin は parent を変えない
5. `old_index_without_parent_field_loads_as_none` — parent フィールドが無い旧形式 index JSON を手書きし、None として読める
6. `record_and_open_wrappers_observe_without_a_parent` — 互換ラッパが None を渡す

既存 352 テストは無変更のまま green（計 358 passed）。

## 検証結果

- `cargo test --locked`: **358 passed; 0 failed**（ベースライン 352 + 追加 6）
- `cargo clippy --locked --all-targets`: **警告 0 件**（ベースラインと同値）
- 変更ファイル: `src/snapshot.rs` のみ（`src/history.rs` は未変更）

## 旧キャッシュ互換の確認方法

1. `cargo test --locked old_index_without_parent_field_loads_as_none` — parent が無い旧形式 index JSON が None で読めることを直接検証
2. 実運用の確認: 本変更前のバイナリが作った index（`{version, path, reviewed, snapshots[{id, captured_ms, pinned}]}`）をそのまま置いた状態で新バイナリを起動しても、`#[serde(default)]` により parent=None で読まれ、エラーにならない
3. 前方互換: FORMAT_VERSION は 1 のままなので、新 index（parent 付き）を旧バイナリが読んでも serde_json の未知フィールド無視により壊れない

## 統合側（timeline-order worktree）への注意点

- **呼び出し差し替え**: `history.rs` の `load_cached` / `open_cached` 内の `cache.record(...)` / `cache.open(...)` を `record_with_parent(...)` / `open_with_parent(...)` に差し替え、観測時点の HEAD oid を渡すこと。`record` / `open` は None を渡す互換ラッパなので、差し替えない限り parent は常に None（現行と同じ並び順のまま）。
- **oid の表現を揃える**: `load_git_revisions` の `Revision.id` は full oid（`git log --format=%H`）。parent と突き合わせるなら parent も full oid（例: `git rev-parse HEAD`）で渡すこと。fixture の "A"/"B" は読みやすさのための短縮表記で、実データでは full oid が前提。
- **差分なしの連続観測では parent は更新されない**: 同一 content が既に最新位置にある場合、`record_with_parent` は既存ロジックどおり何もしない（`captured_ms` と同じ抑制）。parent が更新されるのは「新規スナップショット生成」または「既知 content が最新位置へ移動する再観測」のときだけ。HEAD が動いただけ・content 不変の場合は parent は最後に観測された値のまま残る（契約仕様の「再観測（既知 content を最新位置へ移動する既存ロジック）」の範囲）。
- **acknowledge は parent に関与しない**: `acknowledge` は内部で `record`（= None）を呼ぶため、既知 content を最新位置へ移動するケースではそのスナップショットの parent が None に上書きされ得る。タスク指示（変更不要）どおりの挙動。統合側で「acknowledge 後に parent を保証したい」要件が出た場合は別途議論が必要。
- **parent None のフォールバック**: 非 git 環境・不明・pin 由来・旧キャッシュ由来のスナップショットは parent None。並べ替えロジックは fixture の `orphan_parents_fall_back_to_observation_order_above_git` / `no_git_observations_stay_in_observation_order` にあるように、parent が解決できない場合は観測順で Git より上に置くフォールバックを期待している。
- **fixture の検証**: `testdata/timeline-order-fixtures.json` の `snapshots_oldest_first[].parent` は本 API が保存する値と対応。統合テストではこの fixture を CachedSnapshot に変換して `load_with_local` に食わせる形が想定されている。
