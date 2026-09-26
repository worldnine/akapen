//! tachyonfx effects: the time-machine frame and the toast fade.
//!
//! Both are ordinary tachyonfx [`Effect`]s rendered by [`crate::draw`]
//! after the static UI, so they animate by repainting buffer cells
//! without ever touching the layout. The rotation clock lives in the
//! effect's own state, so skipped frames (a prompt covering the message
//! row) never disturb the wave.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Instant;

use ratatui::style::Color;
use tachyonfx::fx::ShaderFnContext;
use tachyonfx::{fx, CellFilter, CellIterator, Effect, EffectTimer, Interpolation, Motion};

use unicode_width::UnicodeWidthStr;

use crate::app::STATUS_SECS;
use crate::view::{
    ease_out_cubic, lerp_color, perimeter_index, rotation_period_ms, starfield_color,
    starfield_star_at, time_machine_color_at, time_machine_depth_shift, time_machine_palette,
    time_machine_rotation_fraction, warp_ring_color, warp_ring_rect, TIME_MACHINE_ROTATION_MS,
    WARP_INNER_SCALE,
};

/// How long the timeline bar's slide-in/out takes (milliseconds).
pub(crate) const TIMELINE_SLIDE_MS: u32 = 200;

/// How long the scrubber tooltip holds after the last history step
/// before it leaves (milliseconds), and how long its exit dissolve
/// takes. The dissolve is drawn by the bar drawer itself — hidden
/// cells are simply not overdrawn, so the document shows through; a
/// tachyonfx shader could only blank the band's own cells and would
/// leave its background strip sitting over the page (the original
/// artifact this replaced).
pub(crate) const TOOLTIP_HOLD_MS: u32 = 1200;
pub(crate) const TOOLTIP_DISSOLVE_MS: u32 = 250;

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
/// [`time_machine_color_at`]). Markers (`▌`/`▐`), the cursor `❯`,
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
/// The sky is interior-only, so the border rotation and the landing
/// pulse — which repaint only the frame's own cells — never disturb it.
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
            // The scrollbar thumb rides one column inside the right
            // border (a `▐` mirroring the marker gutter). It is chrome,
            // not text: the tail scan must not mistake it for the row's
            // last glyph, or every thumb row would lose its whole sky.
            let scrollbar_col = last_col.saturating_sub(1);
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
                while x < scrollbar_col {
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
                        // The twinkle color rides the SAME depth shift
                        // as the frame: deeper travel sinks the sky
                        // toward indigo-violet with the border, so the
                        // stars and the frame read as one universe (the
                        // density already rode the depth; the color
                        // joins it here).
                        let twinkle = starfield_color(light, star.phase, rot);
                        cell.set_fg(time_machine_depth_shift(light, twinkle, depth));
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
                // The flight stays in a thin band just inside the frame:
                // rings travel between WARP_INNER_SCALE and the frame
                // itself, hugging the inner edge instead of crossing the
                // middle of the page.
                let travel = 1.0 - WARP_INNER_SCALE;
                let scale = if deeper {
                    WARP_INNER_SCALE + travel * eased
                } else {
                    1.0 - travel * eased
                };
                if scale >= 0.99 {
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

/// How long the landing pulse takes (milliseconds): the frame's brief
/// brighten-and-settle beat that marks a history transition's completion.
/// Matches the old flat flash's presence without ever freezing the wave.
pub(crate) const LANDING_PULSE_MS: u32 = 400;

/// The landing pulse: when the text replacement settles, the frame
/// briefly flares toward `bright` and settles back — a soft "landed"
/// beat that confirms the selected revision is fully in place. It rides
/// ON TOP of the rotating time-machine gradient: each frame it reads the
/// border cells' current color (the rotation painted them just before)
/// and brightens it by the pulse envelope, so the wave keeps flowing
/// underneath and the rotation never pauses. Only border-glyph cells
/// join (markers, the cursor `❯`, and message text keep their own
/// colors), and the timeline bar's bottom border row stays calm while it
/// is up — the same guards the border rotation uses.
pub(crate) fn landing_pulse_effect(bright: Color) -> Effect {
    fx::effect_fn_buf(
        (),
        EffectTimer::from_ms(LANDING_PULSE_MS, Interpolation::Linear),
        move |_state: &mut (), ctx, buf| {
            let env = landing_pulse_env(ctx.timer.alpha());
            if env <= 0.0 {
                return;
            }
            let area = ctx.area;
            let (w, h) = (area.width as usize, area.height as usize);
            if w == 0 || h == 0 {
                return;
            }
            let last_col = w - 1;
            let last_row = h - 1;
            for y in 0..h {
                if TIMELINE_BAR_VISIBLE.load(Ordering::Relaxed) && y == last_row {
                    continue;
                }
                for x in 0..w {
                    if x != 0 && y != 0 && x != last_col && y != last_row {
                        continue; // content cells stay untouched
                    }
                    let cell = &mut buf[(area.x + x as u16, area.y + y as u16)];
                    // Only the frame's own glyphs join the pulse.
                    if !matches!(cell.symbol(), "│" | "─" | "┌" | "┐" | "└" | "┘") {
                        continue;
                    }
                    let fg = cell.style().fg.unwrap_or(Color::Reset);
                    cell.set_fg(lerp_color(fg, bright, env * 0.6));
                }
            }
        },
    )
}

/// The pulse's envelope over its lifetime (0.0..=1.0): a quick ease-out
/// rise, a short hold at the peak, then a smooth settle — a landing
/// thump, not a strobe.
fn landing_pulse_env(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.3 {
        ease_out_cubic(t / 0.3)
    } else if t < 0.5 {
        1.0
    } else {
        1.0 - ease_out_cubic((t - 0.5) / 0.5)
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
/// streaming its output: the text types in from the front. `delay_ms`
/// parks the reveal: the base delay holds the block until the delete
/// phase (ghost backspace + layout collapse) has finished — the new text
/// then streams into its FINAL position and never moves — and later
/// blocks cascade by an extra 60 ms each.
///
/// Hidden cells are REPAINTED with their own background color rather than
/// blanked: the glyphs stay (their width never changes, so ratatui's diff
/// never takes the wide→narrow path that broke CJK backgrounds), and the
/// original text color is captured on the first frame so the stream
/// cursor can restore it — no checkerboard, no visible space glyphs.
pub(crate) fn appear_effect(mask: Vec<Vec<(u16, u16)>>, delay_ms: u32) -> Effect {
    fx::delay(
        delay_ms,
        fx::effect_fn_buf(
            (mask, None::<std::collections::HashMap<(u16, u16), Color>>),
            (APPEAR_REVEAL_MS, Interpolation::Linear),
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

/// The deletion ghost's hold-before-backspace: the ghost stays whole
/// long enough to read before its cells scatter out.
pub(crate) const GHOST_HOLD_MS: u32 = 150;
/// The backspace scatter-out: ghost cells blank in reverse reading order
/// (right→left, bottom→top) over this span. Kept shorter than the
/// reveal so the erase reads as a quick tidy-up, not the main event.
pub(crate) const GHOST_BACKSPACE_MS: u32 = 400;
/// The whole delete phase (hold + backspace). The layout collapses at
/// exactly this mark, and the add phase's scatter-in starts after it
/// (see `App::history_ghost_until` and `GHOST_SETTLE_MS`).
pub(crate) const GHOST_PHASE_MS: u32 = GHOST_HOLD_MS + GHOST_BACKSPACE_MS;

/// How long the scatter-in reveal takes: the new characters stream in
/// left→right over this span. Longer than the backspace so the add
/// phase — the point of the journey — gets the eye time to register.
pub(crate) const APPEAR_REVEAL_MS: u32 = 550;

/// Scatter-out for the deletion ghosts, BACKSPACE-style: the ghost stays
/// whole for [`GHOST_HOLD_MS`] (long enough to read), then its cells
/// blank out in REVERSE reading order — right→left, bottom→top, exactly
/// like backspacing through the text — completing when the
/// [`GHOST_PHASE_MS`] ghost lifetime collapses the layout. The ghost
/// rows carry NO background (the deletion band was dropped from the
/// spec), so the cells are BLANKED rather than repainted — wide glyphs
/// go through [`blank_cell`], which keeps the trailing column intact.
pub(crate) fn ghost_effect() -> Effect {
    fx::delay(
        GHOST_HOLD_MS,
        fx::effect_fn_buf(
            (None::<Vec<usize>>, 0usize),
            (GHOST_BACKSPACE_MS, Interpolation::Linear),
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

// ---- marks モードのマーカーが引かれる演出 ------------------------------

/// 段 1（ページ色から琥珀の半分まで、全部同時に上げる）の長さ。
///
/// まず「どこが光るのか」が一望できる。
pub(crate) const MARKS_FADE_MS: u32 = 250;
/// 線が引かれるまで（ミリ秒）。左→右のスイープ。
pub(crate) const MARKS_SWEEP_MS: u32 = 450;
/// 線が通ったセルが確定色まで乾く長さ。
pub(crate) const MARKS_COOL_MS: u32 = 250;
/// 演出の全長（250 + 450 + 250 = 950 ms）。
///
/// **1 秒以内**（2026-09-22 の読み手の注文 3）。判定の到着は数秒かかるが、
/// 演出はそれとは別物で、待たせるためのものではない。
pub(crate) const MARKS_REVEAL_MS: u32 = MARKS_FADE_MS + MARKS_SWEEP_MS + MARKS_COOL_MS;

/// スイープが通る前の濃さ（琥珀への blend の割合）。
const MARKS_PRE_SWEEP: f32 = 0.5;

/// フッタ右下の読み出しが**変化の瞬間だけ**明るくなる長さ。
///
/// 300 ms は「目の端で気づくが、読みに来る頃には戻っている」長さである。
/// マーカーの演出（[`MARKS_REVEAL_MS`] = 950 ms）より短いのは、こちらが
/// 「値が変わった」の合図で、あちらは「線が引かれる」という出来事だから。
pub(crate) const READOUT_FLASH_MS: u32 = 300;

/// **読み出しが一瞬明るくなる演出**（フッタの右下。2026-09-22 まではタイトル
/// 行の右にあった）。
///
/// 問いを変えた・つまみを回した・答えが届いた、の 3 つで立つ。toast は
/// 出さない — 読み手はもう値を見ているので、同じことを 2 か所で言う
/// 必要が無い（読み手の注文、2026-09-22）。
///
/// # 面で掴む（色ではない）
///
/// マーカーの演出は琥珀の背景でセルを選ぶが、こちらは**読み出しの矩形**を
/// 呼び出し側が渡す（`crate::chrome::FooterLayout::flash_x`）。同じ薄さ
/// （`Color::DarkGray`）の要素が同じ行に並んでいる（左のキー案内）ので、
/// 色で掴むとそちらまで光る。
///
/// **座布団があればその 4 桁だけ**を渡す — 変わったのは本数だからである。
/// そこは琥珀の地に黒字なので、前景は琥珀から黒へ**落ちてくる**（数字が
/// 沸いて出る形）。座布団の無いとき（0 本・`analyzing…`・`no scores`）は
/// 読み出し全体で、薄い灰へ戻る形になる。
///
/// 色は `DecorationStyles::mark_tick()`（テーマから解決済みの琥珀）を
/// 受け取る。**焼き込まない。**
pub(crate) fn readout_flash_effect(bright: Color) -> Effect {
    fx::effect_fn_buf(
        bright,
        (READOUT_FLASH_MS, Interpolation::Linear),
        move |bright, ctx, buf| {
            let alpha = ctx.timer.alpha();
            let area = ctx.area;
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    let cell = &mut buf[(x, y)];
                    // 描かれたままの前景（薄い灰）へ向かって戻る。
                    let painted = cell.style().fg.unwrap_or(*bright);
                    cell.set_fg(lerp_color(*bright, painted, alpha.clamp(0.0, 1.0)));
                }
            }
        },
    )
}

/// **マーカーが引かれる演出。** 答えが届いた瞬間に走る、3 段の 950 ms:
///
/// ```text
/// 0 ─── 250 ms ───┬───────── 450 ms ─────────┬── 250 ms ──
///   半分まで一斉に上がる   濃い琥珀の線が左→右に引かれる   後ろが確定色に乾く
/// ```
///
/// 1 つのセルの時間変化は
/// **ページ → 半分 →（線が通過）`bright` → `amber`** である。
/// 前線より右は半分で待ち、前線が来た瞬間に一番濃くなり、そこから乾く。
///
/// **線だけ濃く、確定は読みやすい濃さのまま。** 読み手の注文（2026-09-23）
/// で、確定色は amber（0.27）のまま、引かれる線に
/// [`MARK_FLASH_BLEND`]（0.65）を借りる形になった。以前は線も確定色で、
/// 「左から右にシュッと引かれる」動きはそのままに線だけを強くしている。
///
/// # どのセルを掴むか — 背景色そのもの
///
/// toast が [`CellFilter::BgColor`] でバナーのセルだけを掴んでいるのと
/// 同じ手で、**琥珀の背景色でフィルタする**。光っているセルは琥珀の背景を
/// 持っている（`DecorationKind::SemanticMark` は背景しか書かない）ので、
/// 「今回どこが光ったか」の台帳を別に持たなくてよい。
///
/// 副作用として**カーソル行・選択行の琥珀は演出に入らない**。帯の上の
/// 琥珀の句は確定色ではなく一段濃い琥珀（`DecorationStyles::mark_band_bg`、
/// `docs/gotchas/rendering.md`「帯とマークが重なったとき」）で塗られるので、
/// このフィルタに掛からず、線を引かれずに最初から出ている。
///
/// # 色は焼き込まない
///
/// `amber` / `bright` / `page` は `DecorationStyles`（テーマから解決済み）
/// から来る。`--light` でも `--theme DarkNeon` でも `--mark-blend` を
/// 動かしても同じ演出が乗るのはそのためで、ここに色を書くと片方でしか
/// 合わなくなる。
pub(crate) fn marks_reveal_effect(amber: Color, bright: Color, page: Color) -> Effect {
    let fade = MARKS_FADE_MS as f32 / MARKS_REVEAL_MS as f32;
    let cool = MARKS_COOL_MS as f32 / MARKS_REVEAL_MS as f32;
    // 前線が動ける幅。最後のセルがちょうど演出の終わりに乾き終わる。
    let sweep = 1.0 - fade - cool;
    let half = lerp_color(page, amber, MARKS_PRE_SWEEP);
    let mut effect = fx::effect_fn_buf(
        (amber, bright, page, half),
        (MARKS_REVEAL_MS, Interpolation::Linear),
        move |(amber, bright, page, half), ctx, buf| {
            let alpha = ctx.timer.alpha();
            let area = ctx.area;
            if area.width == 0 {
                return;
            }
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    let cell = &mut buf[(x, y)];
                    // 琥珀のセルだけ。帯の下の行も、素の本文も触らない。
                    if cell.style().bg != Some(*amber) {
                        continue;
                    }
                    let bg = if alpha <= fade {
                        // 段 1: ページ色から半分まで、全部同時に。
                        let t = (alpha / fade).clamp(0.0, 1.0) * MARKS_PRE_SWEEP;
                        lerp_color(*page, *amber, t)
                    } else {
                        // 段 2: 前線がこのセルへ来る時刻。右側は半分で待つ。
                        let front =
                            fade + (x as f32 - area.x as f32) / area.width as f32 * sweep;
                        let age = (alpha - front) / cool;
                        if age <= 0.0 {
                            *half
                        } else {
                            // 段 3: 通った線は `bright` から確定色へ乾く。
                            lerp_color(*bright, *amber, age.clamp(0.0, 1.0))
                        }
                    };
                    cell.set_bg(bg);
                }
            }
        },
    );
    // 掴むのは琥珀のセルだけ。`effect_fn_buf` の中でも見ているが、
    // フィルタを掛けておくと tachyonfx 側が走らせるセルも絞れる。
    effect.filter(CellFilter::BgColor(amber));
    effect
}


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::time::Duration;

    /// The scrollbar thumb (`▐`, one column inside the right border) is
    /// chrome, not text: the tail scan must not mistake it for the
    /// row's last glyph, or every row the thumb covers would lose its
    /// whole sky (the regression: no stars left of the scrollbar).
    #[test]
    fn starfield_shines_left_of_the_scrollbar_thumb() {
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        let thumb_x = area.right() - 2;
        for y in (area.y + 1)..area.bottom() - 1 {
            buf[(thumb_x, y)].set_symbol("▐");
        }
        let mut fx = starfield_effect(false);
        fx.process(Duration::from_millis(16), &mut buf, area);
        // Depth-0 stars survive at every depth (density only grows and
        // the glyph hash ignores depth), so the check holds even if a
        // parallel test's draw() bumps the shared depth. Stop one row
        // short of the interior bottom so a concurrently visible
        // timeline bar cannot shrink the sky under the assertion.
        let mut seen = 0usize;
        for y in (area.y + 1)..area.bottom() - 2 {
            for x in (area.x + 1)..thumb_x {
                if let Some(star) = starfield_star_at(x, y, 0.0) {
                    seen += 1;
                    assert_eq!(
                        buf[(x, y)].symbol(),
                        star.glyph.to_string(),
                        "star at ({x},{y}) must shine left of the thumb"
                    );
                }
            }
        }
        assert!(seen > 0, "the seeded sky places stars in this area");
        // The thumb itself is never overwritten by a star.
        for y in (area.y + 1)..area.bottom() - 1 {
            assert_eq!(buf[(thumb_x, y)].symbol(), "▐");
        }
    }

    // ---- マーカーが引かれる演出 -----------------------------------------

    /// 琥珀のセルだけを並べた 1 行のバッファ。演出は背景色でセルを選ぶので、
    /// 描画ループと同じ状態を作る。
    fn amber_row(area: Rect, amber: Color) -> Buffer {
        let mut buf = Buffer::empty(area);
        for x in area.x..area.right() {
            buf[(x, area.y)].set_bg(amber);
        }
        buf
    }

    fn bg_of(buf: &Buffer, x: u16, y: u16) -> Color {
        buf[(x, y)].style().bg.expect("a background")
    }

    /// 2 色のあいだの距離（RGB のマンハッタン）。「どちらに近いか」だけを
    /// 見るためのもの。
    fn dist(a: Color, b: Color) -> i32 {
        let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg_, bb)) = (a, b) else {
            panic!("RGB")
        };
        (ar as i32 - br as i32).abs()
            + (ag as i32 - bg_ as i32).abs()
            + (ab as i32 - bb as i32).abs()
    }

    /// **演出は、半分まで上がってから、濃い琥珀の線が左→右に引かれ、
    /// 後ろが確定色へ乾く。** 1 つのセルの変化は
    /// **ページ → 半分 → `bright` → `amber`**。
    #[test]
    fn the_reveal_puts_a_bright_line_across_and_settles_behind_it() {
        let amber = Color::Rgb(90, 69, 33);
        let bright = Color::Rgb(176, 124, 16);
        let page = Color::Rgb(30, 30, 46);
        let half = lerp_color(page, amber, MARKS_PRE_SWEEP);
        let area = Rect::new(0, 0, 8, 1);
        let mut fx = marks_reveal_effect(amber, bright, page);
        let mut step = |ms: u64| {
            let mut buf = amber_row(area, amber);
            fx.process(Duration::from_millis(ms), &mut buf, area);
            buf
        };

        // 段 1 の終わり: 全部が半分。
        let buf = step(MARKS_FADE_MS as u64);
        for x in area.x..area.right() {
            assert_eq!(bg_of(&buf, x, 0), half, "段 1 は半分まで");
        }

        // 前線が左端に着いた直後: **左端だけが濃い琥珀**で、右側は半分のまま。
        let buf = step(1);
        let left = bg_of(&buf, 0, 0);
        let right = bg_of(&buf, area.right() - 1, 0);
        assert!(
            dist(left, bright) < dist(left, half),
            "前線の直後が明るくない: {left:?}"
        );
        assert_eq!(right, half, "前線の来ていない右端は半分のまま");

        // 途中: 左端はもう確定色へ乾き始め、右端はまだ半分。
        let buf = step(199);
        let left = bg_of(&buf, 0, 0);
        assert!(
            dist(left, amber) < dist(left, bright),
            "通った線が乾いていない: {left:?}"
        );
        assert_eq!(bg_of(&buf, area.right() - 1, 0), half);

        // 終わり: 全部が確定色。
        let buf = step(MARKS_REVEAL_MS as u64);
        for x in area.x..area.right() {
            assert_eq!(bg_of(&buf, x, 0), amber, "x={x}");
        }
    }
}
