//! tachyonfx effects: the time-machine frame and the toast fade.
//!
//! Both are ordinary tachyonfx [`Effect`]s rendered by [`crate::draw`]
//! after the static UI, so they animate by repainting buffer cells
//! without ever touching the layout. The rotation clock lives in the
//! effect's own state, so skipped frames (the render-complete flash, a
//! prompt covering the message row) never disturb the wave.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use ratatui::style::Color;
use tachyonfx::fx::ShaderFnContext;
use tachyonfx::{fx, CellFilter, CellIterator, Effect, EffectTimer, Interpolation, Motion};

use crate::app::STATUS_SECS;
use crate::view::{
    perimeter_index, time_machine_color_at, time_machine_palette, time_machine_rotation_fraction,
    TIME_MACHINE_ROTATION_MS,
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
/// rotating purple→cyan gradient (one smooth wave per lap — see
/// [`time_machine_color_at`]). Markers (`▌`/`▀`/`▐`), the cursor `>`,
/// and message text are not border glyphs and keep their own colors; the
/// scrollbar thumb and the message row are drawn after the effect
/// anyway. `--no-fx` simply leaves this effect uncreated and the static
/// history border color shows.
pub(crate) fn time_machine_border_effect(light: bool) -> Effect {
    let palette = time_machine_palette(light);
    fx::effect_fn(
        Instant::now(),
        EffectTimer::from_ms(TIME_MACHINE_ROTATION_MS as u32 * 60, Interpolation::Linear),
        move |clock: &mut Instant, ctx: ShaderFnContext, cells: CellIterator| {
            let rot = time_machine_rotation_fraction(*clock);
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
                cell.set_fg(time_machine_color_at(palette, perim, perimeter, rot));
            }
        },
    )
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
pub(crate) fn appear_effect(mask: Vec<Vec<(u16, u16)>>, stagger_ms: u32) -> Effect {
    fx::delay(
        stagger_ms,
        fx::effect_fn_buf(
            mask,
            (450, Interpolation::Linear),
            |mask, ctx, buf| {
                let alpha = ctx.timer.alpha();
                let area = ctx.area;
                // The reading-order index of every new cell: cells before
                // the stream cursor are revealed, cells after stay blank.
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
                        // The stream cursor: cells up to `alpha * total`
                        // are revealed, the rest stay blank.
                        if (order as f32) >= alpha * total as f32 {
                            blank_cell(buf, x, y);
                        }
                    }
                }
            },
        ),
    )
}

/// Blank a cell for the reveal/backspace shaders. A WIDE character is
/// overwritten with a FULL-WIDTH space (U+3000) rather than a regular
/// space: the cell stays two columns wide, so ratatui's diff never takes
/// the wide→narrow transition path (which force-clears the trailing
/// column and can drop the cell's background — the source of the
/// checkerboard behind CJK text). The band then covers both columns of
/// the glyph uniformly.
fn blank_cell(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16) {
    let cell = &mut buf[(x, y)];
    if unicode_width::UnicodeWidthStr::width(cell.symbol()) > 1 {
        cell.set_char('　');
    } else {
        cell.set_char(' ');
    }
}

/// Scatter-out for the deletion ghosts, BACKSPACE-style: the ghost stays
/// whole for 150 ms (long enough to read), then its cells clear in
/// REVERSE reading order — right→left, bottom→top, exactly like
/// backspacing through the text — completing when the 650 ms ghost
/// lifetime collapses the layout.
pub(crate) fn ghost_effect() -> Effect {
    fx::delay(
        150,
        fx::effect_fn_buf(
            (), // no mask: every ghost cell is "new"
            (500, Interpolation::Linear),
            |(), ctx, buf| {
                let alpha = ctx.timer.alpha();
                let area = ctx.area;
                let w = area.width as usize;
                let total = area.width as usize * area.height as usize;
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        let order = (y - area.y) as usize * w + (x - area.x) as usize;
                        // The backspace cursor: cells AFTER the cursor (in
                        // reverse reading order) are cleared.
                        if ((total - 1 - order) as f32) < alpha * total as f32 {
                            blank_cell(buf, x, y);
                        }
                    }
                }
            },
        ),
    )
}
