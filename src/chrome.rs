//! The title/footer chrome: path truncation, the title-bar metrics and
//! drawer, footer hints, and the message-row rendering (prompt/toast).

use std::borrow::Cow;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    let cluster_end = if show_ys {
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
    // **タイトル行に marks の読み出しは無い**（2026-09-22。読み手の決定）。
    // 長いファイル名のとき、読み出しが丸ごと引き下がって「あるはずの値が
    // 消える」形になっていた。path は文書の身元なので譲れない — だから
    // 読み出しの方がフッタの右端へ引っ越した（`footer_metrics`）。
    // ここは path に全部渡す。
    let path_max = cluster_end.saturating_sub(change_w + file_count_w + 1);
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

// ---- フッタ ------------------------------------------------------------
//
// フッタは 3 つの取り分でできている: 左から**モードバッジ**（幅 8 固定）、
// **キー案内**、そして右端に寄る**marks の読み出し**である。
//
// **読み出しは 2026-09-22 にタイトル行の右から引っ越してきた。** 向こうでは
// ファイル名が長いだけで読み出しが丸ごと引き下がっていて、「あるはずの値が
// 消える」形になっていた。path は文書の身元なので譲れない — 譲れるのは
// キー案内の方で、こちらは項目を落としても残りが読める。

/// キー案内の 1 項目。
pub(crate) struct FooterHint {
    pub(crate) text: String,
    /// **最後まで落とさない項目。** View / Source では位置（`L12/80`）と
    /// `? help` の 2 つ — どこにいるかと、残りをどう調べるか。composer では
    /// 確定（`Enter …`）と取り消し（`Esc cancel`）で、入力中に出入りの
    /// 仕方が消えるのは事故である。
    pub(crate) keep: bool,
}

fn hint(text: impl Into<String>) -> FooterHint {
    FooterHint { text: text.into(), keep: false }
}

fn kept(text: impl Into<String>) -> FooterHint {
    FooterHint { text: text.into(), keep: true }
}

/// **フッタ右下の読み出し** — `Essential · 20%` と、座布団に乗る本数。
///
/// 2 つに割れているのは**色が違う**からである。問いの名前と % は薄い灰
/// （`1/3 files` や `y copy` と同じ薄さ）で、本数だけがマーカーと同じ琥珀の
/// 座布団に乗る。いちばん小さくて意味がある値なので、狭くなったときに
/// 最後まで残るのもこちらである。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Readout {
    /// 薄く出る文。問いの名前と `20%`、または `analyzing…` のような状態。
    pub(crate) dim: String,
    /// 光っている Unit の本数。状態を述べているだけのとき（解析中・
    /// スコア無し）は `None`。
    pub(crate) lit: Option<usize>,
}

impl Readout {
    /// 本数の字。**前後 1 桁の余白込み**で、そのまま座布団の幅になる。
    pub(crate) fn count(&self) -> String {
        self.lit.map(|n| format!(" {n} ")).unwrap_or_default()
    }

    /// 座布団を敷くか。**0 本は敷かない** — 光っている箇所が無いのに
    /// 琥珀の四角が出ると、「何かある」と言ってしまう。同じ形のまま
    /// 薄く出る（幅が飛ばないので、つまみが 0 を跨いでも行が揺れない）。
    pub(crate) fn cushioned(&self) -> bool {
        self.lit.is_some_and(|n| n > 0)
    }

    pub(crate) fn width(&self) -> usize {
        UnicodeWidthStr::width(self.dim.as_str())
            + UnicodeWidthStr::width(self.count().as_str())
    }

    /// 平文。**描画は通らない**（色を分けるので `dim` と `count` を
    /// 別々に組む）— 読み出しを 1 本の文字列として突き合わせたい
    /// テストのための口である。
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        format!("{}{}", self.dim, self.count())
    }
}

/// フッタの取り分。[`draw_footer`] と、読み出しのフラッシュが乗る面を
/// 探す `main` が**同じ計算**を通る（光る場所と書いてある場所がずれない）。
#[derive(Debug)]
pub(crate) struct FooterLayout {
    /// バッジの右に出すキー案内（落としたあと）。
    pub(crate) hints: String,
    /// 右端の読み出し。丸ごと引き下がったら `None`。
    pub(crate) readout: Option<Readout>,
    /// 読み出しの開始桁。
    pub(crate) readout_x: u16,
    /// **300 ms のフラッシュが乗る面**（`crate::effects::readout_flash_effect`）。
    /// 座布団があればその矩形だけ、無ければ読み出し全体。読み出しが
    /// 出ていなければ幅 0 で、演出は空振りして消える。
    pub(crate) flash_x: u16,
    pub(crate) flash_w: u16,
}

/// 生きている項目を ` · ` でつなぐ。
fn join_hints(hints: &[FooterHint], alive: &[bool]) -> String {
    hints
        .iter()
        .zip(alive)
        .filter(|(_, live)| **live)
        .map(|(h, _)| h.text.as_str())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `room` 桁に収まるまで**右から**案内を落とす。`keep` の項目は落とさない。
///
/// 落とせる物を全部落としても入らなければ `None` — 呼び手（[`footer_layout`]）
/// が読み出しを 1 段縮めて、もう一度ここへ来る。
fn fit_hints(hints: &[FooterHint], room: usize) -> Option<String> {
    let mut alive = vec![true; hints.len()];
    loop {
        let text = join_hints(hints, &alive);
        if UnicodeWidthStr::width(text.as_str()) <= room {
            return Some(text);
        }
        let droppable = (0..hints.len()).rev().find(|&i| alive[i] && !hints[i].keep)?;
        alive[droppable] = false;
    }
}

/// **フッタの幅の配り方**（純関数。幅ごとのテストはこれを直接呼ぶ）。
///
/// 縮む順は読み手の決定（2026-09-22）そのもので、外側のループが読み出し、
/// 内側の [`fit_hints`] が案内である:
///
/// 1. 左の案内を右から落とす（`? help` と位置 `L12/80` は最後まで残す）
/// 2. 読み出しを 1 段縮める（`20%` → 問いの名前 の順に落ち、**座布団の
///    本数が最後まで残る**）。案内は縮めた分だけ戻ってくる
/// 3. それでも足りなければ読み出しごと引き下がる
///
/// `readouts` は**広い順**に並んだ候補（`App::marks_readouts`）。空なら
/// 読み出しは無い。
pub(crate) fn footer_layout(
    badge_w: u16,
    hints: &[FooterHint],
    readouts: Vec<Readout>,
    width: u16,
) -> FooterLayout {
    let mut chosen = None;
    let mut text = String::new();
    for (i, readout) in readouts.iter().enumerate() {
        // 案内と読み出しの間は最低 1 桁空ける（地続きに見えないように）。
        let room = width.saturating_sub(badge_w + readout.width() as u16 + 1);
        if let Some(fitted) = fit_hints(hints, room as usize) {
            chosen = Some(i);
            text = fitted;
            break;
        }
    }
    let readout = match chosen {
        Some(i) => readouts.into_iter().nth(i),
        None => {
            // 読み出しを丸ごと引き下げた場合。それでも入らなければ刻む
            // （固定の項目だけでも入らない幅 — 40 桁を割ると起きる）。
            let room = width.saturating_sub(badge_w) as usize;
            text = fit_hints(hints, room).unwrap_or_else(|| {
                clip_ellipsis(&join_hints(hints, &vec![true; hints.len()]), room)
            });
            None
        }
    };
    let readout_w = readout.as_ref().map_or(0, |r| r.width() as u16);
    let readout_x = width.saturating_sub(readout_w);
    let (flash_x, flash_w) = match readout.as_ref() {
        Some(r) if r.cushioned() => {
            let count_w = UnicodeWidthStr::width(r.count().as_str()) as u16;
            (width.saturating_sub(count_w), count_w)
        }
        Some(_) => (readout_x, readout_w),
        None => (0, 0),
    };
    FooterLayout { hints: text, readout, readout_x, flash_x, flash_w }
}

/// The footer's mode hint: the cursor's position as `L{line}/{total}`
/// (1-based source line — the cursor IS the review anchor, so the line
/// number is more actionable than a %), a few labeled actions for the
/// current context, then `? help` for the full key reference. Keys keep
/// their relative order across modes so a mode switch never rearranges
/// the hints.
pub(crate) fn footer_hint_items(app: &App) -> Vec<FooterHint> {
    let pos = |line: usize, total: usize| {
        if total == 0 {
            "L0/0".to_string()
        } else {
            format!("L{}/{}", line + 1, total)
        }
    };
    // **Review の一覧を据え付けているあいだ、キーは一覧に届く。** フッタは
    // 一覧のキーを案内する（本文のキーを案内すると、打っても効かない）。
    // 位置は本文のカーソル — 一覧で選んだ候補の行である。
    if crate::review_dock::is_open(app) {
        let line = if app.view_active() { app.view.cursor } else { app.cursor };
        return vec![
            kept(pos(line, app.source.len())),
            kept("j/k move"),
            hint("a accept"),
            hint("x dismiss"),
            hint("e edit"),
            hint("A accept all"),
            hint("Enter select"),
            kept("Esc close"),
        ];
    }
    let mut items = match app.mode {
        // 問いの 1 行プロンプト（`/`）。改行は無いので `^j newline` を
        // 出さない — composer のヒントを借りると、打てない操作を勧める。
        Mode::Input if app.marks_prompt => {
            vec![kept("Enter ask"), hint("←→ move"), kept("Esc cancel")]
        }
        Mode::Input => vec![
            kept("Enter confirm"),
            hint("^j newline"),
            hint("←→↑↓ move"),
            kept("Esc cancel"),
        ],
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
                    vec![
                        kept(p),
                        hint(format!("{}–{}", a + 1, b + 1)),
                        hint("j/k extend"),
                        hint("c comment"),
                        hint("Esc cancel"),
                        kept("? help"),
                    ]
                }
                None => {
                    let mut items = vec![
                        kept(p),
                        hint("j/k scroll"),
                        hint("v select"),
                        hint("c comment"),
                    ];
                    // 沈める先が無いときは案内しない（`App::can_focus`）。
                    // 使えないキーを案内しない、という同じ作法。
                    if app.can_focus() {
                        items.push(hint("f focus"));
                    }
                    items.push(kept("? help"));
                    items
                }
            }
        }
        Mode::Source => {
            let p = pos(app.cursor, app.source.len());
            match app.selection {
                Some(sel) => {
                    let (a, b) = sel.range();
                    vec![
                        kept(p),
                        hint(format!("{}–{}", a + 1, b + 1)),
                        hint("j/k extend"),
                        hint("c comment"),
                        hint("Esc cancel"),
                        kept("? help"),
                    ]
                }
                None => vec![
                    kept(p),
                    hint("j/k move"),
                    hint("v select"),
                    hint("c comment"),
                    kept("? help"),
                ],
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
    //
    // **時間の案内は `? help` の右**に並ぶ。落とせる項目のうちいちばん
    // 右なので、狭くなると真っ先に消える — `←`/`→` は打てば分かる操作で、
    // 位置と `? help` より先に譲る。
    if matches!(app.mode, Mode::View | Mode::Source) && !app.config.reply {
        if app.history().is_some_and(|history| history.revisions.len() > 1) {
            items.push(hint("t detail"));
        }
        items.push(hint("← older"));
        items.push(hint("newer →"));
    }
    items
}

/// 案内を 1 本の文字列に（**1 項目も落とさない形**）。
///
/// 描画は通らない — そちらは幅に応じて落とす [`footer_layout`] を通る。
/// 「この場面でこのキーを案内しているか」だけを見たいテストの口である。
#[cfg(test)]
pub(crate) fn footer_hints(app: &App) -> String {
    let items = footer_hint_items(app);
    join_hints(&items, &vec![true; items.len()])
}

/// The mode badge leads the footer (statusline convention): the title
/// above is file-centric, this is where the mode is read at a glance.
/// Color semantics: gray = view (calm reading), blue = source (the raw
/// editor), cyan = comment input (same as the composer bubble), magenta
/// = selection active (the transient `v` state — the badge flips to
/// SELECT so the mode is unmissable, and Esc cancels it). Yellow is
/// comments only, everywhere (the count lives in the top-right `▌ N`
/// indicator). All badges are width 8, so the hints never shift when
/// the mode changes.
fn mode_badge(app: &App) -> (String, Style) {
    // 据え付けた一覧がキーを持っているあいだは `REVIEW` を名乗る（窓が
    // 無いので、どこにキーが届くかを示すものがバッジしか無い）。色は本文の
    // 候補の下線と同じ青緑 — 一覧と下線が同じ機能だと色で結ぶ。
    if crate::review_dock::is_open(app) {
        return (
            format!("{:^8}", "REVIEW"),
            Style::default()
                .fg(Color::Rgb(0, 0, 0))
                .bg(app.decoration_styles.review_underline()),
        );
    }
    match app.mode {
        // 問いの入力は COMMENT ではない。同じ composer を借りているので、
        // バッジが「COMMENT」のままだと打った文字がコメントになると読める。
        Mode::Input if app.marks_prompt => (
            format!("{:^8}", "ASK"),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Mode::Input => (
            format!("{:^8}", "COMMENT"),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Mode::View | Mode::Source if app.selection.is_some() => (
            format!("{:^8}", "SELECT"),
            Style::default().fg(Color::Black).bg(Color::LightMagenta),
        ),
        // フォーカス（`f`）。**`SELECT` の下、`VIEW` / `SOURCE` の上**に
        // 置く — 選択は Esc が先に引き取る transient な状態なので、
        // 重なったら選択を名乗るほうが操作の順と合う（Esc の順も
        // 終了確認 → 選択 → フォーカス、と同じ並びである）。
        // 色は琥珀（マーカーと同じ系統）。モードの色（灰 / 青 / シアン /
        // マゼンタ）とぶつからない唯一の空きでもある。
        Mode::View | Mode::Source if app.focused() => (
            format!("{:^8}", "FOCUS"),
            Style::default()
                .fg(Color::Black)
                .bg(app.decoration_styles.mark_tick()),
        ),
        Mode::View => (
            format!("{:^8}", "VIEW"),
            Style::default().fg(Color::Black).bg(Color::DarkGray),
        ),
        Mode::Source => (
            format!("{:^8}", "SOURCE"),
            Style::default().fg(Color::Black).bg(Color::LightBlue),
        ),
    }
}

/// 読み出しの候補（広い順）。**出さない場面ではここが空になる。**
///
/// タイムマシン中は出さない（`App::is_historical`）— 過去の世代では
/// フッタを timeline bar が覆っていて、読み出しはその下敷きになる。
/// マーカーは引かれたままである（消えるのは値の方だけ）。
fn footer_readouts(app: &App) -> Vec<Readout> {
    if app.is_historical() {
        return Vec::new();
    }
    let marks = app.marks_readouts();
    let review = app.review_readouts();
    let Some(review) = review.first() else {
        return marks;
    };
    // **Review は marks の左に出て、先に引き下がる**（読み手の決定、
    // 2026-09-23）。候補の列を交互に組むので、狭くなるにつれて
    // `Review · 3/9 · Filler · 20% 13` → `Filler · 20% 13` →
    // `Review · 3/9 · Filler 13` → `Filler 13` → … と落ちる。
    //
    // marks の本数（座布団）は最後まで残る。marks の読み出しはいま画面で
    // 光っている箇所の説明だが、Review の `3/9` は一覧を開けば同じ数が
    // 読める — どちらかを落とすならこちらである。
    if marks.is_empty() {
        return vec![Readout { dim: review.clone(), lit: None }];
    }
    let mut out = Vec::with_capacity(marks.len() * 2);
    for readout in marks {
        // marks が本数だけまで縮んだ段（`dim` が空）には中黒を付けない
        // — 右に何も無いのに区切りだけが残る。末尾の空白 1 桁は
        // `App::marks_readouts` の作法をそのまま引き継ぐ（座布団の琥珀と
        // 字が地続きに見えないように薄い空白を挟む）。
        let dim = if readout.dim.is_empty() {
            format!("{review} ")
        } else {
            format!("{review} · {}", readout.dim)
        };
        out.push(Readout { dim, lit: readout.lit });
        out.push(readout);
    }
    out
}

/// フッタの取り分を測る。`title_metrics` と同じ役回りで、描画と演出が
/// 同じ計算を通るための 1 か所である。
pub(crate) fn footer_metrics(app: &App, width: u16) -> FooterLayout {
    let (badge, _) = mode_badge(app);
    footer_layout(
        UnicodeWidthStr::width(badge.as_str()) as u16,
        &footer_hint_items(app),
        footer_readouts(app),
        width,
    )
}

pub(crate) fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let m = footer_metrics(app, area.width);
    let (badge, badge_style) = mode_badge(app);
    let badge_w = UnicodeWidthStr::width(badge.as_str()) as u16;
    let dim = Style::default().fg(Color::DarkGray);
    let hints_w = UnicodeWidthStr::width(m.hints.as_str()) as u16;
    let mut spans = vec![
        Span::styled(badge, badge_style),
        Span::styled(m.hints, dim),
    ];
    if let Some(readout) = m.readout {
        // 右端へ寄せる（間は空白で埋める）。`Line` の右寄せを使わないのは、
        // 左の案内と右の読み出しが**1 本の Line**に同居しているからで、
        // 埋める幅は `footer_layout` が出した桁からそのまま出る。
        let gap = m.readout_x.saturating_sub(badge_w + hints_w);
        spans.push(Span::raw(" ".repeat(gap as usize)));
        if !readout.dim.is_empty() {
            spans.push(Span::styled(readout.dim.clone(), dim));
        }
        let count = readout.count();
        if !count.is_empty() {
            // **座布団はマーカーと同じ琥珀**（`mark_tick()`）に黒字。
            // フッタの `FOCUS` バッジと同じ取り方なので、`--light` でも
            // `--theme` を変えても読める — 新しい色を作っていない。
            //
            // **字は `Color::Black` ではなく `Rgb(0,0,0)` である。** 300 ms の
            // フラッシュ（`crate::effects::readout_flash_effect`）は
            // `crate::view::lerp_color` で琥珀から**描かれた前景へ**戻すが、
            // あれは RGB 同士でしか混ぜず、名前付きの色は素通りして
            // そのまま返る。`Color::Black` のままだと演出が 1 フレームも
            // 効かない（`docs/gotchas/rendering.md`）。バッジの方は光らない
            // ので `Color::Black` のままでよい。
            let style = if readout.cushioned() {
                Style::default()
                    .fg(Color::Rgb(0, 0, 0))
                    .bg(app.decoration_styles.mark_tick())
            } else {
                dim
            };
            spans.push(Span::styled(count, style));
        }
    }
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
/// メッセージ行の y。**描画・toast の演出・IME の錨が同じ行を見る 1 か所。**
///
/// ふだんはフッタのすぐ上（view では本文の枠の下辺、source では本文の
/// 最後の行）。timeline bar が出ていればその上。**Review の一覧を
/// 据え付けているときは、その題の行**（view では本文の枠の下辺のまま）
/// — フッタのすぐ上は理由の欄の最後の行で、そこに toast が乗ると読んで
/// いる理由が消える。
pub(crate) fn message_row(app: &App, area: Rect) -> u16 {
    if crate::timeline::timeline_active(app) {
        return area.height.saturating_sub(3);
    }
    let middle = Rect { x: 0, y: 1, width: area.width, height: area.height.saturating_sub(2) };
    if let (_, Some(dock)) = crate::review_dock::split(app, middle) {
        return dock.title.y;
    }
    area.height.saturating_sub(2)
}

pub(crate) fn draw_message(f: &mut Frame, app: &App) {
    // The browsing timeline bar owns the bottom two rows (footer and
    // frame border); the message row floats directly above it so
    // prompts and toasts read as part of the bar instead of hovering
    // over the document.
    let row = message_row(app, f.area());
    // 問いの 1 行プロンプトはこの行を**丸ごと**使う。打っている最中の
    // 入力欄なので、中央のバナー（確認・toast）より優先する — 入力中に
    // toast が上に乗ると、打った字が見えなくなる。
    if app.marks_prompt && app.mode == Mode::Input {
        draw_ask_prompt(f, app, row);
        return;
    }
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
mod footer_layout_tests {
    //! **フッタの幅の配り方**（[`super::footer_layout`]）。純関数なので
    //! `App` を組まずに幅だけを動かせる — 実機で試すのは 3 桁くらいが
    //! 限度だが、ここは 1 桁ずつ舐められる。
    //!
    //! 縮む順は読み手の決定（2026-09-22）: 案内を右から落とす → 読み出しを
    //! 1 段縮める → 読み出しごと引き下がる。`? help` と位置は最後まで残り、
    //! 読み出しの側は**座布団の本数**が最後まで残る。

    use super::{FooterHint, Readout, footer_layout};
    use unicode_width::UnicodeWidthStr;

    /// バッジの幅（`VIEW` などは全部 8 桁）。
    const BADGE: u16 = 8;

    /// view モードの案内（`App::can_focus` が真のとき）＋ 時間の案内。
    fn hints() -> Vec<FooterHint> {
        let keep = |t: &str| FooterHint { text: t.into(), keep: true };
        let drop = |t: &str| FooterHint { text: t.into(), keep: false };
        vec![
            keep("L12/80"),
            drop("j/k scroll"),
            drop("v select"),
            drop("c comment"),
            drop("f focus"),
            keep("? help"),
            drop("← older"),
            drop("newer →"),
        ]
    }

    /// `Essential` の問いに 13 本。**`App::marks_readouts` と同じ形**
    /// （末尾の空白 1 桁込み — 座布団の琥珀と地続きに見せないため）。
    fn readouts(question: &str, lit: usize, share: u8) -> Vec<Readout> {
        vec![
            Readout { dim: format!("{question} · {share}% "), lit: Some(lit) },
            Readout { dim: format!("{question} "), lit: Some(lit) },
            Readout { dim: String::new(), lit: Some(lit) },
        ]
    }

    /// フッタ 1 行ぶんの見た目（バッジは幅だけなので `·` で埋める）。
    fn rendered(m: &super::FooterLayout, width: u16) -> String {
        let mut out = "·".repeat(BADGE as usize);
        out.push_str(&m.hints);
        let pad = m.readout_x.saturating_sub(BADGE + UnicodeWidthStr::width(m.hints.as_str()) as u16);
        out.push_str(&" ".repeat(pad as usize));
        if let Some(r) = m.readout.as_ref() {
            out.push_str(&r.text());
        }
        assert!(
            UnicodeWidthStr::width(out.as_str()) <= width as usize,
            "幅 {width} を溢れた: {out}"
        );
        out
    }

    #[test]
    fn a_wide_terminal_draws_the_shape_the_reader_asked_for() {
        // 読み手が出した形（2026-09-22）:
        // `L12/80 · j/k scroll · v select · c comment · f focus · ? help        Essential · 20% [13]`
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 13, 20), 100);
        let line = rendered(&m, 100);
        assert!(
            line.contains("L12/80 · j/k scroll · v select · c comment · f focus · ? help"),
            "{line}"
        );
        let r = m.readout.as_ref().unwrap();
        assert_eq!(r.dim, "Essential · 20% ");
        assert_eq!(r.count(), " 13 ");
        // 右端に寄る。
        assert_eq!(m.readout_x + r.width() as u16, 100);
        // フラッシュは座布団だけに乗る。
        assert_eq!(m.flash_w, 4);
        assert_eq!(m.flash_x, 96);
    }

    #[test]
    fn at_eighty_columns_the_hints_yield_and_the_readout_stays_whole() {
        // **案内を落としきってから読み出しを縮める。** 80 桁では時間の
        // 案内と `f focus` が落ち、読み出しは 1 桁も縮まない。
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 13, 20), 80);
        let line = rendered(&m, 80);
        assert!(line.contains("L12/80"), "{line}");
        assert!(line.contains("? help"), "{line}");
        assert!(line.contains("j/k scroll"), "{line}");
        assert!(!line.contains("newer →"), "右端の案内から落ちる: {line}");
        assert!(!line.contains("f focus"), "案内がまだ落ちていない: {line}");
        let r = m.readout.as_ref().unwrap();
        assert_eq!(r.dim, "Essential · 20% ", "案内より先に読み出しを縮めた");
        assert_eq!(r.count(), " 13 ");
        assert_eq!(m.readout_x + r.width() as u16, 80);
    }

    #[test]
    fn at_sixty_columns_the_hints_go_before_the_percent() {
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 13, 20), 60);
        let line = rendered(&m, 60);
        assert!(line.contains("L12/80") && line.contains("? help"), "{line}");
        let r = m.readout.as_ref().unwrap();
        assert!(r.dim.contains("20%"), "案内より先に % を落とした: {line}");
        assert_eq!(r.count(), " 13 ");
        // 落とせる案内は残り 1 つまで減っている（`? help` と位置は固定）。
        assert!(!line.contains("v select"), "案内が落ちていない: {line}");
        assert!(!line.contains("c comment"), "案内が落ちていない: {line}");
    }

    #[test]
    fn at_forty_columns_only_the_cushion_and_the_pinned_hints_survive() {
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 13, 20), 40);
        let line = rendered(&m, 40);
        // 固定の 2 つは必ず残る。
        assert!(line.contains("L12/80"), "位置が落ちた: {line}");
        assert!(line.contains("? help"), "? help が落ちた: {line}");
        // 読み出しは座布団の本数まで縮む（名前も % も落ちる）。
        let r = m.readout.as_ref().unwrap();
        assert_eq!(r.count(), " 13 ", "{line}");
        assert!(r.dim.is_empty() || !r.dim.contains('%'), "{line}");
    }

    #[test]
    fn a_long_free_question_shrinks_itself_not_the_pinned_hints() {
        // 自由入力の問いは読み手が打った日本語で、いくらでも長い。
        // **`Ask: …` が伸びても `? help` と位置は落ちない。**
        let long = "Ask: 来期の費用と人員の見通しについて述べている箇所";
        let m = footer_layout(BADGE, &hints(), readouts(long, 7, 35), 80);
        let line = rendered(&m, 80);
        assert!(line.contains("L12/80"), "位置が長い問いに押し出された: {line}");
        assert!(line.contains("? help"), "? help が押し出された: {line}");
        let r = m.readout.as_ref().unwrap();
        assert_eq!(r.count(), " 7 ", "本数は最後まで残る: {line}");
    }

    #[test]
    fn cjk_counts_as_two_columns() {
        // 全角は 2 桁。幅の計算を `len()` でやっていると、ここで溢れる
        // （`rendered` の中で幅を検算している）。
        let long = "Ask: 日本語日本語日本語日本語日本語";
        for width in [100u16, 80, 72, 64, 60, 52, 48, 44, 40] {
            let m = footer_layout(BADGE, &hints(), readouts(long, 21, 20), width);
            rendered(&m, width);
        }
    }

    #[test]
    fn the_readout_steps_down_one_rung_at_a_time() {
        // 幅を 1 桁ずつ削っても、**読み出しは広い順にしか動かない**
        // （狭くしたら急に広くなる、が起きない）。案内の方も増えない。
        let rs = |q: &str| readouts(q, 13, 20);
        let mut last = usize::MAX;
        for width in (30u16..=100).rev() {
            let m = footer_layout(BADGE, &hints(), rs("Essential"), width);
            let w = m.readout.as_ref().map_or(0, |r| r.width());
            assert!(w <= last, "幅 {width}: 狭くしたら読み出しが広くなった");
            last = w;
        }
    }

    #[test]
    fn zero_lit_keeps_the_shape_but_loses_the_cushion() {
        // 0 本は座布団を敷かない（薄いまま）。**幅は変えない** — つまみが
        // 0 を跨いだときに行が揺れないためである。
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 0, 1), 80);
        let r = m.readout.as_ref().unwrap();
        assert_eq!(r.count(), " 0 ");
        assert!(!r.cushioned(), "0 本に座布団を敷いている");
        // 座布団が無いのでフラッシュは読み出し全体に乗る。
        assert_eq!(m.flash_w, r.width() as u16);
    }

    #[test]
    fn a_state_readout_has_no_cushion_at_all() {
        // 解析中・スコア無しは値ではなく状態なので、本数そのものが無い。
        let state = vec![
            Readout { dim: "Essential · analyzing…".into(), lit: None },
            Readout { dim: "analyzing…".into(), lit: None },
        ];
        let m = footer_layout(BADGE, &hints(), state, 60);
        let r = m.readout.as_ref().unwrap();
        assert_eq!(r.count(), "");
        assert!(!r.cushioned());
        assert!(r.dim.contains("analyzing"));
    }

    #[test]
    fn without_a_layer_the_hints_own_the_whole_row() {
        // 層の無いセッション: 候補が空なので、案内が右端まで使える。
        let m = footer_layout(BADGE, &hints(), Vec::new(), 100);
        assert!(m.readout.is_none());
        assert_eq!(m.flash_w, 0, "演出の面が無い");
        assert!(m.hints.contains("newer →"), "余った幅が案内に回らない: {}", m.hints);
    }

    #[test]
    fn a_hopeless_width_clips_instead_of_panicking() {
        // 固定の項目だけでも入らない幅。読み出しは引き下がり、案内は刻む。
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 13, 20), 12);
        assert!(m.readout.is_none());
        assert!(UnicodeWidthStr::width(m.hints.as_str()) <= 4, "{}", m.hints);
        // 幅 0 でも落ちない。
        let m = footer_layout(BADGE, &hints(), readouts("Essential", 13, 20), 0);
        assert!(m.readout.is_none());
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
        marks_questions: None,
        review_rules: None,
        review_json: false,
        lint_cmd: None,
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

// ---- marks モードの問いの 1 行プロンプト（`/`） ------------------------

/// `ASK ▸ ` の見出し。`▸` は file picker のカーソルと同じ記号で、
/// 「ここから先があなたの入力」を指す。
const ASK_LEAD: &str = " ASK ▸ ";

/// 表示幅で `s` を `[start, start + cols)` に切る。返すのは切った文字列と、
/// 実際に切れた開始桁（全角の途中では切れないので、要求より左に寄ることが
/// ある）。
fn slice_cols(s: &str, start: usize, cols: usize) -> (String, usize) {
    let mut out = String::new();
    let mut at = 0usize;
    let mut began = None;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if at >= start {
            if began.is_none() {
                began = Some(at);
            }
            if out.width() + w > cols {
                break;
            }
            out.push(ch);
        }
        at += w;
    }
    (out, began.unwrap_or(at.min(start)))
}

/// **問いの 1 行プロンプト。** メッセージ行（フッタのすぐ上、view モード
/// では枠の下辺）を丸ごと使う。
///
/// **composer（コメントの入力欄）を借りるのをやめた理由**が 2 つある
/// （2026-09-22 の実機、読み手の注文 2）:
///
/// - 吹き出しが `comment · 1` と名乗っていた。バッジだけが `ASK` に
///   変わるので、枠の中は「コメントを書いている」と言ったままだった
/// - 1 行の問いのために 3 行（上罫・本文・下罫）を**文書の中に割り込ませ**、
///   行がずれていた。問いは 1 行なので、1 行で足りる
///
/// キャレットとカーソル位置は composer と**同じ関数**を使う
/// （[`crate::cursor_caret_line`] / [`crate::composer_cursor_pos`]）。
/// 日本語・全角・IME の扱いを 2 つ目実装しないためで、ここが分かれると
/// 片方だけ直す事故になる。
///
/// 入力が行幅を超えたら**左へ流す**（キャレットが常に見える窓）。1 行に
/// 収める以上どこかが隠れるので、隠すのは打ち終わった左側にする。
pub(crate) fn draw_ask_prompt(f: &mut Frame, app: &App, row: u16) {
    use ratatui::widgets::Clear;
    let area = f.area();
    let rect = Rect {
        x: area.x,
        y: area.y + row,
        width: area.width,
        height: 1,
    };
    f.render_widget(Clear, rect);
    let cyan = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let lead_cols = UnicodeWidthStr::width(ASK_LEAD);
    let room = (rect.width as usize).saturating_sub(lead_cols);
    // 改行はプロンプトに入らないので、本文は常に 1 行である。
    let (_, caret_col) = crate::composer_cursor_pos(&app.input, app.input_cursor, usize::MAX);
    // キャレットの 1 桁ぶんを残した窓。
    let avail = room.saturating_sub(1);
    let start = caret_col.saturating_sub(avail);
    let (visible, began) = slice_cols(&app.input, start, avail);
    let mut spans = vec![Span::styled(ASK_LEAD, cyan)];
    let caret = crate::cursor_caret_line(&visible, caret_col.saturating_sub(began));
    let used: usize = caret.spans.iter().map(|s| s.content.width()).sum();
    spans.extend(caret.spans);
    // 罫を右端まで伸ばす。composer の「罫が両端まで走る」作法を 1 行に
    // 畳んだもので、入力欄がどこまでかが見える。
    let fill = room.saturating_sub(used);
    if fill > 0 {
        spans.push(Span::styled(
            "─".repeat(fill),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// プロンプトのキャレットが乗る画面位置（IME の変換窓のアンカー）。
/// [`draw_ask_prompt`] と同じ窓の計算を通すので、描いたキャレットから
/// ずれない。
pub(crate) fn ask_prompt_cursor(app: &App, area: Rect, row: u16) -> (u16, u16) {
    let lead_cols = UnicodeWidthStr::width(ASK_LEAD);
    let room = (area.width as usize).saturating_sub(lead_cols);
    let (_, caret_col) = crate::composer_cursor_pos(&app.input, app.input_cursor, usize::MAX);
    let avail = room.saturating_sub(1);
    let start = caret_col.saturating_sub(avail);
    let (_, began) = slice_cols(&app.input, start, avail);
    let col = lead_cols + caret_col.saturating_sub(began);
    (area.x + col as u16, area.y + row)
}
