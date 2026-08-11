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
                // The frame is TWO cells thick while browsing the past
                // (see App::frame_border): both rings join the gradient,
                // the content inside stays untouched.
                if rx > 1 && ry > 1 && rx < w - 2 && ry < h - 2 {
                    continue;
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
/// unchanged text is never hidden. Materialization order within the new
/// characters is a stable per-cell scatter (same threshold every frame),
/// over ~450 ms; `stagger_ms` cascades later blocks.
pub(crate) fn appear_effect(mask: Vec<Vec<(u16, u16)>>, stagger_ms: u32) -> Effect {
    fx::delay(
        stagger_ms,
        fx::effect_fn(
            mask,
            (450, Interpolation::QuadOut),
            |mask, ctx, cells| {
                let alpha = ctx.timer.alpha();
                for (pos, cell) in cells {
                    let (r, c) = (
                        (pos.y - ctx.area.y) as usize,
                        pos.x - ctx.area.x,
                    );
                    let is_new = mask.get(r).is_some_and(|ranges| {
                        ranges.iter().any(|&(s, e)| c >= s && c < e)
                    });
                    if !is_new {
                        continue; // the unchanged text stays put
                    }
                    // A stable scattered reveal: each new cell has a fixed
                    // threshold; it stays blank until the timer's alpha
                    // passes it.
                    let threshold = ((r as u64).wrapping_mul(0x9E37_79B1)
                        ^ (c as u64).wrapping_mul(0x85EB_CA77))
                        % 1000;
                    if (threshold as f32 / 1000.0) > alpha {
                        cell.set_char(' ');
                    }
                }
            },
        ),
    )
}

/// Scatter-out for the deletion ghosts: the ghost stays whole for 150 ms
/// (long enough to read), then its cells dissolve to blank in a stable
/// random order over 500 ms — completing exactly when the 650 ms ghost
/// lifetime collapses the layout, so the block "shrinks" cell by cell.
pub(crate) fn ghost_effect() -> Effect {
    fx::delay(150, fx::dissolve((500, Interpolation::QuadIn)))
}
