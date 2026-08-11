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
