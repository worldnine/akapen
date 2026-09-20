//! Markdown event rendering.
//!
//! [`TextWriter`] owns the event loop and shared output state. The event matches remain here as an
//! index of supported pulldown-cmark events, while each Markdown construct keeps its state,
//! rendering behavior, and tests in the corresponding child module.
//!
//! Inline handlers ultimately write spans through [`TextWriter::push_span`]. Active image
//! descriptions receive those spans first, followed by active table cells, then the output line.
//! This sink order preserves inline event ordering inside buffered constructs.

use std::vec;

use itertools::Itertools;
use pulldown_cmark::{CowStr, Event, Options as ParseOptions, Parser, Tag, TagEnd};
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span, Text};
use tracing::{debug, instrument};

#[cfg(feature = "highlight-code")]
use crate::code_theme::CodeTheme;
use crate::options::{ImageFallback, Options};
use crate::style_sheet::StyleSheet;

mod blockquote;
mod code;
mod definition_list;
mod footnote;
mod formatting;
mod heading;
mod html;
mod image;
mod link;
mod list;
mod math;
mod table;
#[cfg(test)]
mod test_support;

/// Render Markdown `input` into a [`Text`] using the default [`Options`].
///
/// The returned text may borrow from `input`. Image syntax renders as a styled text fallback; this
/// function does not read or render image resources.
///
/// # Example
///
/// ```
/// use tui_markdown::from_str;
///
/// let text = from_str("# Status\n\nReady");
///
/// assert_eq!(text.to_string(), "# Status\n\nReady");
/// ```
pub fn from_str(input: &str) -> Text<'_> {
    from_str_with_options(input, &Options::default())
}

/// Render Markdown `input` into a [`Text`] using the supplied [`Options`].
///
/// The returned text may borrow from `input`. The options control styles, image fallback content,
/// and, with the `highlight-code` feature, fenced-code syntax highlighting.
///
/// # Example
///
/// ```
/// use tui_markdown::{from_str_with_options, ImageFallback, Options};
///
/// let options = Options::default().image_fallback(ImageFallback::AltTextAndUrl);
/// let text = from_str_with_options("![diagram](diagram.png)", &options);
///
/// assert_eq!(text.to_string(), "[img] diagram (diagram.png)");
/// ```
pub fn from_str_with_options<'a, S>(input: &'a str, options: &Options<S>) -> Text<'a>
where
    S: StyleSheet,
{
    from_str_with_options_tagged(input, options).0
}

/// Per-source-line attribution of the rendered output: one entry per
/// [`Text::lines`] entry, holding one `Option<Attr>` per span in the
/// line — the source byte range the span's text came from.
/// `None` marks synthesized spans (borders, padding, quote/list prefixes,
/// paragraph separators) that belong to no source range. The ranges are
/// preserved through the line's spans in order, so a consumer can wrap
/// the text and keep the attribution alongside every fragment.
pub type LineAttrs = Vec<Vec<Option<Attr>>>;

/// One rendered span's source attribution: the byte range of `input` the
/// span came from, plus whether that range is **exact**.
///
/// # The exactness contract
///
/// `exact` means the span's text is a *verbatim slice* of the source:
///
/// ```text
/// span.content == input[attr.range]   (hence span.content.len() == attr.range.len())
/// ```
///
/// An exact range can therefore be sub-sliced by byte offset — a wrapped
/// fragment's range is the matching sub-range of its span's range — and a
/// decoration expressed in source bytes maps onto the rendered text
/// character for character.
///
/// `exact == false` means the range is a **correct superset**: the span's
/// text was produced *from* that source range, but is not a copy of it.
/// `**strong**` renders as `strong` with the whole `**strong**` range;
/// `&amp;` renders as `&`; a softbreak renders as a space whose range is
/// the source's `\n`. Such a range can be used to locate the span in the
/// source (which line, which construct), but **not** to slice it: a
/// consumer that decorates `range` would bleed onto the neighbouring
/// source text.
///
/// Every span that a range decoration must be able to target precisely —
/// plain text, emphasis/strong content, heading text, link labels, list
/// item text, blockquote text, inline code — is exact. See
/// `exactness_by_markdown_construct` in this module's tests for the
/// authoritative per-construct table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attr {
    /// Byte range into the rendered input.
    pub range: std::ops::Range<usize>,
    /// Whether `input[range]` is the span's text verbatim (see the type
    /// docs). `false` marks a correct superset.
    pub exact: bool,
}

impl Attr {
    /// An exact attribution: `input[range]` IS the span's text.
    pub fn exact(range: std::ops::Range<usize>) -> Self {
        Self { range, exact: true }
    }

    /// A superset attribution: the span was produced from `range`, but is
    /// not a verbatim copy of it.
    pub fn inexact(range: std::ops::Range<usize>) -> Self {
        Self {
            range,
            exact: false,
        }
    }

    /// The same range, demoted to a superset. Used where a span's text is
    /// synthesized from (or alongside) source text that is itself exact —
    /// a wrap's hanging pad, a tab expanded to spaces.
    pub fn demoted(&self) -> Self {
        Self::inexact(self.range.clone())
    }

    /// The sub-range `self.range.start + start .. self.range.start + end`,
    /// for slicing an EXACT attribution by byte offset into the span's
    /// text. `exact` is carried over from `still_verbatim`, so a fragment
    /// whose text was rewritten (tab expansion) keeps the correct range
    /// while losing the verbatim claim.
    ///
    /// Only meaningful on an exact attribution; on a superset the whole
    /// range must be kept (see [`Attr::exact`]).
    pub fn slice(&self, start: usize, end: usize, still_verbatim: bool) -> Self {
        debug_assert!(self.exact, "only an exact range may be sliced");
        debug_assert!(end <= self.range.len(), "slice within the attributed range");
        Self {
            range: self.range.start + start..self.range.start + end,
            exact: self.exact && still_verbatim,
        }
    }
}

/// Render Markdown `input` into a [`Text`] plus per-span source-range
/// attribution (see [`LineAttrs`] and [`Attr`]). The text is
/// byte-identical to [`from_str_with_options`]; the attribution is
/// computed from pulldown-cmark's event byte-ranges, so every span that
/// renders source text knows the byte range it came from — exactly,
/// wherever the span's text is a verbatim slice of the input.
pub fn from_str_with_options_tagged<'a, S>(input: &'a str, options: &Options<S>) -> (Text<'a>, LineAttrs)
where
    S: StyleSheet,
{
    let mut parse_opts = ParseOptions::empty();
    parse_opts.insert(ParseOptions::ENABLE_STRIKETHROUGH);
    parse_opts.insert(ParseOptions::ENABLE_TASKLISTS);
    parse_opts.insert(ParseOptions::ENABLE_HEADING_ATTRIBUTES);
    parse_opts.insert(ParseOptions::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    parse_opts.insert(ParseOptions::ENABLE_SUPERSCRIPT);
    parse_opts.insert(ParseOptions::ENABLE_SUBSCRIPT);
    parse_opts.insert(ParseOptions::ENABLE_MATH);
    parse_opts.insert(ParseOptions::ENABLE_FOOTNOTES);
    parse_opts.insert(ParseOptions::ENABLE_DEFINITION_LIST);
    parse_opts.insert(ParseOptions::ENABLE_GFM);
    parse_opts.insert(ParseOptions::ENABLE_TABLES);
    let parser = Parser::new_ext(input, parse_opts);

    let writer = TextWriter::new(
        parser.into_offset_iter(),
        input,
        options.styles.clone(),
        options.image_fallback,
        options.max_width,
        line_starts(input),
    );
    #[cfg(feature = "highlight-code")]
    let writer = writer.with_code_theme(options.selected_code_theme());
    writer.run_tagged()
}

/// Byte offset of the start of every source line (0 at index 0).
///
/// Public so a consumer can derive a source LINE from an [`Attr`] with
/// exactly the function the renderer used — one definition, no chance of
/// the two drifting apart at a file's trailing newline.
pub fn line_starts(input: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in input.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// The source line containing `byte` (binary search over [`line_starts`]).
pub fn line_at(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i - 1,
    }
}

struct TextWriter<'a, 'theme, I, S: StyleSheet> {
    // Core output state.
    /// Iterator supplying (Markdown event, source byte range).
    iter: I,
    /// The Markdown source, so handlers can reproduce source text verbatim
    /// (list markers) instead of synthesizing a replacement.
    source: &'a str,
    /// Rendered terminal text.
    text: Text<'a>,
    /// Byte offset of each source line's start, for the per-source-line
    /// attribution of a multi-line event ([`TextWriter::nth_line_attr`]).
    line_starts: Vec<usize>,
    /// Byte range of the event currently being handled. A `Start(Item)`
    /// range begins at the list marker itself, which is how the item
    /// handler reads the marker character verbatim from [`Self::source`].
    current_range: std::ops::Range<usize>,
    /// Per-span source-range attribution, parallel to [`TextWriter::text`]
    /// (`out_attrs[l][i]` belongs to `text.lines[l].spans[i]`).
    out_attrs: Vec<Vec<Option<Attr>>>,
    /// Styles for nested inline constructs, with the active style at the top.
    inline_styles: Vec<Style>,
    /// Prefixes added to each output line, from the outermost block to the innermost.
    line_prefixes: Vec<Span<'a>>,
    /// Styles for nested line-oriented constructs, with the active style at the top.
    line_styles: Vec<Style>,
    /// The [`StyleSheet`] used to style the output.
    styles: S,
    /// Whether the next block needs to start on a new line.
    needs_newline: bool,
    /// Whether raw text is inside a metadata block.
    in_metadata_block: bool,

    // Code rendering state.
    /// Active syntax highlighter while rendering a recognized fenced code block.
    #[cfg(feature = "highlight-code")]
    code_highlighter: Option<syntect::easy::HighlightLines<'theme>>,
    /// Explicit theme used when a fenced code block starts highlighting.
    ///
    /// When absent, code highlighting resolves the shared built-in default.
    #[cfg(feature = "highlight-code")]
    code_theme: Option<&'theme CodeTheme>,
    /// Keeps the writer's shape consistent when syntax highlighting is disabled.
    #[cfg(not(feature = "highlight-code"))]
    code_theme_lifetime: std::marker::PhantomData<&'theme ()>,

    // Heading rendering state.
    /// Heading attributes to append after heading content.
    heading_meta: Option<heading::HeadingMeta<'a>>,

    // Link rendering state.
    /// A link which will be appended to the current line when the link tag is closed.
    link: Option<CowStr<'a>>,

    // Image rendering state.
    /// Images whose descriptions are currently being collected.
    images: Vec<image::PendingImage<'a>>,
    /// Content to render in place of images.
    image_fallback: ImageFallback,
    /// Maximum layout width, in columns; tables shrink and wrap within it.
    /// `None` (the default) renders tables at their natural width.
    max_width: Option<usize>,

    // List rendering state.
    /// Current list index as a stack of indices.
    list_indices: Vec<Option<u64>>,
    /// Layout of each active list item, from the outermost item to the innermost.
    list_items: Vec<list::ListItemLayout>,

    // Paragraph-suppression state.
    /// Whether we are inside a footnote definition.
    in_footnote_definition: bool,
    /// Whether we are inside a definition-list description.
    in_definition_description: bool,

    // Table rendering state.
    /// Active table builder that accumulates cells during table parsing.
    table_builder: Option<table::TableBuilder<'a>>,
}

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = (Event<'a>, std::ops::Range<usize>)>,
    S: StyleSheet,
{
    fn new(
        iter: I,
        source: &'a str,
        styles: S,
        image_fallback: ImageFallback,
        max_width: Option<usize>,
        line_starts: Vec<usize>,
    ) -> Self {
        Self {
            iter,
            source,
            text: Text::default(),
            line_starts,
            current_range: 0..0,
            out_attrs: Vec::new(),
            inline_styles: vec![],
            line_styles: vec![],
            line_prefixes: vec![],
            styles,
            needs_newline: false,
            in_metadata_block: false,
            #[cfg(feature = "highlight-code")]
            code_highlighter: None,
            #[cfg(feature = "highlight-code")]
            code_theme: None,
            #[cfg(not(feature = "highlight-code"))]
            code_theme_lifetime: std::marker::PhantomData,
            heading_meta: None,
            link: None,
            images: vec![],
            image_fallback,
            max_width,
            list_indices: vec![],
            list_items: vec![],
            in_footnote_definition: false,
            in_definition_description: false,
            table_builder: None,
        }
    }

    fn run_tagged(mut self) -> (Text<'a>, LineAttrs) {
        debug!("Running text writer");
        while let Some((event, range)) = self.iter.next() {
            self.current_range = range;
            self.handle_event(event);
        }
        debug_assert_eq!(self.text.lines.len(), self.out_attrs.len());
        for (line, attrs) in self.text.lines.iter().zip(&self.out_attrs) {
            debug_assert_eq!(
                line.spans.len(),
                attrs.len(),
                "attribution must parallel the rendered spans"
            );
            // The exactness contract, checked at the only place that can
            // see both sides: every span claiming an exact range must BE
            // that slice of the source. Catches any later mutation of an
            // already-attributed span (the task-list marker's `to_mut()`,
            // a table cell's re-merge) that would silently make Phase 2's
            // range decoration bleed into the neighbouring text.
            for (span, attr) in line.spans.iter().zip(attrs) {
                if let Some(attr) = attr {
                    debug_assert!(
                        !attr.exact || self.source.get(attr.range.clone()) == Some(span.content.as_ref()),
                        "span {:?} claims exact range {:?} = {:?}",
                        span.content,
                        attr.range,
                        self.source.get(attr.range.clone())
                    );
                }
            }
        }
        (self.text, self.out_attrs)
    }

    #[instrument(level = "debug", skip(self))]
    fn handle_event(&mut self, event: Event<'a>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(text),
            Event::Code(code) => self.code(code),
            Event::Html(html) => self.html_block(html),
            Event::InlineHtml(html) => self.inline_html(html),
            Event::FootnoteReference(label) => self.footnote_reference(label),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.rule(),
            Event::TaskListMarker(checked) => self.task_list_marker(checked),
            Event::InlineMath(math) => self.inline_math(math),
            Event::DisplayMath(math) => self.display_math(math),
        }
    }

    fn start_tag(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Paragraph => self.start_paragraph(),
            Tag::Heading {
                level,
                id,
                classes,
                attrs,
            } => self.start_heading(level, heading::HeadingMeta { id, classes, attrs }),
            Tag::BlockQuote(kind) => self.start_blockquote(kind),
            Tag::CodeBlock(kind) => self.start_codeblock(kind),
            Tag::HtmlBlock => self.start_html_block(),
            Tag::List(start_index) => self.start_list(start_index),
            Tag::Item => self.start_item(),
            Tag::FootnoteDefinition(label) => self.start_footnote_definition(label),
            Tag::Table(alignments) => self.start_table(alignments),
            Tag::TableHead => {}
            Tag::TableRow => {}
            Tag::TableCell => self.start_table_cell(),
            Tag::Emphasis => self.push_inline_style(Style::new().italic()),
            Tag::Strong => self.push_inline_style(Style::new().bold()),
            Tag::Strikethrough => self.push_inline_style(Style::new().crossed_out()),
            Tag::Subscript => self.push_inline_style(Style::new().dim().italic()),
            Tag::Superscript => self.push_inline_style(Style::new().dim().italic()),
            Tag::Link { dest_url, .. } => self.push_link(dest_url),
            Tag::Image { dest_url, .. } => self.start_image(dest_url),
            Tag::MetadataBlock(_) => self.start_metadata_block(),
            Tag::DefinitionList => self.start_definition_list(),
            Tag::DefinitionListTitle => self.start_definition_title(),
            Tag::DefinitionListDefinition => self.start_definition_description(),
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.end_paragraph(),
            TagEnd::Heading(_) => self.end_heading(),
            TagEnd::BlockQuote(_) => self.end_blockquote(),
            TagEnd::CodeBlock => self.end_codeblock(),
            TagEnd::HtmlBlock => self.end_html_block(),
            TagEnd::List(_is_ordered) => self.end_list(),
            TagEnd::Item => self.end_item(),
            TagEnd::FootnoteDefinition => self.end_footnote_definition(),
            TagEnd::Table => self.end_table(),
            TagEnd::TableHead => self.end_table_header(),
            TagEnd::TableRow => self.end_table_row(),
            TagEnd::TableCell => self.end_table_cell(),
            TagEnd::Emphasis => self.pop_inline_style(),
            TagEnd::Strong => self.pop_inline_style(),
            TagEnd::Strikethrough => self.pop_inline_style(),
            TagEnd::Subscript => self.pop_inline_style(),
            TagEnd::Superscript => self.pop_inline_style(),
            TagEnd::Link => self.pop_link(),
            TagEnd::Image => self.end_image(),
            TagEnd::MetadataBlock(_) => self.end_metadata_block(),
            TagEnd::DefinitionList => self.end_definition_list(),
            TagEnd::DefinitionListTitle => self.end_definition_title(),
            TagEnd::DefinitionListDefinition => self.end_definition_description(),
        }
    }

    fn start_paragraph(&mut self) {
        // Loose list items emit a paragraph start after the item handler has already written the
        // marker. Keep only that first paragraph on the marker line; later paragraphs have either
        // added content or set `needs_newline`.
        let list_marker_line_is_open = self.list_items.last().is_some_and(|item| {
            !self.needs_newline
                && self.text.lines.len() == item.marker_line + 1
                && self.text.lines[item.marker_line].spans.len() == item.marker_span_count
        });
        if list_marker_line_is_open {
            return;
        }

        // Footnote definitions and loose definition-list descriptions start with a paragraph event
        // after their handlers have already written a visible prefix (`[label]: ` or `: `) to the
        // current line. Skip normal paragraph handling only for that first paragraph so its content
        // stays beside the prefix. For a later paragraph, `needs_newline` is true; allowing the
        // normal path below to run preserves the blank line in definitions such as:
        //
        //     [^label]: First paragraph.
        //
        //         Second paragraph.
        let prefix_line_is_open = self.in_footnote_definition || self.in_definition_description;
        if prefix_line_is_open && !self.needs_newline {
            return;
        }
        // Insert an empty line between paragraphs if there is at least one line of text already.
        if self.needs_newline {
            self.push_line(Line::default(), vec![]);
        }
        self.push_line(Line::default(), vec![]);
        self.needs_newline = false;
    }

    fn end_paragraph(&mut self) {
        self.needs_newline = true;
    }

    fn text(&mut self, text: CowStr<'a>) {
        if self.table_builder.is_some() {
            let style = self.inline_styles.last().copied().unwrap_or_default();
            self.push_span(Span::styled(text, style));
            return;
        }

        if self.push_highlighted_text(&text) {
            return;
        }

        // A single event can carry several \n-separated source lines (a
        // paragraph's soft breaks, an unhighlighted code block); each
        // segment is rendered on its own line and belongs to the k-th
        // line after the event's start.
        for (k, (position, line)) in text.lines().with_position().enumerate() {
            if self.needs_newline {
                self.push_line(Line::default(), vec![]);
                self.needs_newline = false;
            }
            if !position.is_first() {
                self.push_line(Line::default(), vec![]);
            }

            let style = self.inline_styles.last().copied().unwrap_or_default();

            let span = Span::styled(line.to_owned(), style);

            // `line` is a subslice of the event's text; when that text is
            // still borrowed from the input (the ordinary case — see
            // `exact_attr`), the slice's own offset is an EXACT range and
            // no per-line bookkeeping is needed. Otherwise the event
            // rewrote the text (an entity reference, a smart replacement)
            // and the k-th source line's span is the tightest correct
            // superset — the same line the old line-attribution used.
            let attr = self
                .exact_subslice_attr(&text, line)
                .or_else(|| self.nth_line_attr(k));
            self.push_span_with_attr(span, attr);
        }
        self.needs_newline = false;
    }

    fn hard_break(&mut self) {
        if self.images.is_empty() {
            self.push_line(Line::default(), vec![]);
        } else {
            self.image_description_break();
        }
    }

    fn start_metadata_block(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default(), vec![]);
        }
        self.line_styles.push(self.styles.metadata_block());
        // The drawn fences correspond to the front matter's own `---`
        // lines: the opening one is the block range's first line, the
        // closing one its last (Start/End events both carry the whole
        // element's range, so the end fence anchors on the range end).
        self.push_line(Line::from("---"), vec![self.event_attr()]);
        self.push_line(Line::default(), vec![]);
        self.in_metadata_block = true;
    }

    fn end_metadata_block(&mut self) {
        if self.in_metadata_block {
            self.push_line(Line::from("---"), vec![self.event_end_attr()]);
            self.line_styles.pop();
            self.in_metadata_block = false;
            self.needs_newline = true;
        }
    }

    fn rule(&mut self) {
        if self.needs_newline {
            self.push_line(Line::default(), vec![]);
        }
        // A rule renders its own source line (`---`/`***`/`___`), so the
        // drawn row is attributed to it — the cursor and selection land on
        // the visible rule, and it is never mistaken for an invisible line.
        // The drawn `---` is the renderer's own glyph, not the source's
        // marker (`***` renders as `---`): a superset, never exact.
        self.push_line(Line::from("---"), vec![self.event_attr()]);
        self.needs_newline = true;
    }

    fn soft_break(&mut self) {
        if self.in_metadata_block {
            self.hard_break();
        } else if self.images.is_empty() {
            self.push_span(Span::raw(" "));
        } else {
            self.image_description_break();
        }
    }

    #[instrument(level = "trace", skip(self))]
    fn push_line(&mut self, line: Line<'a>, attrs: Vec<Option<Attr>>) {
        let style = self.line_styles.last().copied().unwrap_or_default();
        let mut line = line.patch_style(style);

        // Add line prefixes to the start of the line.
        let line_prefixes = self.line_prefixes.iter().cloned().collect_vec();
        let has_prefixes = !line_prefixes.is_empty();
        if has_prefixes {
            line.spans.insert(0, " ".into());
        }
        for prefix in line_prefixes.iter().rev().cloned() {
            line.spans.insert(0, prefix);
        }
        // The prefixes (and their spacer) are synthesized markers: they
        // carry no source range.
        let prefix_count = if has_prefixes { line_prefixes.len() + 1 } else { 0 };
        let mut all_attrs: Vec<Option<Attr>> = Vec::with_capacity(prefix_count + attrs.len());
        all_attrs.extend(std::iter::repeat_n(None, prefix_count));
        all_attrs.extend(attrs);
        self.text.lines.push(line);
        self.out_attrs.push(all_attrs);
        // The parallel track must mirror the line's real span count — a
        // drop (ratatui's empty-content `Line::styled`) would silently
        // shift every later attribution on the line.
        debug_assert_eq!(self.out_attrs.len(), self.text.lines.len());
        debug_assert_eq!(
            self.out_attrs.last().expect("pushed").len(),
            self.text.lines.last().expect("pushed").spans.len()
        );
    }

    /// The current event's own range as a SUPERSET attribution: correct,
    /// but not a verbatim slice (the span's text was produced from the
    /// event, not copied out of it). The default for every synthesized
    /// span that still belongs to a construct — list markers, quote
    /// alert headings, `$…$` math, the `[img]` indicator, link URLs.
    fn event_attr(&self) -> Option<Attr> {
        Some(Attr::inexact(self.current_range.clone()))
    }

    /// The current event's END anchored attribution. Start and End events
    /// both carry the whole element's range, so a closing construct drawn
    /// from an End event (the metadata block's closing `---`) must sit on
    /// the range's last byte, not its first.
    fn event_end_attr(&self) -> Option<Attr> {
        let anchor = self
            .current_range
            .end
            .saturating_sub(1)
            .max(self.current_range.start);
        Some(Attr::inexact(anchor..self.current_range.end))
    }

    /// A SUPERSET attribution for the k-th source line of a multi-line
    /// event: the event's range INTERSECTED with that line's byte span.
    /// For `k == 0` that is the event range itself (clipped at the first
    /// line's end) — the tightest correct superset there is.
    ///
    /// Used where the rendered text is a per-source-line transformation
    /// the renderer cannot slice back — a syntax-highlighted code block
    /// (syntect re-splits the line into owned spans), a reconstructed
    /// `$$…$$` block, a text event pulldown-cmark rewrote. Every drawn
    /// line still lands on its own source line instead of collapsing
    /// onto the event's first one.
    fn nth_line_attr(&self, k: usize) -> Option<Attr> {
        let first = line_at(&self.line_starts, self.current_range.start);
        let line_start = *self.line_starts.get(first + k)?;
        if line_start >= self.current_range.end && k > 0 {
            return self.event_attr();
        }
        let start = line_start.max(self.current_range.start);
        let end = self
            .line_starts
            .get(first + k + 1)
            .copied()
            .unwrap_or(self.source.len())
            .min(self.current_range.end)
            .max(start);
        Some(Attr::inexact(start..end))
    }

    /// The EXACT attribution of `text`, when it is a verbatim borrow of
    /// [`Self::source`] — pulldown-cmark hands out `CowStr::Borrowed`
    /// slices of the input for ordinary text, so the offset falls out of
    /// the pointer with no extra bookkeeping.
    ///
    /// Two guards make this sound rather than merely likely: the slice
    /// must lie inside the input's allocation (an `Owned`/`Boxed`/
    /// `Inlined` CowStr — an entity reference, a smart-quote replacement
    /// — is elsewhere), and `source[offset..]` must actually start with
    /// `text`. An empty string is rejected outright: its pointer is not
    /// required to point anywhere, so a zero-length verbatim check would
    /// accept any offset at all.
    ///
    /// Returns `None` when `text` is not a verbatim borrow; the caller
    /// then falls back to a superset ([`Self::event_attr`]).
    fn exact_attr(&self, text: &str) -> Option<Attr> {
        if text.is_empty() {
            return None;
        }
        let base = self.source.as_ptr() as usize;
        let at = text.as_ptr() as usize;
        let offset = at.checked_sub(base)?;
        if offset.checked_add(text.len())? > self.source.len() {
            return None;
        }
        if self.source.get(offset..offset + text.len()) != Some(text) {
            return None;
        }
        Some(Attr::exact(offset..offset + text.len()))
    }

    /// [`Self::exact_attr`] for `part`, a SUBSLICE of the borrowed string
    /// `whole` (a single line of a multi-line text event). Deriving the
    /// offset from `whole`'s base keeps the pointer arithmetic inside one
    /// allocation, so an empty `part` — the leading `""` of a text event
    /// that starts with a newline — is still placed exactly.
    fn exact_subslice_attr(&self, whole: &str, part: &str) -> Option<Attr> {
        let base = self.exact_attr(whole)?;
        let within = (part.as_ptr() as usize).checked_sub(whole.as_ptr() as usize)?;
        if within.checked_add(part.len())? > whole.len() {
            return None;
        }
        Some(base.slice(within, within + part.len(), true))
    }

    #[instrument(level = "trace", skip(self))]
    fn push_span(&mut self, span: Span<'a>) {
        let attr = self.event_attr();
        self.push_span_with_attr(span, attr);
    }

    #[instrument(level = "trace", skip(self))]
    fn push_span_with_attr(&mut self, span: Span<'a>, attr: Option<Attr>) {
        // An active image owns every span produced by its inline event stream. Checking it before
        // the table sink also lets a completed fallback enter a table cell as one ordered unit.
        if let Some(image) = self.images.last_mut() {
            image.push_span(span, attr);
            return;
        }

        // GFM tables are leaf blocks: their cells parse inline content, and block-level elements
        // cannot occur inside them. Pulldown-cmark preserves that boundary by emitting only inline
        // events inside `TableCell`. Keep the active cell as the single span sink anyway so a new
        // inline event handler cannot accidentally write table content into the surrounding text.
        // See <https://github.github.com/gfm/#tables-extension->.
        if let Some(builder) = &mut self.table_builder {
            builder.push_span(span, attr);
            return;
        }

        if let Some(line_buf) = self.text.lines.last_mut() {
            line_buf.push_span(span);
            self.out_attrs.last_mut().expect("out_attrs parallels text").push(attr);
        } else {
            self.push_line(Line::from(vec![span]), vec![attr]);
        }
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use pretty_assertions::assert_eq;
    use rstest::rstest;

    use super::test_support::{with_tracing, DefaultGuard};
    use super::*;

    /// Every rendered span of `input`, as
    /// `(text, "exact"|"range"|"-", source slice or "")`.
    ///
    /// `exact` = the span's text IS `input[range]` (sub-sliceable);
    /// `range` = the range is a correct superset the span was produced
    /// from; `-` = a synthesized span with no source at all.
    fn attribution(input: &str) -> Vec<(String, &'static str, String)> {
        let (text, attrs) = from_str_with_options_tagged(input, &Options::default());
        let mut out = Vec::new();
        for (line, line_attrs) in text.lines.iter().zip(&attrs) {
            for (span, attr) in line.spans.iter().zip(line_attrs) {
                let (kind, slice) = match attr {
                    None => ("-", String::new()),
                    Some(a) => (
                        if a.exact { "exact" } else { "range" },
                        input[a.range.clone()].to_owned(),
                    ),
                };
                // THE contract, asserted on every span of every case
                // below, not just the named one: an exact range is the
                // span's text verbatim.
                if kind == "exact" {
                    assert_eq!(
                        slice,
                        span.content.as_ref(),
                        "exact range must be the span's text verbatim ({input:?})"
                    );
                }
                out.push((span.content.to_string(), kind, slice));
            }
        }
        out
    }

    /// **The exactness table.** Which Markdown constructs render spans
    /// whose source range can be sub-sliced (`exact`) and which only get
    /// a correct superset (`range`) — the contract every later phase's
    /// range decoration is designed against.
    ///
    /// Exact, because pulldown-cmark hands the content out borrowed from
    /// the input and the renderer copies it verbatim: plain text,
    /// emphasis/strong/strikethrough content, heading text, link and
    /// image labels, list item text, blockquote text, inline code,
    /// autolink text, table-less escaped characters, fenced and indented
    /// code content, HTML *inline* text.
    ///
    /// Superset, because the rendered text is not a copy of the source
    /// it came from: an entity reference (`&amp;` → `&`), a softbreak
    /// (`\n` → ` `), a syntax-highlighted fence (syntect re-splits the
    /// line into owned spans), a `$$…$$` block (reconstructed), an HTML
    /// *block* line, a table cell (re-wrapped and whitespace-collapsed),
    /// a list marker, a rule, the metadata fences, the image fallback.
    ///
    /// No source at all: block prefixes and their spacer, table borders
    /// and padding, the task-list checkbox, the heading marker line.
    #[rstest]
    fn exactness_by_markdown_construct(_with_tracing: DefaultGuard) {
        use pretty_assertions::assert_eq;

        // --- exact: the span's text is a verbatim slice ---------------
        assert_eq!(
            attribution("hello world"),
            [("hello world".into(), "exact", "hello world".into())]
        );
        assert_eq!(
            attribution("日本語のテキスト"),
            [("日本語のテキスト".into(), "exact", "日本語のテキスト".into())]
        );
        assert_eq!(
            attribution("絵文字 🎉 と全角ＡＢ"),
            [("絵文字 🎉 と全角ＡＢ".into(), "exact", "絵文字 🎉 と全角ＡＢ".into())]
        );
        // The whole point of the exactness contract: `**重要**` renders
        // as `重要`, and the range covers `重要` ALONE. A naive
        // event-range attribution would cover `**重要**` and bleed the
        // decoration onto the neighbouring text on the same line.
        assert_eq!(
            attribution("前 **重要** 後"),
            [
                ("前 ".into(), "exact", "前 ".into()),
                ("重要".into(), "exact", "重要".into()),
                (" 後".into(), "exact", " 後".into()),
            ]
        );
        assert_eq!(
            attribution("a *em* ~~del~~ b"),
            [
                ("a ".into(), "exact", "a ".into()),
                ("em".into(), "exact", "em".into()),
                (" ".into(), "exact", " ".into()),
                ("del".into(), "exact", "del".into()),
                (" b".into(), "exact", " b".into()),
            ]
        );
        assert_eq!(
            attribution("# 見出し"),
            [
                // The heading marker line is synthesized layout.
                ("# ".into(), "-", String::new()),
                ("見出し".into(), "exact", "見出し".into()),
            ]
        );
        // A link's LABEL is exact; its URL is not (a reference link's URL
        // is borrowed from the ref-def elsewhere in the document, so the
        // renderer never claims a URL span is the text beside it).
        assert_eq!(
            attribution("[ラベル](http://example.com)"),
            [
                ("ラベル".into(), "exact", "ラベル".into()),
                (" (".into(), "range", "[ラベル](http://example.com)".into()),
                (
                    "http://example.com".into(),
                    "range",
                    "[ラベル](http://example.com)".into()
                ),
                (")".into(), "range", "[ラベル](http://example.com)".into()),
            ]
        );
        assert_eq!(
            attribution("a `コード` b"),
            [
                ("a ".into(), "exact", "a ".into()),
                ("コード".into(), "exact", "コード".into()),
                (" b".into(), "exact", " b".into()),
            ]
        );
        assert_eq!(
            attribution("- 項目ひとつ"),
            [
                // The marker is copied from the source but re-indented,
                // so it is a superset of the item's range.
                ("- ".into(), "range", "- 項目ひとつ".into()),
                ("項目ひとつ".into(), "exact", "項目ひとつ".into()),
            ]
        );
        assert_eq!(
            attribution("> 引用文"),
            [
                // The `> ` prefix and its spacer are the renderer's own.
                (">".into(), "-", String::new()),
                (" ".into(), "-", String::new()),
                ("引用文".into(), "exact", "引用文".into()),
            ]
        );
        // An escape is exact: pulldown-cmark emits the escaped character
        // as its own borrowed slice, so the range is the `*`, not `\*`.
        assert_eq!(
            attribution(r"a \* b"),
            [
                ("a ".into(), "exact", "a ".into()),
                ("* b".into(), "exact", "* b".into()),
            ]
        );
        assert_eq!(
            attribution("<http://example.com>"),
            [
                ("http://example.com".into(), "exact", "http://example.com".into()),
                (" (".into(), "range", "<http://example.com>".into()),
                (
                    "http://example.com".into(),
                    "range",
                    "<http://example.com>".into()
                ),
                (")".into(), "range", "<http://example.com>".into()),
            ]
        );
        // An unhighlighted fence keeps its content verbatim, one exact
        // span per line of the block.
        assert_eq!(
            attribution("```not-a-language\nsome code\nmore\n```"),
            [
                // The default style sheet draws the fences; akapen hides
                // them (`code_block_fence` = "").
                ("```not-a-language".into(), "-", String::new()),
                ("some code".into(), "exact", "some code".into()),
                ("more".into(), "exact", "more".into()),
                ("```".into(), "-", String::new()),
            ]
        );

        // --- superset: correct, but not a verbatim slice --------------
        // An entity reference is rewritten, so pulldown-cmark hands out
        // an owned string: the range covers the whole `&amp;`.
        assert_eq!(
            attribution("a &amp; b"),
            [
                ("a ".into(), "exact", "a ".into()),
                ("&".into(), "range", "&amp;".into()),
                (" b".into(), "exact", " b".into()),
            ]
        );
        // A softbreak renders as a space; its range is the source `\n`.
        assert_eq!(
            attribution("one\ntwo"),
            [
                ("one".into(), "exact", "one".into()),
                (" ".into(), "range", "\n".into()),
                ("two".into(), "exact", "two".into()),
            ]
        );
        // A hard break starts a new physical line; both halves stay exact.
        assert_eq!(
            attribution("a\\\nb"),
            [
                ("a".into(), "exact", "a".into()),
                ("b".into(), "exact", "b".into()),
            ]
        );
        // A task-list checkbox is synthesized; the text beside it is exact.
        assert_eq!(
            attribution("- [x] done"),
            [
                ("- [x] ".into(), "range", "- [x] done".into()),
                ("done".into(), "exact", "done".into()),
            ]
        );
        // A table cell's content is re-wrapped and whitespace-collapsed
        // by the layout, so a cell span can never claim to be a slice.
        assert_eq!(
            attribution("| a |\n|---|\n| b |"),
            [
                ("┌───┐".into(), "-", String::new()),
                ("│".into(), "-", String::new()),
                (" ".into(), "-", String::new()),
                ("a".into(), "range", "a".into()),
                (" ".into(), "-", String::new()),
                ("│".into(), "-", String::new()),
                ("├───┤".into(), "-", String::new()),
                ("│".into(), "-", String::new()),
                (" ".into(), "-", String::new()),
                ("b".into(), "range", "b".into()),
                (" ".into(), "-", String::new()),
                ("│".into(), "-", String::new()),
                ("└───┘".into(), "-", String::new()),
            ]
        );
        // A rule draws the renderer's own `---`, whatever the source wrote.
        assert_eq!(
            attribution("a\n\n***\n\nb"),
            [
                ("a".into(), "exact", "a".into()),
                // The event range of a rule includes its newline.
                ("---".into(), "range", "***\n".into()),
                ("b".into(), "exact", "b".into()),
            ]
        );
        // HTML stays literal text but is re-emitted line by line rather
        // than sliced, so each line keeps its own event's range (newline
        // included) as a superset.
        assert_eq!(
            attribution("<div>\n<p>x</p>\n</div>"),
            [
                ("<div>".into(), "range", "<div>\n".into()),
                ("<p>x</p>".into(), "range", "<p>x</p>\n".into()),
                ("</div>".into(), "range", "</div>".into()),
            ]
        );
        // The metadata fences are drawn by the renderer; the opening one
        // anchors on the block's first line, the closing one on its last.
        assert_eq!(
            attribution("---\ntitle: Demo\n---\n\nBody"),
            [
                // Opening fence: the block's whole range (it anchors on
                // the first line). Closing fence: the range's last byte.
                ("---".into(), "range", "---\ntitle: Demo\n---".into()),
                ("title: Demo".into(), "exact", "title: Demo".into()),
                ("---".into(), "range", "-".into()),
                ("Body".into(), "exact", "Body".into()),
            ]
        );
        // An image renders a synthesized fallback; every part of it maps
        // to the image element's own range.
        assert_eq!(
            attribution("![alt](x.png)"),
            [
                ("[img] ".into(), "range", "![alt](x.png)".into()),
                ("alt".into(), "exact", "alt".into()),
            ]
        );

        // --- a highlighted fence: superset, one range per source line --
        #[cfg(feature = "highlight-code")]
        {
            let attrs = attribution("```rust\nfn main() {}\nlet x = 1;\n```");
            // The drawn fences (`-`) aside, syntect re-splits each line
            // into owned spans: nothing inside a highlighted block can
            // claim to be a verbatim slice.
            assert!(
                attrs.iter().all(|(_, kind, _)| *kind != "exact"),
                "no span of a highlighted block is exact: {attrs:?}"
            );
            // Every span of the first drawn line maps to `fn main() {}`,
            // every span of the second to `let x = 1;` — the rows do not
            // collapse onto the fence's opening line.
            let ranges: Vec<&str> = attrs
                .iter()
                .filter(|(_, kind, _)| *kind == "range")
                .map(|(_, _, slice)| slice.as_str())
                .collect();
            assert_eq!(ranges.first(), Some(&"fn main() {}\n"));
            assert_eq!(ranges.last(), Some(&"let x = 1;\n"));
        }
    }

    #[rstest]
    fn empty(_with_tracing: DefaultGuard) {
        assert_eq!(from_str(""), Text::default());
    }

    #[rstest]
    fn paragraph_single(_with_tracing: DefaultGuard) {
        assert_eq!(from_str("Hello, world!"), Text::from("Hello, world!"));
    }

    #[rstest]
    fn paragraph_soft_break(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                Hello
                World
            "}),
            Text::from(Line::from_iter([
                Span::from("Hello"),
                Span::from(" "),
                Span::from("World"),
            ]))
        );
    }

    #[rstest]
    fn paragraph_hard_break(_with_tracing: DefaultGuard) {
        let markdown = indoc! {r"
            Hello\
            World
        "};

        assert_eq!(from_str(markdown), Text::from_iter(["Hello", "World"]));
    }

    #[rstest]
    fn paragraph_multiple(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                Paragraph 1
                
                Paragraph 2
            "}),
            Text::from_iter(["Paragraph 1", "", "Paragraph 2",])
        );
    }

    #[rstest]
    fn rule(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                Paragraph 1

                ---

                Paragraph 2
            "}),
            Text::from_iter(["Paragraph 1", "", "---", "", "Paragraph 2"])
        );
    }

    #[rstest]
    fn metadata_block(_with_tracing: DefaultGuard) {
        assert_eq!(
            from_str(indoc! {"
                ---
                title: Demo
                ---

                Body
            "}),
            Text::from_iter([
                Line::from("---").style(Style::new().light_yellow()),
                Line::from("title: Demo").style(Style::new().light_yellow()),
                Line::from("---").style(Style::new().light_yellow()),
                Line::default(),
                Line::from("Body"),
            ])
        );
    }
}
