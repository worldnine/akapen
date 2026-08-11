//! tachyonfx effects: the time-machine frame and the toast fade.
//!
//! Both are ordinary tachyonfx [`Effect`]s rendered by [`crate::draw`]
//! after the static UI, so they animate by repainting buffer cells
//! without ever touching the layout. The rotation clock lives in the
//! effect's own state, so skipped frames (the render-complete flash, a
//! prompt covering the message row) never disturb the wave.

use std::time::Instant;

use ratatui::style::Color;
use tachyonfx::fx::ShaderFnContext;
use tachyonfx::{fx, CellFilter, CellIterator, Effect, EffectTimer, Interpolation};

use crate::app::STATUS_SECS;
use crate::view::{
    perimeter_index, time_machine_color_at, time_machine_palette, time_machine_rotation_fraction,
    TIME_MACHINE_ROTATION_MS,
};

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
            let perimeter = w * 2 + h * 2 - 4;
            let (ox, oy) = (area.x as usize, area.y as usize);
            for (pos, cell) in cells {
                let (rx, ry) = (pos.x as usize - ox, pos.y as usize - oy);
                if rx != 0 && ry != 0 && rx != w - 1 && ry != h - 1 {
                    continue; // content cells stay untouched
                }
                // Only the frame's own glyphs join the animation.
                if !matches!(cell.symbol(), "│" | "─" | "┌" | "┐" | "└" | "┘") {
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
        fx::effect_fn(
            mask,
            (450, Interpolation::Linear),
            |mask, ctx, cells| {
                let alpha = ctx.timer.alpha();
                // The reading-order index of every new cell: cells before
                // the stream cursor are revealed, cells after stay blank.
                let row_counts: Vec<usize> = mask
                    .iter()
                    .map(|ranges| {
                        ranges.iter().map(|&(s, e)| (e - s) as usize).sum()
                    })
                    .collect();
                let total: usize = row_counts.iter().sum();
                for (pos, cell) in cells {
                    let (r, c) = (
                        (pos.y - ctx.area.y) as usize,
                        pos.x - ctx.area.x,
                    );
                    let ranges = match mask.get(r) {
                        Some(ranges) => ranges,
                        None => continue,
                    };
                    // The cell's order = new cells in earlier rows + new
                    // cells earlier in this row (masked ranges only, so
                    // the animation time is spread over the new text).
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
                    // The stream cursor: cells up to `alpha * total` are
                    // revealed, the rest stay blank.
                    if (order as f32) >= alpha * total as f32 {
                        cell.set_char(' ');
                    }
                }
            },
        ),
    )
}

/// Scatter-out for the deletion ghosts, BACKSPACE-style: the ghost stays
/// whole for 150 ms (long enough to read), then its cells clear in
/// REVERSE reading order — right→left, bottom→top, exactly like
/// backspacing through the text — completing when the 650 ms ghost
/// lifetime collapses the layout.
pub(crate) fn ghost_effect() -> Effect {
    fx::delay(
        150,
        fx::effect_fn(
            (), // no mask: every ghost cell is "new"
            (500, Interpolation::Linear),
            |(), ctx, cells| {
                let alpha = ctx.timer.alpha();
                let w = ctx.area.width as usize;
                let total = ctx.area.width as usize * ctx.area.height as usize;
                for (pos, cell) in cells {
                    let order = (pos.y - ctx.area.y) as usize * w
                        + (pos.x - ctx.area.x) as usize;
                    // The backspace cursor: cells AFTER the cursor (in
                    // reverse reading order) are cleared.
                    if ((total - 1 - order) as f32) < alpha * total as f32 {
                        cell.set_char(' ');
                    }
                }
            },
        ),
    )
}
