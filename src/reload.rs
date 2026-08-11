//! External-edit handling: file-change polling, the manual (`r`) and
//! reply-mode auto reload paths, `$EDITOR` launching, the local snapshot
//! timeline, and the external-change check behind the file picker's ⚡.

use std::process::Command;
use std::time::{Instant, SystemTime};

use ratatui::crossterm::cursor::Hide;

use crate::app::App;
use crate::comment::Comment;
use crate::highlight::syntax_for;
use crate::overlay::visible_cards;
use crate::source::Source;
use crate::{
    acknowledge_review, draw, refresh_review_marks, render_view_with_cards,
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
    app.confirm_reload = false;
    finish_reload(app, false);
}

/// Reply-mode auto-reload (`--reply`): no confirmation. Fires from the
/// change poll when scripts/akp refreshes the doc. On failure the ⚡ prompt
/// stays up and `r` retries manually.
pub(crate) fn reload_now_auto(app: &mut App) {
    app.confirm_reload = false;
    if finish_reload(app, false) {
        app.flash("auto-reloaded");
    }
}

/// `from_editor`: the reload came from `e` (an edit of one's own), so the
/// new NOW is acknowledged immediately — one's own edit is not up for review.
pub(crate) fn finish_reload(app: &mut App, from_editor: bool) -> bool {
    // A reload that actually runs resolves any pending confirmation —
    // including the open_editor path, which calls this directly.
    app.confirm_reload = false;
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

/// Open the file in `$EDITOR` (fallback `nano`), suspend the TUI while the
/// editor runs, then reload automatically on return. The new generation is
/// retained but acknowledged as the user's own edit. Triggers on `e` in both
/// modes.
pub(crate) fn open_editor(app: &mut App, terminal: &mut ratatui::DefaultTerminal) {
    // Existing comments are preserved on their current generation. When
    // the editor returns, reload_source promotes live comments to the old
    // LOCAL/COMMIT generation before loading the edited NOW.
    app.confirm_edit = false;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
    // `$EDITOR` may include arguments (e.g. `zed --wait`). Split into the
    // binary and its args, then append the file path last. Known
    // limitation: whitespace-split only — quoted paths or args with
    // spaces (`EDITOR="/Applications/My Editor.app/.../bin/editor"`) are
    // not supported; use a wrapper script for those.
    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("nano");
    let args: Vec<&str> = parts.collect();

    // Suspend the TUI entirely: `ratatui::restore()` leaves the alternate
    // screen, disables raw mode, and shows the cursor.
    ratatui::restore();

    let status = Command::new(bin)
        .args(&args)
        .arg(app.current_file_path())
        .status();

    // Replace the terminal with a fresh one: `ratatui::init()` re-enters
    // raw mode + alternate screen and returns a properly initialised
    // `DefaultTerminal`. The old terminal's Drop is harmless (it only
    // frees buffers, never touches the terminal).
    *terminal = ratatui::init();
    // `ratatui::init()` does EnterAlternateScreen + EnableMouseCapture,
    // but NOT Hide — the cursor stays visible until we hide it again.
    let _ = ratatui::crossterm::execute!(std::io::stdout(), Hide);

    match status {
        Ok(s) if s.success() => {
            // Reload and acknowledge the user's own editor change. Existing
            // comments remain pinned to the generation they describe.
            finish_reload(app, true);
        }
        Ok(_) => app.flash_err(format!("{editor} exited with error")),
        Err(e) => app.flash_err(format!("{editor}: {e}")),
    }

    // Draw immediately — the alternate screen was just re-entered and is
    // blank. The fresh terminal is guaranteed to be in the correct state.
    let _ = terminal.draw(|f| draw(f, app));
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
    let had_live_comments = app
        .comments
        .iter()
        .any(|comment| comment.file_path == current && comment.revision.is_none());
    if had_live_comments
        && let Some(cache) = app.snapshot_cache.as_ref()
    {
        cache.pin(&current, &old_content)?;
    }

    let width = source_content_width(app);
    app.source = new_source;
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
        live.content = history_content;
        history.position = 0;
        history.rendered_position = 0;
    }
    if had_live_comments {
        let old_context = app
            .histories
            .get(app.current_file_index)
            .and_then(|history| history.context_for_content(&old_content));
        for comment in app.comments.iter_mut().filter(|comment| {
            comment.file_path == current && comment.revision.is_none()
        }) {
            comment.revision = old_context.clone();
        }
    }
    if reply {
        app.review_changed.clear();
        app.review_deleted_before.clear();
        app.comparison_changed.clear();
        app.comparison_deleted_before.clear();
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
    if reply {
        // The whole message is new, so cumulative review marks stay off.
        app.flash("reloaded");
    } else {
        let count = app.file_review_count(app.current_file_index);
        app.flash(format!("reloaded · {count} to review"));
    }
    Ok(())
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
    use crate::config::{Config, EscQuit};
    use crate::highlight::{Highlighter, syntax_for};
    use crate::ime::ImeMode;
    use crate::source::Source;
    use crate::view::ViewState;
    use crate::{on_source_key, on_view_key};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

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
            fx: true,
        };
        let source = Source::load(path.into()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
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
}
