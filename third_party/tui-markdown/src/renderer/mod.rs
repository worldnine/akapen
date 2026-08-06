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
/// [`Text::lines`] entry, holding one `Option<usize>` per span in the
/// line — the source line (0-based) the span's text came from.
/// `None` marks synthesized spans (borders, padding, quote/list prefixes,
/// paragraph separators) that belong to no source line. The byte-ranges
/// of the lines are preserved, so a consumer can wrap the text and keep
/// the attribution alongside every fragment.
pub type LineAttrs = Vec<Vec<Option<usize>>>;

/// Render Markdown `input` into a [`Text`] plus per-span source-line
/// attribution (see [`LineAttrs`]). The text is byte-identical to
/// [`from_str_with_options`]; the attribution is computed from
/// pulldown-cmark's event byte-ranges, so every span that renders source
/// text knows exactly which line it came from.
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
        options.styles.clone(),
        options.image_fallback,
        line_starts(input),
    );
    #[cfg(feature = "highlight-code")]
    let writer = writer.with_code_theme(options.selected_code_theme());
    writer.run_tagged()
}

/// Byte offset of the start of every source line (0 at index 0).
fn line_starts(input: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in input.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// The source line containing `byte` (binary search over [`line_starts`]).
fn line_at(line_starts: &[usize], byte: usize) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(i) => i - 1,
    }
}

struct TextWriter<'a, 'theme, I, S: StyleSheet> {
    // Core output state.
    /// Iterator supplying (Markdown event, source byte range).
    iter: I,
    /// Rendered terminal text.
    text: Text<'a>,
    /// Byte offset of each source line's start (for event → line mapping).
    line_starts: Vec<usize>,
    /// The source line of the event currently being handled.
    current_line: Option<usize>,
    /// The source line the current event's range ENDS on. Start/End events
    /// both carry the whole element's range, so a closing construct drawn
    /// from an End event (the metadata block's closing `---`) must anchor
    /// on the range end, not the start.
    current_end_line: Option<usize>,
    /// Per-span source-line attribution, parallel to [`TextWriter::text`].
    out_lines: Vec<Vec<Option<usize>>>,
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
    fn new(iter: I, styles: S, image_fallback: ImageFallback, line_starts: Vec<usize>) -> Self {
        Self {
            iter,
            text: Text::default(),
            line_starts,
            current_line: None,
            current_end_line: None,
            out_lines: Vec::new(),
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
            self.current_line = Some(line_at(&self.line_starts, range.start));
            self.current_end_line = Some(line_at(
                &self.line_starts,
                range.end.saturating_sub(1).max(range.start),
            ));
            self.handle_event(event);
        }
        debug_assert_eq!(self.text.lines.len(), self.out_lines.len());
        for (line, attrs) in self.text.lines.iter().zip(&self.out_lines) {
            debug_assert_eq!(
                line.spans.len(),
                attrs.len(),
                "attribution must parallel the rendered spans"
            );
        }
        (self.text, self.out_lines)
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

            self.push_span_with_line(span, self.current_line.map(|l| l + k));
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
        self.push_line(Line::from("---"), vec![self.current_line]);
        self.push_line(Line::default(), vec![]);
        self.in_metadata_block = true;
    }

    fn end_metadata_block(&mut self) {
        if self.in_metadata_block {
            self.push_line(Line::from("---"), vec![self.current_end_line]);
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
        self.push_line(Line::from("---"), vec![self.current_line]);
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
    fn push_line(&mut self, line: Line<'a>, attrs: Vec<Option<usize>>) {
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
        // carry no source line.
        let prefix_count = if has_prefixes { line_prefixes.len() + 1 } else { 0 };
        let mut all_attrs = Vec::with_capacity(prefix_count + attrs.len());
        all_attrs.extend(std::iter::repeat_n(None, prefix_count));
        all_attrs.extend(attrs);
        self.text.lines.push(line);
        self.out_lines.push(all_attrs);
        // The parallel track must mirror the line's real span count — a
        // drop (ratatui's empty-content `Line::styled`) would silently
        // shift every later attribution on the line.
        debug_assert_eq!(self.out_lines.len(), self.text.lines.len());
        debug_assert_eq!(
            self.out_lines.last().expect("pushed").len(),
            self.text.lines.last().expect("pushed").spans.len()
        );
    }

    #[instrument(level = "trace", skip(self))]
    fn push_span(&mut self, span: Span<'a>) {
        self.push_span_with_line(span, self.current_line);
    }

    #[instrument(level = "trace", skip(self))]
    fn push_span_with_line(&mut self, span: Span<'a>, line: Option<usize>) {
        // An active image owns every span produced by its inline event stream. Checking it before
        // the table sink also lets a completed fallback enter a table cell as one ordered unit.
        if let Some(image) = self.images.last_mut() {
            image.push_span(span, line);
            return;
        }

        // GFM tables are leaf blocks: their cells parse inline content, and block-level elements
        // cannot occur inside them. Pulldown-cmark preserves that boundary by emitting only inline
        // events inside `TableCell`. Keep the active cell as the single span sink anyway so a new
        // inline event handler cannot accidentally write table content into the surrounding text.
        // See <https://github.github.com/gfm/#tables-extension->.
        if let Some(builder) = &mut self.table_builder {
            builder.push_span(span, line);
            return;
        }

        if let Some(line_buf) = self.text.lines.last_mut() {
            line_buf.push_span(span);
            self.out_lines.last_mut().expect("out_lines parallels text").push(line);
        } else {
            self.push_line(Line::from(vec![span]), vec![line]);
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
