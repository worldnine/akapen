# HANDOFF: `e` → $EDITOR 復帰後にターミナルを再初期化する（マウスでゴミが出るバグ）

## 問題

akapen で `e` を押して `$EDITOR`（micro 等）を開き、エディタを終了して戻ると
ターミナルが壊れた状態になる。マウスを動かすと SGR マウスシーケンスの生バイト
（`^[[<35;10;8M` 等）が画面に文字として流れ、キー入力も効かなくなる。
ashiato（prefix+m）→ akapen → micro → Ctrl+q の実機フローで確認。

## 原因

`open_editor` はエディタ起動前に `DisableMouseCapture` + `ratatui::restore()` で
TUI を完全にサスペンドする（raw mode OFF・メイン画面へ）。ところが復帰後の処理は
コメント上は「no-blink init でターミナルを作り直す」となっているのに、実際には
`NoBlinkBackend::init()` を呼んでおらず、`EnableMouseCapture, Hide` の送出だけだった。
結果、復帰後の akapen は **raw mode OFF（canonical + echo）のままマウスキャプチャだけ ON**
という不正状態になり:

- マウス移動のたびに tty が SGR シーケンスの生バイトを echo して画面にゴミが出る
- キー入力は行バッファリングされて akapen に届かず、操作不能になる

pty ドライバでの最小再現で確定（修正前は `GARBAGE_ECHOED: True`）。

## 実装内容（src/reload.rs — `open_editor`）

- エディタ終了後、コメントの主張どおり `crate::NoBlinkBackend::init()` を呼んで
  raw mode + alternate screen を再進入し、`*terminal` を新しい Terminal に差し替える。
- `EnableMouseCapture` は init の**後**に送る（run() の起動順と同一。canonical 状態で
  capture を有効化しない）。
- init 失敗時は `flash_err` で通知し続行（ベストエフォート）。
- TestBackend 経路では検出できないクラスのバグなので、リグレッションテストは無し
 （pty 実機確認を検証手順として下記に記録）。

## 検証

- pty 再現スクリプト（python3 + pty.fork, EDITOR=micro, `e` → Ctrl+q → SGR マウス注入）:
  修正前 = ゴミエコーあり / 修正後 = ゴミ無し・`j` `q` とも正常動作・終了時に
  DisableMouseCapture + LeaveAlternateScreen が正しく出る。
- herdr 実ペインでも同フローを再現し、復帰後のマウス移動で `[<35` のゴミが
  スクロールバックに出ないこと（0 件）を `herdr pane read` で確認。
- `cargo test --locked`: 422 passed; 0 failed。`cargo clippy --locked --all-targets`: 警告 0。
- 変更ファイル: `src/reload.rs` / `HANDOFF.md`

---

# HANDOFF: composer のカーソルをブロックキャレット化し、幅文字の残像を抹消（2件）

## 問題1: カーソルが「半角スペース」のように見える

コメント入力中、カーソルを戻していくと半角スペースのようなものが前に現れずれる。
実機のレンダリング（`テストコ▏メント。`）で確定: カーソルグリフ `▏`（U+258F LEFT
ONE EIGHTH BLOCK）はセル左端 1/8 幅の極細縦線で、多くの端末フォントではほぼ透明に
しか描かれない → 「挿入点に半角スペースが入った」ように読める（コードの幅計算・
IME アンカーは正しかった。ユニットテストで確認済み）。

## 修正1: ブロックキャレット化（src/main.rs）

- `cursor_caret_line`（旧 `insert_cursor_glyph` を置換）: 挿入点の**文字の上に
  fg Black / bg Cyan のブロック**を重ねる。文末（文字が無い箇所）は**シアン下線**の
  キャレット（塗りつぶしブロックは「全角っぽくてダサい」という指摘に応えて変更）。
  グリフを一切使わないので、どの端末でも「スペースのようなもの」は出ない。
- `composer_body_rows` — 本文をグリフなしで折り返し（カーソル位置でリフローしない）。
- `composer_cursor_pos` — 挿入点を `text[..cursor]` の折り返しで追従。
- `composer_body_with_caret` — 本文行＋キャレットセル（幅境界で満杯終端の場合は
  キャレット専用行を追加）。`composer_lines` と `composer_line_count` が同一経路。

## 問題2: バックスペースで残像が残る（原因は ratatui の差分描画）

幅文字（CJK）を消すと右半分が画面に残る。最小再現で確定: ratatui-core 0.1.2（最新）の
diff は、幅文字が無くなった時**左半分だけ空白を書き、右半分セルはスキップする**
（ratatui の内部バッファでは右半分は元々「空白」扱いで前後一致だから）。実端末には
幅文字の右半分が表示されたまま = 残像。akapen のバグではない（pyte 実端末エミュレータ
でも確認。TestBackend は毎フレーム全描画するため検出できない）。

## 修正2: 描画後の残像パス（src/main.rs / src/app.rs / src/reload.rs）

- `draw()` 末尾で完成フレームを `app.last_frame` に保存。
- セッションの描画は `draw_frame(terminal, app)` に統一（run ループ 2 箇所 +
  `open_editor` 復帰）。terminal.draw の後に `clear_wide_char_residue` を実行:
  前フレームで幅文字だった列（記録済み `prior_frame`）が、新フレームで幅文字でなく
  かつ次のセルが空白（= diff がスキップした）なら、その右半分セルへ crossterm で
  明示的にスペースを書いて端末上の半グリフを消す。
- `clear_wide_char_residue_to(prev, curr, out)` は writer 注入可能（テストでバイト検証）。

## テスト

- `cursor_movement_does_not_reflow_the_composer` — カーソル全位置で本文行が安定。
- `composer_caret_follows_the_cursor` / `composer_tabs_expand...` — キャレット位置。
- `wide_char_residue_is_blanked_by_the_afterimage_pass` — 消された幅文字の右半分へ
  スペース書き込みが出ること、幅文字→幅文字では出ないこと。
- render 系テストはキャレット検出を「bg Cyan / シアン下線」判定に更新。
  IME アンカーがキャレット上に乗ることは維持。

## 検証

- `cargo test --locked`: 416 passed; 0 failed。clippy 警告 0。
- 変更ファイル: `src/main.rs` / `src/app.rs` / `src/reload.rs` / `src/state_tests.rs` /
  `HANDOFF.md`

---

# HANDOFF: ハードウェアカーソルの高速点滅を抹消（show_cursor を no-op 化）

## 問題

composer 中、挿入点付近のカーソルが速く点滅する。原因: ratatui の `terminal.draw` は
**位置が公開されるフレームごとに `show_cursor` を送る**（render.rs: フレームに
`cursor_position` があると `show_cursor() → set_cursor_position`）。akapen は composer 中
毎フレーム位置を公開する（macOS IME の変換窓アンカー）ため、毎フレーム Show → 直後の
Hide が繰り返され、ターミナルカーソルが赤rawレートで点滅する。

## 実装内容

- `NoBlinkBackend<W>`（src/main.rs）: `CrosstermBackend` のラッパーで
  **`show_cursor` だけを no-op** にする。位置の公開（`set_cursor_position`）はそのまま
  通す — カーソルの「表示状態」と「位置」は独立なので、IME アンカーは失わず、
  カーソルだけが永遠に出ない。
- `NoBlinkBackend::init()`: raw mode + alternate screen + Hide して返す
  （`ratatui::init()` の代替。パニックフックと Drop 復元は既存の TerminalGuard が担う）。
- `AppTerminal` = `Terminal<NoBlinkBackend<Stdout>>` をセッションの型にし、main.rs /
  reload.rs の全 `DefaultTerminal` 使用箇所を置換。`open_editor` 復帰の再初期化も
  `NoBlinkBackend::init()` に。
- run ループの post-draw `hide_cursor()` は防御として残す（実質不要だが無害）。

## テスト

既存の render 系テストは TestBackend 使用で無影響（416 all green、clippy 0）。

---

# HANDOFF: LOCAL の parent が内容重複 dedup で落ちても正しい血統スロットへ（タイムライン順序の修正）

## 問題

統合タイムライン（`DocumentHistory::assemble_timeline`）で、LOCAL スナップショットを
Git 世代の「観測時点 HEAD oid（parent）」に基づいて配置していた。しかし parent の
lookup を**内容重複除去済みの表示用 gits** に対して行っていたため、次のケースで
誤順序が起きていた:

- commit D と B が同じ内容 → 表示用 gits は newest（D）だけ残し B を除去
- B を parent とする LOCAL は B を解決できず **orphan 化し、タイムライン先頭（最深部
  の直後）に浮く**

```text
コミット: A → B → M → D（D と B は同内容 → B は表示されない）
LOCAL1:  B の上で編集 / LOCAL2: D の上で編集

理想:   now → LOCAL2 → D → M → LOCAL1 → A
従来:   now → LOCAL1 → LOCAL2 → D → M → A   ← 最古の LOCAL1 が一番新しい位置に
```

相対順序まで壊れる（LOCAL2 の方が新しいのに LOCAL1 が上）。
これは spec の「既知の劣化」（parent が解決できない orphan を先頭に置く fallback）では
なく、**解決できるはずの parent が dedup で消えたために起きる実バグ**だった。

## 実装内容（src/history.rs — `assemble_timeline`）

- 表示用に重複除去するとき、各表示コミットの**重複除去前の完全列での index** も
  保持する（`displayed: Vec<(full_index, Revision)>`）。
- LOCAL の parent lookup を**完全列**（`git_revisions`）に対して行う。parent が見つかれば
  「parent より新しい表示 commit の直後」= `insert_at = 1 + count_newer` に配置する。
  `count_newer` は `displayed` のうち `full_index < parent_full_index` の個数。
  displayed は newest-first 順なので、parent より新しい表示 commit は先頭から
  `count_newer` 個に一致する。
- 表示から落ちた parent を持つ LOCAL も正しいスロットに入り、orphan へ逃げない。
- 同一 parent を共有する LOCAL 同士は、従来どおり snapshots を oldest-first で処理し
  同じ slot へ insert するので newest が前（NOW 寄り）に並ぶ（挙動不変）。
- 真の orphan（非 git・rebase・別 ancestry・旧キャッシュ由来で完全列にも parent が
  無い）は従来どおり先頭へ観測順で入る（spec の既知の劣化のまま）。

## 付随テスト（fixture 2 追加、計 8 シナリオ）

1. `local_parent_deduped_out_of_gits_stays_in_its_lineage_slot` — 本バグの回帰。
   B を parent とする LOCAL が M と A の間に正しく収まること + LOCAL 同士の
   相対順序（LOCAL2 が LOCAL1 より新しい側）を固定。
2. `pre_commit_locals_with_null_parent_pile_above_git` — 既知の劣化（parent 無しの
   LOCAL が Git より新しい側へ観測順で積む）の契約を明文化。

## 検証

- `cargo test --locked`: 410 passed; 0 failed（fixture シナリオ 6→8）
- `cargo clippy --locked --all-targets`: 警告 0 件
- 変更ファイル: `src/history.rs` / `testdata/timeline-order-fixtures.json` / `HANDOFF.md`

---

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
