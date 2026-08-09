//! External-edit handling: file-change polling, the manual (`r`) and
//! reply-mode auto reload paths, `$EDITOR` launching, the git snapshot
//! loader, and the external-change check behind the file picker's ⚡.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Instant, SystemTime};

use ratatui::crossterm::cursor::Hide;

use crate::app::App;
use crate::git;
use crate::highlight::syntax_for;
use crate::source::Source;
use crate::{
    draw, replace_view_preserving_cursor, render_view_with_cards, source_content_width,
    view_render_width,
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
/// external edits replace the in-memory content. Unsent comments on THIS
/// file? Require a second `r` (same pattern as edit/quit); the persistent
/// prompt banner (prompt_message) carries the message.
pub(crate) fn reload_now(app: &mut App) {
    // The reload clears this file's comments (stale anchors), so a
    // confirmation protects them exactly like `e` and `q` protect theirs.
    let current = app.current_file_path().to_path_buf();
    let has_comments = app.comments.iter().any(|c| c.file_path == current);
    if has_comments && !app.confirm_reload {
        app.confirm_reload = true;
        return;
    }
    app.confirm_reload = false;
    finish_reload(app);
}

/// Reply-mode auto-reload (`--reply`): no confirmation — comments on this
/// file are dropped with the old content. Fires from the change poll when
/// scripts/akp refreshes the doc. On failure the ⚡ prompt stays up and
/// `r` retries manually.
pub(crate) fn reload_now_auto(app: &mut App) {
    app.confirm_reload = false;
    if finish_reload(app) {
        app.flash("auto-reloaded");
    }
}

pub(crate) fn finish_reload(app: &mut App) -> bool {
    match reload_source(app) {
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
/// editor runs, then reload automatically on return. The diff from
/// [`reload_source`] lights up the changes. Triggers on `e` in both modes.
pub(crate) fn open_editor(app: &mut App, terminal: &mut ratatui::DefaultTerminal) {
    // Unsent comments on THIS file? Require a second `e` (same pattern as
    // quit); the persistent prompt banner (prompt_message) carries the
    // message. Other files' comments are unaffected by editing this one.
    let current = app.current_file_path().to_path_buf();
    let has_comments = app.comments.iter().any(|c| c.file_path == current);
    if has_comments && !app.confirm_edit {
        app.confirm_edit = true;
        return;
    }
    app.confirm_edit = false;

    // Clear this file's comments now — the user confirmed the edit.
    if has_comments {
        app.comments.retain(|c| c.file_path != current);
        replace_view_preserving_cursor(app);
    }

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
            // Reload with diff highlighting — the user just edited the file.
            reload_now(app);
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

/// The git snapshot for one file (3-1): the diff vs `diff_ref` plus the
/// per-line mark sets, ready to display. `None`/empty outside a git
/// repository — the non-git behavior is preserved (no marks, `o`
/// disabled). An untracked file counts as all-new (no HEAD side).
pub(crate) fn git_snapshot(
    diff_ref: &str,
    path: &Path,
    new_len: usize,
) -> (Option<git::Diff>, HashSet<usize>, HashSet<usize>) {
    let Some(diff) = git::Diff::load(diff_ref, path) else {
        return (None, HashSet::new(), HashSet::new());
    };
    let added = if diff.untracked {
        (0..new_len).collect()
    } else {
        diff.added(new_len)
    };
    let deleted = diff.deleted_before(new_len);
    (Some(diff), added, deleted)
}

/// Re-read the file after an external (agent) edit. Returns `Ok(())` when
/// the file was handled (read successfully — even if the content is
/// unchanged); `Err(e)` when the read failed mid-write (e.g. non-UTF-8
/// bytes), so the caller can surface the reason and the next attempt
/// retries.
///
/// On content change: the old vs new diff is computed for line-level
/// highlighting, all comments are cleared (anchors are stale), and the
/// view re-renders at the same width preserving the cursor fraction.
/// Triggered by `r` only — never while Input is open.
pub(crate) fn reload_source(app: &mut App) -> anyhow::Result<()> {
    let new_source = Source::load(app.current_file_path().to_path_buf())?;
    if new_source.content == app.source.content {
        return Ok(()); // touched but unchanged
    }
    let old_content = app.source.content.clone();

    // Reply mode: each refresh replaces the whole message, so a diff would
    // just mark everything as changed — noise. Skip the diff and the
    // +N/-M badge; the whole message is "new" by definition.
    let reply = app.config.reply;
    let (added, removed) = if reply {
        app.last_added.clear();
        app.last_deleted_before.clear();
        app.last_diff = None;
        (0, 0)
    } else {
        // The reload diff, normalized into the same git::Diff structure
        // the git snapshot uses (diff-scope step 1): the marks and the
        // +N/-M badge derive from it, so they can never disagree (the
        // +N/-M used to be a prefix/suffix approximation).
        let diff = git::synthesize_diff(&old_content, &new_source.content);
        let new_len = new_source.lines.len();
        let added_set = diff.added(new_len);
        let deleted_set = diff.deleted_before(new_len);
        let (added, removed) = diff.hunks.iter().fold((0, 0), |(a, d), h| {
            let (x, y) = h.counts();
            (a + x, d + y)
        });
        app.last_diff = Some(diff);
        app.last_added = added_set;
        app.last_deleted_before = deleted_set;
        (added, removed)
    };

    // Comments are anchored to the old content; clear them — but only
    // THIS file's (other files' anchors are untouched by this reload).
    let current = app.current_file_path().to_path_buf();
    let before = app.comments.len();
    app.comments.retain(|c| c.file_path != current);
    let comment_count = before - app.comments.len();

    let width = source_content_width(app);
    app.source = new_source;
    app.spans = app
        .highlight
        .highlight_with(&app.source.content, syntax_for(app.current_file_path()));
    // Git snapshot refreshed (論点 5): the marks re-align to the new
    // content vs the diff base, so the agent's fix leaves the marks
    // describing the CURRENT working tree. The old-side toggle is reset
    // — its hunk indices may have moved (snapshot semantics, P4).
    if !reply {
        let (diff, added, deleted) =
            git_snapshot(&app.git_ref, app.current_file_path(), app.source.len());
        app.git_diff = diff;
        app.git_added = added;
        app.git_deleted_before = deleted;
    }
    app.old_side = None;
    // View: re-render at the same width, keeping the cursor fraction.
    let fraction = app.view.cursor_fraction();
    let (w, _) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let mut view = render_view_with_cards(
        &app.source,
        view_render_width(w),
        &app.highlight,
        &app.comments,
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
        // The whole message is new — the +N/-M badge and the diff gutters
        // would mark everything, so they stay off in reply mode.
        app.last_change = None;
        app.flash("reloaded");
    } else {
        app.last_change = Some((added, removed));
        let cleared = if comment_count > 0 {
            format!(" — {comment_count} comment(s) cleared")
        } else {
            String::new()
        };
        app.flash(format!("reloaded (+{added}/-{removed}){cleared}"));
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
