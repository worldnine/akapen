//! Terminal frame rendering for the review screens.
//! This is the Frame layer alongside `chrome.rs`.
//! It turns `App` state into view, source, timeline, card, and composer rows.

use crate::*;

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    // The real per-frame delta advances the tachyonfx effect timers.
    let now = Instant::now();
    let last_tick = app
        .last_draw
        .map(|t| now.saturating_duration_since(t))
        .unwrap_or(Duration::from_millis(16));
    app.last_draw = Some(now);
    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ]);
    let [title, body, footer] = layout.areas(f.area());
    draw_title(f, title, app);
    match app.mode {
        Mode::Input => {
            if app.composer_return == Mode::View {
                draw_view(f, body, app)
            } else {
                draw_source(f, body, app)
            }
        }
        Mode::View => draw_view(f, body, app),
        Mode::Source => draw_source(f, body, app),
    }
    draw_footer(f, footer, app);
    if app.overlay.is_some() {
        draw_overlay(f, app);
    }
    // The browsing timeline bar: drawn after the footer so its state
    // words own the bottom row. It covers the footer, the frame's
    // bottom border (the axis row replaces it), and — on wide terminals
    // — the last content row (the times row). The message row floats
    // one row above it, and the border effect skips the covered row.
    let timeline_on = crate::timeline::timeline_active(app);
    if timeline_on {
        draw_timeline_bar(f, app);
    }
    // The message row floats above everything (overlays included): a
    // notification never displaces content. It renders on the row just
    // above the footer — the frame's bottom border in view mode (the
    // corners stay), the last content row in source mode — so the
    // layout never shifts. The persistent prompt wins over the
    // transient toast: an action that demands the user must not be
    // hidden behind a message that will expire on its own.
    draw_message(f, app);
    // 問いの 1 行プロンプトの IME アンカー。composer と同じ理屈で、
    // ハードウェアカーソルは隠したまま**位置だけ**を毎フレーム publish
    // する（`--no-cursor-anchor` で止まるのも同じ）。プロンプトは
    // `draw_message` が描いた行に乗っているので、行の算段はそちらと共有
    // する。
    if app.marks_prompt && app.mode == Mode::Input && app.config.cursor_anchor {
        let row = if crate::timeline::timeline_active(app) {
            f.area().height.saturating_sub(3)
        } else {
            f.area().height.saturating_sub(2)
        };
        let (x, y) = crate::chrome::ask_prompt_cursor(app, f.area(), row);
        f.set_cursor_position(Position { x, y });
    }
    // tachyonfx effects repaint after the static UI: the time-machine
    // frame (view mode, browsing the past, `--fx` on, outside the
    // render-complete flash), the appear/ghost scatter effects of the
    // last history render, and the toast fade on the message row (only
    // while the toast is the top message).
    if app.config.fx && app.view_active() {
        crate::effects::set_timeline_bar_visible(timeline_on);
        crate::effects::set_time_depth(travel_depth(app));
        // Same geometry draw_view builds: one column off the left edge,
        // the body's full height.
        let frame = Rect {
            x: body.x + 1,
            y: body.y,
            width: body.width.saturating_sub(1),
            height: body.height,
        };
        if app.is_historical() {
            // The sky is interior-only and never blinks off: the border
            // rotation and the landing pulse repaint only the frame's
            // own cells, so the stars keep twinkling underneath.
            if let Some(effect) = app.starfield_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
            // The border rotation runs continuously through the whole
            // transition; the landing pulse rides ON TOP of it once the
            // replacement settles, so the wave never pauses.
            if let Some(effect) = app.time_machine_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
            if let Some(effect) = app.landing_pulse_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
        }
        // The scatter effects live on the text column (view-relative
        // rows mapped through the current scroll offset); finished
        // effects drop themselves.
        let text_x = frame.x + 2;
        let text_w = frame.width.saturating_sub(4);
        let offset = app.view.offset as isize;
        let viewport = frame.height.saturating_sub(2) as isize;
        let rect_for =
            |row: usize, height: usize| -> Option<Rect> {
                let top = frame.y as isize + 1;
                let y = top + row as isize - offset;
                if y + height as isize <= top || y >= top + viewport {
                    return None; // entirely off-screen
                }
                let y0 = y.max(top) as u16;
                let y1 = (y + height as isize).min(top + viewport).max(y0 as isize + 1) as u16;
                Some(Rect {
                    x: text_x,
                    y: y0,
                    width: text_w,
                    height: (y1 - y0).max(1),
                })
            };
        // The composer owns the text column while it is open: the
        // scatter effects AND the warp pause behind it (render_effect is
        // skipped, so their timers hold) — a transition that was under
        // way when the comment bar opened freezes until Enter/Esc
        // returns to the view. The border rotation keeps flying (it
        // lives in the frame's own cells, never over the text).
        //
        // **問いの 1 行プロンプト（`/`）はここに入らない。** あれは本文の
        // 列を 1 行も占めないので、下の演出を止める理由が無い — 止めると
        // 問いを打っているあいだ時間旅行の演出が凍る。
        let composing = app.mode == Mode::Input
            && app.composer_return == Mode::View
            && !app.marks_prompt;
        if !composing {
            // Changed blocks that have scrolled off-screen drop their
            // effects rather than hold them: left un-rendered their
            // timers never advance, so a transition whose changed blocks
            // are all off-screen would never settle (the landing pulse
            // waits on `appear_fx`/`ghost_fx` staying empty) and
            // scrolling the block back in would suddenly start its
            // stream mid-transition. Off-screen changes simply land
            // already-rendered.
            app.appear_fx
                .retain(|(row, height, fx)| !fx.done() && rect_for(*row, *height).is_some());
            for (row, height, effect) in app.appear_fx.iter_mut() {
                if let Some(rect) = rect_for(*row, *height) {
                    f.render_effect(effect, rect, last_tick);
                }
            }
            app.ghost_fx
                .retain(|(row, height, fx)| !fx.done() && rect_for(*row, *height).is_some());
            for (row, height, effect) in app.ghost_fx.iter_mut() {
                if let Some(rect) = rect_for(*row, *height) {
                    f.render_effect(effect, rect, last_tick);
                }
            }
            // The generation warp flies over everything in the frame —
            // content, scatter effects, starfield — the way a window
            // crosses in front of the room. Paused while the composer is
            // open: at its innermost scale the bottom ring crosses the
            // lower text rows, where the comment bar lives.
            if let Some(effect) = app.warp_fx.as_mut() {
                f.render_effect(effect, frame, last_tick);
            }
        }
    }
    // The warp is a one-shot flight: drop it when it completes, or when
    // the view goes away mid-flight (left un-rendered it would never
    // finish and would hold the tick rate high forever).
    if app.warp_fx.as_ref().is_some_and(|fx| fx.done())
        || !(app.config.fx && app.view_active())
    {
        app.warp_fx = None;
    }
    // The landing pulse is a one-shot beat too: drop it when it
    // completes, or when the view leaves browsing mid-pulse (left
    // un-rendered it would never finish).
    if app.landing_pulse_fx.as_ref().is_some_and(|fx| fx.done())
        || !(app.config.fx && app.view_active() && app.is_historical())
    {
        app.landing_pulse_fx = None;
    }
    // The timeline bar's own slide-in/out, rendered over its rows (the
    // static bar is drawn above, the shader wipes and reveals it).
    if app.config.fx && timeline_on
        && let Some(effect) = app.timeline_fx.as_mut()
        && !effect.done()
    {
        let rect = timeline_bar_rect(f.area());
        f.render_effect(effect, rect, last_tick);
    }
    if app.timeline_fx.as_ref().is_some_and(|fx| fx.done()) || !timeline_on {
        app.timeline_fx = None;
    }
    if app.status.is_some()
        && prompt_message(app).is_none()
        && let Some(effect) = app.toast_fx.as_mut()
    {
        let row = Rect {
            x: 0,
            y: f.area().height.saturating_sub(if timeline_on { 3 } else { 2 }),
            width: f.area().width,
            height: 1,
        };
        f.render_effect(effect, row, last_tick);
    }
    // マーカーが引かれる演出は **view / source の両方**で走る。琥珀は
    // 両方で塗られるので、演出だけ view 限定だと「source では効かない
    // 機能」になる。だから上の `view_active()` の塊の外にいる。
    //
    // 面は本文の領域そのもので、どのセルを動かすかは背景色のフィルタが
    // 決める（`effects::marks_settle_effect` /
    // `effects::marks_reveal_effect`）。overlay や timeline bar が
    // 上に出ていても、それらの背景は琥珀ではないので巻き添えにならない。
    if app.config.fx
        && let Some(effect) = app.marks_fx.as_mut()
        && !effect.done()
    {
        f.render_effect(effect, body, last_tick);
    }
    if app.marks_fx.as_ref().is_some_and(|fx| fx.done()) || !app.config.fx {
        app.marks_fx = None;
    }
    // 読み出しの 300 ms。**面で掴む**ので、フッタの読み出しが実際に
    // 占めている矩形を `footer_metrics` からもらう（描画と同じ計算なので、
    // 光る場所と書いてある場所がずれようがない）。座布団があればその
    // 4 桁だけ、無ければ読み出し全体 — 変わったのは本数だからである。
    // 読み出しが出ていないときは矩形が幅 0 になり、演出は空振りして消える。
    //
    // timeline bar が出ている間は走らせない。バーがフッタを覆っている
    // ので、光らせても見えるのはバーの字である。
    if app.config.fx && app.readout_fx.as_ref().is_some_and(|fx| !fx.done()) {
        let m = crate::chrome::footer_metrics(app, f.area().width);
        let rect = Rect {
            x: m.flash_x,
            y: f.area().height.saturating_sub(1),
            width: m.flash_w,
            height: 1,
        };
        if timeline_on || rect.width == 0 {
            // **面が無いときは捨てる。描かないのではない。** tachyonfx の
            // タイマーは `render_effect` の中でしか進まないので、描くのを
            // 見送ると演出は永遠に `done()` にならない。すると下の後始末も
            // 走らず、`App::has_active_fx` が旗を立てたままになって
            // **イベントループが速いティックを掴み続ける**。NOW へ戻った
            // 瞬間に何秒も前のフラッシュが再生される、というおまけも付く。
            app.readout_fx = None;
        } else if let Some(effect) = app.readout_fx.as_mut() {
            f.render_effect(effect, rect, last_tick);
        }
    }
    if app.readout_fx.as_ref().is_some_and(|fx| fx.done()) || !app.config.fx {
        app.readout_fx = None;
    }
    // Keep the completed frame for the wide-char residue pass: the next
    // draw's wrapper compares it against the freshly drawn frame to blank
    // the right halves of wide characters the diff skipped (see
    // [`clear_wide_char_residue`]).
    app.last_frame = Some(f.buffer_mut().clone());
}








/// The rect of the browsing timeline bar: the bottom two rows, full
/// width — the same rows [`draw_timeline_bar`] paints.
fn timeline_bar_rect(area: Rect) -> Rect {
    let rows = crate::timeline::TIMELINE_BAR_ROWS.min(area.height);
    Rect {
        x: 0,
        y: area.height.saturating_sub(rows),
        width: area.width,
        height: rows,
    }
}

/// The browsing timeline bar: two rows, bottom-anchored — the state
/// words on the footer row (`HERE` at the current revision, `NOW` at
/// the right edge) and the axis on the row
/// above (`●` LOCAL / `◼` COMMIT / `◆` the current point / `▮`
/// baseline, dim left of the review baseline). While a history step is
/// fresh, the scrubber tooltip travels with the `◆` on the message row
/// above the axis (see [`timeline_tooltip_layout`]).
/// Drawn over the static UI before the effects; the slide effect
/// animates it.
/// How deep into the timeline the traveler is: 0 = just behind NOW,
/// 1 = the oldest revision. The LIVE position feeds it (not the
/// rendered one), so the effects already deepen while an arrow is held.
/// The frame effects and the timeline bar's marker colors both scale
/// their drama with it.
fn travel_depth(app: &App) -> f32 {
    app.history()
        .filter(|history| history.revisions.len() > 1)
        .map(|history| history.position as f32 / (history.revisions.len() - 1) as f32)
        .unwrap_or(0.0)
}

fn draw_timeline_bar(f: &mut Frame, app: &App) {
    let width = f.area().width as usize;
    let height = f.area().height;
    if width < 60 || height < 4 {
        return;
    }
    let Some(history) = app.history() else {
        return;
    };
    let Some(layout) = crate::timeline::layout_timeline(history, width) else {
        return;
    };
    let words_row = height.saturating_sub(1);
    let axis_row = height.saturating_sub(2);
    // The scrubber's family colors ride the same depth shift as the
    // frame: the deeper the traveler, the further LOCAL's pink and
    // COMMIT's periwinkle sink toward violet — the bar and the frame
    // read as one instrument. `--no-fx` keeps the static colors.
    let depth_color = |c: Color| -> Color {
        if app.config.fx {
            crate::view::time_machine_depth_shift(app.ui_light, c, travel_depth(app))
        } else {
            c
        }
    };

    // Row 2: the axis — markers over the line. The line is dim left of
    // the review baseline (reviewed history is behind you) and normal
    // from the baseline to NOW (the unreviewed stretch); before the
    // first acknowledgement the whole axis reads normally.
    {
        let by_col: std::collections::HashMap<usize, &crate::timeline::TimelinePoint> =
            layout.points.iter().map(|p| (p.col, p)).collect();
        let past = Style::default().fg(Color::DarkGray);
        let future = Style::default().fg(Color::Gray);
        let mut cells: Vec<(char, Style)> = Vec::with_capacity(width);
        for x in 0..width {
            let cell = match by_col.get(&x) {
                Some(p) if p.current => (
                    '◆',
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Some(p) => match p.kind {
                    // NOW always marks the right edge; a baseline
                    // sitting there needs no marker of its own (the
                    // whole axis reads dim — everything is reviewed).
                    crate::timeline::PointKind::Now => (
                        '●',
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    crate::timeline::PointKind::Local if p.baseline => (
                        '▮',
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    crate::timeline::PointKind::Commit if p.baseline => (
                        '▮',
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    crate::timeline::PointKind::Local => (
                        '●',
                        Style::default().fg(depth_color(crate::view::TIMELINE_LOCAL_COLOR)),
                    ),
                    crate::timeline::PointKind::Commit => (
                        '◼',
                        Style::default().fg(depth_color(crate::view::TIMELINE_COMMIT_COLOR)),
                    ),
                },
                None => (
                    '─',
                    match layout.baseline_col {
                        Some(base_col) if x < base_col => past,
                        _ => future,
                    },
                ),
            };
            cells.push(cell);
        }
        // Merge consecutive identical cells into spans.
        let mut spans: Vec<Span> = Vec::new();
        let mut i = 0;
        while i < cells.len() {
            let (ch, style) = cells[i];
            let mut j = i + 1;
            while j < cells.len() && cells[j] == (ch, style) {
                j += 1;
            }
            spans.push(Span::styled(ch.to_string().repeat(j - i), style));
            i = j;
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: 0,
                y: axis_row,
                width: f.area().width,
                height: 1,
            },
        );
    }

    // Row 3: the state words — nothing else. The `t` affordance is
    // already advertised where it is learned (the normal footer shows
    // `t detail` whenever a timeline exists, and `?` documents the
    // scrub keys), so repeating it here only crowded the NOW corner.
    {
        let mut items: Vec<(usize, String, Style)> = layout
            .words
            .iter()
            .map(|(start, text)| {
                // The "you are here" pair (HERE / NOW) reads bright.
                let style = Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
                (*start, text.clone(), style)
            })
            .collect();
        items.sort_by_key(|(start, _, _)| *start);
        let mut spans: Vec<Span> = Vec::new();
        let mut pos = 0usize;
        for (start, text, style) in items {
            let start = start.min(width);
            if start > pos {
                spans.push(Span::raw(" ".repeat(start - pos)));
            }
            let len = text.len();
            spans.push(Span::styled(text, style));
            pos = start + len;
        }
        if pos < width {
            spans.push(Span::raw(" ".repeat(width - pos)));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: 0,
                y: words_row,
                width: f.area().width,
                height: 1,
            },
        );
    }

    // The scrubber tooltip: the revision readout on the message row,
    // riding over the `◆`. It yields the row entirely
    // while a prompt or toast owns it (a centered banner over a longer
    // tooltip would leave stray fragments at its sides).
    if height >= 5
        && app.status.is_none()
        && prompt_message(app).is_none()
        && let Some((start, parts)) = timeline_tooltip_layout(app, width)
    {
        // The exit dissolve is the drawer's own: over the deadline's
        // final stretch a growing fraction of cells is simply NOT
        // overdrawn, so the document underneath shows through. (A
        // post-hoc tachyonfx dissolve could only blank the band's own
        // cells — its background strip kept sitting over the page.)
        // `--no-fx` never enters the window: its deadline excludes the
        // dissolve, so the band stays whole and pops off.
        let gone = app
            .timeline_tooltip_until
            .map(|until| {
                let remaining = until
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u32;
                if !app.config.fx || remaining >= crate::effects::TOOLTIP_DISSOLVE_MS {
                    0.0
                } else {
                    1.0 - remaining as f32 / crate::effects::TOOLTIP_DISSOLVE_MS as f32
                }
            })
            .unwrap_or(0.0);
        let y = height.saturating_sub(3);
        let buf = f.buffer_mut();
        let mut x = start;
        for (text, style) in parts {
            // Contiguous visible runs render together; a hidden cell
            // breaks the run and leaves the underlying cell untouched.
            let mut run = String::new();
            let mut run_x = x;
            for ch in text.chars() {
                let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                // A stable per-column hash (Knuth multiplicative) sets
                // each cell's dissolve threshold, so cells wink out in
                // a fixed spatially-random order as `gone` rises — no
                // flicker from re-randomizing every frame. Wide chars
                // live or die whole, keyed on their leading column.
                let keep = gone == 0.0
                    || (x.wrapping_mul(2_654_435_761) >> 7) % 1000 >= (gone * 1000.0) as usize;
                if keep {
                    if run.is_empty() {
                        run_x = x;
                    }
                    run.push(ch);
                } else if !run.is_empty() {
                    buf.set_string(run_x as u16, y, &run, style);
                    run.clear();
                }
                x += w;
            }
            if !run.is_empty() {
                buf.set_string(run_x as u16, y, &run, style);
            }
        }
    }
}

/// The scrubber tooltip's anchor and content: `(start column, styled
/// parts)` — `BASELINE · ` when the point is the review baseline, the
/// provenance glyph in its (depth-shifted) family color, `id · age`
/// bright, and the summary dim. LOCAL's long deterministic sentence
/// ("akapen local snapshot: uncommitted state captured …") collapses
/// to "local snapshot" — the age next to it already says when.
/// The band is centered over the `◆` and travels with it, clamped to
/// the terminal edges (one-column margin) so it can never spill; the
/// summary may use whatever width is left and is clipped only by the
/// screen. Every part rides the nebula band ([`tooltip_band_bg`]) —
/// the row floats over document text, and glyphs on the terminal's own
/// background would blend into it.
/// `None` once the hold window has expired or while the cursor sits at
/// NOW.
fn timeline_tooltip_layout(app: &App, width: usize) -> Option<(usize, Vec<(String, Style)>)> {
    app.timeline_tooltip_until
        .filter(|until| Instant::now() < *until)?;
    let history = app.history()?;
    let revision = history.current()?;
    let col = crate::timeline::layout_timeline(history, width)?
        .points
        .iter()
        .find(|p| p.current)?
        .col;
    let (glyph, family) = match revision.source {
        crate::history::RevisionSource::Now => return None,
        crate::history::RevisionSource::Local => ('●', crate::view::TIMELINE_LOCAL_COLOR),
        crate::history::RevisionSource::Git => ('◼', crate::view::TIMELINE_COMMIT_COLOR),
    };
    let glyph_color = if app.config.fx {
        crate::view::time_machine_depth_shift(app.ui_light, family, travel_depth(app))
    } else {
        family
    };
    // The row floats over document text. A black band vanished on dark
    // terminals (their background IS black), so the band wears the
    // time-machine nebula instead: a deep indigo clearly distinct from
    // both the terminal background and the page — the readout is an
    // instrument of the machine, not a line of the document.
    let light = app.ui_light;
    let on_band = Style::default().bg(crate::view::tooltip_band_bg(light));
    let mut parts: Vec<(String, Style)> = Vec::new();
    if history.baseline_position() == Some(history.position) {
        parts.push((
            " BASELINE ·".to_string(),
            on_band
                .fg(crate::view::tooltip_band_baseline_fg(light))
                .add_modifier(Modifier::BOLD),
        ));
    }
    parts.push((format!(" {glyph} "), on_band.fg(glyph_color)));
    let mut head = revision.short_id.clone();
    if let Some(then_ms) = revision.timestamp_ms {
        head.push_str(&format!(
            " · {}",
            crate::history::relative_age(crate::snapshot::now_ms(), then_ms)
        ));
    }
    parts.push((
        head,
        on_band
            .fg(crate::view::tooltip_band_fg(light))
            .add_modifier(Modifier::BOLD),
    ));
    let summary = match revision.source {
        crate::history::RevisionSource::Local => "local snapshot".to_string(),
        _ => revision.summary.clone(),
    };
    let used: usize = parts
        .iter()
        .map(|(text, _)| UnicodeWidthStr::width(text.as_str()))
        .sum();
    let budget = width.saturating_sub(2); // one-column margin each side
    if !summary.is_empty() && budget > used + 3 {
        let clipped = clip_title_label(&summary, budget - used - 3);
        parts.push((
            format!(" · {clipped}"),
            on_band.fg(crate::view::tooltip_band_dim_fg(light)),
        ));
    }
    // The band's closing pad, so the text never ends flush against the
    // page text to its right.
    parts.push((" ".to_string(), on_band));
    // Centered over the ◆, clamped so the band never spills over either
    // edge (right clamp first, then the left margin wins for bands as
    // wide as the screen).
    let total: usize = parts
        .iter()
        .map(|(text, _)| UnicodeWidthStr::width(text.as_str()))
        .sum();
    let start = col
        .saturating_sub(total / 2)
        .min(width.saturating_sub(total + 1))
        .max(1);
    Some((start, parts))
}

/// screen; keyboard navigation, clicks, and the mode handoffs reveal it.
fn review_flags(marks: &HashSet<usize>, line_count: usize) -> Vec<bool> {
    (0..line_count).map(|line| marks.contains(&line)).collect()
}

/// View mode: the native render with a row cursor. No per-frame
/// keep_cursor_visible here — like source mode, the wheel scrolls the
/// viewport only and the cursor is an absolute position that may sit off
/// screen until the next j/k.
fn draw_view(f: &mut Frame, area: Rect, app: &mut App) {
    // View mode always draws the frame: the bordered "page" is the reading
    // mode's visual signature (source mode is the frameless raw editor).
    // The page floats one column off the screen's left edge — a margin so
    // the markers riding the border never touch the terminal edge (the
    // title and footer strips stay full-width).
    let margin = 1;
    let frame = Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin),
        height: area.height,
    };
    // The frame's border is one cell: the reading pane's calm edge.
    let m = 1;
    let inner = Rect {
        x: frame.x + m,
        y: frame.y + m,
        width: frame.width.saturating_sub(m * 2),
        height: frame.height.saturating_sub(m * 2),
    };
    // The text column floats one column off each border: the marker on
    // the left border needs a breath before the text (`> print(...)`), and
    // the right edge gets the same gap, so the paragraph reads as a set
    // column instead of a full-bleed block. The pads live INSIDE the
    // rendered rows (see [`ViewState::visible_text`]); `content` is the
    // width the rows (and the bars) are built at.
    let content = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    // Which source lines carry comments → a per-line flag for the view's
    // marker column (drawn over the frame's left border, so no render
    // width is reserved and the layout never shifts when the first comment
    // is added). Every line of a multi-line comment is flagged; the column
    // renders one marker on each line's first display row only, so the
    // range reads as a clean column instead of a noisy band.
    let visible: Vec<Comment> = visible_cards(app).into_iter().cloned().collect();
    let marked = view_marker_flags(&visible, app.source.len(), app.current_file_path());
    // The selection is the LINE range itself (comment-identical);
    // `visible_text` resolves it to the exact spans via the phrase
    // segments.
    let sel = app.selection.map(|s| s.range());
    // View and source read the same baseline → displayed-generation marks.
    let (changed_set, deleted_set) = active_review_mark_sets(app);
    let n = app.source.len();
    let changed = review_flags(&changed_set, n);
    let deleted = review_flags(&deleted_set, n);
    let emphasized = vec![false; n];
    // Review の候補（Pending）の行。層の無いセッションでは全部 false。
    let review = app.review_lines();
    // The composer opened from view mode (`c` in view) is drawn inline
    // right under the cursor line, so the comment can be typed without
    // leaving the rendered view. While it is open it is part of the
    // layout: keep it on screen BEFORE the visible window is built, so
    // the splice below lands on the adjusted offset. The bar extends the
    // view's scrollable extent — a comment on the LAST line would
    // otherwise clamp the scroll at the document's last row and hide the
    // bar — and it grows as you type. Input mode has no scroll keys, so a
    // per-frame nudge cannot fight the user.
    // 問いの入力（`/`）は composer を使わない — メッセージ行の 1 行
    // プロンプトが受ける（[`crate::chrome::draw_ask_prompt`]）。文書に
    // 3 行（上罫・本文・下罫）を割り込ませないので、ここで弾く。
    let composing =
        app.mode == Mode::Input && app.composer_return == Mode::View && !app.marks_prompt;
    if composing {
        app.keep_composer_visible_view(inner.height as usize);
    }
    // The frame border reads as the history state while browsing; the
    // landing pulse (an effect) flares over it and settles without ever
    // replacing this color, and the rotation never pauses for it.
    let page_border = if app.is_historical() {
        app.ui_history_border
    } else {
        app.ui_border
    };
    let border_style = Style::default().fg(page_border);
    // Range decorations (the hidden `--decorations` flag and the Semantic
    // Reading Layer). Source mode paints the SAME list — see
    // [`App::active_decorations`].
    let decorations = app.active_decorations();
    let (mut text, mut gutter) = app.view.visible_text_with_glow(
        inner.height as usize,
        &marked,
        &changed,
        &[], // the transient glow was retired: the streaming reveal and
             // the frame flash already mark what changed
        &deleted,
        &emphasized,
        &review,
        sel,
        app.ui_selected_bg,
        app.ui_history_glow_bg,
        border_style,
        // The decorations reach the PAINT only — `render::render` never
        // sees them, so toggling a decoration cannot re-parse the
        // markdown.
        &decorations,
    );
    if composing {
        let full_width = content.width as usize;
        // Below the selection's rendered block (its END line's block — an
        // upward selection anchors at the range bottom, and a merged last
        // line anchors below the whole paragraph).
        let anchor = view_composer_anchor(app);
        let start_row = anchor.saturating_sub(app.view.offset) + 1;
        let lines = composer_lines(
            &app.input,
            app.input_cursor,
            app.input_start,
            app.input_end,
            full_width,
            app.editing_comment.is_some(),
        );
        if start_row <= text.lines.len() {
            let n = lines.len();
            // The bar floats in the text column like every other row: one
            // pad on each side.
            let padded: Vec<Line> = lines
                .into_iter()
                .map(|line| {
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(line.spans);
                    spans.push(Span::raw(" "));
                    Line::from(spans)
                })
                .collect();
            text.lines.splice(start_row..start_row, padded);
            // The composer rows carry no marker: keep the marker column
            // aligned with the text rows (the border shows through).
            gutter.splice(
                start_row..start_row,
                std::iter::repeat_n(GutterCell::border(border_style), n),
            );
        }
        // Terminal-cursor position inside the bar: on the block caret (the
        // macOS IME anchors its inline composition window here), sharing
        // the drawer's row math via composer_cursor_pos. The hardware
        // cursor is hidden, but the POSITION is published every frame for
        // that anchor — terminals running cursor-following shaders
        // (Ghostty's cursor_blaze etc.) blaze around the motion, so
        // `--no-cursor-anchor` stops publishing (the caret is
        // akapen-drawn and unaffected; only the IME anchor is lost).
        let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
        // Top rule at `start_row`, text rows from `start_row + 1`.
        let row = start_row + 1 + crow;
        if app.config.cursor_anchor && row < inner.height as usize {
            f.set_cursor_position(Position {
                x: content.x + ccol as u16,
                y: inner.y + row as u16,
            });
        }
    }
    // A calm gray frame: the border marks the reading pane without
    // competing with the rendered content. A mid gray — ANSI bright-black
    // sank into the background.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    // The text already starts at `offset` (see [`ViewState::visible_text`]),
    // so no Paragraph scroll is needed — the renderer only ever builds the
    // visible window.
    let p = Paragraph::new(text).block(block);
    f.render_widget(p, frame);
    // The marker column rides the frame's left border: `>` on the cursor
    // row, `▌` on marked rows, the border's `│` everywhere else. Written
    // over the border cells after the frame, so a marker replaces the
    // border glyph in place; the selection background extends over it,
    // running the cursor/selection band to the page edge. (The
    // time-machine gradient is a tachyonfx effect rendered after this
    // pass — it repaints only border-glyph cells, so markers survive.)
    let buf = f.buffer_mut();
    for (i, cell) in gutter.iter().take(inner.height as usize).enumerate() {
        if let Some(c) = buf.cell_mut((frame.x, inner.y + i as u16)) {
            c.set_symbol(cell.glyph);
            c.set_style(cell.style);
        }
    }
    // The scrollbar sits one column inside the frame's right border: a
    // `▐` thumb, mirroring the marker column on the left. The thumb
    // tracks the VIEWPORT offset (wheel scroll moves the viewport only),
    // so it always reflects what is on screen; when the content fits, no
    // scrollbar is drawn and the border stays clean.
    if let Some((start, len)) =
        scroll_thumb(app.view.rows.len(), inner.height as usize, app.view.offset)
    {
        let thumb_fg = app.ui_scrollbar;
        let right = frame.x + frame.width.saturating_sub(2);
        // **溝の目盛り** — マーカーの位置を文書の地図として先に打ち、
        // つまみをその上から描く。溝が無いとき（`scroll_thumb` が `None`）は
        // ここへ来ないので、収まっている文書に点は出ない。
        let ticks = marks_tick_rows(app, inner.height as usize);
        let tick_fg = app.decoration_styles.mark_tick();
        for row in &ticks {
            if let Some(c) = buf.cell_mut((right, inner.y + *row as u16)) {
                let bg = c.style().bg;
                c.set_symbol("·");
                let mut style = Style::default().fg(tick_fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((right, inner.y + i as u16)) {
                // Preserve the cell's current bg (the right pad's bg on
                // selected/cursor rows) so the band runs unbroken.
                let bg = c.style().bg;
                c.set_symbol("▐");
                // **つまみが勝つ**（読み手の決定、2026-09-22）。ただし
                // 目盛りの乗っていた行では**つまみ自身が琥珀になる** —
                // 形はつまみのまま（スクロール位置が読める）で、色が
                // 「ここにマークがある」を言う。溝は 1 桁しかないので、
                // 点でつまみを切ると位置のほうが読めなくなる。
                let fg = if ticks.contains(&i) { tick_fg } else { thumb_fg };
                let mut style = Style::default().fg(fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
    }
}

/// **溝の目盛りの行**（view モード）。
///
/// `App::marks_lines`（ソース行）を表示行へ写し、[`crate::view::mark_ticks`]
/// で溝へ落とす。**ソース行 → 表示行の写像は `view.source_starts`** で、
/// 折り返しやコメントの吹き出しで行が伸びても点がずれない。
fn marks_tick_rows(app: &App, track: usize) -> Vec<usize> {
    if app.marks_lines.is_empty() {
        return Vec::new();
    }
    let rows: Vec<usize> = app
        .marks_lines
        .iter()
        .filter_map(|&line| app.view.source_starts.get(line).copied())
        .collect();
    crate::view::mark_ticks(&rows, app.view.rows.len(), track)
}




















/// Source mode: status/gutter + highlighted content, wrapped by our own
/// width-aware algorithm (ratatui Wrap would double-wrap or misalign CJK).
fn draw_source(f: &mut Frame, area: Rect, app: &mut App) {
    // Source mode never draws a frame: every column goes to the source
    // (the framed view is the reading mode's visual signature). The
    // rightmost column is the scrollbar's track.
    let m = 0;
    let inner = Rect {
        x: area.x + m,
        y: area.y + m,
        width: area.width.saturating_sub(m * 2),
        height: area.height.saturating_sub(m * 2),
    };
    let gutter_cols = 1 + app.source.gutter_width as u16 + 1;
    app.gutter_cols = gutter_cols;
    let content_width = inner.width.saturating_sub(gutter_cols + 1);
    if app.source.is_empty() {
        f.render_widget(Paragraph::new("(empty file)"), area);
        return;
    }
    app.ensure_row_cache(content_width);
    app.refresh_line_rows();
    // While the composer is open, keep its bar on screen: the bar's
    // height is folded into line_rows (it grows as you type) and Input
    // mode has no scrolling keys, so a per-frame nudge cannot fight the
    // user — without it a comment on the last line left the bar below
    // the pane. Wheel/keys keep their own keep_cursor_visible discipline.
    if app.mode == Mode::Input {
        app.keep_composer_visible(inner.height as usize);
    }
    // The cursor is an absolute file position: wheel scroll moves the
    // viewport only, so don't yank the offset back every frame. Keyboard
    // j/k, c, and G call keep_cursor_visible themselves (herdr-review
    // style: scroll away, the cursor stays where it was).

    let (text, composer_cursor) = build_rows(app, inner.height, content_width);
    f.render_widget(Paragraph::new(text), area);
    // The scrollbar rides the pane's own right edge (source mode draws no
    // frame, so unlike view mode there is no border to sit inside — the
    // thumb goes all the way to the last column). The thumb appears only
    // when the content overflows the viewport; it tracks the viewport
    // offset (wheel scroll moves the viewport only), so it always reflects
    // what is on screen.
    if let Some((start, len)) = scroll_thumb(
        app.line_rows.iter().sum(),
        inner.height as usize,
        app.offset,
    ) {
        let thumb_fg = app.ui_scrollbar;
        let right = area.x + area.width.saturating_sub(1);
        // 目盛りは view と同じ約束で source にも出る（装飾はどちらのモードでも
        // 塗られるので、地図が片方にしか無いほうが不自然である）。違うのは
        // 行の数え方だけ — こちらは `line_rows` の累積が表示行になる。
        let ticks = source_tick_rows(app, inner.height as usize);
        let tick_fg = app.decoration_styles.mark_tick();
        let buf = f.buffer_mut();
        for row in &ticks {
            if let Some(c) = buf.cell_mut((right, inner.y + *row as u16)) {
                let bg = c.style().bg;
                c.set_symbol("·");
                let mut style = Style::default().fg(tick_fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
        for i in start..start + len {
            if let Some(c) = buf.cell_mut((right, inner.y + i as u16)) {
                // Preserve the cell's current bg (the gutter's bg on
                // selected/cursor rows) so the band runs unbroken.
                let bg = c.style().bg;
                c.set_symbol("▐");
                let fg = if ticks.contains(&i) { tick_fg } else { thumb_fg };
                let mut style = Style::default().fg(fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                c.set_style(style);
            }
        }
    }
    // The logical cursor position is still published every frame even
    // though the hardware cursor is hidden: the macOS IME anchors its
    // inline composition window to this spot, so conversion stays inside
    // the bar and follows the block caret. `--no-cursor-anchor` stops the
    // publishing for terminals whose shaders blaze around cursor motion.
    if app.config.cursor_anchor
        && let Some((col, row)) = composer_cursor
    {
        f.set_cursor_position(Position {
            x: inner.x + col,
            y: inner.y + row,
        });
    }
}

/// **溝の目盛りの行**（source モード）。
///
/// view 版（[`marks_tick_rows`]）との違いは行の数え方だけ: ソース行 →
/// 表示行は `line_rows` の累積（折返しのぶん伸びる）である。
fn source_tick_rows(app: &App, track: usize) -> Vec<usize> {
    if app.marks_lines.is_empty() || app.line_rows.is_empty() {
        return Vec::new();
    }
    let rows: Vec<usize> = app
        .marks_lines
        .iter()
        .filter(|&&line| line < app.line_rows.len())
        .map(|&line| app.line_rows[..line].iter().sum())
        .collect();
    crate::view::mark_ticks(&rows, app.line_rows.iter().sum(), track)
}

/// Build the visible source-mode rows: `[status][number] ` + content, each
/// source line wrapped to `content_width`, continuation rows bare content.
/// The rows are exactly the viewport window `[offset, offset + height)`:
/// a band straddling the top edge is trimmed to its visible tail, so the
/// screen never drifts from the row math (`keep_cursor_visible`, the
/// scrollbar, and the mouse mapping all count in display rows).
/// Selected lines get the selection background; the cursor line a `>` marker.
/// While the composer is open, also returns where the terminal cursor should
/// sit inside it (body-relative column/row), so the real bar cursor (and the
/// macOS IME's inline composition window) renders inside the bar.
pub(crate) fn build_rows(app: &App, height: u16, content_width: u16) -> (Text<'static>, Option<(u16, u16)>) {
    let mut out: Vec<Line> = Vec::new();
    let mut composer_cursor: Option<(u16, u16)> = None;
    let width = content_width as usize;
    // Range decorations — the same list the rendered view paints
    // (`App::active_decorations`), so a byte that is MARKED in view mode
    // is MARKED here. They are applied per wrapped row, AFTER the wrap
    // and BEFORE the cursor/selection/changed bands, exactly as
    // `ViewState::visible_text_with_glow` does it.
    //
    // `Dim` is dropped for a banded row, and for the same reason view.rs
    // drops it: `Dim` writes a FOREGROUND (see `decoration`'s module
    // docs), and a background band cannot patch a foreground back to what
    // it was — a dimmed line under the cursor would read as "I put the
    // cursor on it and the text went pale". Source mode paints a
    // full-row background for the CHANGED band too (view mode marks
    // changes in the gutter only), so `changed_bg` counts as banded here
    // where view mode has no equivalent.
    // Review の候補（Pending）の行。ガターの `!` がここを見る。
    let review_lines = app.review_lines();
    let decorations = app.active_decorations();
    let undimmed: Vec<crate::decoration::Decoration> = decorations
        .iter()
        .filter(|d| d.kind != crate::decoration::DecorationKind::Dim)
        .cloned()
        .collect();
    // Baseline → displayed-generation marks. Green `▌` marks a line that
    // is present and changed; the baseline text of every change renders
    // inline as red `▌` deleted rows (see `deleted_block_lines`), so
    // source mode needs no red `▌` position mark — that stays view-only.
    let scoped_added = &app.comparison_changed;
    let landing_pulse = app
        .landing_pulse_until
        .is_some_and(|until| Instant::now() < until);
    // Comment bars span the whole pane (gutter included), pi.dev-style.
    let full_width = (content_width + app.gutter_cols) as usize;
    // The current file's cards (the edited one is hidden while composing).
    let cards = visible_cards(app);
    let mut row = 0usize;
    // Rows of the first emitted band that lie ABOVE the scroll offset:
    // the offset lands mid-band whenever a wrapped line, a comment card,
    // or a deleted block straddles the pane's top edge, but the loop
    // emits whole bands — the surplus is trimmed after the loop.
    let mut skip: Option<usize> = None;
    for idx in 0..app.source.len() {
        let line_rows = app.rows_of(idx);
        if row + line_rows <= app.offset {
            row += line_rows;
            continue;
        }
        if row >= app.offset + height as usize {
            break;
        }
        if skip.is_none() {
            skip = Some(app.offset.saturating_sub(row));
        }
        // The baseline text deleted at this position renders first: red
        // `▌` rows above their anchor line. Blocks that fell past the
        // last line (an EOF deletion) render below its text instead.
        //
        // Lighting rule: the difference is the DIFF PAIR, so deleted rows
        // light up (bright red on the DarkGray cursor band) whenever their
        // anchor line is the cursor line or inside the selection — an
        // `n`-landed rewrite lights old and new content as one block, and
        // j/k passing the anchor lights its deletion too. `n`'s
        // pure-deletion focus additionally moves the `>` glyph onto the
        // first deleted row and renders the (untouched) anchor line as an
        // ordinary line — two cursor bands would fight the eye.
        let deletion_focused = app.deletion_focus() == Some(idx);
        let deletion_lit = deletion_focused
            || idx == app.cursor
            || app.selection.is_some_and(|s| s.contains(idx));
        let (deleted_above, deleted_below) = app.deleted_blocks_at(idx);
        for (i, block) in deleted_above.iter().enumerate() {
            out.extend(deleted_block_lines(
                app,
                &block.content,
                width,
                full_width,
                deletion_lit,
                deletion_focused && i == 0,
            ));
        }
        let selected = app.selection.is_some_and(|s| s.contains(idx));
        let revision = app.current_revision_context();
        let commented = app.comments.iter().any(|c| {
            c.covers(idx)
                && c.file_path == app.current_file_path()
                && crate::history::same_revision(c.revision.as_deref(), revision.as_deref())
        });
        let added = scoped_added.contains(&idx);
        let is_cursor = idx == app.cursor && !deletion_focused;
        // `▌` marks a current changed line; deletions are their own rows.
        // Review の候補（Pending）は `!` — view モードのガターと同じ
        // 形である。accept した候補は Pending でなくなるので、この旗と
        // コメントの印（source では行番号が黄色になる）が同じ行で
        // 競合することは無い。
        let review_line = review_lines.get(idx).copied().unwrap_or(false);
        let cursor_mark = if is_cursor {
            ">"
        } else if review_line {
            "!"
        } else if added {
            "▌"
        } else {
            " "
        };
        // The cursor line gets the same calm DarkGray background as view
        // mode (text colors untouched — a full-row reversal was fatiguing
        // and clashed with the syntax highlighting). A selected row shares
        // the background; the `>` marker keeps the cursor visible at the
        // selection edge. Present/changed review lines get a restrained
        // green background.
        let cursor_bg = is_cursor || selected;
        let changed_bg = added && !cursor_bg;
        let gutter_style = if cursor_bg {
            Style::default().bg(app.ui_selected_bg)
        } else if changed_bg {
            Style::default().bg(app.ui_changed_bg)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        // Selected lines: the number turns the composer's cyan — the
        // pending range reads at a glance and matches the color it will be
        // typed under. Commented lines: yellow (no `#` marker), so the
        // gutter shows at a glance which lines carry comments.
        let mut num_style = if selected {
            Style::default().fg(Color::Cyan).bg(app.ui_selected_bg)
        } else if commented && !is_cursor {
            Style::default().fg(Color::Yellow)
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else {
            gutter_style
        };
        // Source has no permanent frame, so its completion signal uses
        // the stable line-number rail instead — the same window as the
        // view mode's landing pulse. It starts only after the newly
        // selected source has already been painted once.
        if landing_pulse {
            num_style = num_style
                .fg(app.ui_landing_pulse)
                .add_modifier(Modifier::BOLD);
        }
        let num = Span::styled(
            format!("{:>width$} ", idx + 1, width = app.source.gutter_width),
            num_style,
        );
        // Source mode has no hanging indent (continuation rows are
        // aligned by the gutter-width pad the drawer pushes below), so
        // `hang` is 0 — with which `wrap_spans_tagged` IS `wrap_spans`,
        // plus the attribution.
        let line = &app.spans[idx];
        let wrapped = wrap_spans_tagged(&line.spans, &line.attrs, width, 0);
        // The cursor glyph is bold — it must be findable at a glance
        // (yellow is the comment marker's color), same as view mode. Its
        // color inherits the review mark under it. Mark-less rows keep the
        // classic LightCyan.
        let mark_style = if is_cursor {
            let fg = if added { Color::LightGreen } else { Color::LightCyan };
            let s = Style::default().fg(fg).add_modifier(Modifier::BOLD);
            if cursor_bg { s.bg(app.ui_selected_bg) } else { s }
        } else if review_line {
            // view モードの `!` と同じ青緑。
            let s = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
            if changed_bg { s.bg(app.ui_changed_bg) } else { s }
        } else if changed_bg {
            Style::default().fg(Color::Green).bg(app.ui_changed_bg)
        } else {
            gutter_style
        };
        let effective: &[crate::decoration::Decoration] = if cursor_bg || changed_bg {
            &undimmed
        } else {
            &decorations
        };
        for (k, (frags, frag_attrs)) in wrapped.iter().enumerate() {
            // The decoration goes on FIRST, so the bands below still win.
            // Splitting a span leaves the row's concatenated text
            // unchanged, so the row count and the trailing-fill width are
            // the same either way — `base_rows` (built from `wrap_spans`)
            // stays correct.
            let decorated: std::borrow::Cow<'_, [HiSpan]> = if effective.is_empty() {
                std::borrow::Cow::Borrowed(frags.as_slice())
            } else {
                std::borrow::Cow::Owned(
                    crate::decoration::decorate_row(
                        frags,
                        frag_attrs,
                        effective,
                        &app.decoration_styles,
                    )
                    .0,
                )
            };
            let frags = decorated.as_ref();
            let mut spans: Vec<Span> = Vec::new();
            if k == 0 {
                spans.push(Span::styled(cursor_mark, mark_style));
                spans.push(num.clone());
            } else {
                // Continuation rows of a wrapped line: indent by the gutter
                // width so the text stays aligned under the first row's
                // text instead of starting at column 0 under the line
                // number. The reversed/selected background spans the whole
                // continuation row so the cursor/selection reads as one
                // block. A changed line's `▌` repeats on every wrapped row
                // (same color family as the first-row mark), so the
                // left-edge mark runs unbroken — a wrapped changed line or
                // a run of adjacent changed lines reads as one solid band,
                // not a scattered strip of first-row marks.
                let indent_style = if cursor_bg {
                    Style::default().bg(app.ui_selected_bg)
                } else if changed_bg {
                    Style::default().bg(app.ui_changed_bg)
                } else {
                    Style::default()
                };
                // `added` implies one of the bands above, so the mark cell
                // always sits on a painted background. The color mirrors
                // the first-row mark: LightGreen under the cursor glyph,
                // Green on the changed band, and the selection band's
                // plain style when the line is only selected.
                let (mark, mark_style) = if added {
                    if is_cursor {
                        (
                            "▌",
                            Style::default()
                                .fg(Color::LightGreen)
                                .bg(app.ui_selected_bg),
                        )
                    } else if changed_bg {
                        ("▌", Style::default().fg(Color::Green).bg(app.ui_changed_bg))
                    } else {
                        ("▌", Style::default().bg(app.ui_selected_bg))
                    }
                } else {
                    (" ", Style::default())
                };
                spans.push(Span::styled(mark, mark_style));
                spans.push(Span::styled(
                    " ".repeat(app.gutter_cols as usize - 1),
                    indent_style,
                ));
            }
            for f in frags {
                let style = if cursor_bg {
                    f.style.bg(app.ui_selected_bg)
                } else if changed_bg {
                    f.style.bg(app.ui_changed_bg)
                } else {
                    f.style
                };
                spans.push(Span::styled(f.text.clone(), style));
            }
            // A cursor/selection/changed row's background runs to the
            // RIGHT EDGE of the pane (same calm color, pi.dev-style band)
            // — the text extent alone made the selection ragged and easy
            // to misread. This also covers blank lines (no fragments)
            // with no extra glyph.
            if cursor_bg || changed_bg {
                let used: usize = spans
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                    .sum();
                let bg = if cursor_bg {
                    app.ui_selected_bg
                } else {
                    app.ui_changed_bg
                };
                if used < full_width {
                    spans.push(Span::styled(
                        " ".repeat(full_width - used),
                        Style::default().bg(bg),
                    ));
                }
            }
            out.push(Line::from(spans));
        }
        for (i, block) in deleted_below.iter().enumerate() {
            out.extend(deleted_block_lines(
                app,
                &block.content,
                width,
                full_width,
                deletion_lit,
                deletion_focused && deleted_above.is_empty() && i == 0,
            ));
        }
        // Inline comment bars: one per comment ending on this line, stacked
        // (current file only; the one being re-edited is hidden under the
        // edit composer).
        for c in cards
            .iter()
            .filter(|c| c.end as usize - 1 == idx)
        {
            out.extend(comment_bar_lines(c, full_width));
        }
        // The composer bar while typing, under the target range's last line.
        if app.mode == Mode::Input && !app.marks_prompt && app.input_end == idx {
            let start_row = out.len();
            out.extend(composer_lines(
                &app.input,
                app.input_cursor,
                app.input_start,
                app.input_end,
                full_width,
                app.editing_comment.is_some(),
            ));
            // Terminal-cursor position inside the bar: on the block caret.
            // The macOS IME draws its inline composition window at this
            // spot too. composer_cursor_pos shares the drawer's row math,
            // so the anchor can never drift from the caret.
            let (crow, ccol) = composer_cursor_pos(&app.input, app.input_cursor, full_width);
            // Top rule at `start_row`, text rows from `start_row + 1`.
            composer_cursor = Some((ccol as u16, (start_row + 1 + crow) as u16));
        }
        row += line_rows;
    }
    // Trim the top band's rows above the offset and cap at the viewport.
    // Without the trim everything on screen sits `offset - band_start`
    // rows LOWER than the row math believes, and a cursor computed to be
    // on the pane's last row renders below the bottom edge — off screen.
    let skip = skip.unwrap_or(0).min(out.len());
    if skip > 0 {
        out.drain(..skip);
    }
    out.truncate(height as usize);
    // The composer-cursor row was recorded in emitted-band coordinates:
    // shift it by the trim, and drop it once it leaves the viewport (the
    // IME anchor must never point at a row that is not on screen).
    let composer_cursor = composer_cursor.and_then(|(col, r)| {
        let r = (r as usize).checked_sub(skip)?;
        (r < height as usize).then_some((col, r as u16))
    });
    (Text::from(out), composer_cursor)
}

/// The rows one deleted block paints: per baseline line, a red `▌` mark
/// and a blank number column on the first wrapped row (the line has no
/// number in the displayed document), a gutter-width indent on
/// continuation rows, and the text in red on the full-row red band —
/// the diff-pair partner of the green `▌` changed band. Display-only
/// rows: no selection or comment treatment applies. While `n`'s deletion
/// focus is on the block (`focused`), the rows take the cursor's own
/// visual language — bright red text on the DarkGray cursor band running
/// the full pane width, with the `>` glyph on the first row when this
/// block leads the focused set (`cursor_glyph`). The row count must
/// match [`crate::app::deleted_block_rows`] — same per-line [`wrap_spans`]
/// at the same width.
fn deleted_block_lines(
    app: &App,
    content: &str,
    width: usize,
    full_width: usize,
    focused: bool,
    cursor_glyph: bool,
) -> Vec<Line<'static>> {
    // Deleted rows are the diff pair of the green `▌` changed band: the
    // static state is a full-row red band (same restraint, same extent).
    // While lit, the gray cursor band replaces the red band across the
    // whole row and the text brightens.
    let text_style = if focused {
        Style::default()
            .fg(Color::LightRed)
            .bg(app.ui_selected_bg)
    } else {
        Style::default().fg(Color::Red).bg(app.ui_deleted_bg)
    };
    let mark_style = if focused {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD)
            .bg(app.ui_selected_bg)
    } else {
        Style::default().fg(Color::Red).bg(app.ui_deleted_bg)
    };
    let fill_style = if focused {
        Style::default().bg(app.ui_selected_bg)
    } else {
        Style::default().bg(app.ui_deleted_bg)
    };
    let mut out = Vec::new();
    let mut first_row = true;
    for line in content.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: line.to_string(),
                style: text_style,
            }],
            width,
        );
        for (k, frags) in wrapped.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if k == 0 {
                let mark = if cursor_glyph && first_row { ">" } else { "▌" };
                spans.push(Span::styled(mark, mark_style));
                spans.push(Span::styled(
                    " ".repeat(app.source.gutter_width + 1),
                    fill_style,
                ));
            } else {
                // The red `▌` repeats on every wrapped row, exactly like a
                // green changed line's continuation rows: the left-edge
                // mark runs unbroken down the whole block.
                spans.push(Span::styled("▌", mark_style));
                spans.push(Span::styled(
                    " ".repeat(app.gutter_cols as usize - 1),
                    fill_style,
                ));
            }
            for f in frags {
                spans.push(Span::styled(f.text.clone(), f.style));
            }
            // The band runs to the pane's right edge, exactly like the
            // cursor/selection band on ordinary rows — and the green `▌`
            // band on changed rows: the red band is its diff-pair
            // partner, so a rewrite reads as one matched block. The
            // focused state keeps the same extent (the gray cursor band
            // replaces the red one).
            let used: usize = spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if used < full_width {
                spans.push(Span::styled(
                    " ".repeat(full_width - used),
                    fill_style,
                ));
            }
            out.push(Line::from(spans));
            first_row = false;
        }
    }
    out
}

/// Display rows a saved-comment bar occupies under its anchor line
/// (top rule + wrapped body + bottom rule). Must match
/// [`comment_bar_lines`].
pub(crate) fn card_line_count(c: &Comment, full_width: usize) -> usize {
    let body: usize = c
        .text
        .split('\n')
        .map(|l| {
            wrap_spans(
                &[HiSpan {
                    text: l.to_string(),
                    style: Style::default(),
                }],
                full_width,
            )
            .len()
        })
        .sum();
    body.max(1) + 2
}

/// The composer's wrapped body rows as plain strings: the input split
/// into logical lines and wrapped to `full_width`, WITHOUT any cursor
/// glyph. The body is stable no matter where the insertion point sits, so
/// moving the cursor can never reflow the bar. (The old design embedded a
/// `▏` glyph in the text before wrapping: the near-invisible 1/8-width
/// sliver read as a half-width space, and wide-char + glyph interplay at
/// width boundaries flipped the row segmentation — "a half-width space
/// slips in and it shifts".) The caret is painted afterwards by
/// [`cursor_caret_line`] at the column [`composer_cursor_pos`] reports.
fn composer_body_rows(text: &str, full_width: usize) -> Vec<String> {
    // The cursor glyph is deliberately NOT part of this wrap: the body
    // must be stable no matter where the insertion point sits, so moving
    // the cursor can never reflow the bar. (Embedding the `▏` in the text
    // before wrapping made a wide char + glyph interplay leave a stray
    // blank cell at width boundaries, and flipped the segmentation as the
    // cursor crossed a wrap — "a half-width space slips in and it shifts".)
    // [`composer_lines`] overlays the glyph afterwards at the column
    // [`composer_cursor_pos`] reports, so the IME anchor still lands on it.
    let mut rows = Vec::new();
    for logical in text.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            rows.push(row.iter().map(|s| s.text.as_str()).collect::<String>());
        }
    }
    rows
}

/// The caret's cell inside the composer body: (body row, display
/// column). Row 0 is the first text row under the top rule. Computed by
/// wrapping the PREFIX `text[..cursor]` with the same wrap the body uses,
/// so the insertion point — and the IME anchor — exactly tracks the drawn
/// glyph without reflowing the wrapped rows. A prefix that ends flush at
/// the width boundary puts the cursor at the start of the next row.
pub(crate) fn composer_cursor_pos(text: &str, cursor: usize, full_width: usize) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    // Guard: `cursor` must be on a char boundary to slice the prefix.
    let cursor = (0..=cursor)
        .rev()
        .find(|&c| text.is_char_boundary(c))
        .unwrap_or(0);
    let prefix = &text[..cursor];
    let mut rows: Vec<String> = Vec::new();
    for logical in prefix.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            rows.push(row.iter().map(|s| s.text.as_str()).collect::<String>());
        }
    }
    let Some(last) = rows.last() else {
        return (0, 0);
    };
    let col = UnicodeWidthStr::width(last.as_str());
    let row = rows.len() - 1;
    // An insertion point flush at the width boundary sits on the NEXT row
    // (the character after it would wrap there). [`composer_lines`] appends
    // a caret-only row when that next row does not exist (input ending
    // exactly at the boundary) so the caret always has a cell.
    if col >= full_width {
        (row + 1, 0)
    } else {
        (row, col)
    }
}

/// The composer's body rows plus the caret cell, with a caret-only
/// trailing row appended when the insertion point lands flush at the
/// width boundary and there is no next row. Both [`composer_lines`] and
/// [`composer_line_count`] go through here, so height and render always
/// agree.
fn composer_body_with_caret(
    text: &str,
    cursor: usize,
    full_width: usize,
) -> (Vec<String>, (usize, usize)) {
    let mut body = composer_body_rows(text, full_width);
    let (mut crow, ccol) = composer_cursor_pos(text, cursor, full_width);
    if crow >= body.len() {
        body.push(String::new());
        crow = body.len() - 1;
    }
    (body, (crow, ccol))
}

/// Render the cursor as a background block over the character at the
/// insertion point (classic block caret) instead of inserting a `▏`
/// glyph. The old glyph was a LEFT ONE EIGHTH BLOCK — a 1/8-wide sliver
/// that most terminal fonts render as a nearly invisible gap, so the
/// caret read as "a half-width space slipped in" and the insertion point
/// was untraceable (`テストコ▏メント。` → `コ` and `メ` looked
/// disconnected). A background block on the existing character needs no
/// glyph: nothing can render as a stray space, nothing shifts, and the
/// caret is unmistakable.
pub(crate) fn cursor_caret_line(row: &str, col: usize) -> Line<'static> {
    // Composer cyan on black type: the block caret inverts the char under
    // it, readable in dark and light themes alike.
    let caret = Style::default().fg(Color::Black).bg(Color::Cyan);
    let mut width = 0usize;
    let mut caret_at = row.len();
    for (byte, ch) in row.char_indices() {
        if width >= col {
            caret_at = byte;
            break;
        }
        width += UnicodeWidthChar::width(ch).unwrap_or(0);
        caret_at = byte + ch.len_utf8();
    }
    if caret_at < row.len() {
        // Block over the character sitting at the insertion point.
        let ch = row[caret_at..].chars().next().unwrap();
        let end = caret_at + ch.len_utf8();
        Line::from(vec![
            Span::raw(row[..caret_at].to_string()),
            Span::styled(ch.to_string(), caret),
            Span::raw(row[end..].to_string()),
        ])
    } else {
        // End of the line: a thin underline caret (a filled block cell
        // there reads as a bulky full-width square — the "ダサい" initial
        // cursor on an empty draft). Cyan underline on the empty cell
        // where the next character — or the IME composition — lands.
        Line::from(vec![
            Span::raw(row.to_string()),
            Span::styled(
                " ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ])
    }
}

/// Display rows the composer bar occupies (top rule + wrapped input +
/// bottom rule). Independent of the cursor position: the body is wrapped
/// without the cursor glyph, so the height never changes as you move the
/// insertion point (and never differs between count and render).
pub(crate) fn composer_line_count(text: &str, cursor: usize, full_width: usize) -> usize {
    let (body, _) = composer_body_with_caret(text, cursor, full_width);
    body.len().max(1) + 2
}

/// A saved comment as a full-width bar: top and bottom rules only (no side
/// borders, no background fill), so it reads like a speech bubble whose
/// rules run to both screen edges. The rule color shows the state — saved
/// comments are dark gray with a yellow title, composing is cyan.
pub(crate) fn comment_bar_lines(c: &Comment, full_width: usize) -> Vec<Line<'static>> {
    let rule = Style::default().fg(Color::DarkGray);
    let title = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let label = format!(" comment · {} ", c.range_label());
    let fill = "─".repeat(full_width.saturating_sub(label.width()));
    let mut lines = vec![Line::from(vec![
        Span::styled(label, title),
        Span::styled(fill, rule),
    ])];
    for logical in c.text.split('\n') {
        let wrapped = wrap_spans(
            &[HiSpan {
                text: logical.to_string(),
                style: Style::default(),
            }],
            full_width,
        );
        for row in wrapped {
            let piece: String = row.iter().map(|s| s.text.as_str()).collect();
            lines.push(Line::from(Span::raw(piece)));
        }
    }
    lines.push(Line::from(Span::styled("─".repeat(full_width), rule)));
    lines
}

/// The inline comment input bar: the same top/bottom rules in cyan while
/// composing. The block caret sits at the insertion point, painted over
/// the character there (the hardware cursor stays hidden — see run();
/// this is the modern-TUI pattern, language-independent and immune to the
/// IME commit drift of a real cursor). `editing` flips the label to
/// `edit` when the composer is replacing an existing comment.
pub(crate) fn composer_lines(
    text: &str,
    cursor: usize,
    start: usize,
    end: usize,
    full_width: usize,
    editing: bool,
) -> Vec<Line<'static>> {
    let rule = Style::default().fg(Color::Cyan);
    let title = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let word = if editing { "edit" } else { "comment" };
    let label = if start == end {
        format!(" {word} · {} ", start + 1)
    } else {
        format!(" {word} · {}-{} ", start + 1, end + 1)
    };
    let fill = "─".repeat(full_width.saturating_sub(label.width()));
    let mut lines = vec![Line::from(vec![
        Span::styled(label, title),
        Span::styled(fill, rule),
    ])];
    // Wrap the text without any glyph, then paint a background block
    // caret over the character at the insertion point — the body rows stay
    // stable as the cursor moves, and the caret (unlike a glyph) can never
    // read as a stray space or shift the layout.
    let (body, (cursor_row, cursor_col)) =
        composer_body_with_caret(text, cursor, full_width);
    for (i, piece) in body.into_iter().enumerate() {
        if i == cursor_row {
            lines.push(cursor_caret_line(&piece, cursor_col));
        } else {
            lines.push(Line::from(Span::raw(piece)));
        }
    }
    lines.push(Line::from(Span::styled("─".repeat(full_width), rule)));
    lines
}
