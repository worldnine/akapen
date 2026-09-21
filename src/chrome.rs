//! The title/footer chrome: path truncation, the title-bar metrics and
//! drawer, footer hints, and the message-row rendering (prompt/toast).

use std::borrow::Cow;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::clip_if_needed;

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

/// Clip a history label (`COMMIT · 2/5 · 5b5f349 · subject`) or a
/// tooltip summary: the free-form tail is the most expendable part, so
/// it is cut first — a structured head stays whole, and a trailing
/// ` · base N/M` (the review baseline context) survives even when the
/// summary above it must go. Never exceeds `max_cols` display columns.
pub(crate) fn clip_title_label(label: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if UnicodeWidthStr::width(label) <= max_cols {
        return label.to_string();
    }
    // The baseline context rides the tail; keep it when clipping.
    let (head, base) = match label.rsplit_once(" · base ") {
        Some((head, base)) => (head, Some(format!(" · base {base}"))),
        None => (label, None),
    };
    let base_w = base.as_ref().map_or(0, |s| UnicodeWidthStr::width(s.as_str()));
    let budget = max_cols.saturating_sub(base_w + 1); // +1 for the ellipsis
    let mut out = String::new();
    let mut w = 0usize;
    for ch in head.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1);
        if w + cw > budget {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if w < UnicodeWidthStr::width(head) {
        out.push('…');
    }
    out.push_str(base.as_deref().unwrap_or(""));
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
    /// The pending/review/history badge (⚡ / ! N / revision) and its width.
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
    // Historical generations keep only a tiny `◆ 3/7` badge here: the
    // full readout (provenance · id · age · summary) lives in the
    // scrubber tooltip next to the timeline's `◆`, and the purple frame
    // already says "you are in the past". The badge is the fallback
    // identity for narrow terminals where the bar never appears. At NOW
    // an external edit still outranks everything; otherwise the
    // baseline gets a compact persistent identity of its own.
    let change = if app.is_historical() {
        app.history()
            .map(|history| {
                format!(
                    " ◆ {}/{} ",
                    history.revisions.len().saturating_sub(history.position),
                    history.revisions.len()
                )
            })
            .unwrap_or_default()
    } else if app.file_changed {
        " ⚡ ".to_string()
    } else if app
        .history()
        .is_some_and(|history| history.baseline_position() == Some(history.position))
    {
        " BASELINE ".to_string()
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
/// The title's neutral count badge.
/// The text is identical to the plain badge, so the width math and the
/// click areas in [`title_metrics`] are unaffected.
fn change_badge_spans(change: &str) -> Vec<Span<'static>> {
    let base = Style::default().fg(Color::Yellow);
    vec![Span::styled(change.to_string(), base)]
}

pub(crate) fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let m = title_metrics(app, area.width);
    if m.change_w > 0 {
        f.render_widget(
            Paragraph::new(Line::from(change_badge_spans(&m.change))),
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

/// The number of unreviewed blocks or lines. The pending ⚡ badge takes
/// precedence in the caller. `!` is the classic "needs attention" mark:
/// a count badge, not a bullet. Same width as the old `●` badge, so the
/// path budget in [`title_metrics`] is untouched.
fn scoped_change_badge(app: &App) -> String {
    let count = app.file_review_count(app.current_file_index);
    if count == 0 {
        String::new()
    } else {
        format!(" ! {count} ")
    }
}

/// The footer's mode hint: the cursor's position as `L{line}/{total}`
/// (1-based source line — the cursor IS the review anchor, so the line
/// number is more actionable than a %), a few labeled actions for the
/// current context, then `? help` for the full key reference. Keys keep
/// their relative order across modes so a mode switch never rearranges
/// the hints.
pub(crate) fn footer_hints(app: &App) -> String {
    let pos = |line: usize, total: usize| {
        if total == 0 {
            "L0/0".to_string()
        } else {
            format!("L{}/{}", line + 1, total)
        }
    };
    // `READ 73%` rides beside the position: both describe the document
    // being read, not the mode. Shown ONLY while a semantic annotation is
    // actually loaded — without one the budget exists as a number but
    // means nothing, and advertising it would promise keys that refuse.
    //
    // `analyzing…` is the exception: an external `--semantic-cmd` shells out
    // and goes over the network, and a status line that says nothing for
    // that long is indistinguishable from a feature that does not work. It rides
    // beside the budget because the budget is what the answer will act
    // on — and it appears even before the first annotation exists, which
    // is exactly when the silence would be most confusing.
    let read = |p: String| match (&app.semantic_doc, app.semantic_inflight) {
        (_, Some(_)) => format!("{p} · READ {}% · analyzing…", app.reading_budget),
        (Some(_), None) => format!("{p} · READ {}%", app.reading_budget),
        (None, None) => p,
    };
    let hints = match app.mode {
        Mode::Input => "Enter confirm · ^j newline · ←→↑↓ move · Esc cancel".to_string(),
        Mode::View => {
            let p = read(pos(app.view.cursor, app.source.len()));
            // With a selection active, j/k EXTENDS it (the parallel model —
            // same as source mode); the footer must say so, or "j/k
            // scroll" silently grows the range after a Tab handoff. Esc
            // cancels the selection — spelled out, since it is the way
            // out of the SELECT state.
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    format!("{p} · {}–{} · j/k extend · c comment · Esc cancel · ? help", a + 1, b + 1)
                }
                None => format!("{p} · j/k scroll · v select · c comment · ? help"),
            }
        }
        Mode::Source => {
            let p = read(pos(app.cursor, app.source.len()));
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    format!("{p} · {}–{} · j/k extend · c comment · Esc cancel · ? help", a + 1, b + 1)
                }
                None => format!("{p} · j/k move · v select · c comment · ? help"),
            }
        }
    };
    // The same document timeline is available in rendered and source mode;
    // the generation label itself lives in the title bar's state slot
    // (provenance, position, id, baseline context) — one place, so the
    // top and bottom never show the same text. The footer keeps the
    // short navigation affordance only. `t` opens the full revision list
    // whenever a timeline actually exists (more than one point). Reply
    // mode has no timeline at all — `t`/`←`/`→` all flash "history
    // unavailable" — so advertising those keys would be lying.
    if matches!(app.mode, Mode::View | Mode::Source) {
        if app.config.reply {
            hints
        } else if app.history().is_some_and(|history| history.revisions.len() > 1) {
            format!("{hints} · t detail · ← older · newer →")
        } else {
            format!("{hints} · ← older · newer →")
        }
    } else {
        hints
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
    // render on the message row directly above this strip (see
    // draw_message) — never inside the footer, so the hints never get
    // displaced.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A centered banner inside `area` at `row`: black background, bold,
/// one row tall. Clear erases only the banner's own rect — the layout
/// never shifts, nothing scrolls. Errors render red (info stays yellow).
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

/// The persistent prompt text, if any: the quit confirmation or a pending
/// file change. Priority: quit > change.
pub(crate) fn prompt_message(app: &App) -> Option<Cow<'static, str>> {
    if app.confirm_quit {
        if app.esc_quit_enabled() {
            Some("unsent comments — Esc/q to quit".into())
        } else {
            Some("unsent comments — q to quit, Esc to cancel".into())
        }
    } else if app.file_changed {
        Some("file changed — r reload first · i ignore".into())
    } else {
        None
    }
}

/// The one message row, at the bottom edge — the row just above the
/// footer (vim's message-line position), shared by the persistent prompt
/// and the transient toast. The prompt demands an action and must not be
/// missed, so it outranks the toast: a confirmation stays until
/// answered, while a toast would expire on its own. In view mode this
/// row is the frame's bottom border (the corners stay, so the message
/// reads as a status strip built into the page edge); in source mode it
/// floats over the last content row. Either way the row exists in the
/// layout already — no space is reserved, nothing shifts. Toasts keep
/// their color semantics (yellow = info, red = an operation that could
/// not be done — flash_err also beeped).
pub(crate) fn draw_message(f: &mut Frame, app: &App) {
    // The browsing timeline bar owns the bottom two rows (footer and
    // frame border); the message row floats directly above it so
    // prompts and toasts read as part of the bar instead of hovering
    // over the document.
    let row = if crate::timeline::timeline_active(app) {
        f.area().height.saturating_sub(3)
    } else {
        f.area().height.saturating_sub(2)
    };
    if let Some(msg) = prompt_message(app) {
        draw_banner(f, f.area(), row, &msg, false);
    } else if let Some((msg, _, is_error)) = &app.status {
        draw_banner(f, f.area(), row, msg, *is_error);
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
    use super::{change_badge_spans, clip_title_label};
    use crate::truncate_path;
    use ratatui::style::Color;
    use std::path::Path;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn review_badge_is_one_neutral_span() {
        let spans = change_badge_spans(" ! 3 ");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(spans[0].content.as_ref(), " ! 3 ");
    }

    #[test]
    fn history_label_is_clipped_to_keep_the_path_visible() {
        // The full label would crowd the path out of the bar; the summary
        // tail is cut while the provenance/position/id head survives.
        let label = "COMMIT · 2/5 · 5b5f349 · akapen 初版: markdown 行コメント TUI";
        let clipped = clip_title_label(label, 40);
        assert!(UnicodeWidthStr::width(clipped.as_str()) <= 40);
        assert!(clipped.starts_with("COMMIT · 2/5 · 5b5f349"));
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn history_label_clipping_keeps_the_baseline_context() {
        // ` · base N/M` is the review reference point: it survives the
        // clip even when the summary above it is cut away.
        let label = "COMMIT · 2/5 · 5b5f349 · a long subject line that must go · base 1/5";
        let clipped = clip_title_label(label, 30);
        assert!(UnicodeWidthStr::width(clipped.as_str()) <= 30);
        assert!(clipped.contains("base 1/5"));
        assert!(clipped.starts_with("COMMIT · 2/5"));
    }

    #[test]
    fn short_history_labels_pass_through_unclipped() {
        let label = "COMMIT · 4/5 · 5b5f349";
        assert_eq!(clip_title_label(label, 40), label);
    }

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

#[cfg(test)]
mod footer_tests {
    use super::footer_hints;
    use crate::app::{App, Mode};
    use crate::config::{Config, EscQuit};
    use crate::highlight::Highlighter;
    use crate::ime::ImeMode;
    use crate::source::Source;
    use crate::view::ViewState;

    /// A minimal app for the footer: a temp file with a little content,
    /// in source mode (the time-hint block matches View and Source).
    fn footer_app(reply: bool) -> App {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
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
            decoration_blend: Default::default(),
            decorations: Vec::new(),
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight, Default::default());
        let mut app = App::new(config, source, highlight, view, false);
        app.mode = Mode::Source;
        app
    }

    #[test]
    fn reply_footer_hides_the_unavailable_time_moves() {
        // Reply mode has no history: `t`/`←`/`→` all error, so the
        // footer must not advertise them.
        let hints = footer_hints(&footer_app(true));
        assert!(!hints.contains("t detail"), "no t detail in reply mode");
        assert!(!hints.contains("← older"), "no ← in reply mode");
        assert!(!hints.contains("newer →"), "no → in reply mode");
        // The source-mode hints themselves stay.
        assert!(hints.contains("j/k move"));
        assert!(hints.contains("? help"));
    }

    #[test]
    fn non_reply_footer_keeps_the_time_moves() {
        let hints = footer_hints(&footer_app(false));
        assert!(
            hints.contains("← older"),
            "the timeline hint stays outside reply mode"
        );
        assert!(hints.contains("newer →"));
    }
}
