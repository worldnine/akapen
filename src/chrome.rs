//! The title/footer chrome: path truncation, the title-bar metrics and
//! drawer, footer hints, and the floating banner/toast/prompt rendering.

use std::borrow::Cow;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DiffScope, Mode};
use crate::clip_if_needed;
use crate::hunknav::{
    deleted_above_count, hunk_ref, scoped_diffs, scoped_hunk_refs, scoped_mark_sets,
};
use crate::git;

/// Smart-truncate a path for the title bar: keep the basename whole, add
/// directory components from the right while they fit, and collapse the
/// dropped remainder to `…/`. Never exceeds `max_cols` display columns
/// (unicode-aware, so CJK file names never overflow).
pub(crate) fn truncate_path(path: &Path, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if max_cols == 0 {
        return String::new();
    }
    let disp = path.display().to_string();
    if UnicodeWidthStr::width(disp.as_str()) <= max_cols {
        return disp;
    }
    // Prefer shorter spellings: cwd-relative, then ~-shortened.
    let mut candidates: Vec<String> = vec![disp.clone()];
    let cwd_rel = std::env::current_dir().ok().and_then(|cwd| {
        path.strip_prefix(&cwd)
            .ok()
            .map(|r| r.to_string_lossy().trim_start_matches('/').to_string())
            .filter(|r| !r.is_empty())
    });
    if let Some(rel) = cwd_rel {
        candidates.push(rel);
    }
    let home_rel = std::env::var("HOME")
        .ok()
        .and_then(|home| disp.strip_prefix(&home).map(|rest| format!("~{rest}")));
    if let Some(h) = home_rel {
        candidates.push(h);
    }
    let width = |s: &str| UnicodeWidthStr::width(s);
    let base = match candidates
        .iter()
        .filter(|c| width(c) <= max_cols)
        .max_by_key(|c| width(c))
    {
        Some(b) => b.clone(),
        None => candidates
            .into_iter()
            .min_by_key(|c| width(c))
            .unwrap_or(disp),
    };
    if width(&base) <= max_cols {
        return base;
    }
    // Drop directory components from the left; the basename stays whole.
    let components: Vec<&str> = base.split('/').collect();
    let file = components.last().copied().unwrap_or("");
    let dirs = &components[..components.len().saturating_sub(1)];
    let file_w = width(file);
    if file_w > max_cols {
        // Even the basename alone is too wide: clip it with a trailing `…`.
        return clip_ellipsis(file, max_cols);
    }
    // Re-add dirs from the right while the `…/` prefix still fits.
    let mut out = file.to_string();
    for d in dirs.iter().rev() {
        let candidate = format!("{d}/{out}");
        if width(&candidate) + 2 <= max_cols {
            out = candidate;
        } else {
            break;
        }
    }
    if out != file {
        format!("…/{out}")
    } else if !dirs.is_empty() && file_w + 2 <= max_cols {
        // The file name is deep in a tree and there is room for the marker:
        // `…/name.md` reads better than a bare `name.md`.
        format!("…/{file}")
    } else {
        out
    }
}

/// Clip `s` from the left to `max_cols` columns and end with `…`.
pub(crate) fn clip_ellipsis(s: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max_cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        if w + cw > max_cols.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if !s.is_empty() && w < max_cols {
        out.push('…');
    }
    out
}

/// What clicking a title-bar element does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TitleHit {
    /// The file path: copy the full path to the clipboard.
    Path,
    /// The `1/3 files` counter: open the file picker.
    FileCount,
    /// The `▌ N` counter: open the comment list.
    CommentCount,
}

/// Title-bar layout: the x extents of every clickable element, computed
/// once and shared by [`draw_title`] and the mouse hit-testing so a click
/// always lands exactly on what is drawn.
#[derive(Debug)]
pub(crate) struct TitleMetrics {
    /// The change badge (⚡ / +N/-M) and its width.
    pub(crate) change: String,
    pub(crate) change_w: u16,
    /// The truncated path text (click → copy full path).
    pub(crate) path: String,
    pub(crate) path_w: u16,
    /// The `1/3 files` counter (click → file picker) and its x/width.
    pub(crate) file_count: String,
    pub(crate) file_count_x: u16,
    pub(crate) file_count_w: u16,
    /// The y/s hint (not clickable) and its x/width.
    pub(crate) ys: String,
    pub(crate) ys_x: u16,
    pub(crate) ys_w: u16,
    /// Whether the y/s hint is shown (it yields to a long path).
    pub(crate) show_ys: bool,
    /// The `▌ N` counter (click → comment list) and its x/width.
    pub(crate) indicator: String,
    pub(crate) indicator_x: u16,
    pub(crate) indicator_w: u16,
    /// The `esc close` badge (esc-quit enabled only, not clickable) and
    /// its x/width. Flush right, to the right of the counter.
    pub(crate) esc_close: String,
    pub(crate) esc_close_x: u16,
    pub(crate) esc_close_w: u16,
}

/// The layout math for the title bar, shared by drawing and hit-testing
/// so they can never disagree.
pub(crate) fn title_metrics(app: &App, width: u16) -> TitleMetrics {
    let indicator = if app.comments.is_empty() {
        String::new()
    } else {
        format!(" ▌ {} ", app.comments.len())
    };
    let indicator_w = UnicodeWidthStr::width(indicator.as_str()) as u16;
    // `esc close`: the esc-quit affordance, shown only while it is real
    // (esc-quit enabled, and not composing — Esc cancels the composer
    // there). It is the top-right element, right of the comment counter;
    // the path yields to it before the y/s hint does.
    let esc_close = if app.esc_quit_enabled() && app.mode != Mode::Input {
        " esc close ".to_string()
    } else {
        String::new()
    };
    let esc_close_w = UnicodeWidthStr::width(esc_close.as_str()) as u16;
    // The y/s explainer reads the room: with no comments there is nothing
    // to copy or send (both keys flash "no comments yet"), and without a
    // send target (--send-cmd or --send-agent) there is nothing to send —
    // show only what is actionable. Still low priority: it right-aligns
    // before the indicator only when the path keeps ≥16 columns, and it
    // is hidden while composing (y/s are typing keys there).
    let ys = if app.mode == Mode::Input || app.comments.is_empty() {
        String::new()
    } else {
        let mut parts = vec!["y copy"];
        if app.config.send_cmd.is_some() || app.config.send_agent {
            parts.push("s send");
        }
        format!(" {}", parts.join(" · "))
    };
    let ys_w = UnicodeWidthStr::width(ys.as_str()) as u16;
    let cluster_w = indicator_w + ys_w + esc_close_w + 1;
    let ys_x = width.saturating_sub(cluster_w);
    let show_ys = ys_w > 0 && ys_x >= 16;
    let path_area_end = if show_ys {
        ys_x
    } else {
        width.saturating_sub(indicator_w + esc_close_w)
    };
    // While an external edit is pending the title shows ⚡ (it outranks
    // every scope's count); otherwise the active scope picks the badge
    // (diff-scope step ①): Last → the reload's +N/-M, Git → the git
    // snapshot's g+K/-L, Off → nothing.
    let change = if app.file_changed {
        " ⚡ ".to_string()
    } else {
        scoped_change_badge(app)
    };
    let change_w = UnicodeWidthStr::width(change.as_str()) as u16;
    // `1/3 files`: the current position in the session, matching the ]/[
    // navigation model (vim's `1/3` in the arg list). Hidden for a single
    // file. Click → file picker.
    let file_count = if app.files.len() > 1 {
        format!(
            " {}/{} {}",
            app.current_file_index + 1,
            app.files.len(),
            if app.config.reply { "msgs" } else { "files" }
        )
    } else {
        String::new()
    };
    let file_count_w = UnicodeWidthStr::width(file_count.as_str()) as u16;
    let path_max = path_area_end.saturating_sub(change_w + file_count_w + 1);
    let path = if app.config.reply {
        // Reply mode: the doc is a temp copy of the agent's message — the
        // path is noise; the label says what this pane is for.
        "reply".to_string()
    } else {
        truncate_path(app.current_file_path(), path_max as usize)
    };
    let path_w = UnicodeWidthStr::width(path.as_str()) as u16;
    TitleMetrics {
        change,
        change_w,
        path,
        path_w,
        file_count_x: change_w + 1 + path_w,
        file_count,
        file_count_w,
        ys_x,
        ys,
        ys_w,
        show_ys,
        indicator_x: width.saturating_sub(indicator_w + esc_close_w),
        indicator,
        indicator_w,
        esc_close_x: width.saturating_sub(esc_close_w),
        esc_close,
        esc_close_w,
    }
}

/// Which title-bar element the column `x` hits, using the same layout
/// math as [`title_metrics`].
pub(crate) fn title_hit_at(app: &App, width: u16, x: u16) -> Option<TitleHit> {
    let m = title_metrics(app, width);
    if m.indicator_w > 0 && x >= m.indicator_x && x < m.indicator_x + m.indicator_w {
        Some(TitleHit::CommentCount)
    } else if m.file_count_w > 0 && x >= m.file_count_x && x < m.file_count_x + m.file_count_w {
        Some(TitleHit::FileCount)
    } else if m.path_w > 0 && x > m.change_w && x < m.change_w + 1 + m.path_w {
        Some(TitleHit::Path)
    } else {
        None
    }
}

/// The title is file-centric: path + change state + session position. The
/// mode badge lives in the footer (statusline convention). The top-right
/// corner carries the comment-presence indicator with the `esc close`
/// badge to its right (esc-quit only); the y/s explainer sits between
/// (low priority, vanishes when the path needs the room). The path, the
/// file counter, and the comment counter are clickable buttons (see
/// on_mouse).
pub(crate) fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let m = title_metrics(app, area.width);
    if m.change_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.change,
                Style::default().fg(Color::Yellow),
            ))),
            Rect {
                x: area.x,
                y: area.y,
                width: m.change_w,
                height: 1,
            },
        );
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::raw(m.path))),
        Rect {
            x: area.x + m.change_w + 1,
            y: area.y,
            width: m.path_w,
            height: 1,
        },
    );
    if m.file_count_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.file_count,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: area.x + m.file_count_x,
                y: area.y,
                width: m.file_count_w,
                height: 1,
            },
        );
    }
    if m.show_ys {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.ys,
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                x: area.x + m.ys_x,
                y: area.y,
                width: m.ys_w,
                height: 1,
            },
        );
    }
    if m.indicator_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.indicator,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: area.x + m.indicator_x,
                y: area.y,
                width: m.indicator_w,
                height: 1,
            },
        );
    }
    // The esc-quit affordance: a filled badge (dark gray background) at
    // the very top-right. It is not clickable; Esc does the work itself.
    if m.esc_close_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                m.esc_close,
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ))),
            Rect {
                x: area.x + m.esc_close_x,
                y: area.y,
                width: m.esc_close_w,
                height: 1,
            },
        );
    }
}

/// The title's change badge for the active scope (diff-scope step ①).
/// Last shows the last reload's `+N/-M`; Git shows the git
/// snapshot's `g+K/-L` — the sum of every hunk's [`Hunk::counts`] — and
/// nothing for an untracked file (no ref side) or an empty diff; Off
/// shows nothing. The pending ⚡ badge takes precedence (caller).
fn scoped_change_badge(app: &App) -> String {
    match app.scope {
        DiffScope::Last => match app.last_change {
            Some((a, r)) => format!(" +{a}/-{r} "),
            None => String::new(),
        },
        DiffScope::Git => {
            let Some(diff) = &app.git_diff else { return String::new() };
            if diff.untracked || diff.hunks.is_empty() {
                return String::new();
            }
            let (k, l) = diff.hunks.iter().fold((0, 0), |(k, l), h| {
                let (a, d) = h.counts();
                (k + a, l + d)
            });
            format!(" g+{k}/-{l} ")
        }
        DiffScope::Off => String::new(),
    }
}

/// The footer's scope badge (diff-scope step ①): `m:last` / `m:git` /
/// `m:off`, appended to the hints so the scope is always visible
/// (spec 3-4). Hidden outside a git repository while Last — the
/// default there, and the non-git session must look exactly as before
/// (P1) — and in reply mode, where the scope machinery is off.
fn scope_footer_hint(app: &App) -> Option<&'static str> {
    if app.config.reply {
        return None;
    }
    match (app.scope, app.git_diff.is_some()) {
        (DiffScope::Last, false) => None,
        (DiffScope::Last, true) => Some("m:last"),
        (DiffScope::Git, _) => Some("m:git"),
        (DiffScope::Off, _) => Some("m:off"),
    }
}

/// The hunk hint leading the footer when the cursor sits on a change:
/// inside a scoped hunk (context lines included) the hunk's range, scale
/// and action — `hunk L29-33 +1/-2 · o old side` (zero count sides
/// omitted; a pure-deletion hunk shows its owner line) — or, outside
/// every hunk on a deletion mark row (reload-only marks, or a mark the
/// EOF clamp dropped outside every hunk range), the count-bearing
/// `-N deleted above · o: old side` / count-less `deleted above · o: old
/// side`. Deletion marks inside a hunk are covered by the counts' `-N`,
/// so no separate mention is added. The hunk/rows resolve through the
/// ACTIVE SCOPE (diff-scope step ④): Off shows no hint. On-demand only
/// (cursor-anchored): no always-on information (P2).
fn hunk_hint(app: &App) -> Option<String> {
    let line = match app.mode {
        Mode::View => app.view.cursor,
        Mode::Source => app.cursor,
        Mode::Input => return None,
    };
    let new_len = app.source.len();
    // Cursor inside a scoped hunk: the hunk display wins over the
    // deletion hint (the deletion count is visible in the counts).
    for r in scoped_hunk_refs(app) {
        let h = hunk_ref(app, r)?;
        let covers = match h.new_range() {
            Some((a, b)) => line >= a && line <= b,
            None => line == h.owner(new_len),
        };
        if covers {
            return Some(hunk_range_hint(h, new_len));
        }
    }
    // A deletion mark row outside every hunk: the mark must be one the
    // scope actually shows.
    let (_, _, deleted) = scoped_mark_sets(app);
    if !deleted.contains(&line) {
        return None;
    }
    // The count sums over the scope's diffs.
    let count: usize = scoped_diffs(app)
        .iter()
        .map(|d| deleted_above_count(d, line, new_len))
        .sum();
    Some(if count > 0 {
        format!("-{count} deleted above · o: old side")
    } else {
        "deleted above · o: old side".to_string()
    })
}

/// `hunk L{a}-{b} +A/-D · o old side`: the hunk's 1-based new range
/// (the owner line alone for a pure-deletion hunk) and its added/deleted
/// counts ([`git::Hunk::counts`], zero sides omitted).
fn hunk_range_hint(hunk: &git::Hunk, new_len: usize) -> String {
    let range = match hunk.new_range() {
        Some((a, b)) => format!("L{}-{}", a + 1, b + 1),
        None => format!("L{}", hunk.owner(new_len) + 1),
    };
    let (added, deleted) = hunk.counts();
    let mut counts = String::new();
    if added > 0 {
        counts.push_str(&format!("+{added}"));
    }
    if deleted > 0 {
        if !counts.is_empty() {
            counts.push('/');
        }
        counts.push_str(&format!("-{deleted}"));
    }
    if counts.is_empty() {
        format!("hunk {range} · o old side")
    } else {
        format!("hunk {range} {counts} · o old side")
    }
}

/// The footer's mode hint: the cursor's position as `L{line}/{total}`
/// (1-based source line — the cursor IS the review anchor, so the line
/// number is more actionable than a %), a few labeled actions for the
/// current context, then `? help` for the full key reference. Keys keep
/// their relative order across modes so a mode switch never rearranges
/// the hints. A change row under the cursor leads the hints with the
/// hunk / `deleted above` snippet (see [`hunk_hint`]) — it is prepended,
/// so the footer's existing right-edge truncation drops the generic hints
/// first when the terminal is narrow.
pub(crate) fn footer_hints(app: &App) -> String {
    let pos = |line: usize, total: usize| {
        if total == 0 {
            "L0/0".to_string()
        } else {
            format!("L{}/{}", line + 1, total)
        }
    };
    let lead = hunk_hint(app)
        .map(|d| format!("{d} · "))
        .unwrap_or_default();
    let hints = match app.mode {
        Mode::Input => "Enter confirm · ^j newline · ←→↑↓ move · Esc cancel".to_string(),
        Mode::View => {
            let p = pos(app.view.cursor, app.source.len());
            // With a selection active, j/k EXTENDS it (the parallel model —
            // same as source mode); the footer must say so, or "j/k
            // scroll" silently grows the range after a Tab handoff. Esc
            // cancels the selection — spelled out, since it is the way
            // out of the SELECT state.
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    format!("{lead}{p} · {}–{} · j/k extend · c comment · Esc cancel · ? help", a + 1, b + 1)
                }
                None => format!("{lead}{p} · j/k scroll · v select · c comment · ? help"),
            }
        }
        Mode::Source => {
            let p = pos(app.cursor, app.source.len());
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    format!("{lead}{p} · {}–{} · j/k extend · c comment · Esc cancel · ? help", a + 1, b + 1)
                }
                None => format!("{lead}{p} · j/k move · v select · c comment · ? help"),
            }
        }
    };
    // The scope badge rides the hints' tail (diff-scope step ①).
    match scope_footer_hint(app) {
        Some(scope) => format!("{hints} · {scope}"),
        None => hints,
    }
}

pub(crate) fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        footer_hints(app),
        Style::default().fg(Color::DarkGray),
    )];
    // The mode badge leads the footer (statusline convention): the title
    // above is file-centric, this is where the mode is read at a glance.
    // Color semantics: gray = view (calm reading), blue = source (the raw
    // editor), cyan = comment input (same as the composer bubble), magenta
    // = selection active (the transient `v` state — the badge flips to
    // SELECT so the mode is unmissable, and Esc cancels it). Yellow is
    // comments only, everywhere (the count lives in the top-right `▌ N`
    // indicator). All badges are width 8, so the hints never shift when
    // the mode changes.
    let (badge, badge_style) = match app.mode {
        Mode::Input => (
            format!("{:^8}", "COMMENT"),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Mode::View | Mode::Source if app.selection.is_some() => (
            format!("{:^8}", "SELECT"),
            Style::default().fg(Color::Black).bg(Color::LightMagenta),
        ),
        Mode::View => (
            format!("{:^8}", "VIEW"),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Mode::Source => (
            format!("{:^8}", "SOURCE"),
            Style::default().fg(Color::Black).bg(Color::LightBlue),
        ),
    };
    spans.insert(0, Span::styled(badge, badge_style));
    // Prompts (quit/edit confirmations, file-change) and transient toasts
    // are NOT in the footer: they float over the content as top-center
    // banners (see draw_prompt / draw_toast), so the hints never get
    // displaced.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A top-center banner over the content (row 1): the toast and the
/// persistent prompts share this rendering. Clear erases only the
/// banner's own rect — the layout never shifts, nothing scrolls.
/// A banner over the content at the given row: transient toasts float at
/// the BOTTOM (one row above the footer — the classic message-line
/// position), persistent prompts stay at the TOP. Clear erases only the
/// banner's own rect — the layout never shifts, nothing scrolls. Errors
/// render red (info stays yellow).
pub(crate) fn draw_banner(f: &mut Frame, area: Rect, row: u16, msg: &str, is_error: bool) {
    use ratatui::widgets::Clear;
    let w = UnicodeWidthStr::width(msg) as u16 + 2;
    let w = w.min(area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let rect = Rect {
        x,
        y: area.y + row,
        width: w,
        height: 1,
    };
    f.render_widget(Clear, rect);
    let text = clip_if_needed(msg, w.saturating_sub(2) as usize);
    let fg = if is_error { Color::Red } else { Color::Yellow };
    let style = Style::default()
        .fg(fg)
        .add_modifier(Modifier::BOLD)
        .bg(Color::Black);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text} "), style))),
        rect,
    );
}

/// A transient message floating at the bottom, one row above the footer
/// (vim's message-line position). Red = an operation that could not be
/// done (flash_err also beeped).
pub(crate) fn draw_toast(f: &mut Frame, app: &App) {
    if let Some((msg, _, is_error)) = &app.status {
        let h = f.area().height;
        draw_banner(f, f.area(), h.saturating_sub(2), msg, *is_error);
    }
}

/// The persistent prompt text, if any: quit confirmation, edit
/// confirmation, reload confirmation, or a pending file change.
/// Priority: quit > edit > reload > change (the old footer order). The
/// quit line advertises the Esc binding that is actually active.
pub(crate) fn prompt_message(app: &App) -> Option<Cow<'static, str>> {
    if app.confirm_quit {
        if app.esc_quit_enabled() {
            Some("unsent comments — Esc/q to quit".into())
        } else {
            Some("unsent comments — q to quit, Esc to cancel".into())
        }
    } else if app.confirm_edit {
        Some("unsent comments — e again to edit & clear, Esc to cancel".into())
    } else if app.confirm_reload {
        Some("unsent comments — r again to reload & clear, Esc to cancel".into())
    } else if app.file_changed {
        Some("file changed — r reload · i ignore".into())
    } else {
        None
    }
}

/// The persistent prompt banner: TOP-center, yellow. Prompts demand an
/// action and must not be missed; transient toasts live at the bottom, so
/// the two never clash.
pub(crate) fn draw_prompt(f: &mut Frame, app: &App) {
    if let Some(msg) = prompt_message(app) {
        draw_banner(f, f.area(), 1, &msg, false);
    }
}

#[cfg(test)]
mod width_tests {
    use crate::view_render_width;

    #[test]
    fn view_render_width_matches_the_pane() {
        // The view always draws its frame: the render width is the terminal
        // minus the page's left margin, the two border columns, and the
        // text column's 1-column pads on each side (the marker column
        // rides the left border, reserving no width). The startup render
        // and the resize re-render both go through this, so they can't
        // disagree.
        assert_eq!(view_render_width(100), 95);
        assert_eq!(view_render_width(1), 0, "clamps at zero");
        assert_eq!(view_render_width(0), 0);
    }
}

#[cfg(test)]
mod title_tests {
    use crate::truncate_path;
    use std::path::Path;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn short_paths_pass_through() {
        assert_eq!(truncate_path(Path::new("a.md"), 80), "a.md");
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 80),
            "wiki/cases/aozora-plan.md"
        );
    }

    #[test]
    fn keeps_the_basename_whole() {
        // Narrow: drops dirs, never splits the file name.
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 20),
            "…/aozora-plan.md"
        );
    }

    #[test]
    fn adds_dirs_from_the_right() {
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 22),
            "…/cases/aozora-plan.md"
        );
    }

    #[test]
    fn whole_path_when_enough_room() {
        assert_eq!(
            truncate_path(Path::new("wiki/cases/aozora-plan.md"), 30),
            "wiki/cases/aozora-plan.md"
        );
    }

    #[test]
    fn clips_when_the_basename_alone_is_too_wide() {
        let out = truncate_path(Path::new("a-very-long-file-name.md"), 10);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn zero_max_is_empty() {
        assert_eq!(truncate_path(Path::new("a.md"), 0), "");
    }

    #[test]
    fn cjk_width_counts_as_wide() {
        // 日本語ドキュメント (9 × 2 cols) + ".md" would overflow 8 cols;
        // the result must stay inside the budget.
        let out = truncate_path(Path::new("日本語ドキュメント.md"), 8);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 8);
    }
}
