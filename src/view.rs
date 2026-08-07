//! View mode: natively rendered markdown held in memory.
//!
//! The markdown is rendered in-process by [`crate::render`] (tui-markdown
//! over pulldown-cmark) into styled display rows plus the source-line
//! mapping; this module owns the row cursor, scrolling, and the view↔comment
//! handoff.
//!
//! Scrolling is row-based; the scroll fraction (offset / total rows) is the
//! handoff between view mode and source mode.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;
use ratatui::text::{Line, Text};

use crate::highlight::{Highlighter, Span};
use crate::render::{self, Rendered, Segment};
use crate::source::Source;

/// Cursor/selection background for dark terminal themes: a neutral gray a
/// step brighter than ANSI bright-black.
const SELECTED_BG_DARK: Color = Color::Rgb(88, 91, 112);
/// Selection background for light terminal themes: a pale cool gray.
const SELECTED_BG_LIGHT: Color = Color::Rgb(210, 210, 220);

/// Changed-line background for dark terminal themes: a muted green.
const CHANGED_BG_DARK: Color = Color::Rgb(40, 55, 40);
/// Changed-line background for light terminal themes: a pale mint.
const CHANGED_BG_LIGHT: Color = Color::Rgb(210, 240, 210);

pub fn selected_bg(light: bool) -> Color {
    if light { SELECTED_BG_LIGHT } else { SELECTED_BG_DARK }
}

pub fn changed_bg(light: bool) -> Color {
    if light { CHANGED_BG_LIGHT } else { CHANGED_BG_DARK }
}

/// View-mode frame border color.
pub fn border_color(light: bool) -> Color {
    if light { Color::Rgb(180, 180, 190) } else { Color::Rgb(127, 132, 156) }
}

/// A 1-column marker cell drawn over the frame's left border (see
/// [`ViewState::visible_text`]): `>` marks the cursor row, `▌` a
/// comment-covered row; rows with no marker reproduce the border's `│`,
/// so the column reads as the frame itself.
#[derive(Debug, Clone)]
pub struct GutterCell {
    pub glyph: &'static str,
    pub style: Style,
}

impl GutterCell {
    /// The neutral cell: the frame's `│` with the border style.
    pub fn border(style: Style) -> Self {
        Self { glyph: "│", style }
    }
}

/// The rendered view: one styled row per rendered output line.
#[derive(Debug, Default)]
pub struct ViewState {
    pub rows: Vec<Vec<Span>>,
    /// Scroll offset in rows.
    pub offset: usize,
    /// Source-line cursor (0-based, so `v`/Esc hand the exact same line
    /// between view and source mode instead of a rounded fraction).
    pub cursor: usize,
    /// First rendered row of each source line (source line i spans
    /// `source_starts[i] .. source_starts[i+1]`, last line to rows.len()).
    pub source_starts: Vec<usize>,
    /// Per-row phrase segments (the byte range each source line's text
    /// occupies in the row), used to highlight exactly the selected lines'
    /// text instead of whole rows.
    pub row_segments: Vec<Vec<Segment>>,
    /// True for rows that are inline comment cards (inserted into the
    /// layout after a comment's end line). Card rows render exactly as
    /// built — no gutter, no cursor/selection background — like comment
    /// mode's cards.
    pub card_rows: Vec<bool>,
    /// Raw source text of invisible lines (see [`Rendered::ghost`]),
    /// painted onto a nearby blank row when the cursor or the selection
    /// touches the line. No mode switch, no reflow: information only.
    pub ghost: Vec<Option<String>>,
    /// The wrap width the rows were rendered at (ghost text is truncated
    /// to it).
    pub width: usize,
}

impl ViewState {
    /// Render `source` natively at `columns` wide with the `--theme`
    /// (used for fenced-code highlighting). The native renderer never
    /// fails, so there is no fallback path.
    pub fn render(source: &Source, columns: u16, highlighter: &Highlighter) -> Self {
        let Rendered {
            rows,
            source_starts,
            row_segments,
            ghost,
        } = render::render(source, columns as usize, highlighter);
        let card_rows = vec![false; rows.len()];
        Self {
            rows,
            offset: 0,
            cursor: 0,
            source_starts,
            row_segments,
            card_rows,
            ghost,
            width: columns as usize,
        }
    }

    /// Move the cursor by *display* rows: j/k step through the rendered
    /// rows and land on the source line each row belongs to. Moving down
    /// jumps past a wrapped line's continuation rows (same source line) to
    /// the next visible row, and skips source lines that share one
    /// rendered row (the renderer joins a paragraph's soft line breaks) — so the
    /// cursor always lands on a line that visibly advances instead of
    /// "sticking" on rows that share a source line.
    pub fn move_cursor_display(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        // Anchor on the line's last rendered row when moving down (jumps
        // past wrapped continuations), its first row when moving up.
        let cur = if delta > 0 {
            self.cursor_end_row()
        } else {
            self.cursor_row()
        } as isize;
        let target = (cur + delta).clamp(0, last) as usize;
        if let Some(line) = self.source_line_at(target) {
            self.cursor = line;
        }
    }

    pub fn jump_top(&mut self) {
        self.cursor = 0;
        self.offset = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.cursor = self.max_cursor();
    }

    /// Largest valid source line.
    pub fn max_cursor(&self) -> usize {
        self.source_starts.len().saturating_sub(1)
    }

    /// Keep the cursor's first rendered row visible. The down-branch uses
    /// the cursor's LAST row (wrapped continuations and comment cards
    /// included), so navigating never leaves the block clipped at the
    /// bottom — mirrors source mode's end-based scroll.
    pub fn keep_cursor_visible(&mut self, viewport: usize) {
        let viewport = viewport.max(1);
        let row = self.cursor_row();
        if row < self.offset {
            self.offset = row;
        } else if self.cursor_end_row() >= self.offset + viewport {
            self.offset = self.cursor_end_row() + 1 - viewport;
        }
    }

    /// The first rendered row of the cursor's source line.
    pub fn cursor_row(&self) -> usize {
        self.source_starts.get(self.cursor).copied().unwrap_or(0)
    }

    /// The last rendered row of the cursor's source line (so wrapped /
    /// merged lines keep their whole extent inside the viewport). Mirrors
    /// `text()`'s extent logic: merged source lines share a row, so the
    /// span is at least one row even when the next line starts on the
    /// same rendered row (end <= start).
    pub fn cursor_end_row(&self) -> usize {
        let start = self.cursor_row();
        let mut end = self
            .source_starts
            .get(self.cursor + 1)
            .copied()
            .unwrap_or(self.rows.len());
        if end <= start {
            end = (start + 1).min(self.rows.len());
        }
        end.saturating_sub(1)
    }

    /// The source line whose rendered rows contain `display_row` (0-based
    /// position in the whole rendered document, before `offset`). Used by
    /// mouse clicks to map a screen row back to a source line.
    pub fn source_line_at(&self, display_row: usize) -> Option<usize> {
        for (i, &start) in self.source_starts.iter().enumerate() {
            let end = self
                .source_starts
                .get(i + 1)
                .copied()
                .unwrap_or(self.rows.len());
            if display_row >= start && display_row < end {
                return Some(i);
            }
        }
        None
    }

    /// The source line under display position `(row, col)`: the phrase
    /// segment whose text occupies the column (a merged row holds several
    /// lines' phrases side by side, so clicking a phrase selects its own
    /// line), falling back to the row's attribution when the column misses
    /// every segment (the marker column on the border, a wrapped gap).
    /// Used by mouse clicks and
    /// drags so the selection follows source mode's line units.
    pub fn line_at_position(&self, display_row: usize, col: usize) -> Option<usize> {
        let text: String = self
            .rows
            .get(display_row)?
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        let segs = self.row_segments.get(display_row)?;
        for s in segs {
            let start = UnicodeWidthStr::width(&text[..s.start]);
            let end = UnicodeWidthStr::width(&text[..s.end]);
            if col >= start && col < end {
                return Some(s.line);
            }
        }
        self.source_line_at(display_row)
    }

    /// Wheel scroll (herdr-review style, matching source mode): move only
    /// the viewport. The cursor keeps its absolute file position and may
    /// scroll off screen — scrolling back finds it exactly where it was.
    /// Keyboard navigation (j/k/G/PgUp/PgDn/Ctrl+u/d), mouse clicks, and
    /// the mode handoffs call [`ViewState::keep_cursor_visible`] themselves.
    pub fn wheel_scroll(&mut self, dir: i32, viewport: usize) {
        let max_offset = self.rows.len().saturating_sub(viewport).max(1);
        if dir > 0 {
            self.offset = (self.offset + 1).min(max_offset);
        } else {
            self.offset = self.offset.saturating_sub(1);
        }
    }

    /// Put the cursor on source line `n` (exact handback from source mode).
    pub fn goto_source_line(&mut self, n: usize) {
        self.cursor = n.min(self.max_cursor());
    }

    /// The handoff fraction over source lines (0..=1), so resizes and mode
    /// switches stay on the same source line.
    pub fn cursor_fraction(&self) -> f64 {
        let max = self.max_cursor();
        if max == 0 {
            0.0
        } else {
            self.cursor as f64 / max as f64
        }
    }

    /// Put the cursor at the given 0..=1 fraction of the source lines.
    pub fn goto_fraction(&mut self, fraction: f64) {
        let max = self.max_cursor();
        self.cursor = (fraction.clamp(0.0, 1.0) * max as f64).round() as usize;
        self.cursor = self.cursor.min(max);
    }

    /// The rows visible in a `viewport`-tall window starting at `offset`, as
    /// a ratatui `Text`, plus the matching marker column (one [`GutterCell`]
    /// per visible row). The marker column is drawn over the frame's left
    /// border by the caller (see `draw_view`): `>` marks the cursor row, a
    /// yellow `▌` a comment-covered row (`marked` is indexed by source
    /// line); rows without a marker carry the border's `│`. Rows inside
    /// `selection` (an inclusive
    /// DISPLAY-ROW range — the view's selection is tracked by rows, immune
    /// to source-line mapping imprecision) and the cursor row get the calm
    /// DarkGray background ([`SELECTED_BG`]) — including blank lines inside
    /// the range and wrapped continuation rows, so a comment range or a
    /// selection reads as one continuous band. Only the visible window is
    /// built, so a per-frame redraw costs O(viewport) instead of re-cloning
    /// the whole rendered document (which matters for the 100k-line files
    /// the view keeps in memory).
    /// Build the visible window (rows [`Self::offset`..]) and the marker
    /// column (`>` cursor marker, `▌` comment marker, `│` elsewhere) and
    /// the selection highlight. Every row is wrapped in a 1-column pad on
    /// each side, so the text column floats off both borders; the pads
    /// carry the highlight background on cursor/selection rows, keeping
    /// the band unbroken from the marker to the frame.
    /// The selection is a LINE range (identical to source mode): a span is
    /// highlighted exactly when its text intersects a selected line's
    /// phrase segment (see [`Segment`]), so merged rows highlight only the
    /// selected lines' text — rows without segments (blanks, unattributed
    /// wraps) fall back to the row span.
    #[allow(clippy::too_many_arguments)]
    pub fn visible_text(
        &self,
        viewport: usize,
        marked: &[bool],
        changed: &[bool],
        deleted: &[bool],
        selection: Option<(usize, usize)>,
        selected_bg: Color,
        border_style: Style,
    ) -> (Text<'static>, Vec<GutterCell>) {
        if self.rows.is_empty() {
            return (Text::default(), Vec::new());
        }
        let end = (self.offset + viewport).min(self.rows.len());
        let mut start = self.cursor_row();
        let mut c_end = self
            .source_starts
            .get(self.cursor + 1)
            .copied()
            .unwrap_or(self.rows.len());
        // Blank lines share the previous row; still show at least one.
        if c_end <= start {
            c_end = (start + 1).min(self.rows.len());
        }
        // The selection's row span (the fallback for unattributed rows).
        let sel_rows = selection.map(|(a, b)| {
            let f = self.source_starts.get(a).copied().unwrap_or(0);
            let e = self
                .source_starts
                .get(b + 1)
                .copied()
                .unwrap_or(self.rows.len());
            (f, e.saturating_sub(1).max(f))
        });
        let ghosts = self.ghost_rows(selection, end);
        // The cursor line's visual home: when its ghost was painted on a
        // BORROWED row (a closing fence shares the last code row, so the
        // ghost sits one below), the `>` marker and the cursor band follow
        // the ghost — the marker must point at the row that shows the
        // line, not at a neighbour's content.
        if let Some((&g, _)) = ghosts.iter().find(|&(_, &l)| l == self.cursor)
            && g != start
        {
            start = g;
            c_end = g + 1;
        }
        let mut lines = Vec::with_capacity(end.saturating_sub(self.offset));
        let mut gutter: Vec<GutterCell> = Vec::with_capacity(lines.capacity());
        // Walk the visible window; `src` tracks the source line each row
        // belongs to (source_starts is sorted, so one pointer suffices).
        let mut src = 0usize;
        for abs in self.offset..end {
            // Inline comment cards render exactly as built: full-width rules
            // and text, no gutter, no cursor/selection background.
            if self.card_rows.get(abs).copied().unwrap_or(false) {
                // Inline cards float in the text column like every other
                // row: one pad on each side.
                let mut spans: Vec<ratatui::text::Span> =
                    vec![ratatui::text::Span::raw(" ")];
                spans.extend(self.rows[abs].iter().map(|s| ratatui::text::Span {
                    content: s.text.clone().into(),
                    style: s.style,
                }));
                spans.push(ratatui::text::Span::raw(" "));
                lines.push(Line::from(spans));
                // Inline cards keep the frame's border: no marker.
                gutter.push(GutterCell::border(border_style));
                continue;
            }
            while src + 1 < self.source_starts.len() && self.source_starts[src + 1] <= abs {
                src += 1;
            }
            let cursor_row = abs >= start && abs < c_end;
            // A row-group is the set of source lines sharing one start row
            // (merged paragraphs, wrapped continuations, blank lines). The
            // row is marked when any member is comment-covered.
            let row_start = self.source_starts[src];
            let mut marked_row = false;
            for i in (0..=src).rev() {
                marked_row |= marked.get(i).copied().unwrap_or(false);
                if i == 0 || self.source_starts[i - 1] != row_start {
                    break;
                }
            }
            let mut changed_row = false;
            for i in (0..=src).rev() {
                changed_row |= changed.get(i).copied().unwrap_or(false);
                if i == 0 || self.source_starts[i - 1] != row_start {
                    break;
                }
            }
            let mut deleted_row = false;
            for i in (0..=src).rev() {
                deleted_row |= deleted.get(i).copied().unwrap_or(false);
                if i == 0 || self.source_starts[i - 1] != row_start {
                    break;
                }
            }
            // The selection is a LINE range; the row span is the rows
            // those lines render on (merged rows can share one). The exact
            // gray highlights a span exactly when its byte range
            // intersects a selected line's phrase segment; rows with no
            // segments (blanks, unattributed wraps) fall back to the row
            // span.
            let in_sel_row = sel_rows.is_some_and(|(a, b)| abs >= a && abs <= b);
            let segments = self.row_segments.get(abs).map(|s| s.as_slice()).unwrap_or(&[]);
            // The cursor line's phrase inside this row; rows with no
            // segments (blanks, unattributed wraps) fall back to the whole
            // row, so the line cursor stays visible everywhere.
            let cursor_seg = segments.iter().find(|s| s.line == self.cursor);
            let span_hl = |range: (usize, usize)| {
                let sel_hl = match selection {
                    None => false,
                    Some((a, b)) => {
                        let line_sel = |l: usize| l >= a && l <= b;
                        if segments
                            .iter()
                            .any(|s| line_sel(s.line) && s.start < range.1 && s.end > range.0)
                        {
                            true
                        } else {
                            segments.is_empty() && in_sel_row
                        }
                    }
                };
                if sel_hl {
                    return true;
                }
                if !cursor_row {
                    return false;
                }
                match cursor_seg {
                    Some(s) => s.start < range.1 && s.end > range.0,
                    None => segments.is_empty(),
                }
            };
            let highlight_style = Style::default().bg(selected_bg);
            let gutter_hl = cursor_row || in_sel_row;
            let mut spans: Vec<ratatui::text::Span> = Vec::new();
            // The marker column rides the frame's left border (drawn by
            // `draw_view` over the border cells): `>` marks the cursor
            // line's FIRST display row (parallel to source mode, where the
            // marker sits on the first wrapped row only), `▌` a
            // comment-covered row; rows without a marker reproduce the
            // border's `│`. The selection background extends over the
            // marker, so cursor/selection rows read as one band running to
            // the page edge. The cursor glyph is bold LightCyan — it must
            // be findable at a glance (yellow is the comment marker's
            // color).
            let (glyph, mut marker_style) = if abs == start {
                (
                    ">",
                    Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
                )
            } else if in_sel_row {
                // Selected rows carry a cyan bar (the composer's color, the
                // same width as the yellow comment bar): the pending range
                // reads in the marker column, not just as the background
                // band.
                ("▌", Style::default().fg(Color::Cyan))
            } else if marked_row {
                ("▌", Style::default().fg(Color::Yellow))
            } else if changed_row {
                ("▌", Style::default().fg(Color::Green))
            } else if deleted_row {
                ("▌", Style::default().fg(Color::Red))
            } else {
                ("│", border_style)
            };
            if gutter_hl {
                marker_style = marker_style.bg(selected_bg);
            }
            gutter.push(GutterCell { glyph, style: marker_style });
            // The text column floats one column off each border: a pad on
            // both sides (the marker at the left border, the frame at the
            // right). Cursor/selection rows carry the highlight background
            // over the pads too, so the band runs unbroken from the marker
            // to the frame.
            let pad_style = if gutter_hl {
                highlight_style
            } else {
                Style::default()
            };
            spans.push(ratatui::text::Span::styled(" ", pad_style));
            // Content spans, with the exact highlight background applied.
            let mut off = 0usize;
            let mut content: Vec<ratatui::text::Span> = self.rows[abs]
                .iter()
                .map(|s| {
                    let range = (off, off + s.text.len());
                    off = range.1;
                    let style = if span_hl(range) {
                        s.style.bg(selected_bg)
                    } else {
                        s.style
                    };
                    ratatui::text::Span {
                        content: s.text.clone().into(),
                        style,
                    }
                })
                .collect();
            // A blank row inside the highlight still needs a visible glyph
            // for the background; a blank row hosting a ghost shows the
            // invisible line's raw source instead. Blank renderer rows
            // parse to a single empty-text span, so "no visible text" is
            // the test, not "no spans".
            let has_text = content.iter().any(|s| !s.content.is_empty());
            if !has_text {
                if let Some(text) = ghosts.get(&abs).and_then(|l| self.ghost_text(*l)) {
                    let mut style = Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC);
                    if cursor_row || in_sel_row {
                        style = style.bg(selected_bg);
                    }
                    content.push(ratatui::text::Span {
                        content: text.into(),
                        style,
                    });
                } else if cursor_row || in_sel_row {
                    content.push(ratatui::text::Span {
                        content: " ".to_string().into(),
                        style: highlight_style,
                    });
                }
            }
            spans.extend(content);
            spans.push(ratatui::text::Span::styled(" ", pad_style));
            lines.push(Line::from(spans));
        }
        (Text::from(lines), gutter)
    }

    /// The ghost assignments for this frame (display row → invisible source
    /// line): a line that rendered no text and is touched by the cursor or
    /// the selection paints its raw source onto a nearby blank row — its
    /// own start row, or the row just below (a closing fence shares the
    /// last code row, so it borrows the spacer beneath). Best-effort by
    /// design: one ghost per row, the cursor's line first, and a line with
    /// no free blank row nearby is skipped silently. Paint-time only —
    /// the mapping and the layout are never touched.
    fn ghost_rows(&self, selection: Option<(usize, usize)>, end: usize) -> HashMap<usize, usize> {
        let mut ghosts: HashMap<usize, usize> = HashMap::new();
        let place = |line: usize, ghosts: &mut HashMap<usize, usize>| {
            if !matches!(self.ghost.get(line), Some(Some(_))) {
                return;
            }
            let start = self.source_starts.get(line).copied().unwrap_or(0);
            for row in [start, start + 1] {
                if row >= self.rows.len() || self.card_rows.get(row).copied().unwrap_or(false) {
                    continue;
                }
                let blank = self.rows[row].iter().all(|s| s.text.is_empty());
                if blank && !ghosts.contains_key(&row) {
                    ghosts.insert(row, line);
                    return;
                }
            }
        };
        place(self.cursor, &mut ghosts);
        if let Some((a, b)) = selection {
            // Only lines whose rows can reach the visible window matter
            // (a select-all must not scan the whole file every frame).
            let lo = self
                .source_starts
                .partition_point(|&s| s + 1 < self.offset);
            let hi = self.source_starts.partition_point(|&s| s <= end);
            for line in a.max(lo)..(b + 1).min(hi) {
                place(line, &mut ghosts);
            }
        }
        ghosts
    }

    /// The ghost text for `line`, truncated to the render width (ghost
    /// text must never wrap — reflow would break the row layout the
    /// mapping is built on).
    fn ghost_text(&self, line: usize) -> Option<String> {
        let raw = self.ghost.get(line)?.as_deref()?;
        if UnicodeWidthStr::width(raw) <= self.width {
            return Some(raw.to_string());
        }
        let budget = self.width.saturating_sub(1);
        let mut out = String::new();
        let mut w = 0usize;
        for ch in raw.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > budget {
                break;
            }
            out.push(ch);
            w += cw;
        }
        out.push('…');
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{selected_bg, Span, ViewState};
    use crate::highlight::Highlighter;
    use ratatui::style::Color;
    use crate::source::Source;
    use ratatui::style::Style;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn every_segment_resolves_back_to_its_own_line() {
        // Clicking at a segment's display column resolves to the segment's
        // source line — the phrase-precise mapping the view promises.
        // This is the functional invariant the exact renderer attribution
        // must satisfy for every row of the whole fixture.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.md");
        std::fs::copy("testdata/full.md", &path).unwrap();
        let source = Source::load(path).unwrap();
        let view = ViewState::render(&source, 80, &Highlighter::new(None, false));
        for (r, segs) in view.row_segments.iter().enumerate() {
            for seg in segs {
                let text: String = view.rows[r].iter().map(|s| s.text.as_str()).collect();
                let col = UnicodeWidthStr::width(&text[..seg.start]);
                let resolved = view.line_at_position(r, col).unwrap_or(0);
                assert_eq!(
                    resolved, seg.line,
                    "row {r} col {col}: expected line {}, got {resolved}",
                    seg.line
                );
            }
        }
    }

    /// The concatenated text of `visible_text`'s row `i` (content only —
    /// the marker column is separate; see [`gutter_at`]).
    fn row_text(view: &ViewState, sel: Option<(usize, usize)>, i: usize) -> String {
        view.visible_text(100, &[], &[], &[], sel, Color::Rgb(88, 91, 112), Style::default())
            .0
            .lines[i]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// The marker glyph of `visible_text`'s row `i` (the cell drawn over
    /// the frame's left border).
    fn gutter_at(view: &ViewState, sel: Option<(usize, usize)>, i: usize) -> &'static str {
        view.visible_text(100, &[], &[], &[], sel, Color::Rgb(88, 91, 112), Style::default())
            .1
            .get(i)
            .map(|c| c.glyph)
            .unwrap_or("")
    }

    #[test]
    fn ghost_paints_the_invisible_line_under_the_cursor() {
        // A ref-def renders nothing and shares the blank's row; putting the
        // cursor on it paints its raw source there — no mode switch, no
        // reflow, information only.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "本文の段落です\n\n[ref1]: https://example.com\n\n## 見出し\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let mut view = ViewState::render(&source, 60, &Highlighter::new(None, false));
        let ghost_row = view.source_starts[2];
        // Cursor elsewhere: the blank row stays blank (the view is quiet).
        view.cursor = 0;
        assert_eq!(row_text(&view, None, ghost_row).trim(), "");
        // Cursor on the ref-def: its raw source appears on its start row.
        view.cursor = 2;
        assert!(
            row_text(&view, None, ghost_row).contains("[ref1]: https://example.com"),
            "ghost shows the raw ref-def"
        );
        // A selection covering the line triggers the ghost too.
        view.cursor = 0;
        assert!(
            row_text(&view, Some((1, 3)), ghost_row).contains("[ref1]:"),
            "selection paints the ghost"
        );
        // The layout is untouched: same number of rows either way.
        assert_eq!(
            view.visible_text(100, &[], &[], &[], None, Color::Rgb(88, 91, 112), Style::default())
                .0
                .lines
                .len(),
            view.rows.len().min(100)
        );
    }

    #[test]
    fn ghost_borrows_the_row_below_when_its_own_row_has_text() {
        // A closing fence shares the LAST code row (which has text): the
        // ghost falls back to the blank row just below. The opening fence
        // sits on the code block's leading blank row and paints in place.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "```rust\nfn main() {}\n```\n\n次の段落\n").unwrap();
        let source = Source::load(path).unwrap();
        let mut view = ViewState::render(&source, 60, &Highlighter::new(None, false));
        // The closing fence's own row is the last code row — occupied.
        let code_row = view.source_starts[2];
        let code_text: String = view.rows[code_row].iter().map(|s| s.text.as_str()).collect();
        assert!(!code_text.trim().is_empty(), "closing fence shares a content row");
        view.cursor = 2;
        let text = row_text(&view, None, code_row + 1);
        assert!(text.contains("```"), "ghost borrowed the blank row below: {text:?}");
        // The borrowed row keeps its own identity in the mapping.
        assert_eq!(view.source_starts[3], code_row + 1);
        // The `>` marker and the cursor band follow the ghost onto the
        // borrowed row — the marker points at the row that shows the line,
        // not at the last code line's content.
        assert_eq!(
            gutter_at(&view, None, code_row + 1),
            ">",
            "the cursor marker sits on the ghost row"
        );
        assert_ne!(
            gutter_at(&view, None, code_row),
            ">",
            "the shared content row loses the marker"
        );
    }

    #[test]
    fn ghost_is_one_per_row_with_the_cursor_first() {
        // Two consecutive ref-defs share one blank row; under a selection
        // covering both, only one ghost appears — the cursor's line wins.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "本文\n\n[a]: https://a.example\n[b]: https://b.example\n\n## 次\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let mut view = ViewState::render(&source, 60, &Highlighter::new(None, false));
        assert_eq!(view.source_starts[2], view.source_starts[3], "ref-defs share a row");
        view.cursor = 3;
        let text = row_text(&view, Some((2, 3)), view.source_starts[3]);
        assert!(
            text.contains("[b]:") && !text.contains("[a]:"),
            "the cursor's line wins the shared row: {text:?}"
        );
    }

    #[test]
    fn ghost_text_truncates_to_the_render_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let long = format!("[ref]: https://example.com/{}\n\nx\n", "long/".repeat(40));
        std::fs::write(&path, format!("本文\n\n{long}")).unwrap();
        let source = Source::load(path).unwrap();
        let mut view = ViewState::render(&source, 40, &Highlighter::new(None, false));
        view.cursor = 2;
        let text = row_text(&view, None, view.source_starts[2]);
        // The row carries one pad on each side; strip them before
        // measuring the ghost itself.
        let text = text.trim();
        assert!(text.ends_with('…'), "over-wide ghost is cut with an ellipsis");
        assert!(
            UnicodeWidthStr::width(text) <= 40,
            "ghost stays within the text column"
        );
    }

    #[test]
    fn native_render_on_this_machine() {
        // The in-process renderer must produce rows and a mapping without
        // any external binary.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "# 見出し\n\n日本語 **太字** [link](https://x.com)\n").unwrap();
        let source = Source::load(path).unwrap();
        let view = ViewState::render(&source, 60, &Highlighter::new(None, false));
        assert!(view.rows.len() >= 3);
        let all: String = view
            .rows
            .iter()
            .flatten()
            .map(|s| s.text.as_str())
            .collect();
        assert!(all.contains("見出し"));
        assert!(all.contains("太字"));
        assert!(!all.contains('\x1b'), "no escape sequences leak into rows");
    }

    #[test]
    fn cursor_fractions_and_visibility() {
        let mut view = ViewState {
            rows: vec![vec![]; 10],
            offset: 0,
            cursor: 0,
            source_starts: (0..10).collect(),
            ..Default::default()
        };
        assert_eq!(view.cursor_fraction(), 0.0);
        view.move_cursor_display(5);
        assert_eq!(view.cursor, 5);
        view.keep_cursor_visible(3);
        assert_eq!(view.offset, 3, "cursor 5 in a 3-row viewport scrolls to 3");
        view.keep_cursor_visible(20);
        assert_eq!(view.offset, 3, "a tall viewport does not scroll back down");
        view.goto_fraction(1.0);
        assert_eq!(view.cursor, view.max_cursor());
        view.goto_fraction(0.0);
        assert_eq!(view.cursor, 0);
        view.jump_bottom();
        assert_eq!(view.cursor, view.max_cursor());
        view.jump_top();
        assert_eq!(view.cursor, 0);
        assert_eq!(view.offset, 0);
        // Clamped at the edges.
        view.move_cursor_display(100);
        assert_eq!(view.cursor, view.max_cursor());
        view.move_cursor_display(-100);
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn display_cursor_walks_rendered_rows() {
        // 3 source lines: line 0 wraps to 2 rows, lines 1-2 are one row
        // each. source_starts: [0, 2, 3].
        let span = |text: &str| Span {
            text: text.into(),
            style: Style::default(),
        };
        let mut view = ViewState {
            rows: vec![
                vec![span("aaa")],
                vec![span("bbb")],
                vec![span("ccc")],
                vec![span("ddd")],
            ],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 2, 3],
            ..Default::default()
        };
        // j: row 0 -> row 2 (the wrapped continuation row 1 is skipped,
        // landing on the next visible row's source line).
        view.move_cursor_display(1);
        assert_eq!(view.cursor, 1, "wrapped continuations are skipped");
        // j: row 2 -> row 3 (source line 2).
        view.move_cursor_display(1);
        assert_eq!(view.cursor, 2);
        // k: row 3 -> row 2 (back to source line 1).
        view.move_cursor_display(-1);
        assert_eq!(view.cursor, 1);
        // k: row 2 -> row 0 (source line 0).
        view.move_cursor_display(-1);
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn cursor_row_gets_a_background_and_empty_rows_still_show() {
        let view = ViewState {
            rows: vec![
                vec![],
                vec![crate::highlight::Span {
                    text: "x".into(),
                    style: Style::default(),
                }],
            ],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 1],
            ..Default::default()
        };
        // Can't easily inspect styles through Text, so just ensure the rows
        // render without panicking and the cursor row stays in bounds.
        assert_eq!(
            view.visible_text(10, &[], &[], &[], None, Color::Rgb(88, 91, 112), Style::default())
                .0
                .lines
                .len(),
            2
        );
    }

    #[test]
    fn marker_covers_the_whole_line_block() {
        // Line 0 is comment-covered and spans rows 0-1 (a wrapped line or a
        // blank row sharing the previous line). The cursor sits on row 0, so
        // it shows the `>` marker; the continuation row keeps the ▌ — the
        // covered block reads as one continuous marker column.
        let view = ViewState {
            rows: vec![vec![], vec![], vec![]],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 2, 3],
            ..Default::default()
        };
        let marked = vec![true, false, false]; // line 0 is comment-covered
        let (_, gutter) = view.visible_text(
            10,
            &marked,
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            Style::default(),
        );
        assert_eq!(gutter[0].glyph, ">", "cursor row shows the > marker");
        assert_eq!(gutter[1].glyph, "▌", "continuation row keeps the marker");
        assert_eq!(gutter[2].glyph, "│", "unrelated row below keeps the border");
    }

    #[test]
    fn selection_rows_get_background_and_the_cursor_a_marker() {
        // 1:1 rows; selection covers source lines 1-3, cursor at 3 (the
        // selection extent). Selected rows get the DarkGray background;
        // the cursor row shows `>` in the gutter so it stays visible
        // inside the selection.
        let view = ViewState {
            rows: vec![vec![], vec![], vec![], vec![], vec![]],
            offset: 0,
            cursor: 3,
            source_starts: vec![0, 1, 2, 3, 4],
            ..Default::default()
        };
        let (text, gutter) = view.visible_text(
            10,
            &[],
            &[],
            &[],
            Some((1, 3)),
            Color::Rgb(88, 91, 112),
            Style::default(),
        );
        let has_bg = |i: usize| {
            text.lines[i]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(selected_bg(false)))
        };
        assert!(!has_bg(0), "row outside the selection stays clean");
        assert!(has_bg(1) && has_bg(2), "selected rows are highlighted");
        assert_eq!(gutter[3].glyph, ">", "cursor row carries the > marker");
        assert!(has_bg(3), "the cursor row is highlighted too");
        // Without a selection the cursor row still shows `>`.
        let (_, gutter) = view.visible_text(
            10,
            &[],
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            Style::default(),
        );
        assert_eq!(gutter[3].glyph, ">", "standalone cursor keeps its marker");
    }

    #[test]
    fn source_line_handoff_is_exact() {
        let mut view = ViewState {
            rows: vec![vec![]; 10],
            offset: 0,
            cursor: 0,
            source_starts: (0..10).collect(),
            ..Default::default()
        };
        view.move_cursor_display(7);
        assert_eq!(view.cursor, 7, "v hands the exact source line");
        view.goto_source_line(3);
        assert_eq!(view.cursor, 3, "Esc lands on the exact line");
        view.goto_source_line(999);
        assert_eq!(view.cursor, 9, "clamped to the last line");
        view.goto_source_line(0);
        assert_eq!(view.cursor_row(), 0);
    }


    fn test_view(rows: usize) -> ViewState {
        ViewState {
            rows: vec![vec![]; rows],
            offset: 0,
            cursor: 2,
            source_starts: (0..rows).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn wheel_scroll_moves_viewport_only() {
        let mut v = test_view(10);
        v.wheel_scroll(1, 5);
        assert_eq!((v.offset, v.cursor), (1, 2), "cursor stays put");
        v.wheel_scroll(1, 5);
        assert_eq!((v.offset, v.cursor), (2, 2));
    }

    #[test]
    fn wheel_scroll_leaves_the_cursor_at_the_top_edge() {
        let mut v = test_view(10);
        v.offset = 2;
        v.cursor = 2;
        v.wheel_scroll(1, 5);
        assert_eq!((v.offset, v.cursor), (3, 2), "viewport moves, cursor stays put");
    }

    #[test]
    fn wheel_scroll_leaves_the_cursor_at_the_bottom_edge() {
        let mut v = test_view(10);
        v.offset = 5;
        v.cursor = 9;
        v.wheel_scroll(-1, 5);
        assert_eq!((v.offset, v.cursor), (4, 9), "viewport moves, cursor stays put");
    }

    #[test]
    fn wheel_scroll_clamps_at_ends() {
        let mut v = test_view(10);
        v.offset = 5; // max_offset for a viewport of 5
        v.cursor = 9;
        v.wheel_scroll(1, 5);
        assert_eq!((v.offset, v.cursor), (5, 9), "no scroll past the end");
        let mut v = test_view(10);
        v.wheel_scroll(-1, 5);
        assert_eq!((v.offset, v.cursor), (0, 2));
    }

    #[test]
    fn wheel_scroll_moves_the_viewport_over_merged_rows() {
        // The renderer merges paragraphs: source line 3 spans rendered rows 3..=5
        // (source_starts = [0,1,2,3,6,...]). The wheel never pulls the
        // cursor, so the viewport scrolls freely past merged rows and
        // there is nothing to stall (regression for the old cursor-follow).
        let mut v = ViewState {
            rows: vec![vec![]; 10],
            offset: 0,
            cursor: 3,
            source_starts: vec![0, 1, 2, 3, 6, 7, 8, 9],
            ..Default::default()
        };
        for _ in 0..5 {
            v.wheel_scroll(1, 5);
        }
        assert_eq!(v.offset, 5, "viewport reaches the bottom stop");
        assert_eq!(v.cursor, 3, "cursor keeps its absolute position");
    }

    #[test]
    fn wheel_scroll_up_moves_the_viewport_over_merged_rows() {
        let mut v = ViewState {
            rows: vec![vec![]; 10],
            offset: 5,
            cursor: 4, // rendered row 6 (source line 4 starts at row 6)
            source_starts: vec![0, 1, 2, 3, 6, 7, 8, 9],
            ..Default::default()
        };
        for _ in 0..6 {
            v.wheel_scroll(-1, 5);
        }
        assert_eq!(v.offset, 0, "viewport reaches the top");
        assert_eq!(v.cursor, 4, "cursor keeps its absolute position");
    }

    #[test]
    fn wheel_scroll_moves_a_render_shorter_than_the_viewport() {
        // A 3-row render (the renderer joined the whole doc into one paragraph)
        // with 15 source lines; viewport 22 > rows. max_offset is forced
        // to 1 by .max(1); the wheel still moves the viewport and the
        // cursor keeps its absolute position (viewport-only scroll).
        let mut v = ViewState {
            rows: vec![vec![]; 3],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2],
            ..Default::default()
        };
        v.wheel_scroll(1, 22);
        assert_eq!(v.offset, 1, "viewport moves down one row");
        assert_eq!(v.cursor, 0, "cursor stays put");
        v.wheel_scroll(1, 22);
        assert_eq!(v.offset, 1, "clamped at the forced max_offset");
        v.wheel_scroll(-1, 22);
        assert_eq!(v.offset, 0, "wheel up returns");
    }
}

    #[test]
    fn wheel_scroll_moves_a_render_shorter_than_the_viewport() {
        // A 3-row render (the renderer joined the whole doc into one paragraph)
        // with 15 source lines; viewport 22 > rows. max_offset is forced
        // to 1 by .max(1); the wheel still moves the viewport and the
        // cursor keeps its absolute position (viewport-only scroll).
        let mut v = ViewState {
            rows: vec![vec![]; 3],
            offset: 0,
            cursor: 0,
            source_starts: vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2],
            ..Default::default()
        };
        v.wheel_scroll(1, 22);
        assert_eq!(v.offset, 1, "viewport moves down one row");
        assert_eq!(v.cursor, 0, "cursor stays put");
        v.wheel_scroll(1, 22);
        assert_eq!(v.offset, 1, "clamped at the forced max_offset");
        v.wheel_scroll(-1, 22);
        assert_eq!(v.offset, 0, "wheel up returns");
    }

