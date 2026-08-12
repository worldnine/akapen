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
/// whole for 150 ms (long enough to read), then its cells fade into the
/// band in REVERSE reading order — right→left, bottom→top, exactly like
/// backspacing through the text — completing when the 650 ms ghost
/// lifetime collapses the layout. Cells are REPAINTED band-on-band
/// rather than blanked: the glyphs stay (their width never changes, so
/// ratatui's diff never takes the wide→narrow path that broke CJK
/// backgrounds), and painting them the band color makes them invisible
/// against it — no checkerboard, no visible space glyphs.
pub(crate) fn ghost_effect(band: Color) -> Effect {
    fx::delay(
        150,
        fx::effect_fn_buf(
            band,
            (500, Interpolation::Linear),
            |band, ctx, buf| {
                let alpha = ctx.timer.alpha();
                let area = ctx.area;
                // Pass 1: the text-cell count per row (non-blank symbols).
                // The backspace cursor is normalized over the TEXT only,
                // so a ghost whose row is mostly trailing blank space
                // still erases its words smoothly right→left instead of
                // chewing through the empty tail first.
                let mut text_counts = vec![0usize; area.height as usize];
                let mut total = 0usize;
                for y in area.y..area.bottom() {
                    let mut n = 0usize;
                    for x in area.x..area.right() {
                        if !buf[(x, y)].symbol().trim().is_empty() {
                            n += 1;
                        }
                    }
                    text_counts[(y - area.y) as usize] = n;
                    total += n;
                }
                if total == 0 {
                    return;
                }
                // Pass 2: absorb text cells in REVERSE reading order.
                for y in area.y..area.bottom() {
                    let before_rows: usize =
                        text_counts[..(y - area.y) as usize].iter().sum();
                    let mut k = 0usize; // text cells before this one in the row
                    for x in area.x..area.right() {
                        let cell = &buf[(x, y)];
                        if cell.symbol().trim().is_empty() {
                            continue; // the row's blank padding is never absorbed
                        }
                        let order = before_rows + k;
                        if ((total - 1 - order) as f32) < alpha * total as f32 {
                            let cell = &mut buf[(x, y)];
                            // Repaint band-on-band: the glyph stays (its
                            // width never changes, so ratatui's diff never
                            // takes the wide→narrow path that broke CJK
                            // backgrounds), invisible against the band —
                            // no checkerboard, no visible space glyphs.
                            cell.set_fg(*band);
                            cell.set_bg(*band);
                        }
                        k += 1;
                    }
                }
            },
        ),
    )
}

