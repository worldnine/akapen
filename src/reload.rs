//! External-edit handling: file-change polling, the manual (`r`) and
//! reply-mode auto reload paths, `$EDITOR` launching, the local snapshot
//! timeline, and the external-change check behind the file picker's ⚡.

use std::process::Command;
use std::time::{Instant, SystemTime};

use ratatui::crossterm::cursor::Hide;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};

use crate::app::{App, Mode};
use crate::comment::Comment;
use crate::highlight::syntax_for;
use crate::overlay::visible_cards;
use crate::source::Source;
use crate::{
    AppTerminal, acknowledge_review, draw_frame, refresh_review_marks, render_view_with_cards,
    source_content_width, view_render_width,
};

/// Poll the file's mtime+size; a difference from the last loaded stamp
/// arms the (debounced) reload. The poll runs every frame (~100 ms), which
/// is finer than the 500 ms in the design; the 300 ms debounce absorbs
/// burst writes either way.
pub(crate) fn poll_file_change(app: &mut App) {
    let Ok(meta) = std::fs::metadata(app.current_file_path()) else {
        return;
    };
    let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
    app.file_stamp = Some(stamp);
    if app.last_loaded_stamp != Some(stamp) && !app.file_changed && app.reload_pending.is_none() {
        app.reload_pending = Some(Instant::now());
    }
}

/// The debounced notification for an external edit: the persistent ⚡ badge
/// in the title and the footer prompt (`r reload · i ignore`) are the
/// notification — no transient toast (the prompt supersedes it anyway).
pub(crate) fn notify_file_changed(app: &mut App) {
    if app.file_changed {
        return;
    }
    app.file_changed = true;
}

/// Manual reload (`r`), Vim's `:e` model: the user decides when the
/// external edits replace the in-memory content. Comments remain attached
/// to the exact generation they were written against.
pub(crate) fn reload_now(app: &mut App) {
    finish_reload(app, false);
}

/// Reply-mode auto-reload (`--reply`). Fires from the change poll when
/// scripts/akp refreshes the doc. On failure the ⚡ prompt stays up and
/// `r` retries manually.
pub(crate) fn reload_now_auto(app: &mut App) {
    if finish_reload(app, false) {
        match app.review_reload_note.clone() {
            Some(note) => app.flash(format!("auto-reloaded · {note}")),
            None => app.flash("auto-reloaded"),
        }
    }
}

/// `from_editor`: the reload came from `e` (an edit of one's own), so the
/// new NOW is acknowledged immediately — one's own edit is not up for review.
pub(crate) fn finish_reload(app: &mut App, from_editor: bool) -> bool {
    match reload_source(app, from_editor) {
        Ok(()) => {
            // Refresh the on-disk stamp so the next poll_file_change won't
            // re-detect the same edit as a pending change.
            if let Ok(meta) = std::fs::metadata(app.current_file_path()) {
                let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
                app.file_stamp = Some(stamp);
                app.last_loaded_stamp = Some(stamp);
            }
            app.file_changed = false;
            true
        }
        // The read failed (non-UTF-8 content, e.g. a binary write or a
        // mid-write agent edit): toast the reason and keep the in-memory
        // content. Mark the change seen so the auto path does not retry
        // every poll — the ⚡ prompt stays up and `r` retries manually.
        Err(e) => {
            app.flash_err(format!("reload failed: {e:#}"));
            app.file_changed = true;
            false
        }
    }
}

/// The 1-based source line where the editor should open: the selection's
/// start when one is active, otherwise the current cursor line. Both modes
/// keep the cursor as a 0-based source line, so the handoff is exact from
/// view as well as source.
pub(crate) fn editor_target_line(app: &App) -> usize {
    let line = match app.selection {
        Some(selection) => selection.anchor.min(selection.cursor),
        None => match app.mode {
            Mode::View => app.view.cursor,
            _ => app.cursor,
        },
    };
    // Clamp to the document (a trailing newline yields a phantom last
    // line in some editors; `lines()` does not count it).
    let last = app.source.content.lines().count().max(1);
    (line + 1).min(last)
}

/// Whether the editor binary understands the `+N FILE` convention for
/// opening at a 1-based line (vi family, nano/pico, emacs, micro). Editors
/// outside this list may choke on the extra argument (`zed --wait` treats
/// it as an unknown file), so the jump is opt-in per binary.
fn editor_supports_line_jump(bin: &str) -> bool {
    let name = std::path::Path::new(bin)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(bin);
    matches!(
        name,
        "vi"
            | "vim"
            | "nvim"
            | "view"
            | "vimdiff"
            | "ex"
            | "nano"
            | "pico"
            | "emacs"
            | "emacsclient"
            | "micro"
    )
}

/// Open the file in `$EDITOR` (fallback `nano`), suspend the TUI while the
/// editor runs, then reload automatically on return. The new generation is
/// retained but acknowledged as the user's own edit. Triggers on `e` in both
/// modes.
pub(crate) fn open_editor(app: &mut App, terminal: &mut AppTerminal) {
    let line = editor_target_line(app);
    open_editor_at(app, terminal, line);
}

/// [`open_editor`] at an explicit 1-based line — the Review list's `e`
/// opens at the candidate's first line rather than the cursor's.
pub(crate) fn open_editor_at(app: &mut App, terminal: &mut AppTerminal, line: usize) {
    // Existing comments are preserved on their current generation. When
    // the editor returns, reload_source promotes live comments to the old
    // LOCAL/COMMIT generation before loading the edited NOW. The pre-edit
    // on-disk content is captured first so it stays inspectable in the
    // timeline even when it differs from what akapen last loaded (an
    // agent edit skipped with `i`).
    capture_pre_edit_snapshot(app);

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
    let path = app.current_file_path().to_path_buf();

    // Suspend the TUI entirely: `ratatui::restore()` leaves the alternate
    // screen, disables raw mode, and shows the cursor — but it does not
    // touch mouse capture. Drop the capture first, or every mouse move
    // while the editor runs is injected into its stdin as SGR escape
    // bytes (vim's cursor jumps around, nano renders garbage).
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    let status = run_editor(&editor, line, &path);

    // Replace the terminal with a fresh one: the no-blink init re-enters
    // raw mode + alternate screen (keeping the hardware cursor never
    // visible) and returns a properly initialised terminal. The old one's
    // Drop is harmless (it only frees buffers, never touches the
    // terminal).
    //
    // This re-init is load-bearing: the editor ran on a restored (cooked,
    // main-screen) terminal, so WITHOUT it akapen would keep reading input
    // with raw mode OFF while mouse capture is ON — every mouse motion's
    // SGR bytes get echoed by the tty line discipline as visible garbage
    // and keystrokes sit in the canonical line buffer (the TUI freezes).
    // Mouse capture goes back on only AFTER raw mode is restored, mirroring
    // run()'s startup order.
    let mut init_error = None;
    match crate::NoBlinkBackend::init() {
        Ok(fresh) => *terminal = fresh,
        Err(e) => init_error = Some(e),
    }
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture, Hide);

    if let Some(e) = init_error {
        app.flash_err(format!("terminal re-init failed: {e}"));
    }

    after_editor(app, &editor, status);

    // Draw immediately — the alternate screen was just re-entered and is
    // blank. The fresh terminal is guaranteed to be in the correct state.
    let _ = draw_frame(terminal, app);
}

/// Run `editor` on `path` at the 1-based `line` and wait for it. The
/// terminal handling lives in [`open_editor_at`]; this is the part a test
/// can drive with a script standing in for `$EDITOR`.
pub(crate) fn run_editor(
    editor: &str,
    line: usize,
    path: &std::path::Path,
) -> std::io::Result<std::process::ExitStatus> {
    // `$EDITOR` may include arguments (e.g. `zed --wait`). Split into the
    // binary and its args, then append the file path last. Known
    // limitation: whitespace-split only — quoted paths or args with
    // spaces (`EDITOR="/Applications/My Editor.app/.../bin/editor"`) are
    // not supported; use a wrapper script for those.
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("nano");
    let mut argv: Vec<String> = parts.map(str::to_string).collect();
    if editor_supports_line_jump(bin) {
        argv.push(format!("+{line}"));
    }
    argv.push(path.display().to_string());
    Command::new(bin).args(&argv).status()
}

/// The editor has returned: reload and acknowledge one's own change, or
/// say why nothing was reloaded.
pub(crate) fn after_editor(
    app: &mut App,
    editor: &str,
    status: std::io::Result<std::process::ExitStatus>,
) {
    match status {
        Ok(s) if s.success() => {
            // Reload and acknowledge the user's own editor change. Existing
            // comments remain pinned to the generation they describe.
            finish_reload(app, true);
        }
        Ok(_) => app.flash_err(format!("{editor} exited with error")),
        Err(e) => app.flash_err(format!("{editor}: {e}")),
    }
}

/// Record the pre-edit on-disk content as a LOCAL generation before the
/// editor runs, so the version the editor opens stays inspectable in the
/// timeline even when it differs from what akapen last loaded (e.g. an
/// agent edit the user skipped with `i`). Without this capture the reload
/// on return rebuilds the timeline from the cache and the never-loaded
/// on-disk content would vanish — along with the ability to compare
/// pre/post edit in the time machine. Best-effort: a read or cache
/// failure must never block the edit.
///
/// Cacheless environments (reply mode, no cache dirs) have no persistent
/// timeline to lose the content from; reload_source's in-memory parking
/// keeps the loaded generation, which is all they can retain anyway.
pub(crate) fn capture_pre_edit_snapshot(app: &mut App) {
    let Some(cache) = app.snapshot_cache.clone() else {
        return;
    };
    let path = app.current_file_path().to_path_buf();
    let Ok(disk) = std::fs::read_to_string(&path) else {
        return;
    };
    if disk == app.source.content {
        // The disk version is the version akapen shows — it was already
        // recorded at load; re-recording would only re-observe it.
        return;
    }
    let _ = cache.record_with_parent(&path, &disk, crate::history::head_oid(&path));
}

/// Explicitly skip an external edit (`i`): the on-disk state counts as
/// seen, so the prompt does not re-appear until the next change.
pub(crate) fn ignore_change(app: &mut App) {
    if !app.file_changed {
        return;
    }
    app.file_changed = false;
    app.last_loaded_stamp = app.file_stamp;
    app.flash("file change ignored");
}

/// Re-read the file after an external (agent) edit. Returns `Ok(())` when
/// the file was handled (read successfully — even if the content is
/// unchanged); `Err(e)` when the read failed mid-write (e.g. non-UTF-8
/// bytes), so the caller can surface the reason and the next attempt
/// retries.
///
/// On content change: comments on the former NOW are attached to that
/// snapshot, and the view re-renders at the same width preserving the
/// cursor fraction.
/// Triggered by `r` only — never while Input is open.
pub(crate) fn reload_source(app: &mut App, from_editor: bool) -> anyhow::Result<()> {
    app.review_reload_note = None;
    let new_source = Source::load(app.current_file_path().to_path_buf())?;
    if new_source.content == app.source.content {
        return Ok(()); // touched but unchanged
    }
    let old_content = app.source.content.clone();

    // Reply mode: each refresh replaces the whole message, so review marks
    // would only paint the entire document. The whole message is "new" by
    // definition and snapshots are intentionally disabled in this mode.
    let reply = app.config.reply;
    let current = app.current_file_path().to_path_buf();
    if reply {
        // Auto-reload replaces the message wholesale, so comments pinned
        // to the old message go with it (the event loop's call-site
        // comment says as much: they were either sent or are stale).
        // Keeping them would leave stale quoted snippets in the `l` list
        // and let `s` send them against the new message. Comments on
        // other message files survive — reply mode opens several
        // messages as files.
        app.comments.retain(|comment| comment.file_path != current);
    }
    // **見届けた書き換え**（`docs/design/marks-only-and-review-mode.md`
    // 4 節「直接編集」）。前後の版が両方手元にあるのはここだけなので、
    // 1 バイトも変わっていない範囲に限って、捨てた判断と accept 済みの
    // review コメントを新しい位置へ写す。reply は文書を丸ごと差し替える
    // （コメントも上で落とした）ので写す相手が無い。
    let mut resolved = 0;
    if !reply {
        let map = crate::edit_map::EditMap::between(&old_content, &new_source.content);
        if let Some(store) = app.review_dismissed_store.as_ref() {
            store.carry(
                &crate::semantic::source_digest(&old_content),
                &crate::semantic::source_digest(&new_source.content),
                &map,
            );
        }
        resolved = app.settle_review_comments(&map, &new_source.content);
        if let Some(anchor) = app.review_edit_anchor.take() {
            app.review_cursor_target = Some(map.map_pos(anchor));
        }
    }
    // 生きている review コメントはもう新しい版に付け直してあるので、
    // 古い版へ留め置くのは人の赤入れだけである。
    let pinned_here = |comment: &Comment| {
        comment.file_path == current
            && comment.revision.is_none()
            && crate::review::parse_comment_text(&comment.text).is_none()
    };
    let had_live_comments = app.comments.iter().any(pinned_here);
    if had_live_comments
        && let Some(cache) = app.snapshot_cache.as_ref()
    {
        cache.pin(&current, &old_content)?;
    }

    let width = source_content_width(app);
    app.source = new_source;
    // The document changed under the annotation — the design document's
    // 「編集時」 chapter: a provider is re-asked when the text settles,
    // never per keystroke and never on a budget change.
    app.reanalyze_semantics();
    let history_path = app.current_file_path().to_path_buf();
    let history_content = app.source.content.clone();
    if let Some(cache) = app.snapshot_cache.clone() {
        if let Some(history) = app.histories.get_mut(app.current_file_index) {
            // Cache failures do not invalidate a successful file reload;
            // retain a one-point NOW timeline as the graceful fallback.
            if history
                .replace_from_cache(&history_path, &history_content, 64, &cache)
                .is_err()
            {
                *history = crate::history::DocumentHistory::load(
                    &history_path,
                    &history_content,
                    64,
                );
            }
        }
    } else if let Some(history) = app.histories.get_mut(app.current_file_index)
        && let Some(live) = history.revisions.first_mut()
    {
        // Without a snapshot cache (reply mode, or HOME/XDG_CACHE_HOME/
        // AKAPEN_CACHE_DIR all unset) the replaced NOW would vanish from
        // the timeline, and the live-comment re-anchoring below could not
        // resolve `context_for_content(&old_content)` — the comments
        // would keep `revision: None` and silently re-anchor to the new
        // content with a stale snippet. Park the old NOW as an in-memory
        // LOCAL generation at position 1 first (duplicate-guarded: a Git
        // revision may already carry the same content).
        live.content = history_content;
        history.position = 0;
        history.rendered_position = 0;
        if !history
            .revisions
            .iter()
            .any(|revision| revision.content == old_content)
        {
            let id = crate::snapshot::content_id(old_content.as_bytes());
            // キャッシュ無し経路には captured_ms が無いので、park 時点の現在時刻を
            // 使って説明文を一度だけ焼き込む。キャッシュが無い以上セッションを
            // 跨ぐ復元は元々不可能なので、セッション内で不変であれば良い。
            // parent は分からないので説明文の「on top of commit」句は省略する。
            let captured = crate::snapshot::now_ms();
            history.revisions.insert(
                1,
                crate::history::Revision {
                    id: Some(format!("local:{id}")),
                    short_id: id.chars().take(7).collect(),
                    summary: crate::history::local_revision_summary(captured, None),
                    content: old_content.clone(),
                    source: crate::history::RevisionSource::Local,
                    timestamp_ms: Some(captured),
                },
            );
        }
    }
    if had_live_comments {
        let old_context = app
            .histories
            .get(app.current_file_index)
            .and_then(|history| history.context_for_content(&old_content));
        for comment in app.comments.iter_mut().filter(|comment| pinned_here(comment)) {
            comment.revision = old_context.clone();
        }
    }
    if reply {
        app.review_changed.clear();
        app.review_deleted_before.clear();
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
        app.comparison_deleted_blocks.clear();
    } else {
        refresh_review_marks(app);
    }
    if from_editor {
        acknowledge_review(app, false);
    }
    app.spans = app
        .highlight
        .highlight_with(&app.source.content, syntax_for(app.current_file_path()));
    // View: re-render at the same width, keeping the cursor fraction.
    // Only the CURRENT revision's cards are folded into the layout:
    // comments made against the former NOW were rebased onto its LOCAL
    // snapshot above, so a card must not linger in the new NOW (it
    // would stay until the next full re-render — the regression where
    // render_view_with_cards got ALL comments).
    let fraction = app.view.cursor_fraction();
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let file_comments: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
    let mut view = render_view_with_cards(
        &app.source,
        view_render_width(w),
        &app.highlight,
        &file_comments,
        app.config.decoration_blend,
    );
    view.goto_fraction(fraction);
    view.keep_cursor_visible(app.view_viewport_rows());
    app.view = view;
    // Comment layout: force the wrap cache to rebuild for the new spans.
    app.content_width = 0;
    app.ensure_row_cache(width);
    app.refresh_line_rows();
    // Cursor clamps to the new length; a selection is stale.
    let last = app.source.len().saturating_sub(1);
    app.cursor = app.cursor.min(last);
    app.selection = None;
    app.offset = app.offset.min(app.max_offset(app.source_viewport_rows() as u16));
    app.file_changed = false;
    app.review_reload_note = resolved_note(resolved);
    if reply {
        // The whole message is new, so cumulative review marks stay off.
        app.flash("reloaded");
    } else {
        let count = app.file_review_count(app.current_file_index);
        let note = app
            .review_reload_note
            .as_deref()
            .map(|note| format!(" · {note}"))
            .unwrap_or_default();
        app.flash(format!("reloaded · {count} to review{note}"));
    }
    Ok(())
}

/// 外した review コメントの本数を知らせる 1 行。0 本なら何も言わない。
fn resolved_note(resolved: usize) -> Option<String> {
    match resolved {
        0 => None,
        1 => Some("1 review comment resolved by edit".into()),
        n => Some(format!("{n} review comments resolved by edit")),
    }
}

/// Whether `files[i]`'s on-disk state differs from what the session last
/// loaded (or acknowledged with `i`) — the file picker's ⚡ badge. Reads
/// the disk stamp fresh: the poll loop only watches the CURRENT file, so
/// background files edited by an agent are checked here on demand.
pub(crate) fn file_externally_changed(app: &App, i: usize) -> bool {
    let seen = if i == app.current_file_index {
        app.last_loaded_stamp
    } else {
        app.file_states.get(i).and_then(|s| s.last_loaded_stamp)
    };
    let Some(seen) = seen else { return false };
    let Ok(meta) = std::fs::metadata(&app.files[i]) else {
        return false;
    };
    (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()) != seen
}

#[cfg(test)]
mod handoff_tests {
    use crate::app::{App, Mode};
    use crate::comment::Comment;
    use crate::config::{Config, EscQuit};
    use crate::highlight::{Highlighter, syntax_for};
    use crate::history::{DocumentHistory, RevisionSource};
    use crate::ime::ImeMode;
    use crate::source::Source;
    use crate::view::ViewState;
    use crate::{on_source_key, on_view_key};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use std::io::Write;
    use std::path::Path;

    fn real_app() -> App {
        let path = "testdata/full.md";
        let config = Config {
            files: vec![path.into()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
        marks_questions: None,
        review_rules: None,
        review_json: false,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path.into()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::View;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app
    }

    #[test]
    fn tab_round_trip_preserves_the_line() {
        let mut app = real_app();
        // 46行目 (0-based 45, タイトル付き — マージ行の2行目) に移動。
        app.view.goto_source_line(45);
        // View → Comment: 正確な行が引き継がれる（グループ先頭 44 に飛ばない）。
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        assert_eq!(app.cursor, 45, "選択なしでも正確な行");
        // Comment → View → Comment: 行が変わらない。
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        assert_eq!(app.view.cursor, 45);
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 45, "ラウンドトリップで行が動かない");
    }

    /// A fresh app over a temp file with `n` lines ("line1"..), mirroring
    /// state_tests' make_app (the tempdir is returned so the tests can
    /// rewrite the file on disk before reloading). `reply` builds the
    /// `--reply` variant: same file, snapshots/cache disabled.
    fn temp_app(n: usize, mode: Mode, reply: bool) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 1..=n {
            writeln!(f, "line{i}").unwrap();
        }
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
            cursor_anchor: true,
            fx: true,
            semantic: None,
            semantic_cmd: None,
        marks_questions: None,
        review_rules: None,
        review_json: false,
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        // App::new no longer tokenizes (run() supplies the spans), so
        // fill them here exactly like run() does.
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        let mut history = DocumentHistory::load(&app.files[0], &app.source.content, 0);
        history.acknowledge_in_memory(&app.source.content);
        app.histories = vec![history];
        app.mode = mode;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        (app, dir)
    }

    fn add_comment(app: &mut App, path: &Path, text: &str) {
        app.comments.push(Comment {
            file_path: path.to_path_buf(),
            start: 1,
            end: 1,
            lines: "line1".into(),
            revision: None,
            text: text.into(),
        });
    }

    #[test]
    fn reply_reload_drops_current_file_comments_and_keeps_other_files() {
        let (mut app, dir) = temp_app(3, Mode::Source, true);
        let current = app.current_file_path().to_path_buf();
        // Reply mode opens each message as its own file; a comment on
        // another message must survive the current one's auto-reload.
        let other = dir.path().join("other.md");
        std::fs::write(&other, "another message\n").unwrap();
        app.files.push(other.clone());
        add_comment(&mut app, &current, "on the old message");
        add_comment(&mut app, &other, "on another message");
        // The agent refreshes the message: content actually changes.
        std::fs::write(&current, "new message line 1\nnew message line 2\n").unwrap();
        assert!(crate::reload::reload_source(&mut app, false).is_ok());
        assert_eq!(
            app.comments.len(),
            1,
            "the old message's comment was dropped with it"
        );
        assert_eq!(
            app.comments[0].file_path, other,
            "other files' comments survive"
        );
    }

    #[test]
    fn cacheless_reload_anchors_live_comments_to_an_in_memory_local_generation() {
        let (mut app, _dir) = temp_app(3, Mode::Source, false);
        assert!(app.snapshot_cache.is_none(), "no HOME cache in tests");
        let current = app.current_file_path().to_path_buf();
        add_comment(&mut app, &current, "on the old NOW");
        let old = app.source.content.clone();
        std::fs::write(app.current_file_path(), "line1\nchanged\nline3\n").unwrap();
        assert!(crate::reload::reload_source(&mut app, false).is_ok());
        // The comment leaves `None` and pins to the parked LOCAL
        // generation — without the in-memory insert the old content would
        // have vanished from history and the comment would silently
        // re-anchor to the new NOW with a stale snippet.
        let history = &app.histories[0];
        let expected = history
            .context_for_content(&old)
            .expect("the old content resolves to a generation");
        assert!(
            expected.starts_with("local:"),
            "in-memory LOCAL id: {expected}"
        );
        assert_eq!(app.comments[0].revision.as_deref(), Some(expected.as_str()));
        let parked = &history.revisions[1];
        assert_eq!(parked.content, old, "the old NOW survives as a generation");
        // id は裸の `local:<id>`、expected は新形式の「id — 説明文」。
        // 同一性判定は identity 部だけで行う（旧形式の保存コメントと新形式
        // context の照合が移行なしで動くための契約）。
        assert!(crate::history::same_revision(
            parked.id.as_deref(),
            Some(expected.as_str())
        ));
        assert_eq!(parked.source, RevisionSource::Local);
        // 説明文は park 時点の現在時刻で一度だけ焼き込まれる。
        assert!(
            parked
                .summary
                .starts_with("akapen local snapshot: uncommitted state captured 20"),
            "self-describing summary: {}",
            parked.summary
        );
        assert!(
            parked.summary.ends_with("(not a git object)"),
            "negative git hint: {}",
            parked.summary
        );
    }

    #[test]
    fn edit_blocked_while_file_change_pending() {
        // Source mode.
        let (mut app, _dir) = temp_app(3, Mode::Source, false);
        app.file_changed = true;
        on_source_key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE, None);
        assert!(
            app.status.as_ref().is_some_and(|(msg, _, err)| *err
                && msg.contains("r reload first")),
            "source mode: the block explains itself"
        );
        // View mode.
        let (mut app, _dir) = temp_app(3, Mode::View, false);
        app.file_changed = true;
        on_view_key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE, None);
        assert!(
            app.status.as_ref().is_some_and(|(msg, _, err)| *err
                && msg.contains("r reload first")),
            "view mode: the block explains itself"
        );
        // No pending change: `e` falls through (terminal None in tests), no
        // spurious block toast.
        let (mut app, _dir) = temp_app(3, Mode::Source, false);
        on_source_key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE, None);
        assert!(app.status.is_none(), "no pending change: not blocked");
    }

    #[test]
    fn editor_opens_at_the_cursor_line() {
        // Source mode: the cursor is a 0-based source line → +N is 1-based.
        let (mut app, _dir) = temp_app(5, Mode::Source, false);
        app.cursor = 2;
        assert_eq!(crate::reload::editor_target_line(&app), 3);
        // View mode: same source-line semantics.
        let (mut app, _dir) = temp_app(5, Mode::View, false);
        app.view.cursor = 1;
        assert_eq!(crate::reload::editor_target_line(&app), 2);
        // A selection opens at its start, not its extension end.
        let (mut app, _dir) = temp_app(5, Mode::Source, false);
        let mut selection = crate::comment::Selection::new(4);
        selection.cursor = 2;
        app.selection = Some(selection);
        assert_eq!(crate::reload::editor_target_line(&app), 3);
    }

    #[test]
    fn editor_line_clamps_to_the_document() {
        // 3 lines of content; cursor past EOF clamps to the last line.
        let (mut app, _dir) = temp_app(3, Mode::Source, false);
        app.cursor = 99;
        assert_eq!(crate::reload::editor_target_line(&app), 3);
        // An empty document still yields line 1.
        let (app, _dir) = temp_app(0, Mode::Source, false);
        assert_eq!(crate::reload::editor_target_line(&app), 1);
    }

    #[test]
    fn only_plus_n_editors_get_the_line_jump() {
        use crate::reload::editor_supports_line_jump;
        for bin in ["vim", "nvim", "vi", "nano", "emacs", "micro", "/usr/bin/nvim"] {
            assert!(editor_supports_line_jump(bin), "{bin} supports +N");
        }
        for bin in ["zed", "code", "hx", "kak", "/opt/zed --wait"] {
            assert!(!editor_supports_line_jump(bin), "{bin} does not");
        }
        // A path with a directory component resolves to the basename.
        assert!(editor_supports_line_jump("/usr/local/bin/nano"));
    }

    #[test]
    fn capture_pre_edit_keeps_the_ignored_disk_content_in_the_timeline() {
        let (mut app, dir) = temp_app(3, Mode::Source, false);
        let path = app.current_file_path().to_path_buf();
        let initial = app.source.content.clone();
        let cache = crate::snapshot::SnapshotCache::at(dir.path().join("cache"));
        // The startup observation, like open_cached at launch.
        cache.record_with_parent(&path, &initial, None).unwrap();
        app.snapshot_cache = Some(cache);
        // The agent writes v2 on disk; the user skipped it (`i`) and pressed
        // `e` — the editor will open v2, which akapen never loaded.
        let agent_version = "line1\nagent version\nline3\n";
        std::fs::write(&path, agent_version).unwrap();
        crate::reload::capture_pre_edit_snapshot(&mut app);
        // The user edits to v3 in the editor; on return the reload runs.
        let user_version = "line1\nuser version\nline3\n";
        std::fs::write(&path, user_version).unwrap();
        assert!(crate::reload::reload_source(&mut app, true).is_ok());
        let history = &app.histories[0];
        assert_eq!(
            history.revisions[0].content, user_version,
            "the edited content is NOW"
        );
        assert!(
            history
                .revisions
                .iter()
                .any(|revision| revision.content == agent_version),
            "the pre-edit disk content (the agent's version) survives as a \
             generation:\n{}",
            history
                .revisions
                .iter()
                .map(|revision| revision.content.clone())
                .collect::<Vec<_>>()
                .join("\n---\n")
        );
        assert!(
            history.revisions[0].content != agent_version
                && history.revisions[0].content != initial,
            "NOW is the user's version"
        );
    }
}
