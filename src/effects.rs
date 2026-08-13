//! tachyonfx effects: the time-machine frame and the toast fade.
//!
//! Both are ordinary tachyonfx [`Effect`]s rendered by [`crate::draw`]
//! after the static UI, so they animate by repainting buffer cells
//! without ever touching the layout. The rotation clock lives in the
//! effect's own state, so skipped frames (the render-complete flash, a
//! prompt covering the message row) never disturb the wave.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Instant;

use ratatui::style::Color;
use tachyonfx::fx::ShaderFnContext;
use tachyonfx::{fx, CellFilter, CellIterator, Effect, EffectTimer, Interpolation, Motion};

use unicode_width::UnicodeWidthStr;

use crate::app::STATUS_SECS;
use crate::view::{
    ease_out_cubic, perimeter_index, rotation_period_ms, starfield_color, starfield_star_at,
    time_machine_color_at, time_machine_depth_shift, time_machine_palette,
    time_machine_rotation_fraction, warp_ring_color, warp_ring_rect, TIME_MACHINE_ROTATION_MS,
};

/// How long the timeline bar's slide-in/out takes (milliseconds).
pub(crate) const TIMELINE_SLIDE_MS: u32 = 200;

/// Set by `draw` while the timeline bar covers the frame's bottom
/// border row; the time-machine rotation then leaves that row alone so
/// the axis line stays calm instead of joining the wave.
static TIMELINE_BAR_VISIBLE: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_timeline_bar_visible(visible: bool) {
    TIMELINE_BAR_VISIBLE.store(visible, Ordering::Relaxed);
}

/// The travel depth (0 = just behind NOW, 1000 = the oldest revision),
/// set by `draw` each frame from the live history position. The frame
/// effects are created once at startup, so the depth reaches their
/// shader closures through this channel — the same way the timeline
/// bar's visibility does. Deeper travel means a denser sky, a faster
/// border wave, and a palette sunk toward violet.
static TIME_DEPTH_PERMILLE: AtomicU16 = AtomicU16::new(0);

pub(crate) fn set_time_depth(depth: f32) {
    let permille = (depth.clamp(0.0, 1.0) * 1000.0) as u16;
    TIME_DEPTH_PERMILLE.store(permille, Ordering::Relaxed);
}

fn time_depth() -> f32 {
    TIME_DEPTH_PERMILLE.load(Ordering::Relaxed) as f32 / 1000.0
}

/// The timeline bar's drawer opening: the bottom-anchored rows rise
/// from the bottom edge over [`TIMELINE_SLIDE_MS`]. The static bar is
/// drawn every frame under the effect; the shader wipes it blank and
/// lets the rows back in bottom-first (see `fx::slide_in`).
pub(crate) fn timeline_slide_in() -> Effect {
    fx::slide_in(
        Motion::DownToUp,
        2,
        0,
        Color::Reset,
        (TIMELINE_SLIDE_MS, Interpolation::Linear),
    )
}

/// The timeline bar's drawer closing: the rows sink downward and out.
pub(crate) fn timeline_slide_out() -> Effect {
    fx::slide_out(
        Motion::UpToDown,
        2,
        0,
        Color::Reset,
        (TIMELINE_SLIDE_MS, Interpolation::Linear),
    )
}

/// The animated frame while browsing the past (`--fx`, the default): a
/// custom shader repaints every border-glyph cell of the frame with the
/// rotating purple→pink gradient (one smooth wave per lap — see
/// [`time_machine_color_at`]). Markers (`▌`/`▀`/`▐`), the cursor `>`,
/// and message text are not border glyphs and keep their own colors; the
/// scrollbar thumb and the message row are drawn after the effect
/// anyway. `--no-fx` simply leaves this effect uncreated and the static
/// history border color shows.
pub(crate) fn time_machine_border_effect(light: bool) -> Effect {
    let palette = time_machine_palette(light);
    fx::effect_fn(
        (Instant::now(), 0.0f32),
        EffectTimer::from_ms(TIME_MACHINE_ROTATION_MS as u32 * 60, Interpolation::Linear),
        move |(last, phase): &mut (Instant, f32), ctx: ShaderFnContext, cells: CellIterator| {
            // The wave advances by dt/period, so a depth change (the
            // period tightens the deeper the traveler goes) speeds the
            // rotation up smoothly instead of jumping the phase.
            let now = Instant::now();
            let dt = now.saturating_duration_since(*last).as_secs_f32();
            *last = now;
            let depth = time_depth();
            *phase = (*phase + dt * 1000.0 / rotation_period_ms(depth)) % 1.0;
            let rot = *phase;
            let area = ctx.area;
            let (w, h) = (area.width as usize, area.height as usize);
            // A degenerate frame (a terminal that briefly reports 0×0,
            // or a window shrunk to nothing) must not crash the
            // rotation: saturating math turns it into a no-op — the
            // iterator over an empty area yields no cells anyway.
            let perimeter = w
                .saturating_mul(2)
                .saturating_add(h.saturating_mul(2))
                .saturating_sub(4);
            let (ox, oy) = (area.x as usize, area.y as usize);
            let last_col = w.saturating_sub(1);
            let last_row = h.saturating_sub(1);
            for (pos, cell) in cells {
                let (rx, ry) = (pos.x as usize - ox, pos.y as usize - oy);
                if rx != 0 && ry != 0 && rx != last_col && ry != last_row {
                    continue; // content cells stay untouched
                }
                // Only the frame's own glyphs join the animation.
                if !matches!(cell.symbol(), "│" | "─" | "┌" | "┐" | "└" | "┘") {
                    continue;
                }
                // The browsing timeline bar owns the frame's bottom
                // border row while it is on screen; the rotation must
                // not repaint the axis line under it.
                if TIMELINE_BAR_VISIBLE.load(Ordering::Relaxed) && ry == h - 1 {
                    continue;
                }
                let perim = perimeter_index(rx, ry, w, h);
                let c = time_machine_color_at(palette, perim, perimeter, rot);
                cell.set_fg(time_machine_depth_shift(light, c, depth));
            }
        },
    )
}

/// The starfield behind the page while browsing the past (`--fx`): a
/// sparse, screen-fixed sprinkle of stars twinkling in the page's empty
/// cells — the space the time-machine frame floats in. Stars are only
/// painted where nothing lives: past each row's text tail (one breathing
/// cell after the last glyph, its full width honored so a CJK tail never
/// exposes its continuation cell), on blank rows, and never over a
/// colored background (selection, review bands, the toast banner) — the
/// text column stays calm while the emptiness around it becomes sky.
/// Placement, glyph, and phase are a pure hash of the screen cell (see
/// [`starfield_star_at`]); the twinkle rides the same rotation lap as
/// the frame gradient, and the clock lives in the effect's own state so
/// skipped frames never make the sky stutter.
pub(crate) fn starfield_effect(light: bool) -> Effect {
    fx::effect_fn_buf(
        Instant::now(),
        EffectTimer::from_ms(TIME_MACHINE_ROTATION_MS as u32 * 60, Interpolation::Linear),
        move |clock: &mut Instant, ctx, buf| {
            // Twinkle speed stays constant; only the DENSITY rides the
            // travel depth (the sky thickens, it does not flicker
            // faster).
            let rot = time_machine_rotation_fraction(*clock);
            let depth = time_depth();
            let area = ctx.area;
            if area.width < 3 || area.height < 3 {
                return; // no interior to sprinkle
            }
            let last_col = area.right() - 1; // the right border column
            // The interior rows. The browsing timeline bar owns the
            // bottom content row while it is up (the times row on wide
            // terminals): the sky stops one row short of it.
            let mut bottom = area.bottom() - 1;
            if TIMELINE_BAR_VISIBLE.load(Ordering::Relaxed) {
                bottom = bottom.saturating_sub(1);
            }
            for y in (area.y + 1)..bottom {
                // Pass 1: the row's text tail — the column one past the
                // last non-blank glyph plus one breathing cell. Stepping
                // by glyph width skips the hidden continuation cells of
                // wide characters.
                let mut tail = area.x + 1;
                let mut x = area.x + 1;
                while x < last_col {
                    let sym = buf[(x, y)].symbol();
                    let w = sym.width().max(1) as u16;
                    if !sym.trim().is_empty() {
                        tail = x + w + 1;
                    }
                    x += w;
                }
                // Pass 2: stars only in the emptiness beyond the tail.
                for x in tail..last_col {
                    let cell = &mut buf[(x, y)];
                    if !cell.symbol().trim().is_empty() {
                        continue;
                    }
                    if cell.style().bg.is_some_and(|bg| bg != Color::Reset) {
                        continue;
                    }
                    if let Some(star) = starfield_star_at(x, y, depth) {
                        cell.set_char(star.glyph);
                        cell.set_fg(starfield_color(light, star.phase, rot));
                    }
                }
            }
        },
    )
}

/// How long the generation-warp zoom takes (milliseconds).
pub(crate) const WARP_MS: u32 = 450;
/// How many window outlines fly during a warp.
const WARP_RINGS: usize = 4;
/// Each ring launches this fraction of the flight after the previous.
const WARP_STAGGER: f32 = 0.18;

/// The generation warp: Mac Time Machine's flying windows, translated.
/// When the selected revision finishes rendering, a few window outlines
/// (`┌─┐│└┘` rings) fly through the frame — DEEPER into the past they
/// approach out of the depth (small → full frame, dim → bright) and
/// hand off to the real border; BACK toward NOW they recede the other
/// way (full → small, bright → dim) and vanish. Rings are staggered so
/// the flight reads as a cascade, and each ring decelerates on a cubic
/// ease-out. The rings repaint buffer cells for [`WARP_MS`] only — the
/// text underneath returns untouched on the next frame.
pub(crate) fn warp_effect(deeper: bool, light: bool) -> Effect {
    fx::effect_fn_buf(
        (),
        EffectTimer::from_ms(WARP_MS, Interpolation::Linear),
        move |_state: &mut (), ctx, buf| {
            let t = ctx.timer.alpha();
            let area = ctx.area;
            if area.width < 8 || area.height < 6 {
                return; // too small for a flight to read
            }
            // Total flight time covers the last ring's stagger.
            let span = 1.0 + WARP_STAGGER * (WARP_RINGS - 1) as f32;
            for k in 0..WARP_RINGS {
                let tk = (t * span - WARP_STAGGER * k as f32).clamp(0.0, 1.0);
                if tk <= 0.0 || tk >= 1.0 {
                    continue; // not launched yet, or already gone
                }
                let eased = ease_out_cubic(tk);
                let scale = if deeper {
                    0.2 + 0.8 * eased
                } else {
                    1.0 - 0.8 * eased
                };
                if scale >= 0.98 {
                    continue; // coincides with the real frame: hand off
                }
                let ring = warp_ring_rect(area, scale);
                draw_ring(buf, ring, area, warp_ring_color(light, scale));
            }
        },
    )
}

/// Draw one warp ring outline into the buffer. Wide glyphs need care in
/// both directions: a wide glyph LEFT of a ring cell shadows it (the
/// renderer skips cells behind a wide symbol), so it is blanked; a ring
/// glyph landing ON a wide glyph's lead cell leaves its continuation
/// cell orphaned, so that is blanked too. Both blanks last one frame —
/// the static draw repaints the text underneath.
fn draw_ring(
    buf: &mut ratatui::buffer::Buffer,
    ring: ratatui::layout::Rect,
    bounds: ratatui::layout::Rect,
    color: Color,
) {
    if ring.width < 2 || ring.height < 2 {
        return;
    }
    let (l, r) = (ring.left(), ring.right() - 1);
    let (t, b) = (ring.top(), ring.bottom() - 1);
    for x in l..=r {
        let top_ch = if x == l { '┌' } else if x == r { '┐' } else { '─' };
        set_ring_cell(buf, x, t, top_ch, color, bounds);
        let bot_ch = if x == l { '└' } else if x == r { '┘' } else { '─' };
        set_ring_cell(buf, x, b, bot_ch, color, bounds);
    }
    for y in (t + 1)..b {
        set_ring_cell(buf, l, y, '│', color, bounds);
        set_ring_cell(buf, r, y, '│', color, bounds);
    }
}

fn set_ring_cell(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    ch: char,
    color: Color,
    bounds: ratatui::layout::Rect,
) {
    if x > bounds.left() && buf[(x - 1, y)].symbol().width() == 2 {
        buf[(x - 1, y)].set_char(' '); // unshadow: the wide glyph would hide the ring cell
    }
    let wide = buf[(x, y)].symbol().width() == 2;
    let cell = &mut buf[(x, y)];
    cell.set_char(ch);
    cell.set_style(
        ratatui::style::Style::new()
            .fg(color)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );
    if wide && x + 1 < bounds.right() {
        buf[(x + 1, y)].set_char(' '); // the lead cell shrank: free its continuation
    }
}

/// The toast's lifetime effect: fade in from black (120 ms), hold, fade
/// out to black (120 ms). The total matches [`STATUS_SECS`], so the
/// message row hands back to the frame exactly when the status expires.
/// A [`CellFilter::BgColor`] isolates the banner's own cells (black
/// background) — the rest of the row (frame border, content) never
/// flashes. Prompts are persistent and stay abrupt on purpose; only the
/// transient toast fades.
pub(crate) fn toast_effect() -> Effect {
    let black = Color::Black;
    let total = STATUS_SECS.as_millis() as u32;
    let fade_ms = 120u32;
    let hold = total.saturating_sub(fade_ms * 2);
    let mut effect = fx::sequence(&[
        fx::fade_from(black, black, (fade_ms, Interpolation::Linear)),
        fx::sleep((hold, Interpolation::Linear)),
        fx::fade_to(black, black, (fade_ms, Interpolation::Linear)),
    ]);
    effect.filter(CellFilter::BgColor(Color::Black));
    effect
}

/// Scatter-in for the blocks that appeared in the selected revision,
/// masked to the characters that are GENUINELY new: `mask[row]` holds the
/// display-column ranges of inserted characters (diffed against the old
/// revision's rendered text), and only those cells materialize — the
/// unchanged text is never hidden. The reveal order is READING order
/// (left→right, top→bottom) across the new characters, like an LLM
/// streaming its output: the text types in from the front. `stagger_ms`
/// cascades later blocks.
///
/// Hidden cells are REPAINTED with their own background color rather than
/// blanked: the glyphs stay (their width never changes, so ratatui's diff
/// never takes the wide→narrow path that broke CJK backgrounds), and the
/// original text color is captured on the first frame so the stream
/// cursor can restore it — no checkerboard, no visible space glyphs.
pub(crate) fn appear_effect(mask: Vec<Vec<(u16, u16)>>, stagger_ms: u32) -> Effect {
    fx::delay(
        stagger_ms,
        fx::effect_fn_buf(
            (mask, None::<std::collections::HashMap<(u16, u16), Color>>),
            (450, Interpolation::Linear),
            |(mask, fg_cache), ctx, buf| {
                let alpha = ctx.timer.alpha();
                let area = ctx.area;
                let first_frame = fg_cache.is_none();
                if first_frame {
                    *fg_cache = Some(std::collections::HashMap::new());
                }
                let fg_cache = fg_cache.as_mut().expect("initialized above");
                // The reading-order index of every new cell: cells before
                // the stream cursor are revealed, cells after stay hidden.
                let row_counts: Vec<usize> = mask
                    .iter()
                    .map(|ranges| {
                        ranges.iter().map(|&(s, e)| (e - s) as usize).sum()
                    })
                    .collect();
                let total: usize = row_counts.iter().sum();
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        let (r, c) = ((y - area.y) as usize, x - area.x);
                        let ranges = match mask.get(r) {
                            Some(ranges) => ranges,
                            None => continue,
                        };
                        // The cell's order = new cells in earlier rows +
                        // new cells earlier in this row (masked ranges
                        // only, so the animation time is spread over the
                        // new text).
                        let mut order = row_counts[..r].iter().sum::<usize>();
                        let mut covered = false;
                        for &(s, e) in ranges {
                            if c >= s && c < e {
                                covered = true;
                                order += (c - s) as usize;
                                break;
                            }
                            order += (e - s) as usize;
                        }
                        if !covered {
                            continue; // the unchanged text stays put
                        }
                        let cell = &mut buf[(x, y)];
                        if first_frame {
                            fg_cache.insert((x, y), cell.style().fg.unwrap_or(Color::Reset));
                        }
                        // The stream cursor: cells up to `alpha * total`
                        // are revealed (their original color restored),
                        // the rest stay hidden (painted their own
                        // background, so the glyph is invisible against
                        // it — and stays full-width, so the diff never
                        // breaks the row's background).
                        if (order as f32) >= alpha * total as f32 {
                            match cell.style().bg {
                                // A real background: repaint the glyph with
                                // it (invisible, width preserved). `Reset`
                                // is ratatui's "no background" — there is
                                // nothing to blend into, so blank instead.
                                Some(bg) if bg != Color::Reset => {
                                    cell.set_fg(bg);
                                }
                                _ => blank_cell(cell),
                            }
                        } else if let Some(&fg) = fg_cache.get(&(x, y)) {
                            cell.set_fg(fg);
                        }
                    }
                }
            },
        ),
    )
}

/// Blank a single-width cell for the reveal shaders. A WIDE character is
/// overwritten with a FULL-WIDTH space (U+3000) rather than a regular
/// space: the cell stays two columns wide, so ratatui's diff never takes
/// the wide→narrow transition path (which force-clears the trailing
/// column and can drop the cell's background). Only used where the row
/// has no background to repaint with (the rare no-bg fallback); rows
/// with a background use the repaint trick instead, which leaves no
/// visible glyph at all.
fn blank_cell(cell: &mut ratatui::buffer::Cell) {
    cell.set_char(' ');
}

/// Scatter-out for the deletion ghosts, BACKSPACE-style: the ghost stays
/// whole for 150 ms (long enough to read), then its cells blank out in
/// REVERSE reading order — right→left, bottom→top, exactly like
/// backspacing through the text — completing when the 650 ms ghost
/// lifetime collapses the layout. The ghost rows carry NO background
/// (the deletion band was dropped from the spec), so the cells are
/// BLANKED rather than repainted — wide glyphs go through
/// [`blank_cell`], which keeps the trailing column intact.
pub(crate) fn ghost_effect() -> Effect {
    fx::delay(
        150,
        fx::effect_fn_buf(
            (None::<Vec<usize>>, 0usize),
            (500, Interpolation::Linear),
            |(text_counts, total), ctx, buf| {
                let alpha = ctx.timer.alpha();
                let area = ctx.area;
                // Pass 1 (first frame only): the text-cell count per row
                // (non-blank symbols). The backspace cursor is normalized
                // over the TEXT only, so a ghost whose row is mostly
                // trailing blank space still erases its words smoothly
                // right→left instead of chewing through the empty tail
                // first. Captured once: blanked cells keep their width in
                // the pacing, so the fade never accelerates.
                if text_counts.is_none() {
                    let mut counts = vec![0usize; area.height as usize];
                    let mut n = 0usize;
                    for y in area.y..area.bottom() {
                        let mut row_n = 0usize;
                        for x in area.x..area.right() {
                            if !buf[(x, y)].symbol().trim().is_empty() {
                                row_n += 1;
                            }
                        }
                        counts[(y - area.y) as usize] = row_n;
                        n += row_n;
                    }
                    *text_counts = Some(counts);
                    *total = n;
                }
                let counts = text_counts.as_ref().expect("set above");
                let total = *total;
                if total == 0 {
                    return;
                }
                // Pass 2: blank text cells in REVERSE reading order.
                for y in area.y..area.bottom() {
                    // The captured row counts were taken against the
                    // rect this effect first rendered into; a rect that
                    // shifted since (stale ghost rows after a history
                    // move) must not index past them or underflow the
                    // backspace cursor — skip the row instead.
                    let row = (y - area.y) as usize;
                    if row >= counts.len() {
                        continue;
                    }
                    let before_rows: usize = counts[..row].iter().sum();
                    let mut k = 0usize; // text cells before this one in the row
                    for x in area.x..area.right() {
                        let cell = &buf[(x, y)];
                        if cell.symbol().trim().is_empty() {
                            continue; // the row's blank padding is never absorbed
                        }
                        let order = before_rows + k;
                        if order >= total {
                            continue; // pacing captured against a smaller rect
                        }
                        if ((total - 1 - order) as f32) < alpha * total as f32 {
                            // The ghost has no band to blend into: blank
                            // the cell (full-width glyphs keep their
                            // trailing column, so the diff never breaks
                            // the row).
                            blank_cell(&mut buf[(x, y)]);
                        }
                        k += 1;
                    }
                }
            },
        ),
    )
}

