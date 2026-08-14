# HANDOFF: `l` コメント一覧から過去リビジョンのコメントへ正しく遷移（feat/lkey）

## 問題

`l` で開くコメント一覧には、タイムライン（過去リビジョン）で付けたコメントも混在する。
従来は Enter で選択しても**リビジョンを復元せず**、現在のツリーの行番号へそのまま
ジャンプしていた。過去コメントの行番号は当時の文書に対するものなので、

- 選択範囲が現在の文書の無関係な行に落ちる
- カードは `visible_cards` が `c.revision == current_revision_context()` で
  フィルタするため表示されない（「コメントにきちんと遷移しない」）

## 実装内容

### src/overlay.rs — `activate_overlay_selection`（Comments 分岐）

Enter / ダブルクリックでコメントを選択したとき、ジャンプ前に**そのコメントが
書かれたリビジョンを復元**する:

- `revision: Some(rev)` のコメント → 対象ファイルの履歴から
  `Revision::context()` が一致する世代を探し `history.position` をそこへ移動。
  見つからなければ従来どおり現在の文書へプレーンジャンプ（fallback）。
- `revision: None`（NOW のコメント）→ 過去閲覧中に選択した場合は NOW
  （position 0）へ戻す。通常の NOW→NOW ジャンプは position が既に 0 なので無変化。
- 復元後、`history.position != rendered_position` のときだけ
  `render_pending_history(app, false)` を**同期実行**（`jump_review_mark` と同じ
  ガード）。オーバーレイは閉じた直後なので即座に描画され、その後に選択範囲を設定
  するため「描画が選択を消す」競合がない。共通パス（NOW で NOW コメント選択）は
  描画をスキップし、ランディングパルス（250ms フレームフラッシュ）も出さない。
- スクラブ中の保留描画（`history_render_due` が残っている）があっても、
  同期描画がフラグを消費するか、同一 content の early-return が選択を保持する
  ため、後から選択が消えることはない。

### src/history.rs — `anchored_line` の空 old ガード（付随修正）

`old` が空（空ファイルの起動直後に履歴をブラウズする等）だと
`old[0]` で index out of bounds パニックする潜在バグを発見。
`new.is_empty()` ガードと対称に `old.is_empty()` なら 0 を返すようにした。

## 追加テスト（5 件、全 green）

1. `comments_overlay_enter_restores_a_historical_revision` — 過去コメント選択で
   position が復元され、当該リビジョンが描画され、選択範囲・カード表示が正しい
2. `comments_overlay_enter_restores_a_revision_on_another_file` — 別ファイルの
   過去コメント選択で対象ファイルの履歴が復元される（複数ファイル）
3. `comments_overlay_enter_falls_back_when_the_revision_is_stale` — 履歴に
   存在しないリビジョン（pruned 等）はプレーンジャンプにフォールバック
4. `comments_overlay_enter_returns_to_now_for_a_live_comment` — 過去閲覧中に
   NOW コメントを選ぶと NOW へ戻ってジャンプ
5. `anchored_line_survives_an_empty_old_document` — 空 old でパニックしない

## 検証結果

- `cargo test --locked`: **382 passed; 0 failed**（ベースライン 377 + 追加 5）
- `cargo clippy --locked --all-targets`: **警告 0 件**
- 変更ファイル: `src/overlay.rs` / `src/history.rs` / `src/state_tests.rs` /
  `README.ja.md`（オーバーレイ共通操作の説明にリビジョン復元を追記）

## 動作確認の手順

1. git 履歴のある markdown で `akapen doc.md` を起動
2. `←` で過去へ遡り、その世代で `c` → コメント作成
3. `l` でコメント一覧 → 過去コメントにカーソルを合わせ Enter
4. タイムラインがそのリビジョンまで戻り、コメントの行が選択され、カードが表示される
