//! The overlay stack: the files picker (Ctrl+p), the comments | changes
//! list (`l`), and the help reference (`?`) — their key handling, the
//! shared panel/cursor/scroll math, and their drawers.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DiffScope, Mode, supports_view};
use crate::clip_ellipsis;
use crate::comment::{Comment, Selection};
use crate::hunknav::{
    ChangesEntry, changes_entries, changes_rows, entry_diff, file_len, hunk_label, land_on_hunk,
};
use crate::reload::file_externally_changed;
use crate::replace_view_preserving_cursor;

/// The kind of overlay currently open (Ctrl+p = files, `l` = comments,
/// `?` = help).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Overlay {
    /// File picker: switch to another file in the session.
    Files,
    /// All-comments list across every file.
    Comments,
    /// The full key reference (`?`).
    Help,
}

/// Which tab the all-comments overlay (`l`) shows: the comments list or
/// the git changes list (hunks across every file). Tab toggles.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum OverlayTab {
    #[default]
    Comments,
    Changes,
}

/// Open an overlay, resetting the double-click tracker: a click in a
/// fresh session must never be mistaken for the tail of an old
/// double-click (and a delete may have shifted the entry indices). The
/// pending `]`/`[` chord is dropped too — overlay keys must never
/// complete it.
pub(crate) fn open_overlay(app: &mut App, kind: Overlay, cursor: usize) {
    app.overlay = Some(kind);
    app.overlay_cursor = cursor;
    app.overlay_offset = 0;
    app.last_overlay_click = None;
    app.pending_chord = None;
}

/// Raw indices of `app.comments` in the overlay's order (file, then
/// start line). The overlay cursor indexes THIS list — Enter, delete,
/// and the drawer all map through it, so they can never disagree with
/// what is highlighted.
pub(crate) fn sorted_comment_indices(app: &App) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..app.comments.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ca, cb) = (&app.comments[a], &app.comments[b]);
        ca.file_path.cmp(&cb.file_path).then(ca.start.cmp(&cb.start))
    });
    idx
}

/// The display rows of the open overlay, top to bottom: `None` = a group
/// header (not selectable), `Some(i)` = the entry index in overlay order
/// (files: the file index; comments: the [`sorted_comment_indices`]
/// position). Shared by the drawer and the scroll math.
pub(crate) fn overlay_rows(app: &App) -> Vec<Option<usize>> {
    match app.overlay {
        Some(Overlay::Files) => (0..app.files.len()).map(Some).collect(),
        Some(Overlay::Comments) => {
            if app.overlay_tab == OverlayTab::Changes {
                return changes_rows(app);
            }
            let idx = sorted_comment_indices(app);
            let mut rows = Vec::new();
            let mut last_file: Option<&PathBuf> = None;
            for (pos, &raw) in idx.iter().enumerate() {
                let file = &app.comments[raw].file_path;
                if last_file != Some(file) {
                    last_file = Some(file);
                    rows.push(None); // group header
                }
                rows.push(Some(pos));
            }
            rows
        }
        Some(Overlay::Help) | None => Vec::new(),
    }
}

/// The number of selectable entries in the open overlay — the j/k and
/// wheel clamps. The comments overlay counts its current tab.
pub(crate) fn overlay_entry_count(app: &App) -> usize {
    match app.overlay {
        Some(Overlay::Files) => app.files.len(),
        Some(Overlay::Comments) => {
            if app.overlay_tab == OverlayTab::Changes {
                changes_entries(app).len()
            } else {
                app.comments.len()
            }
        }
        Some(Overlay::Help) | None => 0,
    }
}

/// How many list rows the open overlay's panel can show: its inner
/// height minus the title, the blank, and the footer hint rows.
pub(crate) fn overlay_visible_rows() -> usize {
    let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let panel = overlay_panel(Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    });
    panel.height.saturating_sub(5).max(1) as usize
}

/// The comments that render as cards in the CURRENT file right now:
/// only this file's comments, minus the one being re-edited (its card
/// would stack redundantly under the edit composer — the composer
/// already carries the text, so `comment` + `edit` never pile up).
pub(crate) fn visible_cards(app: &App) -> Vec<&Comment> {
    let current = app.current_file_path();
    let revision = app.current_revision_context();
    app.comments
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            c.file_path == *current
                && c.revision == revision
                && !(app.mode == Mode::Input && app.editing_comment == Some(*i))
        })
        .map(|(_, c)| c)
        .collect()
}

/// Keep the overlay cursor's row inside the visible window: the offset
/// scrolls only when the content overflows (a list that fits never
/// scrolls).
pub(crate) fn keep_overlay_cursor_visible(app: &mut App) {
    let rows = overlay_rows(app);
    let visible = overlay_visible_rows();
    let max_offset = rows.len().saturating_sub(visible);
    let Some(cursor_row) = rows.iter().position(|r| *r == Some(app.overlay_cursor)) else {
        app.overlay_offset = app.overlay_offset.min(max_offset);
        return;
    };
    if cursor_row < app.overlay_offset {
        app.overlay_offset = cursor_row;
    } else if cursor_row >= app.overlay_offset + visible {
        app.overlay_offset = cursor_row + 1 - visible;
    }
    app.overlay_offset = app.overlay_offset.min(max_offset);
}

/// Handle keys while an overlay (files, comments, or help) is open.
pub(crate) fn on_overlay_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    match app.overlay {
        Some(Overlay::Files) => on_files_overlay_key(app, key, modifiers),
        Some(Overlay::Comments) => on_comments_overlay_key(app, key, modifiers),
        Some(Overlay::Help) => on_help_overlay_key(app, key, modifiers),
        None => {}
    }
}

/// The help overlay (`?`): j/k or the wheel scroll the reference — but
/// only when it overflows the panel (content that fits never scrolls).
/// Esc / q / `?` close it.
pub(crate) fn on_help_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let max = help_rows(app.esc_quit_enabled(), app.config.reply, app.git_diff.is_some())
        .len()
        .saturating_sub(overlay_visible_rows());
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            app.overlay_cursor = (app.overlay_cursor + 1).min(max);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1).min(max);
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => app.overlay = None,
        _ => {}
    }
}

/// The file picker (Ctrl+p): j/k move, Enter switches to the file, and
/// Esc / q / Ctrl+p close it. The current file is marked in the list.
/// Enter-equivalent for the overlay selection: files → switch to the
/// file, comments → jump to the comment's file+line. Shared by the Enter
/// key and a double-click (see on_mouse).
pub(crate) fn activate_overlay_selection(app: &mut App) {
    match app.overlay {
        Some(Overlay::Files) => {
            if app.overlay_cursor < app.files.len() {
                app.overlay = None;
                app.switch_to_file(app.overlay_cursor);
            }
        }
        Some(Overlay::Comments) => {
            if app.overlay_tab == OverlayTab::Changes {
                // Changes tab: jump to the selected hunk (switch to its
                // file first), or to an untracked file's top.
                let entries = changes_entries(app);
                let Some(e) = entries.get(app.overlay_cursor).copied() else {
                    return;
                };
                app.overlay = None;
                match e {
                    // The entry's hunk index is an index into the scope's
                    // merged hunk list (scoped_hunk_refs), which is what
                    // land_on_hunk resolves after the switch.
                    ChangesEntry::Hunk { file, hunk, .. } => {
                        if file != app.current_file_index {
                            app.switch_to_file(file);
                        }
                        land_on_hunk(app, hunk);
                    }
                    ChangesEntry::Untracked { file } => {
                        if file != app.current_file_index {
                            app.switch_to_file(file);
                        }
                        app.selection = None;
                        if app.mode == Mode::View {
                            app.view.goto_source_line(0);
                            app.view.keep_cursor_visible(app.view_viewport_rows());
                        } else {
                            app.cursor = 0;
                            app.keep_cursor_visible(app.source_viewport_rows() as u16);
                        }
                        app.flash("untracked — all lines are new");
                    }
                }
                return;
            }
            // Comments tab: jump to the selected comment's file; its
            // whole range becomes the selection (all lines light up),
            // cursor on the extent. The cursor indexes the SORTED list;
            // map through the indices.
            let idx = sorted_comment_indices(app);
            if let Some(&raw) = idx.get(app.overlay_cursor) {
                let c = &app.comments[raw];
                let target_path = c.file_path.clone();
                let target_start = c.start;
                let target_end = c.end;
                if let Some(pos) = app.files.iter().position(|f| f == &target_path) {
                    app.overlay = None;
                    app.switch_to_file(pos);
                    let last = app.source.len().saturating_sub(1);
                    let start = (target_start.saturating_sub(1) as usize).min(last);
                    let end = (target_end.saturating_sub(1) as usize).min(last);
                    app.selection = Some(Selection {
                        anchor: start,
                        cursor: end,
                    });
                    if app.mode == Mode::View {
                        app.view.goto_source_line(end);
                        app.view.keep_cursor_visible(app.view_viewport_rows());
                    } else {
                        app.cursor = end;
                        app.keep_cursor_visible(app.source_viewport_rows() as u16);
                    }
                }
            }
        }
        Some(Overlay::Help) | None => {}
    }
}

pub(crate) fn on_files_overlay_key(app: &mut App, key: KeyCode, modifiers: KeyModifiers) {
    let total = app.files.len();
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 {
                app.overlay_cursor = (app.overlay_cursor + 1).min(total - 1);
                keep_overlay_cursor_visible(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
            keep_overlay_cursor_visible(app);
        }
        KeyCode::Enter => activate_overlay_selection(app),
        // Esc or q closes the picker (q never quits the app while a
        // picker is open); Ctrl+o toggles it closed again.
        KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => app.overlay = None,
        _ => {}
    }
}

/// The all-comments list (`l`): j/k move, Enter jumps to the comment's
/// file+line, `d` deletes it, and Esc / q / `l` close it.
pub(crate) fn on_comments_overlay_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let total = overlay_entry_count(app);
    match key {
        // Tab toggles the comments | changes tab.
        KeyCode::Tab => {
            app.overlay_tab = match app.overlay_tab {
                OverlayTab::Comments => OverlayTab::Changes,
                OverlayTab::Changes => OverlayTab::Comments,
            };
            app.overlay_cursor = 0;
            app.overlay_offset = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if total > 0 {
                app.overlay_cursor = (app.overlay_cursor + 1).min(total - 1);
                keep_overlay_cursor_visible(app);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.overlay_cursor = app.overlay_cursor.saturating_sub(1);
            keep_overlay_cursor_visible(app);
        }
        KeyCode::Enter => activate_overlay_selection(app),
        KeyCode::Char('d') => {
            // Delete the selected comment (comments tab only — the
            // changes tab has nothing to delete). The cursor indexes the
            // SORTED list; map through the indices to the raw vec.
            if app.overlay_tab != OverlayTab::Comments {
                return;
            }
            let idx = sorted_comment_indices(app);
            if let Some(&raw) = idx.get(app.overlay_cursor) {
                app.comments.remove(raw);
                if app.overlay_cursor > 0 && app.overlay_cursor >= app.comments.len() {
                    app.overlay_cursor = app.comments.len().saturating_sub(1);
                }
                keep_overlay_cursor_visible(app);
                replace_view_preserving_cursor(app);
            }
        }
        // Esc or q closes the list; `l` toggles it closed again.
        KeyCode::Esc | KeyCode::Char('q') => app.overlay = None,
        KeyCode::Char('l') => app.overlay = None,
        _ => {}
    }
}

/// Draw the all-comments overlay (Ctrl+p). Centered panel with sorted comment list.
pub(crate) fn draw_overlay(f: &mut Frame, app: &App) {
    match app.overlay {
        Some(Overlay::Files) => draw_files_overlay(f, app),
        Some(Overlay::Comments) => draw_comments_overlay(f, app),
        Some(Overlay::Help) => draw_help_overlay(f, app),
        None => {}
    }
}

/// The help reference rows (label, keys). Shared by the drawer and the
/// scroll clamp, so the list never scrolls past its own end; scrollable
/// with j/k or the wheel (small screens), closed by Esc / q / `?` or a
/// click outside the panel. The quit row reflects the active Esc binding.
pub(crate) fn help_rows(esc_quit: bool, reply: bool, in_git: bool) -> Vec<(&'static str, &'static str)> {
    let mut rows = vec![
        ("move", "j/k · g/G · PgUp/PgDn · ^u/^d"),
        ("comment", "v select · Esc cancel · c add · d delete · ^n/^p jump"),
        ("mode", "Tab view⇄source"),
        ("output", "y copy · s send"),
        ("list", "l comments/changes · Tab tab · ? help"),
    ];
    if reply {
        // Reply mode: a single message document — no file navigation, no
        // edit, and reloads are automatic (r stays as a manual retry).
        // ]/[ moves between the recent messages akp materialized.
        rows.push(("msg", "]/[ older/newer"));
        rows.push(("reload", "auto-reload on change · r manual"));
    } else {
        rows.insert(1, ("file", "]/[ · ^o files"));
        rows.insert(2, ("time", "← older · newer → · hold:scrub"));
        rows.push(("reload", "r reload · i ignore · e edit"));
        rows.push(("git", "n/N next/prev change · F7/]c/Alt+j next · o old side vs HEAD"));
        // Outside a repository the cycle is Last ↔ Off only (P1).
        rows.push((
            "marks",
            if in_git {
                "m last/git/off"
            } else {
                "m last/off"
            },
        ));
    }
    rows.push(("quit", if esc_quit { "Esc/q quit" } else { "q quit · Esc cancel" }));
    rows
}

/// The full key reference (`?`): label + keys per category, scrollable
/// with j/k or the wheel (small screens), closed by Esc / q / `?` or a
/// click outside the panel.
pub(crate) fn draw_help_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label_style = Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::BOLD);

    let rows = help_rows(app.esc_quit_enabled(), app.config.reply, app.git_diff.is_some());
    let visible = overlay_visible_rows();
    // Scroll only when the reference overflows the panel; a reference
    // that fits stays put (j/k are no-ops there).
    let offset = app.overlay_cursor.min(rows.len().saturating_sub(visible));
    let title_text = " help ".to_string();
    let title_fill =
        "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ])];
    for (label, keys) in rows.iter().skip(offset).take(visible) {
        lines.push(Line::from(vec![
            Span::styled(format!(" {label:<11}"), label_style),
            Span::styled(keys.to_string(), Style::default().fg(Color::White)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:scroll  Esc/q/?:close  click outside:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// The shortest suffix (component-wise) of `path` that no other file
/// shares — the file picker's row for it. A unique basename stays a bare
/// basename (no redundant directory prefix on every row); colliding
/// basenames grow parent components until the row is unambiguous.
pub(crate) fn unique_suffix(path: &Path, all: &[PathBuf]) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for len in 1..=parts.len() {
        let tail = &parts[parts.len() - len..];
        let collides = all.iter().any(|other| {
            if other == path {
                return false;
            }
            let op: Vec<String> = other
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            op.len() >= len && &op[op.len() - len..] == tail
        });
        if !collides {
            return tail.join("/");
        }
    }
    path.display().to_string()
}

/// The directory shared by every file in the session, shown in the picker
/// title so the rows stay short (e.g. `files (3) · testdata/`).
pub(crate) fn common_parent(files: &[PathBuf]) -> Option<String> {
    let lists: Vec<Vec<String>> = files
        .iter()
        .map(|f| {
            let comps: Vec<String> = f
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            comps[..comps.len().saturating_sub(1)].to_vec()
        })
        .collect();
    let first = lists.first()?;
    let mut n = 0usize;
    while n < first.len() && lists.iter().all(|l| l.get(n) == Some(&first[n])) {
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let dir = first[..n].join("/");
    Some(clip_if_needed(&dir, 24) + "/")
}

/// Clip `s` to `max_cols` with a trailing `…` only when it actually
/// overflows ([`clip_ellipsis`] always marks the clip, so short strings
/// would wrongly gain a `…`).
pub(crate) fn clip_if_needed(s: &str, max_cols: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_cols {
        s.to_string()
    } else {
        clip_ellipsis(s, max_cols)
    }
}

/// A centered panel rect (70% width/height) for overlay contents.
pub(crate) fn overlay_panel(area: Rect) -> Rect {
    let w = (area.width as f64 * 0.7) as u16;
    let h = (area.height as f64 * 0.7) as u16;
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    Rect {
        x: area.x + x,
        y: area.y + y,
        width: w,
        height: h,
    }
}

/// Map a screen row to the overlay entry under it (the cursor index), or
/// None for the title, group headers, gaps, or outside the panel.
/// Mirrors draw_overlay's layout (border at panel.y, title at +1, entries
/// from +2) so clicks land exactly on what is drawn.
pub(crate) fn overlay_entry_at(app: &App, row: u16) -> Option<usize> {
    let overlay = app.overlay?;
    let (w, h) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    let panel = overlay_panel(Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    });
    let first_entry = panel.y + 2; // below border + title
    if row < first_entry {
        return None;
    }
    // The visible list starts at the scroll offset; the clicked row maps
    // to that list position.
    let rel = (row - first_entry) as usize + app.overlay_offset;
    match overlay {
        Overlay::Files => (rel < app.files.len()).then_some(rel),
        // Help has no selectable entries (the wheel/j-k scroll it).
        Overlay::Help => None,
        Overlay::Comments => {
            // Same grouped layout as draw_comments_overlay: per file a
            // header row (not selectable) then one row per comment.
            overlay_rows(app).get(rel).copied().flatten()
        }
    }
}

/// The file picker (Ctrl+p): every file in session order with its comment
/// count; the current file is highlighted. Files whose on-disk state
/// changed since their last load carry the title bar's ⚡ badge.
pub(crate) fn draw_files_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);

    let count = app.files.len();
    let title_text = match common_parent(&app.files) {
        Some(dir) => format!(" files ({count}) · {dir} "),
        None => format!(" files ({count}) "),
    };
    let title_fill = "─".repeat(panel.width.saturating_sub(title_text.width() as u16 + 2) as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled(title_text, yellow),
        Span::styled(title_fill, dark_gray),
    ])];

    let inner = panel.width.saturating_sub(2) as usize;
    let visible = overlay_visible_rows();
    let offset = app.overlay_offset.min(app.files.len().saturating_sub(visible));
    for (i, file) in app.files.iter().enumerate().skip(offset).take(visible) {
        let n = app.comments.iter().filter(|c| c.file_path == *file).count();
        let current = i == app.current_file_index;
        let selected = i == app.overlay_cursor;
        let cursor_mark = if selected { "▸ " } else { "  " };
        let name_style = if selected {
            cyan.add_modifier(Modifier::BOLD)
        } else if current {
            yellow
        } else {
            Style::default().fg(Color::White)
        };
        let count_str = if n > 0 {
            format!("  ({n})")
        } else {
            String::new()
        };
        let mode_tag = if supports_view(file) { " [view]" } else { " [src]" };
        // External edit pending: the same ⚡ the title bar shows, so a
        // glance at the picker tells which files the agent touched.
        let changed_mark = if file_externally_changed(app, i) {
            " ⚡"
        } else {
            ""
        };
        // The row: basename when unique, else the shortest unique path
        // suffix; clip only when the panel is too narrow for the right
        // side (⚡ + count + mode tag).
        let suffix = unique_suffix(file, &app.files);
        let reserved = 2
            + UnicodeWidthStr::width(changed_mark)
            + UnicodeWidthStr::width(count_str.as_str())
            + mode_tag.len();
        let name = clip_if_needed(&suffix, inner.saturating_sub(reserved));
        lines.push(Line::from(vec![
            Span::styled(format!("{cursor_mark}{name}"), name_style),
            Span::styled(changed_mark, Style::default().fg(Color::Yellow)),
            Span::styled(count_str, dark_gray),
            Span::styled(mode_tag, dark_gray),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:move  Enter:switch  Esc/q:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// The all-comments list (`l`): every file's comments sorted by path then
/// start line, with jump (Enter) and delete (d).
/// The overlay title's leading spans: the comments | changes tab
/// indicator with the active tab highlighted, plus the shared directory
/// suffix. Both tab drawers lead with this, so the tabs read as one
/// element wherever the list is.
pub(crate) fn overlay_title_spans(
    app: &App,
    comments: usize,
    changes: usize,
    dir: Option<String>,
) -> Vec<Span<'static>> {
    let dark_gray = Style::default().fg(Color::DarkGray);
    let cyan = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let tab = |label: &str, n: usize, active: bool| {
        Span::styled(
            format!(" {label} ({n}) "),
            if active { cyan } else { dark_gray },
        )
    };
    let mut spans = vec![
        tab("comments", comments, app.overlay_tab == OverlayTab::Comments),
        Span::styled("|", dark_gray),
        tab("changes", changes, app.overlay_tab == OverlayTab::Changes),
    ];
    if let Some(dir) = dir {
        spans.push(Span::styled(format!("· {dir} "), yellow));
    }
    spans
}

/// The all-comments list (`l`): every file's comments sorted by path then
/// start line, with jump (Enter) and delete (d). Tab switches to the
/// changes tab (git hunks across every file).
pub(crate) fn draw_comments_overlay(f: &mut Frame, app: &App) {
    if app.overlay_tab == OverlayTab::Changes {
        draw_changes_overlay(f, app);
        return;
    }
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);

    let mut lines: Vec<Line> = Vec::new();
    let idx = sorted_comment_indices(app);
    let count = idx.len();
    // Distinct files that carry comments: the title shows their common
    // parent and each group header the shortest unique suffix, so no
    // path component repeats on every row.
    let mut comment_files: Vec<PathBuf> = Vec::new();
    for &raw in &idx {
        let file = &app.comments[raw].file_path;
        if !comment_files.contains(file) {
            comment_files.push(file.clone());
        }
    }
    let dir = (count > 0).then(|| common_parent(&comment_files).unwrap_or_default());
    let mut title = overlay_title_spans(app, count, changes_entries(app).len(), dir);
    let used: usize = title
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    title.push(Span::styled(
        "─".repeat(panel.width.saturating_sub(used as u16 + 2) as usize),
        dark_gray,
    ));
    lines.push(Line::from(title));

    if count == 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(" no comments yet", dark_gray)));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Tab:tab  Esc/q:close",
            dark_gray,
        )));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dark_gray);
        f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
        return;
    }

    let inner = panel.width.saturating_sub(2) as usize;
    // Scroll only when the list overflows; a list that fits never moves.
    let rows = overlay_rows(app);
    let visible = overlay_visible_rows();
    let offset = app.overlay_offset.min(rows.len().saturating_sub(visible));
    for (p, row) in rows.iter().enumerate().skip(offset).take(visible) {
        match row {
            // A group header: the (shortest unique) path and its comment
            // count, once per file.
            None => {
                let Some(first_entry) = rows[p..].iter().find_map(|r| *r) else {
                    continue;
                };
                let file = &app.comments[idx[first_entry]].file_path;
                let suffix = unique_suffix(file, &comment_files);
                let group_count = idx
                    .iter()
                    .filter(|&&raw| app.comments[raw].file_path == *file)
                    .count();
                lines.push(Line::from(vec![Span::styled(
                    format!(" {suffix} ({group_count})"),
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            Some(entry) => {
                let c = &app.comments[idx[*entry]];
                let range = c.range_label();
                let selected = *entry == app.overlay_cursor;
                let loc_style = if selected {
                    cyan.add_modifier(Modifier::BOLD)
                } else {
                    yellow
                };
                let cursor_mark = if selected { "▸ " } else { "  " };
                // First line of comment text (indented). Range and body
                // share one row — the colors already separate them
                // (yellow range, gray body), so each comment stays on a
                // single line.
                let body_first = c.text.lines().next().unwrap_or("");
                let body_style = if selected {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let revision = c
                    .revision
                    .as_deref()
                    .and_then(|context| context.split_whitespace().next())
                    .map(|short| format!(" [{short}]"))
                    .unwrap_or_default();
                let used = 2
                    + UnicodeWidthStr::width(range.as_str())
                    + UnicodeWidthStr::width(revision.as_str())
                    + 2;
                let budget = inner.saturating_sub(used);
                let body = clip_if_needed(body_first, budget);
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor_mark}{range}"), loc_style),
                    Span::styled(revision, Style::default().fg(Color::Magenta)),
                    Span::styled(format!("  {body}"), body_style),
                ]));
            }
        }
    }

    // Footer hints.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Tab:tab  j/k:move  Enter:jump  d:delete  Esc/q:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}

/// The changes tab of the `l` overlay: every session file's git hunks
/// (one row per hunk with its location, +N/-M, and a preview line), plus
/// untracked files. Enter jumps to the hunk (or the file's top for
/// untracked).
pub(crate) fn draw_changes_overlay(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let panel = overlay_panel(area);
    f.render_widget(Clear, panel);
    let dark_gray = Style::default().fg(Color::DarkGray);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);

    let mut lines: Vec<Line> = Vec::new();
    let entries = changes_entries(app);
    let count = entries.len();
    // Distinct files with changes, for the shared-dir title and the
    // shortest-unique-suffix group headers.
    let mut change_files: Vec<PathBuf> = Vec::new();
    for e in &entries {
        let file = &app.files[e.file()];
        if !change_files.contains(file) {
            change_files.push(file.clone());
        }
    }
    let dir = (count > 0).then(|| common_parent(&change_files).unwrap_or_default());
    let mut title = overlay_title_spans(app, app.comments.len(), count, dir);
    let used: usize = title
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    title.push(Span::styled(
        "─".repeat(panel.width.saturating_sub(used as u16 + 2) as usize),
        dark_gray,
    ));
    lines.push(Line::from(title));

    if count == 0 {
        lines.push(Line::from(""));
        // The empty state explains the scope (diff-scope step ②): Off
        // points at `m`, Last at the reload that has not happened yet.
        let empty = match app.scope {
            DiffScope::Off => " marks off — m: cycle scopes",
            DiffScope::Last => " no reload changes yet — r reloads",
            DiffScope::Git => " no changes yet (git diff vs HEAD)",
        };
        lines.push(Line::from(Span::styled(empty, dark_gray)));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Tab:tab  Esc/q:close",
            dark_gray,
        )));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dark_gray);
        f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
        return;
    }

    let inner = panel.width.saturating_sub(2) as usize;
    // Scroll only when the list overflows; a list that fits never moves.
    let rows = changes_rows(app);
    let visible = overlay_visible_rows();
    let offset = app.overlay_offset.min(rows.len().saturating_sub(visible));
    for (p, row) in rows.iter().enumerate().skip(offset).take(visible) {
        match row {
            // A group header: the (shortest unique) path and its change
            // count, once per file.
            None => {
                let Some(first_entry) = rows[p..].iter().find_map(|r| *r) else {
                    continue;
                };
                let file = &app.files[entries[first_entry].file()];
                let suffix = unique_suffix(file, &change_files);
                let group_count = entries
                    .iter()
                    .filter(|e| e.file() == entries[first_entry].file())
                    .count();
                lines.push(Line::from(vec![Span::styled(
                    format!(" {suffix} ({group_count})"),
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            Some(entry_pos) => {
                let selected = *entry_pos == app.overlay_cursor;
                let cursor_mark = if selected { "▸ " } else { "  " };
                let loc_style = if selected {
                    cyan.add_modifier(Modifier::BOLD)
                } else {
                    yellow
                };
                match &entries[*entry_pos] {
                    ChangesEntry::Hunk { file, source, hunk } => {
                        let diff =
                            entry_diff(app, *file, *source).expect("entry implies a diff");
                        let h = &diff.hunks[*hunk];
                        let (a, d) = h.counts();
                        let label = hunk_label(h, file_len(app, *file));
                        let counts = format!("+{a}/-{d}");
                        let preview = h.preview_line().unwrap_or("");
                        let used =
                            2 + UnicodeWidthStr::width(label.as_str()) + 2 + counts.len() + 1;
                        let budget = inner.saturating_sub(used);
                        let preview = clip_if_needed(preview, budget);
                        lines.push(Line::from(vec![
                            Span::styled(format!("{cursor_mark}{label}"), loc_style),
                            Span::styled(format!(" {counts}  {preview}"), dark_gray),
                        ]));
                    }
                    ChangesEntry::Untracked { file } => {
                        let n = file_len(app, *file);
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("{cursor_mark}untracked"),
                                loc_style,
                            ),
                            Span::styled(
                                format!("  {n} lines — all new"),
                                dark_gray,
                            ),
                        ]));
                    }
                }
            }
        }
    }

    // Footer hints.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Tab:tab  j/k:move  Enter:jump  Esc/q:close",
        dark_gray,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dark_gray);
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), panel);
}
