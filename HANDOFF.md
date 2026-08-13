# HANDOFF: feat/timeline-order（history.rs 側）実装完了レポート

作業 worktree: `akapen-feat-timeline-order`（branch `feat/timeline-order`、契約コミット 77e2b07 を含む main から分岐）

## コミット構成（2 件）

1. `de95db2` — **スタブ**: src/snapshot.rs に `record_with_parent` / `open_with_parent` を追加（parent 引数は捨てて record / open に委譲）
2. 本コミット — **本実装**: src/history.rs（head_oid + 配置アルゴリズム + fixture テスト）+ この HANDOFF.md

push・マージはしていない。

## 実装内容

### 1. 観測時 HEAD oid の取得（src/history.rs）

- `fn head_oid(path: &Path) -> Option<String>` を追加。
  - 既存の `repo_root(path)` を再利用し、`git -C <root> rev-parse HEAD` の stdout を trim して返す。
  - さらに `git -C <root> ls-files --error-unmatch -- <rel>` で untracked を弾く（untracked は git の血統に載っておらず、parent を付けても orphan 扱いになるだけなので None が正直）。
  - 非 git・git エラー・untracked・空出力はすべて None に落とす（soft failure が既存の方針）。
- `load_cached` → `cache.record_with_parent(path, live, head_oid(path))`、`open_cached` → `cache.open_with_parent(path, live, head_oid(path))` に変更。

### 2. `load_with_local` の配置アルゴリズム

- `load_with_local(path, live, limit, local)` は `load_git_revisions(path, limit)` を取得した後、純粋関数 `assemble_timeline(live, gits, local)` に委譲する 2 段構成にした。
  - **理由**: fixture の gits は擬似 oid（`"A"` / `"B"` / `"R"`）で実 git では再現できないため、配置ロジックを git I/O から分離して fixture テストを実 git なしで成立させる。fixture テストは `load_with_local` の実体である `assemble_timeline` を直接呼ぶ（load_with_local を「通して」検証している）。
- アルゴリズム（契約＝正典どおり）:
  1. NOW を revisions[0] に置く（NEW は revisions[0] のまま）。
  2. gits（newest-first）から `content == live` を除去し、同じ content のコミットは newest だけ残す（重複除去は従来と同じ方針）。
  3. working = `[NOW] ++ gits`。
  4. スナップショットを **oldest-first** で処理:
     - content が live または working 内のいずれかと一致 → スキップ（内容一致の LOCAL は COMMIT に統合。そのコミットは自分の自然な位置に留まる）。
     - `parent` が gits の id（Git revision の id = フル oid）と一致 → **そのコミットの直前（newer 側）に挿入**。挿入位置は parent の gits 上の index + 1 の固定アンカーなので、同じ parent を共有する複数スナップショットは old-first 処理の結果 newest が前（NOW 寄り）で並ぶ。
     - parent が None / 不一致（非 git・rebase・別 ancestry の orphan）→ 後回しリストへ。
  5. orphan を NOW 直後（index 1）に観測順（newest が前）で挿入（従来の「LOCAL は Git より新しい側・観測順」の劣化動作を維持）。
- `reviewed_id` / `reviewed_content` の取り扱いは従来のまま。LOCAL revision の構築（`id: "local:<content id>"`、short_id 先頭 7 文字、summary "local snapshot"、RevisionSource::Local）も従来のまま。

### 3. テスト（src/history.rs の tests に追加）

- `timeline_order_fixtures_follow_the_parent_contract`: `testdata/timeline-order-fixtures.json` を serde_json で読み、6 シナリオすべてについて `revisions` の id 列（NOW は id None → `"now"`）が `expected_newest_first_ids` と一致することを assert。シナリオ数 == 6 も assert。
- `head_oid_needs_a_tracked_file_with_a_commit`: tempfile 上で非 git → None / コミット無し → None / untracked → None（コミット後も）/ tracked + コミット済み → `rev-parse HEAD` のフル oid（%H と一致）を検証。

## fixture テストの結果（6/6 green）

| scenario | 結果 |
| --- | --- |
| commit_interleaves_between_two_local_observations | green |
| uncommitted_chain_sharing_one_head_stays_in_observation_order | green |
| orphan_parents_fall_back_to_observation_order_above_git | green |
| no_git_observations_stay_in_observation_order | green |
| content_equal_local_merges_into_the_commit_at_its_natural_position | green |
| snapshot_equal_to_live_is_not_a_generation | green |

## cargo test / clippy の結果

- `cargo test --locked`: **354 passed**（ベースライン 352 + 新規 2）。既存の history tests（`loads_complete_markdown_snapshots_newest_first` / `content_equal_local_and_git_generations_are_shown_once_as_commit` / `loads_previous_observation_as_local_without_git` / `first_observation_remains_the_baseline_even_when_git_exists` / `timeline_clamps_at_both_ends` / `timeline_label_always_identifies_the_baseline_position` を含む）はすべて green のまま。意図が変わった既存テストはない。
- `cargo clippy --locked --all-targets`: **警告 0**（ベースライン 0 のまま）。
- 依存追加なし。実時間依存・固定 TMPDIR のテストなし（tempfile のみ使用）。

## スタブの場所と置き換え時の注意

- **場所**: src/snapshot.rs の `impl SnapshotCache` 内
  - `record_with_parent` … `record` の直後
  - `open_with_parent` … `open` の直後
  - どちらも parent 引数を捨てて `record` / `open` に委譲。スタブ単独コミットを clippy クリーンに保つため `#[allow(dead_code)]` 付き。
- **置き換え時**: feat/snapshot-parent の本実装とマージする際、この 2 メソッドを本実装で置き換えること（`#[allow(dead_code)]` も一緒に消える）。本実装は parent を SnapshotMeta / CachedSnapshot.parent に保存し、旧キャッシュは `#[serde(default)]` で None として読む想定（契約コミット 77e2b07 の設計どおり）。
- マージ後も fixture テスト 6 件は green のままであるべき。配置ロジックは `CachedSnapshot.parent` を直接読むので record 経路（スタブ/本実装の差）に依存しない。

## 従来挙動から変わった点

1. **内容一致 LOCAL の位置**: 従来は Git コミットと内容が一致する LOCAL を「最新の LOCAL 位置」に移動していた（＝古いコミットへ巻き戻した内容が「最新」に見える誤順序）。新実装では LOCAL を生成せず、そのコミットは自分の自然な位置に留まる。
2. **血統順の挿入**: parent が gits に一致する LOCAL は観測時 HEAD コミットの直前（newer 側）に挿入される。レビュー中にエージェントがコミットしても、LOCAL がコミットより新しい側へ押し出される「嘘の順序」が直る。
3. **orphan の劣化動作は維持**: 非 git・rebase・別 ancestry（parent が None / 不一致）の LOCAL は従来どおり NOW 直後・観測順（newest が前）に並ぶ。
4. 非 git 環境では parent = None なので実質従来と同じ並びになる。
5. gits の `content == live` のコミットは明示的に除去（従来は重複除去で暗黙的に除去していた）。

## 注意点

- この worktree 単体では snapshot.rs はスタブのため parent を保存しない（タイムラインの実挙動はまだ血統順にならない）。parent の保存は feat/snapshot-parent の本実装待ち。配置ロジックと fixture テストの検証は本 worktree 内で完結している。
- `assemble_timeline` は load_with_local の実体として抽出した純粋関数。呼び出し箇所は `load_with_local`（load / load_cached / open_cached 経由）のみ。
