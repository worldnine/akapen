//! Table rendering support for tui-markdown.
//!
//! A table must be buffered before rendering because every cell can increase its column's terminal
//! display width. [`TableBuilder`] collects the header and body rows, then renders their content,
//! alignment, padding, and Unicode box-drawing borders once pulldown-cmark closes the table.
//!
//! Tables are width-adaptive: given a layout budget ([`Options::max_width`], minus the display
//! width an enclosing list marker or blockquote prefix takes), a table whose natural width does
//! not fit shrinks its columns (proportional to the natural widths, floored at each column's
//! widest unsplittable token) and wraps cell content across multiple rows instead of overflowing.
//! Wrapped cell lines keep the source attribution of the spans they carry. A width-constrained
//! table also draws a separator between every pair of body rows, so the boundaries of multi-line
//! wrapped rows stay readable; a table at its natural width keeps the light look (header separator
//! only).
//!
//! The central renderer dispatches events and owns shared inline state. This module owns the table
//! event handlers, buffered table state, list-aware output placement, and final table layout.

use pulldown_cmark::Alignment;
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span};

use super::{Attr, TextWriter};
use crate::StyleSheet;

const HORIZONTAL_BORDER: char = '─';
const VERTICAL_BORDER: &str = "│";
const TOP_BORDER: BorderGlyphs = BorderGlyphs::new('┌', '┬', '┐');
const HEADER_SEPARATOR: BorderGlyphs = BorderGlyphs::new('├', '┼', '┤');
const BOTTOM_BORDER: BorderGlyphs = BorderGlyphs::new('└', '┴', '┘');

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = (pulldown_cmark::Event<'a>, std::ops::Range<usize>)>,
    S: StyleSheet,
{
    pub fn start_table(&mut self, alignments: Vec<Alignment>) {
        if self.needs_newline {
            self.push_line(Line::default(), vec![]);
        }
        self.table_builder = Some(TableBuilder::new(alignments));
        self.needs_newline = false;
    }

    pub fn end_table_header(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.finish_header();
        }
    }

    pub fn end_table_row(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.finish_row();
        }
    }

    pub fn start_table_cell(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.start_cell();
        }
    }

    pub fn end_table_cell(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.finish_cell();
        }
    }

    pub fn end_table(&mut self) {
        if let Some(builder) = self.table_builder.take() {
            // Every table line loses the same horizontal budget: the first
            // line of a table inside a list item shares the marker line
            // (and the rest carry the continuation prefix), and blockquote
            // lines all carry their prefixes plus the spacer before them.
            // The table is still on the construct stack when it closes, so
            // the active indent is exactly what `push_table_lines` will
            // apply. Without this, the laid-out table would be re-cut by
            // the view's own wrap.
            let list_indent = self
                .list_items
                .last()
                .map_or(0, |item| item.continuation_width);
            let prefix_indent = self.line_prefixes.iter().map(|s| s.width()).sum::<usize>()
                + usize::from(!self.line_prefixes.is_empty());
            let available = self.max_width.map_or(usize::MAX, |w| {
                w.saturating_sub(list_indent + prefix_indent)
            });
            let lines = builder.render(&self.styles, available);
            self.push_table_lines(lines);
            self.needs_newline = true;
        }
    }

    /// Adds a buffered table to the output while preserving an active list item's layout.
    ///
    /// A table that is the first content in an item starts on the marker line. Its remaining lines
    /// are indented by the marker's display width. A later table cannot reuse the marker line, but
    /// all of its lines still need the continuation indentation.
    ///
    /// Table rendering currently puts styles on individual spans and leaves the line style and
    /// alignment at their defaults. This makes it safe to move the first rendered line's spans
    /// onto the existing marker line.
    fn push_table_lines(&mut self, lines: Vec<(Line<'a>, Vec<Option<Attr>>)>) {
        let Some(list_item) = self.list_items.last().copied() else {
            for (line, attrs) in lines {
                self.push_line(line, attrs);
            }
            return;
        };

        let mut lines = lines.into_iter();
        // The line position alone is insufficient: inline item content may already have appended
        // spans to the marker line before the table was buffered.
        let marker_line_is_last = self.text.lines.len() == list_item.marker_line + 1;
        let marker_has_no_content =
            self.text.lines[list_item.marker_line].spans.len() == list_item.marker_span_count;
        let table_starts_on_marker = marker_line_is_last && marker_has_no_content;
        if table_starts_on_marker {
            if let Some((first_line, first_attrs)) = lines.next() {
                self.text.lines[list_item.marker_line]
                    .spans
                    .extend(first_line.spans);
                self.out_attrs[list_item.marker_line].extend(first_attrs);
            }
        }

        let continuation = " ".repeat(list_item.continuation_width);
        for (mut line, attrs) in lines {
            line.spans.insert(0, Span::raw(continuation.clone()));
            let mut attrs = attrs;
            attrs.insert(0, None);
            self.push_line(line, attrs);
        }
    }
}

/// Accumulates a complete table before calculating its column widths and rendering it.
///
/// The parent renderer starts and finishes each cell as pulldown-cmark emits table events. It then
/// finishes the header or body row and calls [`Self::render`] after the table closes.
pub struct TableBuilder<'a> {
    alignments: Vec<Alignment>,
    header: TableHeader<'a>,
    rows: Vec<TableRow<'a>>,
    current_row: TableRow<'a>,
    current_cell: TableCell<'a>,
}

impl<'a> TableBuilder<'a> {
    pub fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            header: TableHeader::default(),
            rows: Vec::new(),
            current_row: TableRow::default(),
            current_cell: TableCell::default(),
        }
    }

    pub fn start_cell(&mut self) {
        self.current_cell = TableCell::default();
    }

    pub fn push_span(&mut self, span: Span<'a>, attr: Option<Attr>) {
        self.current_cell.push(span, attr);
    }

    pub fn finish_cell(&mut self) {
        let cell = std::mem::take(&mut self.current_cell);
        self.current_row.cells.push(cell);
    }

    pub fn finish_header(&mut self) {
        self.header.cells = std::mem::take(&mut self.current_row.cells);
    }

    pub fn finish_row(&mut self) {
        self.rows.push(std::mem::take(&mut self.current_row));
    }

    /// Renders the buffered table within `available_width` display columns
    /// (`usize::MAX` = natural width). Returns one output line per drawn row;
    /// a cell that wraps to several lines makes its whole row several lines.
    pub fn render<S: StyleSheet>(
        self,
        styles: &S,
        available_width: usize,
    ) -> Vec<(Line<'a>, Vec<Option<Attr>>)> {
        let column_count = self.column_count();
        if column_count == 0 {
            return Vec::new();
        }

        let (column_widths, constrained) = self.column_widths(column_count, available_width);
        let border_style = styles.table_border();

        let top_border = TOP_BORDER.render(&column_widths, border_style);
        let header = self.header.render(&column_widths, &self.alignments, styles);
        let header_separator = HEADER_SEPARATOR.render(&column_widths, border_style);
        let body: Vec<_> = self
            .rows
            .iter()
            .map(|row| row.render(&column_widths, &self.alignments, styles))
            .collect();
        let bottom_border = BOTTOM_BORDER.render(&column_widths, border_style);

        let mut lines = vec![top_border];
        lines.extend(header);
        lines.push(header_separator);
        for (index, row) in body.into_iter().enumerate() {
            // A width-constrained table wraps every row onto several lines;
            // a separator between body rows keeps the row boundaries
            // readable. A natural-width table keeps the light look (header
            // separator only), byte-identical to the unbounded render.
            if constrained && index > 0 {
                lines.push(HEADER_SEPARATOR.render(&column_widths, border_style));
            }
            lines.extend(row);
        }
        lines.push(bottom_border);
        lines
    }

    fn column_count(&self) -> usize {
        self.alignments.len().max(self.header.cells.len()).max(
            self.rows
                .iter()
                .map(|row| row.cells.len())
                .max()
                .unwrap_or(0),
        )
    }

    /// Column widths for a table laid out within `available` columns, and
    /// whether the table had to shrink to fit (`true` = width-constrained).
    ///
    /// The natural width (widest cell per column, display-width measured) is
    /// used unchanged when the whole table fits. Otherwise the fixed
    /// overhead — the `n + 1` border glyphs plus the two padding columns
    /// each of the `n` cells reserves (`3n + 1` in total) — is subtracted
    /// from the budget and the remainder is shared proportionally to the
    /// natural widths, never dropping a column below its floor: the widest
    /// token wrapping cannot split (a whitespace-separated word; every wide
    /// CJK/emoji character counts as a token of its own). When the floors
    /// themselves do not fit, they drop to one display column each and the
    /// budget is distributed again (see the fallback branch below).
    fn column_widths(&self, column_count: usize, available: usize) -> (Vec<usize>, bool) {
        let mut natural = vec![0usize; column_count];
        let mut floors = vec![1usize; column_count];
        for (col_idx, cell) in self.header.cells.iter().enumerate() {
            natural[col_idx] = natural[col_idx].max(cell.width());
            floors[col_idx] = floors[col_idx].max(cell.longest_token());
        }
        for row in &self.rows {
            for (col_idx, cell) in row.cells.iter().enumerate() {
                natural[col_idx] = natural[col_idx].max(cell.width());
                floors[col_idx] = floors[col_idx].max(cell.longest_token());
            }
        }
        for width in &mut natural {
            *width = (*width).max(1);
        }
        let overhead = 3 * column_count + 1; // borders (n + 1) + per-cell padding (2n)
        let total: usize = natural.iter().sum();
        if total + overhead <= available {
            return (natural, false);
        }
        let budget = available.saturating_sub(overhead);
        if floors.iter().sum::<usize>() <= budget {
            return (proportional_with_floor(&natural, &floors, budget), true);
        }
        // The token floors do not fit: drop them to one display column each
        // and distribute again, so a single oversized token (a long URL, an
        // unbroken word) costs its column only what fairness allows instead
        // of collapsing the whole table. Wrapping then splits such tokens
        // character by character. Only when even one column per column does
        // not fit (`available < 4n + 1`) do the columns stay at one and the
        // table overflows the pane — the physical minimum; the table itself
        // never drops content.
        if column_count <= budget {
            let ones = vec![1usize; column_count];
            return (proportional_with_floor(&natural, &ones, budget), true);
        }
        (vec![1; column_count], true)
    }
}

/// Distribute `budget` display columns across the columns: each column keeps
/// its floor, the remainder is shared proportionally to the natural widths
/// (rounded down, with the leftover given to the widest columns), and no
/// column exceeds its natural width. The returned widths sum to exactly
/// `min(budget, Σnatural)`.
fn proportional_with_floor(natural: &[usize], floors: &[usize], budget: usize) -> Vec<usize> {
    let sum_natural: usize = natural.iter().sum();
    if budget >= sum_natural {
        return natural.to_vec();
    }
    let sum_floor: usize = floors.iter().sum();
    let mut widths = floors.to_vec();
    let span = sum_natural.saturating_sub(sum_floor);
    if span == 0 || budget <= sum_floor {
        return widths;
    }
    let extra = budget - sum_floor;
    // Widest share first, so integer rounding favors the widest columns.
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(natural[i] - floors[i]));
    let mut remainder = extra;
    for &i in &order {
        let share = (natural[i] - floors[i]) * extra / span;
        widths[i] += share;
        remainder -= share;
    }
    for &i in &order {
        if remainder == 0 || widths[i] >= natural[i] {
            continue;
        }
        widths[i] += 1;
        remainder -= 1;
    }
    debug_assert_eq!(remainder, 0, "all budget columns allocated");
    widths
}

#[derive(Default)]
struct TableHeader<'a> {
    cells: Vec<TableCell<'a>>,
}

impl<'a> TableHeader<'a> {
    fn render<S: StyleSheet>(
        &self,
        column_widths: &[usize],
        alignments: &[Alignment],
        styles: &S,
    ) -> Vec<(Line<'a>, Vec<Option<Attr>>)> {
        render_line(
            &self.cells,
            column_widths,
            alignments,
            styles.table_header(),
            styles.table_border(),
        )
    }
}

#[derive(Default)]
struct TableRow<'a> {
    cells: Vec<TableCell<'a>>,
}

impl<'a> TableRow<'a> {
    fn render<S: StyleSheet>(
        &self,
        column_widths: &[usize],
        alignments: &[Alignment],
        styles: &S,
    ) -> Vec<(Line<'a>, Vec<Option<Attr>>)> {
        render_line(
            &self.cells,
            column_widths,
            alignments,
            styles.table_cell(),
            styles.table_border(),
        )
    }
}

#[derive(Default)]
struct TableCell<'a> {
    /// (span, source range) — the range of the cell content the span was
    /// buffered from. Cell text is re-wrapped and whitespace-collapsed
    /// below, so these attributions are supersets by construction (see
    /// [`Attr`]); nothing here may claim to be a verbatim slice.
    spans: Vec<(Span<'a>, Option<Attr>)>,
}

impl<'a> TableCell<'a> {
    fn push(&mut self, span: Span<'a>, attr: Option<Attr>) {
        self.spans.push((span, attr.map(|a| a.demoted())));
    }

    fn width(&self) -> usize {
        self.spans.iter().map(|(span, _)| Span::width(span)).sum()
    }

    /// The widest token in the cell — the piece wrapping can never split:
    /// a run of narrow non-whitespace characters (a word, a URL) counts as
    /// one token, while every wide CJK/emoji character is breakable on its
    /// own and counts as a token of its display width. Zero-width characters
    /// (combining marks, variation selectors) ride their neighbours and add
    /// nothing.
    fn longest_token(&self) -> usize {
        let mut longest = 0usize;
        let mut run = 0usize;
        for (span, _) in &self.spans {
            for ch in span.content.chars() {
                let width = char_display_width(ch);
                if ch.is_whitespace() {
                    run = 0;
                } else if width >= 2 {
                    longest = longest.max(run).max(width);
                    run = 0;
                } else if width == 1 {
                    run += 1;
                }
            }
        }
        longest.max(run)
    }

    /// Wrap the cell content to `column_width` display columns and render
    /// every line: the padding for `alignment` (applied per line, so
    /// alignment survives wrapping), the cell's `style` on content and
    /// padding alike, and one source attribution per output span — a
    /// wrapped fragment inherits its span's whole (superset) range.
    fn render_lines(
        &self,
        column_width: usize,
        alignment: Alignment,
        style: Style,
    ) -> CellLines<'a> {
        wrap_cell(&self.spans, column_width)
            .into_iter()
            .map(|line| {
                let content_width: usize = line.iter().map(|(span, _)| Span::width(span)).sum();
                let (pad_left, pad_right) = padding(column_width, content_width, alignment);
                // Padding is synthesized filler: it carries no source range.
                let mut spans = vec![Span::styled(" ".repeat(pad_left + 1), style)];
                let mut attrs: Vec<Option<Attr>> = vec![None];
                for (span, attr) in line {
                    let mut span = span;
                    span.style = span.style.patch(style);
                    spans.push(span);
                    attrs.push(attr);
                }
                spans.push(Span::styled(" ".repeat(pad_right + 1), style));
                attrs.push(None);
                (spans, attrs)
            })
            .collect()
    }
}

/// One rendered line of a cell: its spans plus the per-span source
/// attribution (padding spans carry `None`).
type CellLine<'a> = (Vec<Span<'a>>, Vec<Option<Attr>>);

/// The rendered lines of a whole cell (one per wrap row).
type CellLines<'a> = Vec<CellLine<'a>>;

/// One character of a cell with the source span it came from, flattened so
/// wrapping can cut at any boundary without losing the span's style, source
/// range, or identity (a wrapped line re-merges its characters by span
/// index, so a cell that does not wrap keeps its input spans exactly).
///
/// The span index alone carries the attribution: style and range are both
/// read back from the cell's span list, so the character stays `Copy`.
#[derive(Clone, Copy)]
struct CellChar {
    ch: char,
    /// Index into the cell's span list; also the span's style and range.
    span: usize,
}

/// Greedy word-wrap state for one cell.
struct CellWrap {
    /// The column's content width.
    width: usize,
    /// Completed lines.
    lines: Vec<Vec<CellChar>>,
    /// The line being filled.
    current: Vec<CellChar>,
    current_width: usize,
    /// The pending word: a run of narrow non-whitespace characters (plus
    /// any zero-width characters glued to them).
    word: Vec<CellChar>,
    word_width: usize,
    /// The whitespace run consumed since the last committed word; its first
    /// character is re-inserted as the single separator space when the next
    /// word lands on a non-empty line. `None` when the original text had no
    /// whitespace (CJK text must not gain spaces) or the separator was
    /// already dropped at a line break.
    pending_space: Option<CellChar>,
}

impl CellWrap {
    fn new(width: usize) -> Self {
        Self {
            width: width.max(1),
            lines: Vec::new(),
            current: Vec::new(),
            current_width: 0,
            word: Vec::new(),
            word_width: 0,
            pending_space: None,
        }
    }

    /// Close the current line and start a fresh one.
    fn close_line(&mut self) {
        if !self.current.is_empty() {
            self.lines.push(std::mem::take(&mut self.current));
            self.current_width = 0;
        }
    }

    /// Append `ch` to the current line, closing it first when the character
    /// does not fit in the remainder. A wide character that cannot fit even
    /// on an empty line is placed anyway (it overflows the column — the
    /// physical minimum; wide characters are never split). Zero-width
    /// characters always ride the current line.
    fn put(&mut self, ch: CellChar) {
        let w = char_display_width(ch.ch);
        if w > 0 && self.current_width + w > self.width {
            self.close_line();
        }
        self.current_width += w;
        self.current.push(ch);
    }

    /// Commit the pending word: place it on the current line when it fits
    /// (preceded by the recorded separator space), otherwise start a new
    /// line with it; a single word wider than the column is split character
    /// by character without splitting wide characters.
    fn commit_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word);
        let word_width = std::mem::take(&mut self.word_width);
        let space = self.pending_space.take();
        if self.current.is_empty() {
            if word_width <= self.width {
                self.current = word;
                self.current_width = word_width;
            } else {
                for ch in word {
                    self.put(ch);
                }
            }
            return;
        }
        let space_width = usize::from(space.is_some());
        if self.current_width + space_width + word_width <= self.width {
            if let Some(space) = space {
                self.current.push(space);
                self.current_width += 1;
            }
            self.current.extend(word);
            self.current_width += word_width;
        } else if word_width <= self.width {
            self.close_line();
            self.current = word;
            self.current_width = word_width;
        } else {
            self.close_line();
            for ch in word {
                self.put(ch);
            }
        }
    }
}

/// Word-wrap cell content to `width` display columns, returning one list of
/// (span, source line) pairs per line. Break opportunities are whitespace
/// and every wide (CJK, emoji) character, so Japanese text wraps between any
/// two characters while `https://example.com` stays whole when it fits. A
/// single token wider than the column is split character by character so no
/// text is ever dropped; a wide character is never split, and one that does
/// not fit even on an empty line overflows the column (the physical
/// minimum). Whitespace runs collapse to one separator space, which is
/// dropped at a line break and never invented between words the source did
/// not separate.
fn wrap_cell<'a>(
    spans: &[(Span<'a>, Option<Attr>)],
    width: usize,
) -> Vec<Vec<(Span<'a>, Option<Attr>)>> {
    let mut chars: Vec<CellChar> = Vec::new();
    for (span_idx, (span, _)) in spans.iter().enumerate() {
        for ch in span.content.chars() {
            chars.push(CellChar { ch, span: span_idx });
        }
    }

    let mut wrap = CellWrap::new(width);
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        let w = char_display_width(ch.ch);
        if ch.ch.is_whitespace() {
            // Whitespace ends the word; the run collapses to one separator
            // space carrying the run's first character's style and range.
            wrap.commit_word();
            wrap.pending_space = Some(ch);
            while i < chars.len() && chars[i].ch.is_whitespace() {
                i += 1;
            }
            continue;
        }
        if w >= 2 {
            // A wide character breaks the line on its own.
            wrap.commit_word();
            wrap.word.push(ch);
            wrap.word_width = w;
            wrap.commit_word();
            i += 1;
            continue;
        }
        wrap.word.push(ch);
        wrap.word_width += w;
        i += 1;
    }
    wrap.commit_word();
    wrap.close_line();
    if wrap.lines.is_empty() {
        wrap.lines.push(Vec::new());
    }

    wrap.lines
        .into_iter()
        .map(|line| {
            // Re-merge consecutive characters of the same source span, so a
            // span that was never cut keeps its exact input text (and a
            // wrapped span becomes one fragment per line). Every fragment
            // inherits its span's whole range: a wrapped or
            // whitespace-collapsed cell cannot be sliced, which is why a
            // cell's attribution is a superset to begin with.
            let mut out: Vec<(Span<'a>, Option<Attr>)> = Vec::new();
            let mut cur: Option<(usize, String)> = None;
            for ch in line {
                match &mut cur {
                    Some((span, text)) if *span == ch.span => {
                        text.push(ch.ch);
                    }
                    _ => {
                        if let Some((span, text)) = cur.take() {
                            out.push((Span::styled(text, spans[span].0.style), spans[span].1.clone()));
                        }
                        cur = Some((ch.span, ch.ch.to_string()));
                    }
                }
            }
            if let Some((span, text)) = cur {
                out.push((Span::styled(text, spans[span].0.style), spans[span].1.clone()));
            }
            out
        })
        .collect()
}

/// The terminal display width of `ch` (unicode-width; 0 for combining marks
/// and other zero-width characters).
fn char_display_width(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    ch.width().unwrap_or(0)
}

#[derive(Clone, Copy)]
struct BorderGlyphs {
    left: char,
    intersection: char,
    right: char,
}

impl BorderGlyphs {
    const fn new(left: char, intersection: char, right: char) -> Self {
        Self {
            left,
            intersection,
            right,
        }
    }

    fn render<'a>(self, column_widths: &[usize], style: Style) -> (Line<'a>, Vec<Option<Attr>>) {
        let mut border = String::new();
        border.push(self.left);
        for (index, width) in column_widths.iter().enumerate() {
            for _ in 0..(width + 2) {
                border.push(HORIZONTAL_BORDER);
            }
            if index + 1 < column_widths.len() {
                border.push(self.intersection);
            }
        }
        border.push(self.right);
        // Border rows are synthesized: they carry no source range.
        (Line::from(Span::styled(border, style)), vec![None])
    }
}

/// Renders one header/body row as one output line per wrap row: every cell
/// wraps to its column width, the row's height is the tallest cell, shorter
/// cells continue as blank padded lines, and every line — wrapped or not —
/// is framed by its `│` borders.
fn render_line<'a>(
    cells: &[TableCell<'a>],
    column_widths: &[usize],
    alignments: &[Alignment],
    content_style: Style,
    border_style: Style,
) -> Vec<(Line<'a>, Vec<Option<Attr>>)> {
    let empty_cell = TableCell::default();
    // Wrap every cell once; the per-column blank line fills a shorter
    // cell's continuation rows.
    let wrapped: Vec<CellLines<'a>> = column_widths
        .iter()
        .enumerate()
        .map(|(column_index, &column_width)| {
            let cell = cells.get(column_index).unwrap_or(&empty_cell);
            let alignment = alignments
                .get(column_index)
                .copied()
                .unwrap_or(Alignment::None);
            cell.render_lines(column_width, alignment, content_style)
        })
        .collect();
    let blank_lines: Vec<CellLine<'a>> = column_widths
        .iter()
        .enumerate()
        .map(|(column_index, &column_width)| {
            let alignment = alignments
                .get(column_index)
                .copied()
                .unwrap_or(Alignment::None);
            empty_cell
                .render_lines(column_width, alignment, content_style)
                .pop()
                .expect("an empty cell renders exactly one line")
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);

    (0..height)
        .map(|line_index| {
            let mut spans = vec![Span::styled(VERTICAL_BORDER, border_style)];
            let mut attrs = vec![None];
            for (column_index, column_lines) in wrapped.iter().enumerate() {
                let (cell_spans, cell_attrs) = column_lines
                    .get(line_index)
                    .cloned()
                    .unwrap_or_else(|| blank_lines[column_index].clone());
                spans.extend(cell_spans);
                attrs.extend(cell_attrs);
                // The cell separator belongs to the same rendered row as the
                // cell content, but it is a synthesized glyph: no source line.
                spans.push(Span::styled(VERTICAL_BORDER, border_style));
                attrs.push(None);
            }
            (Line::from(spans), attrs)
        })
        .collect()
}

fn padding(column_width: usize, content_width: usize, alignment: Alignment) -> (usize, usize) {
    if content_width >= column_width {
        return (0, 0);
    }
    let total_pad = column_width - content_width;
    match alignment {
        Alignment::Left | Alignment::None => (0, total_pad),
        Alignment::Right => (total_pad, 0),
        Alignment::Center => {
            let left = total_pad / 2;
            (left, total_pad - left)
        }
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use itertools::Itertools;
    use pretty_assertions::assert_eq;
    use ratatui_core::style::{Style, Stylize};
    use ratatui_core::text::{Line, Span, Text};

    use super::*;
    use crate::{
        from_str, from_str_with_options, from_str_with_options_tagged, DefaultStyleSheet, Options,
        StyleSheet,
    };

    #[test]
    fn empty_table() {
        let builder = TableBuilder::new(vec![]);
        assert!(builder.render(&DefaultStyleSheet, usize::MAX).is_empty());
    }

    #[test]
    fn single_cell() {
        let mut builder = TableBuilder::new(vec![Alignment::None]);
        builder.start_cell();
        builder.push_span(Span::raw("hi"), Some(Attr::inexact(0..2)));
        builder.finish_cell();
        builder.finish_header();
        assert_eq!(builder.render(&DefaultStyleSheet, usize::MAX).len(), 4);
    }

    #[test]
    fn padding_for_each_alignment() {
        assert_eq!(padding(10, 3, Alignment::Left), (0, 7));
        assert_eq!(padding(10, 3, Alignment::Right), (7, 0));
        assert_eq!(padding(10, 4, Alignment::Center), (3, 3));
        assert_eq!(padding(10, 3, Alignment::Center), (3, 4));
    }

    #[test]
    fn cell_style_covers_padding_and_empty_cells() {
        let style = Style::new().on_green();
        let cell = TableCell {
            spans: vec![(Span::raw("x"), Some(Attr::inexact(3..4)))],
        };
        assert_eq!(
            cell.render_lines(4, Alignment::Center, style),
            [(
                vec![
                    Span::styled("  ", style),
                    Span::styled("x", style),
                    Span::styled("   ", style),
                ],
                vec![None, Some(Attr::inexact(3..4)), None]
            )]
        );

        let empty_cell = TableCell::default();
        assert_eq!(
            empty_cell.render_lines(4, Alignment::Right, style),
            [(
                vec![Span::styled("     ", style), Span::styled(" ", style)],
                vec![None, None]
            )]
        );
    }

    #[test]
    fn column_widths_have_a_minimum_of_one() {
        let mut builder = TableBuilder::new(vec![]);
        builder.header.cells.push(TableCell::default());
        assert_eq!(builder.column_widths(1, usize::MAX), (vec![1], false));
    }

    #[test]
    fn styled_cell_width() {
        let cell = TableCell {
            spans: vec![
                (Span::from("hello").bold(), None),
                (Span::raw(" world"), None),
            ],
        };
        assert_eq!(cell.width(), 11);
    }

    #[test]
    fn emoji_cell_width() {
        let cell = TableCell {
            spans: vec![(Span::raw("✅"), None), (Span::raw(" ok"), None)],
        };
        assert_eq!(cell.width(), 5);
    }

    #[test]
    fn cjk_cell_width() {
        let cell = TableCell {
            spans: vec![(Span::raw("日本"), None), (Span::raw(" ok"), None)],
        };
        assert_eq!(cell.width(), 7);
    }

    #[test]
    fn table_with_alignment() {
        let text = from_str(indoc! {"
                | Left | Center | Right |
                |:-----|:------:|------:|
                | a    | b      | c     |
            "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "┌──────┬────────┬───────┐",
                "│ Left │ Center │ Right │",
                "├──────┼────────┼───────┤",
                "│ a    │   b    │     c │",
                "└──────┴────────┴───────┘",
            ]
        );
    }

    #[test]
    fn table_without_outer_pipes() {
        let text = from_str(indoc! {"
            A | B
            ---|---
            a | b
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        assert_eq!(
            rendered,
            [
                "┌───┬───┐",
                "│ A │ B │",
                "├───┼───┤",
                "│ a │ b │",
                "└───┴───┘",
            ]
        );
    }

    #[test]
    fn escaped_pipe_stays_inside_its_cell() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | a \\| b |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        assert_eq!(
            rendered,
            [
                "┌───────┐",
                "│ Value │",
                "├───────┤",
                "│ a | b │",
                "└───────┘",
            ]
        );
    }

    #[test]
    fn table_with_cjk_content() {
        let text = from_str(indoc! {"
                | Latin | CJK |
                |-------|-----|
                | a     | 日本 |
            "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "┌───────┬──────┐",
                "│ Latin │ CJK  │",
                "├───────┼──────┤",
                "│ a     │ 日本 │",
                "└───────┴──────┘",
            ]
        );
        assert!(text.lines.iter().all(|line| line.width() == 16));
    }

    #[derive(Clone)]
    struct CustomTableStyleSheet;

    impl StyleSheet for CustomTableStyleSheet {
        fn heading(&self, _level: u8) -> Style {
            Style::default()
        }

        fn code(&self) -> Style {
            Style::default()
        }

        fn link(&self) -> Style {
            Style::new().blue().underlined()
        }

        fn blockquote(&self) -> Style {
            Style::default()
        }

        fn heading_meta(&self) -> Style {
            Style::default()
        }

        fn metadata_block(&self) -> Style {
            Style::default()
        }

        fn table_header(&self) -> Style {
            Style::new().on_blue()
        }

        fn table_cell(&self) -> Style {
            Style::new().red().on_green()
        }

        fn table_border(&self) -> Style {
            Style::new().red()
        }
    }

    #[test]
    fn custom_styles_apply_to_header_cells_body_cells_and_borders() {
        let border_style = Style::new().red();
        let header_style = Style::new().on_blue();
        let cell_style = Style::new().red().on_green();
        let options = Options::new(CustomTableStyleSheet);
        let text = from_str_with_options(
            indoc! {"
                | A |
                |---|
                | a |
            "},
            &options,
        );
        assert_eq!(
            text,
            Text::from_iter([
                Line::from(Span::styled("┌───┐", border_style)),
                Line::from_iter([
                    Span::styled("│", border_style),
                    Span::styled(" ", header_style),
                    Span::styled("A", header_style),
                    Span::styled(" ", header_style),
                    Span::styled("│", border_style),
                ]),
                Line::from(Span::styled("├───┤", border_style)),
                Line::from_iter([
                    Span::styled("│", border_style),
                    Span::styled(" ", cell_style),
                    Span::styled("a", cell_style),
                    Span::styled(" ", cell_style),
                    Span::styled("│", border_style),
                ]),
                Line::from(Span::styled("└───┘", border_style)),
            ])
        );
    }

    #[test]
    fn custom_cell_style_composes_with_inline_formatting() {
        let options = Options::new(CustomTableStyleSheet);
        let text = from_str_with_options(
            indoc! {"
                | A |
                |---|
                | **bold** |
            "},
            &options,
        );
        let body = &text.lines[3];

        assert!(body
            .spans
            .contains(&Span::styled("bold", Style::new().bold().red().on_green())));
    }

    #[test]
    fn table_cell_style_overrides_conflicting_inline_properties() {
        let options = Options::new(CustomTableStyleSheet);
        let text = from_str_with_options(
            indoc! {"
                | A |
                |---|
                | [docs](url) |
            "},
            &options,
        );
        let link = text.lines[3]
            .spans
            .iter()
            .find(|span| span.content == "docs")
            .expect("link cell content");

        assert_eq!(
            link,
            &Span::styled("docs", Style::new().red().underlined().on_green())
        );
    }

    #[test]
    fn table_preserves_surrounding_paragraph_spacing() {
        let text = from_str(indoc! {"
                Before

                | A |
                |---|
                | a |

                After
            "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "Before",
                "",
                "┌───┐",
                "│ A │",
                "├───┤",
                "│ a │",
                "└───┘",
                "",
                "After",
            ]
        );
    }

    #[test]
    fn consecutive_tables_keep_separate_layout_state() {
        let text = from_str(indoc! {"
            | Long |
            |------|
            | value |

            | A |
            |---|
            | b |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        assert_eq!(
            rendered,
            [
                "┌───────┐",
                "│ Long  │",
                "├───────┤",
                "│ value │",
                "└───────┘",
                "",
                "┌───┐",
                "│ A │",
                "├───┤",
                "│ b │",
                "└───┘",
            ]
        );
    }

    #[test]
    fn empty_cells_keep_minimum_column_width() {
        let text = from_str(indoc! {"
            | A | B |
            |---|---|
            |   |   |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "┌───┬───┐",
                "│ A │ B │",
                "├───┼───┤",
                "│   │   │",
                "└───┴───┘",
            ]
        );
    }

    #[test]
    fn header_only_table_has_a_complete_frame() {
        let text = from_str(indoc! {"
            | A |
            |---|
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        #[rustfmt::skip]
        let expected = [
            "┌───┐",
            "│ A │",
            "├───┤",
            "└───┘",
        ];
        assert_eq!(rendered, expected);
    }

    #[test]
    fn short_rows_are_padded_and_extra_cells_are_ignored() {
        let text = from_str(indoc! {"
            | A | B |
            |---|---|
            | one |
            | x | y | ignored |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        assert_eq!(
            rendered,
            [
                "┌─────┬───┐",
                "│ A   │ B │",
                "├─────┼───┤",
                "│ one │   │",
                "│ x   │ y │",
                "└─────┴───┘",
            ]
        );
    }

    #[test]
    fn table_with_inline_code() {
        let text = from_str(indoc! {"
            | Name | Type |
            |------|------|
            | foo  | `u32` |
        "});
        let code_style = Style::new().white().on_black();
        let code = text.lines[3]
            .spans
            .iter()
            .find(|span| span.content == "u32")
            .expect("inline code cell content");
        assert_eq!(code, &Span::styled("u32", code_style));
    }

    #[test]
    fn table_with_bold_in_cells() {
        let text = from_str(indoc! {"
            | Col |
            |-----|
            | **bold** |
        "});
        let bold = text.lines[3]
            .spans
            .iter()
            .find(|span| span.content == "bold")
            .expect("bold cell content");
        assert_eq!(bold, &Span::styled("bold", Style::new().bold()));
    }

    #[test]
    fn table_keeps_link_destination_in_cell() {
        let text = from_str(indoc! {"
            | Link |
            |------|
            | [docs](u) |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "┌──────────┐",
                "│ Link     │",
                "├──────────┤",
                "│ docs (u) │",
                "└──────────┘",
            ]
        );
        let link_style = DefaultStyleSheet.link();
        assert_eq!(
            text.lines[3],
            Line::from_iter([
                Span::styled("│", DefaultStyleSheet.table_border()),
                Span::raw(" "),
                Span::styled("docs", link_style),
                Span::raw(" ("),
                Span::styled("u", link_style),
                Span::raw(")"),
                Span::raw(" "),
                Span::styled("│", DefaultStyleSheet.table_border()),
            ])
        );
    }

    #[test]
    fn table_keeps_inline_features_in_cell() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | <em>x</em> $y$ |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "┌────────────────┐",
                "│ Value          │",
                "├────────────────┤",
                "│ <em>x</em> $y$ │",
                "└────────────────┘",
            ]
        );

        let row = &text.lines[3];
        assert!(row
            .spans
            .contains(&Span::styled("<em>", DefaultStyleSheet.html())));
        assert!(row
            .spans
            .contains(&Span::styled("$y$", DefaultStyleSheet.math_inline())));
    }

    #[test]
    fn table_routes_inline_content_through_the_active_cell() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | **bold** `code` [link](url) <em>x</em> $y$ [^n] |

            [^n]: note
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        assert_eq!(rendered[3], "│ bold code link (url) <em>x</em> $y$ [n] │");
    }

    #[test]
    fn block_markers_inside_cells_remain_inline_text() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | # heading |
            | > quote |
            | - list |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();

        assert_eq!(
            rendered,
            [
                "┌───────────┐",
                "│ Value     │",
                "├───────────┤",
                "│ # heading │",
                "│ > quote   │",
                "│ - list    │",
                "└───────────┘",
            ]
        );
    }

    #[test]
    fn table_in_blockquote_keeps_quote_prefix() {
        let text = from_str(indoc! {"
            > | A |
            > |---|
            > | a |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        #[rustfmt::skip]
        let expected = [
            "> ┌───┐",
            "> │ A │",
            "> ├───┤",
            "> │ a │",
            "> └───┘",
        ];
        assert_eq!(rendered, expected);
    }

    #[test]
    fn table_list_item_keeps_marker_and_continuation_indent() {
        let text = from_str(indoc! {"
            - | A |
              |---|
              | a |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        #[rustfmt::skip]
        let expected = [
            "- ┌───┐",
            "  │ A │",
            "  ├───┤",
            "  │ a │",
            "  └───┘",
        ];
        assert_eq!(rendered, expected);
    }

    #[test]
    fn later_table_in_list_uses_continuation_indent() {
        let text = from_str(indoc! {"
            - | A |
              |---|
              | a |

              | B |
              |---|
              | b |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "- ┌───┐",
                "  │ A │",
                "  ├───┤",
                "  │ a │",
                "  └───┘",
                "",
                "  ┌───┐",
                "  │ B │",
                "  ├───┤",
                "  │ b │",
                "  └───┘",
            ]
        );
    }

    #[test]
    fn ordered_table_list_item_uses_full_marker_width() {
        let text = from_str(indoc! {"
            10. | A |
                |---|
                | a |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "10. ┌───┐",
                "    │ A │",
                "    ├───┤",
                "    │ a │",
                "    └───┘",
            ]
        );
    }

    #[test]
    fn nested_table_list_item_uses_nested_marker_width() {
        let text = from_str(indoc! {"
            - Parent
              - | A |
                |---|
                | a |
        "});
        let rendered = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(
            rendered,
            [
                "- Parent",
                "    - ┌───┐",
                "      │ A │",
                "      ├───┤",
                "      │ a │",
                "      └───┘",
            ]
        );
    }

    #[test]
    fn table_snapshot() {
        let text = from_str(indoc! {"
                | Name | Value |
                |------|-------|
                | foo  | bar   |
                | baz  | qux   |
            "});
        insta::assert_snapshot!(text);
    }

    // ---- width-adaptive layout (Options::max_width) ----

    fn render_at_width(markdown: &str, width: usize) -> Vec<String> {
        let options = Options::new(DefaultStyleSheet).max_width(width);
        from_str_with_options(markdown, &options)
            .lines
            .iter()
            .map(ToString::to_string)
            .collect_vec()
    }

    #[test]
    fn table_shrinks_to_the_available_width() {
        // Natural widths [8, 8, 28] + 10 overhead = 54; at 40 the columns
        // shrink to [4, 3, 23] (budget 30, floors [2, 2, 21] — the URL is
        // one 21-column token) and the table uses exactly the budget.
        let rendered = render_at_width(
            indoc! {"
                | 左揃え | 中央揃え | 右揃え |
                |:-------|:--------:|-------:|
                | a | b | c |
                | 長いセル | 中央 | 1000 |
                | `コード` | **太字** | [リンク](https://example.com) |
            "},
            40,
        );
        assert_eq!(rendered[0], "┌──────┬─────┬─────────────────────────┐");
        assert_eq!(rendered[0].chars().count(), 40);
        assert!(rendered.iter().all(|l| l.chars().count() <= 40));
        // The wrapped link cell: the URL stays whole on its own line.
        assert_eq!(
            rendered[rendered.len() - 2],
            "│ ド   │ 字  │   (https://example.com) │"
        );
    }

    #[test]
    fn width_constrained_tables_separate_body_rows() {
        // The 3-row table from testdata/full.md at width 40: every cell
        // wraps onto several lines, so a separator runs between each pair
        // of body rows and the header separator stays where it was. At a
        // width where the table fits, no body separators appear at all.
        let markdown = indoc! {"
            | 左揃え | 中央揃え | 右揃え |
            |:-------|:--------:|-------:|
            | a | b | c |
            | 長いセル | 中央 | 1000 |
            | `コード` | **太字** | [リンク](https://example.com) |
        "};

        let narrow = render_at_width(markdown, 40);
        let separator_rows: Vec<usize> = narrow
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with('├'))
            .map(|(i, _)| i)
            .collect();
        // Header separator (after the 4-line wrapped header) + one between each
        // body row pair (3 body rows → 2).
        assert_eq!(separator_rows, vec![5, 7, 10]);
        assert!(separator_rows
            .iter()
            .all(|&i| narrow[i] == "├──────┼─────┼─────────────────────────┤"));
        assert!(narrow.iter().all(|l| l.chars().count() <= 40));

        let wide = render_at_width(markdown, 200);
        assert_eq!(
            wide.iter().filter(|l| l.starts_with('├')).count(),
            1,
            "fits naturally: header separator only, light look"
        );
    }

    #[test]
    fn table_keeps_natural_width_when_it_fits() {
        let rendered = render_at_width(
            indoc! {"
                | A | B |
                |---|---|
                | a | b |
            "},
            40,
        );
        assert_eq!(
            rendered,
            [
                "┌───┬───┐",
                "│ A │ B │",
                "├───┼───┤",
                "│ a │ b │",
                "└───┴───┘",
            ]
        );
    }

    #[test]
    fn column_widths_are_proportional_with_floor_and_exact_budget() {
        let natural = vec![8, 8, 28];
        let floors = vec![2, 2, 21];
        assert_eq!(
            proportional_with_floor(&natural, &floors, 30),
            vec![4, 3, 23]
        );
        // The budget is used up exactly.
        let widths = proportional_with_floor(&natural, &floors, 25);
        assert_eq!(widths.iter().sum::<usize>(), 25);
        assert!(widths.iter().zip(&floors).all(|(w, f)| w >= f));
        assert!(widths.iter().zip(&natural).all(|(w, n)| w <= n));
        // A budget at or above the natural total keeps natural widths.
        assert_eq!(proportional_with_floor(&natural, &floors, 44), natural);
    }

    #[test]
    fn columns_shrink_to_one_when_the_floors_cannot_fit() {
        // 2 columns with floors 19 and 2 need 21 + 7 = 28 columns; at a
        // budget below the floor sum every column becomes one display
        // column (wrapping then splits even the URL, char by char).
        let mut builder = TableBuilder::new(vec![]);
        builder.header.cells.push(TableCell {
            spans: vec![(Span::raw("https://example.com"), None)],
        });
        builder.header.cells.push(TableCell {
            spans: vec![(Span::raw("日本語"), None)],
        });
        assert_eq!(builder.column_widths(2, 8), (vec![1, 1], true));
    }

    #[test]
    fn longest_token_measures_display_width() {
        let cell = TableCell {
            spans: vec![(Span::raw("foo 日本語 https://example.com"), None)],
        };
        // "foo" 3, each CJK char 2, the URL 19 — the URL is the widest
        // unsplittable piece.
        assert_eq!(cell.longest_token(), 19);
        let cjk = TableCell {
            spans: vec![(Span::raw("日本語"), None)],
        };
        assert_eq!(cjk.longest_token(), 2);
        let empty = TableCell::default();
        assert_eq!(empty.longest_token(), 0);
    }

    #[test]
    fn cell_content_wraps_into_multiple_rows() {
        // Natural [30, 1] + 7 = 38 > 36 → columns [28, 1]; the 30-column
        // Japanese cell wraps at CJK boundaries into 28 + 2.
        let rendered = render_at_width(
            indoc! {"
                | Column 1 | x |
                |----------|---|
                | 日本語の長いテキストが入るセル | 1 |
            "},
            36,
        );
        assert_eq!(rendered[0], "┌──────────────────────────────┬───┐");
        assert_eq!(rendered[3], "│ 日本語の長いテキストが入るセ │ 1 │");
        assert_eq!(rendered[4], "│ ル                           │   │");
        assert_eq!(rendered[5], "└──────────────────────────────┴───┘");
        assert!(rendered.iter().all(|l| l.chars().count() <= 36));
    }

    #[test]
    fn word_wrap_keeps_long_tokens_whole_and_splits_them_last() {
        // Natural 25 + 4 = 29 > 28 → column 24. "foo https://example.com"
        // (23) fits the first line with the URL whole; "bar" wraps.
        let rendered = render_at_width(
            indoc! {"
                | Link |
                |------|
                | foo https://example.com bar |
            "},
            28,
        );
        assert_eq!(rendered[3], "│ foo https://example.com  │");
        assert_eq!(rendered[4], "│ bar                      │");
        assert!(rendered.iter().all(|l| l.chars().count() <= 28));
    }

    #[test]
    fn wrapped_lines_keep_alignment() {
        // Natural 10 + 4 = 14 > 12 → column 8, right-aligned: every
        // wrapped line is padded on the left.
        let rendered = render_at_width(
            indoc! {"
                | Right |
                |------:|
                | aa bbbbb c |
            "},
            12,
        );
        assert_eq!(rendered[3], "│ aa bbbbb │");
        assert_eq!(rendered[4], "│        c │");
    }

    #[test]
    fn wrapped_cell_lines_carry_the_source_attribution() {
        // Column 7: "aa bb cc" (8) wraps to "aa bb" / "cc"; both lines'
        // content spans carry the cell's source range (source line 2,
        // 0-based). A cell's attribution is a superset by construction:
        // wrapping and whitespace collapsing rewrite the text, so a
        // fragment can only inherit the whole range.
        let options = Options::new(DefaultStyleSheet).max_width(11);
        let (text, attrs) = from_str_with_options_tagged(
            indoc! {"
                | Long |
                |------|
                | aa bb cc |
            "},
            &options,
        );
        let rendered: Vec<String> = text.lines.iter().map(ToString::to_string).collect_vec();
        assert_eq!(rendered[3], "│ aa bb   │");
        assert_eq!(rendered[4], "│ cc      │");
        let cell = Attr::inexact(20..28); // `aa bb cc` within the row
        for line_attrs in &attrs[3..=4] {
            assert_eq!(
                line_attrs,
                &vec![None, None, Some(cell.clone()), None, None],
                "wrapped lines inherit the cell's source range as a superset"
            );
        }
    }

    #[test]
    fn wrapped_rows_pad_shorter_cells_to_the_row_height() {
        // Natural [11, 1] + 7 = 19 > 16 → columns [8, 1]; "aaaa bbbb" wraps
        // to two lines and the second column continues on a blank padded
        // line; every line keeps its │ borders.
        let rendered = render_at_width(
            indoc! {"
                | A | B |
                |---|---|
                | aaaa bbbb | x |
            "},
            16,
        );
        assert_eq!(rendered[0], "┌──────────┬───┐");
        assert_eq!(rendered[3], "│ aaaa     │ x │");
        assert_eq!(rendered[4], "│ bbbb     │   │");
        assert_eq!(rendered[5], "└──────────┴───┘");
    }

    #[test]
    fn cjk_cell_wrapping_never_inserts_spaces() {
        // Natural [9, 2] + 7 = 18 > 16 → columns [7, 2]. 日本語 (6) then
        // abc (3): no space is invented between CJK and Latin text the
        // source did not separate, and the wide い overflows its 2-column
        // cell only because that is its exact natural width.
        let rendered = render_at_width(
            indoc! {"
                | あ | い |
                |---|---|
                | 日本語abc | z |
            "},
            16,
        );
        assert_eq!(rendered[0], "┌─────────┬────┐");
        assert_eq!(rendered[3], "│ 日本語  │ z  │");
        assert_eq!(rendered[4], "│ abc     │    │");
        assert!(rendered.iter().all(|l| l.chars().count() <= 16));
    }

    #[test]
    fn table_in_list_item_uses_the_reduced_width() {
        // The list marker takes 2 columns, so the table lays out within
        // 40 − 2 = 38: columns [3, 2, 23] (naturals [8, 8, 28], budget 28)
        // instead of [4, 3, 23] at the full 40. Without the deduction the
        // view's own wrap would re-cut the laid-out table.
        let rendered = render_at_width(
            indoc! {"
                - | 左揃え | 中央揃え | 右揃え |
                  |:-------|:--------:|-------:|
                  | a | b | c |
                  | 長いセル | 中央 | 1000 |
                  | `コード` | **太字** | [リンク](https://example.com) |
            "},
            40,
        );
        assert_eq!(rendered[0], "- ┌─────┬────┬─────────────────────────┐");
        assert_eq!(rendered[0].chars().count(), 40);
        assert!(rendered.iter().all(|l| l.chars().count() <= 40));
    }

    #[test]
    fn table_in_blockquote_uses_the_reduced_width() {
        // The "  " prefix (spacer + ">") leaves 18 columns; a 14-column
        // cell wraps to 13 + 2 inside that budget.
        let rendered = render_at_width(
            indoc! {"
                > | A |
                > |---|
                > | aa bb cc dd ee |
            "},
            17,
        );
        assert_eq!(rendered[0], "> ┌─────────────┐");
        assert_eq!(rendered[3], "> │ aa bb cc dd │");
        assert_eq!(rendered[4], "> │ ee          │");
        assert!(rendered.iter().all(|l| l.chars().count() <= 17));
    }

    #[test]
    fn wide_character_is_never_split_across_wrapped_lines() {
        // A 2-column CJK character in a 1-column cell overflows the column
        // rather than being split in half.
        let cell = TableCell {
            spans: vec![(Span::raw("日"), None)],
        };
        let rendered = cell.render_lines(1, Alignment::None, Style::default());
        assert_eq!(rendered.len(), 1, "the wide char stays on one line");
        let text: String = rendered[0].0.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('日'));
    }


}
