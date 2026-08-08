//! Native markdown rendering for view mode.
//!
//! The document is rendered in-process by the vendored `tui-markdown`
//! (pulldown-cmark under the hood) into styled ratatui lines, which are
//! then wrapped into display rows with the same width-aware algorithm
//! source mode uses ([`wrap_spans_tagged`]). The renderer never fails,
//! needs no external binary, and keeps full truecolor.
//!
//! The source-line mapping is **exact**: the vendored renderer threads
//! pulldown-cmark's event byte-ranges through its span sinks, so every
//! rendered span knows the source line it came from
//! (`tui_markdown::from_str_with_options_tagged`). [`build_starts_from_tags`]
//! and [`build_row_segments_from_tags`] derive `source_starts` and
//! `row_segments` directly from that attribution — no text matching, no
//! heuristics. Synthesized spans (table borders and padding, quote/list
//! prefixes, paragraph separators) carry no source line (`None`); rows
//! with no attributable text fall back to row-level highlighting in the
//! view, exactly as before.

use ratatui::style::{Modifier, Style};
use tui_markdown::{BuiltinCodeTheme, CodeTheme, Options, StyleSheet};

use crate::highlight::{Highlighter, Span, wrap_spans_tagged};
use crate::source::Source;

/// One source line's visible phrase inside a rendered row: the byte range
/// (over the concatenated span text) the line's text occupies. A merged
/// paragraph row holds one segment per source line, so the view can
/// highlight exactly the selected lines' text instead of the whole row.
/// Rows that cannot be attributed (a blank row, a wrap inside an
/// unrendered region) carry no segments — the view falls back to
/// row-level highlighting there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// The rendered view: display rows plus the source-line mapping.
#[derive(Debug)]
pub struct Rendered {
    /// One styled row per display line, pre-wrapped to the pane width.
    pub rows: Vec<Vec<Span>>,
    /// First rendered row of each source line (line i spans
    /// `source_starts[i] .. source_starts[i+1]`, last line to rows.len()).
    pub source_starts: Vec<usize>,
    /// Per-row phrase segments (see [`Segment`]).
    pub row_segments: Vec<Vec<Segment>>,
    /// Raw source text of each invisible line (a non-blank line that
    /// rendered no text: ref-defs, fences, HTML) — the view paints it as a
    /// ghost onto a nearby blank row when the cursor or the selection
    /// touches the line. `None` for rendered and blank lines.
    pub ghost: Vec<Option<String>>,
}

/// The style sheet akapen renders markdown with. Block-level colors
/// (headings, links, blockquotes, inline code) are resolved from the
/// `--theme` syntect theme's markdown scopes, so view mode matches source
/// mode's colors; constructs the theme has no rule for fall back to
/// tui-markdown's ANSI-palette defaults. Presentation symbols (heading
/// `#` markers, code fences) are hidden.
#[derive(Clone, Copy)]
struct MdcommentStyleSheet {
    heading: [Style; 6],
    link: Style,
    blockquote: Style,
    code: Style,
}

/// The default style sheet's structural heading modifiers, applied on top
/// of the theme color when the theme's heading rule sets no font style
/// (headings must still read as headings).
fn heading_modifier(level: u8) -> Modifier {
    match level {
        1 => Modifier::BOLD | Modifier::UNDERLINED,
        2 => Modifier::BOLD,
        3 => Modifier::BOLD | Modifier::ITALIC,
        _ => Modifier::ITALIC,
    }
}

impl MdcommentStyleSheet {
    /// Resolve every markdown construct from `highlighter`'s theme.
    fn from_theme(h: &Highlighter) -> Self {
        let theme = |scope: &str| h.scope_style(scope);
        let heading = std::array::from_fn(|i| {
            let level = i as u8 + 1;
            let scope = format!("markup.heading.{level}.markdown");
            match theme(&scope) {
                Some(mut s) => {
                    if s.add_modifier.is_empty() {
                        s = s.add_modifier(heading_modifier(level));
                    }
                    s
                }
                None => tui_markdown::DefaultStyleSheet.heading(level),
            }
        });
        Self {
            heading,
            link: theme("markup.underline.link.markdown")
                .unwrap_or_else(|| tui_markdown::DefaultStyleSheet.link()),
            blockquote: theme("markup.quote.markdown")
                .unwrap_or_else(|| tui_markdown::DefaultStyleSheet.blockquote()),
            code: theme("markup.raw.inline.markdown")
                .unwrap_or_else(|| tui_markdown::DefaultStyleSheet.code()),
        }
    }
}

impl StyleSheet for MdcommentStyleSheet {
    fn heading(&self, level: u8) -> Style {
        self.heading[(level as usize).saturating_sub(1).min(5)]
    }

    fn link(&self) -> Style {
        self.link
    }

    fn blockquote(&self) -> Style {
        self.blockquote
    }

    fn code(&self) -> Style {
        self.code
    }

    fn code_block_fence(&self) -> &str {
        ""
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }
}

/// Serialize a parsed syntect theme back to `.tmTheme` plist XML, so
/// tui-markdown's own code highlighting uses the very same theme as comment
/// mode (the parsed [`Theme`](syntect::highlighting::Theme) round-trips
/// through `CodeTheme::from_textmate`). Rules keep their optional fields
/// (a rule that omits `foreground` stays omitted), preserving the theme's
/// inheritance semantics.
pub fn theme_to_tmtheme(theme: &syntect::highlighting::Theme) -> String {
    use syntect::highlighting::FontStyle;
    let hex = |c: syntect::highlighting::Color| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    let font = |f: FontStyle| {
        let mut parts = Vec::new();
        if f.contains(FontStyle::BOLD) {
            parts.push("bold");
        }
        if f.contains(FontStyle::ITALIC) {
            parts.push("italic");
        }
        if f.contains(FontStyle::UNDERLINE) {
            parts.push("underline");
        }
        parts.join(" ")
    };
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>name</key>
  <string>akapen</string>
  <key>settings</key>
  <array>
    <dict>
      <key>settings</key>
      <dict>"#,
    );
    for (key, color) in [
        ("foreground", theme.settings.foreground),
        ("background", theme.settings.background),
        ("caret", theme.settings.caret),
        ("lineHighlight", theme.settings.line_highlight),
        ("misspelling", theme.settings.misspelling),
    ] {
        if let Some(c) = color {
            out.push_str(&format!("\n        <key>{key}</key>\n        <string>{}</string>", hex(c)));
        }
    }
    out.push_str("\n      </dict>\n    </dict>");
    for item in &theme.scopes {
        let selector = item
            .scope
            .selectors
            .iter()
            .map(|sel| {
                let mut s = sel.path.to_string();
                for ex in &sel.excludes {
                    s.push_str(" - ");
                    s.push_str(&ex.to_string());
                }
                s
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str("\n    <dict>\n      <key>scope</key>\n      <string>");
        out.push_str(&selector);
        out.push_str("</string>\n      <key>settings</key>\n      <dict>");
        if let Some(fg) = item.style.foreground {
            out.push_str(&format!("\n        <key>foreground</key>\n        <string>{}</string>", hex(fg)));
        }
        if let Some(bg) = item.style.background {
            out.push_str(&format!("\n        <key>background</key>\n        <string>{}</string>", hex(bg)));
        }
        if let Some(fs) = item.style.font_style {
            out.push_str(&format!("\n        <key>fontStyle</key>\n        <string>{}</string>", font(fs)));
        }
        out.push_str("\n      </dict>\n    </dict>");
    }
    out.push_str("\n  </array>\n</dict>\n</plist>\n");
    out
}

/// The syntax-highlighting theme for fenced code blocks: the parsed
/// `--theme` serialized back to `.tmTheme`, so view code colors match
/// source mode exactly. Falls back to tui-markdown's default
/// (tui-markdown's built-in base16-ocean.dark) if the serialized theme cannot be re-parsed.
fn code_theme(highlighter: &Highlighter) -> CodeTheme {
    let xml = theme_to_tmtheme(highlighter.theme());
    CodeTheme::from_textmate(&xml)
        .unwrap_or_else(|_| CodeTheme::from(BuiltinCodeTheme::Base16OceanDark))
}

/// Render `source` at `width` columns. The returned rows are display rows
/// (wrapped, one per terminal line); `source_starts` maps each source line
/// to its first display row. All colors come from `highlighter`'s theme.
pub fn render(source: &Source, width: usize, highlighter: &Highlighter) -> Rendered {
    let options = Options::new(MdcommentStyleSheet::from_theme(highlighter))
        .code_theme(code_theme(highlighter))
        // Tables lay out within the pane width (shrinking columns and
        // wrapping cells instead of overflowing); everything else renders
        // at its natural width. The width must agree with the wrap width
        // below, or a laid-out table would be re-cut.
        .max_width(width);
    let (text, line_attrs) =
        tui_markdown::from_str_with_options_tagged(&source.content, &options);
    let mut rows: Vec<Vec<Span>> = Vec::new();
    let mut row_lines: Vec<Vec<Option<usize>>> = Vec::new();
    for (line, attrs) in text.lines.iter().zip(&line_attrs) {
        // Block-level styles (headings, front matter, quotes, tables) live
        // on the Line, not the spans — ratatui renders each span as
        // `line.style.patch(span.style)`. Merge the line style in the same
        // order, or every block-level construct renders unstyled.
        let base = line.style;
        let spans: Vec<Span> = line
            .spans
            .iter()
            .map(|s| Span {
                text: s.content.to_string(),
                style: base.patch(s.style),
            })
            .collect();
        for (r_spans, r_lines) in wrap_spans_tagged(&spans, attrs, width) {
            rows.push(r_spans);
            row_lines.push(r_lines);
        }
    }
    // tui-markdown hardcodes "- " as the unordered-list marker; replace it
    // with a bullet so lists read as rendered, not raw markdown. The marker
    // is its own span ("- " with optional indent), while code lines render
    // as one content span per line — so "- literal dash line" in a code
    // block is left alone. Blockquote prefixes (">") are skipped so lists
    // inside quotes get bullets too.
    for row in &mut rows {
        for span in row.iter_mut() {
            let t = span.text.trim();
            if t == ">" || t.is_empty() {
                continue;
            }
            if t == "-" || t == "- [x]" || t == "- [ ]" {
                let indent = span.text.len() - span.text.trim_start().len();
                span.text = format!("{}●{}", &span.text[..indent], &span.text[indent + 1..]);
            }
            break;
        }
    }
    let source_starts = build_starts_from_tags(&rows, &row_lines, &source.lines);
    let row_segments = build_row_segments_from_tags(&rows, &row_lines);
    // Which source lines rendered any text at all (appear in the tags).
    // Invisible non-blank lines (ref-defs, fences, HTML) share a row by
    // design; the blank-row insertion and the ghost display both need to
    // tell them apart from rendered lines.
    let mut rendered = vec![false; source.lines.len()];
    for lines in &row_lines {
        for l in lines.iter().flatten() {
            rendered[*l] = true;
        }
    }
    let ghost = source
        .lines
        .iter()
        .zip(&rendered)
        .map(|(line, &r)| {
            (!r && !line.trim().is_empty()).then(|| line.trim_end().to_string())
        })
        .collect();
    let (rows, source_starts, row_segments) =
        insert_missing_blank_rows(rows, source_starts, row_segments, &source.lines, &rendered);
    Rendered { rows, source_starts, row_segments, ghost }
}

/// The first display row of each source line, derived from the renderer's
/// own attribution: the first display row whose spans carry the line.
///
/// Unrendered lines get their position from the neighbours:
/// - the FIRST blank of a run homes on the first BLANK row after the
///   previous line's LAST row — synthesized rows with text (a table's
///   bottom border) belong to their block and are skipped — but never
///   past the next rendered line's first row; the blank-row insertion
///   then gives the run a row when the renderer folded it away.
/// - later blanks of a run SHARE the run's row (the view is a markdown
///   preview: standard rendering collapses a blank run to one paragraph
///   break, so the view does too — the same shared-row treatment merged
///   paragraphs get).
/// - fences and reference definitions render no text at all: they keep
///   the previous line's row.
///
/// The result is monotonic by construction (the renderer emits rows in
/// source order); a regression surfaces as a debug assertion instead of a
/// cursor jump.
fn build_starts_from_tags(
    rows: &[Vec<Span>],
    row_lines: &[Vec<Option<usize>>],
    source_lines: &[String],
) -> Vec<usize> {
    let row_has_text =
        |r: usize| rows.get(r).is_some_and(|row| row.iter().any(|s| !s.text.trim().is_empty()));
    let mut first = vec![usize::MAX; source_lines.len()];
    let mut last = vec![0usize; source_lines.len()];
    for (r, lines) in row_lines.iter().enumerate() {
        for l in lines.iter().flatten() {
            if first[*l] == usize::MAX {
                first[*l] = r;
            }
            last[*l] = r;
        }
    }
    let max_row = row_lines.len().saturating_sub(1);
    // The next rendered line's first row for each source line (backward
    // scan; trailing lines fall back to the last row).
    let mut next_first = vec![max_row; source_lines.len()];
    let mut nxt = max_row;
    for i in (0..source_lines.len()).rev() {
        next_first[i] = nxt;
        if first[i] != usize::MAX {
            nxt = first[i];
        }
    }
    let mut starts = first;
    let mut prev_start = 0usize;
    let mut prev_last = 0usize;
    for i in 0..source_lines.len() {
        if starts[i] != usize::MAX {
            prev_start = starts[i];
            prev_last = last[i];
            continue;
        }
        if source_lines[i].trim().is_empty() {
            let run_head = i == 0 || !source_lines[i - 1].trim().is_empty();
            if run_head {
                // Home on the first BLANK row after the previous content:
                // synthesized rows with text (a table's bottom border)
                // belong to their block, never to the gap.
                let mut r = prev_last + 1;
                while r < next_first[i] && row_has_text(r) {
                    r += 1;
                }
                starts[i] = r.min(next_first[i]).min(max_row);
                prev_last = starts[i];
            } else {
                starts[i] = prev_last;
            }
        } else {
            starts[i] = prev_last;
        }
        debug_assert!(
            starts[i] >= prev_start,
            "source_starts went backwards at line {i} ({:?})",
            source_lines[i]
        );
        prev_start = starts[i];
    }
    starts
}

/// Per-row phrase segments derived from the renderer's attribution: for
/// each display row, the byte ranges (over the concatenated span text)
/// each source line occupies. Consecutive spans of the same line merge;
/// `None` spans (synthesized borders, prefixes) break the run, so the
/// segments cover exactly the attributed text. Rows with no attributed
/// text get no segments — the view falls back to row-level highlighting.
fn build_row_segments_from_tags(
    rows: &[Vec<Span>],
    row_lines: &[Vec<Option<usize>>],
) -> Vec<Vec<Segment>> {
    rows.iter()
        .zip(row_lines)
        .map(|(row, lines)| {
            let mut segments: Vec<Segment> = Vec::new();
            let mut start = 0usize;
            let mut cur: Option<usize> = None;
            let mut seg_start = 0usize;
            for (span, line) in row.iter().zip(lines) {
                if let Some(l) = line {
                    if cur != Some(*l) {
                        if let Some(c) = cur {
                            segments.push(Segment { line: c, start: seg_start, end: start });
                        }
                        cur = Some(*l);
                        seg_start = start;
                    }
                } else if let Some(c) = cur {
                    segments.push(Segment { line: c, start: seg_start, end: start });
                    cur = None;
                }
                start += span.text.len();
            }
            if let Some(c) = cur {
                segments.push(Segment { line: c, start: seg_start, end: start });
            }
            segments
        })
        .collect()
}

/// Insert a display row for every blank RUN the renderer folded away (the
/// blank after a heading or fence, say): the run's head then owns a row,
/// so selecting a blank in source mode shows its selection at the gap's
/// position in the view instead of the neighbor's text. Later blanks of a
/// run share the head's row (standard markdown rendering collapses a run
/// to one paragraph break — the view is a preview, not a source mirror).
/// A head whose start already precedes the next owner's start rendered a
/// row of its own and is untouched.
///
/// "Next" skips lines that never own a row: invisible non-blank lines
/// (`rendered[j]` false — a ref-def or fence shares its neighbour's row
/// BY DESIGN) and a run's later blanks. Comparing against those would
/// falsely re-insert a row the head already has.
fn insert_missing_blank_rows(
    mut rows: Vec<Vec<Span>>,
    mut starts: Vec<usize>,
    mut segments: Vec<Vec<Segment>>,
    source_lines: &[String],
    rendered: &[bool],
) -> (Vec<Vec<Span>>, Vec<usize>, Vec<Vec<Segment>>) {
    let blank = |i: usize| source_lines[i].trim().is_empty();
    let run_head = |i: usize| blank(i) && (i == 0 || !blank(i - 1));
    for i in 0..source_lines.len() {
        if !run_head(i) {
            continue;
        }
        let s = starts[i];
        let next = (i + 1..source_lines.len())
            .find(|&j| rendered[j] || run_head(j))
            .map(|j| starts[j])
            .unwrap_or(rows.len());
        if s >= rows.len() || next > s {
            continue; // the renderer already emitted a row for it
        }
        // Before the row the blank shares with the next line. A
        // leading blank's advance can overshoot the first content row
        // (starts[i] = 1 while the content is at 0): insert before the
        // earlier of the two.
        let at = s.min(next);
        rows.insert(
            at,
            vec![Span {
                text: String::new(),
                style: Style::default(),
            }],
        );
        segments.insert(at, Vec::new());
        for st in starts.iter_mut().skip(i + 1) {
            *st += 1;
        }
        starts[i] = at;
        // The run's later blanks stay on the head's new row (the shift
        // above pushed them one past it, onto the next owner's row).
        let mut j = i + 1;
        while j < source_lines.len() && blank(j) {
            starts[j] = at;
            j += 1;
        }
    }
    (rows, starts, segments)
}

#[cfg(test)]
mod tests {
    use super::{Rendered, code_theme, render};
    use crate::highlight::{Highlighter, Span};
    use crate::source::Source;
    use ratatui::style::{Color, Modifier};

    fn full_source() -> Source {
        let root = env!("CARGO_MANIFEST_DIR");
        Source::load(std::path::Path::new(root).join("testdata/full.md")).unwrap()
    }

    fn dump_rows(rows: &[Vec<Span>]) -> String {
        rows.iter()
            .map(|r| {
                r.iter()
                    .map(|s| format!("{:?}|{:?}", s.text, s.style))
                    .collect::<Vec<_>>()
                    .join(";;")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn check_golden(golden_path: &str, dump: &str) {
        let root = env!("CARGO_MANIFEST_DIR");
        let golden = std::path::Path::new(root).join(golden_path);
        if std::env::var_os("REGEN").is_some() {
            std::fs::write(&golden, dump).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&golden).unwrap();
        assert_eq!(dump, expected, "{golden_path} drifted (REGEN=1 to update)");
    }

    #[test]
    fn raw_renderer_output_matches_the_golden() {
        // The vendored renderer's raw output (text + styles, pre-wrap) is
        // frozen in testdata/golden_raw.txt — the appearance baseline
        // itself. Vendoring and the line-attribution instrumentation must
        // not change a single drawn character or style.
        let source = full_source();
        let highlighter = Highlighter::new(None, false);
        let options = tui_markdown::Options::new(super::MdcommentStyleSheet::from_theme(&highlighter))
            .code_theme(code_theme(&highlighter));
        let text = tui_markdown::from_str_with_options(&source.content, &options);
        let dump = text
            .lines
            .iter()
            .map(|l| {
                let spans: Vec<String> = l
                    .spans
                    .iter()
                    .map(|s| format!("{:?}|{:?}", s.content, s.style))
                    .collect();
                format!("{:?}|{}", l.style, spans.join(";;"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        check_golden("testdata/golden_raw.txt", &dump);
    }

    #[test]
    fn rendered_rows_match_golden_snapshot() {
        // The final pipeline output (wrap + bullet replacement + blank
        // insertion) at width 80.
        let source = full_source();
        let Rendered { rows, .. } = render(&source, 80, &Highlighter::new(None, false));
        check_golden("testdata/golden_final_80.txt", &dump_rows(&rows));
    }

    #[test]
    fn rendered_rows_match_golden_snapshot_narrow() {
        // Width 40 exercises the wrap-splitting path (mid-span splits,
        // CJK display width) that width 80 rarely touches.
        let source = full_source();
        let Rendered { rows, .. } = render(&source, 40, &Highlighter::new(None, false));
        check_golden("testdata/golden_final_40.txt", &dump_rows(&rows));
    }

    #[test]
    fn source_starts_are_total_monotonic_and_in_range() {
        let source = full_source();
        let Rendered { rows, source_starts, .. } = render(&source, 80, &Highlighter::new(None, false));
        assert_eq!(source_starts.len(), source.lines.len(), "one start per source line");
        let max_row = rows.len().saturating_sub(1);
        assert!(
            source_starts.iter().all(|&s| s <= max_row),
            "every start within the rendered rows"
        );
        assert!(
            source_starts.windows(2).all(|w| w[0] <= w[1]),
            "source_starts must be monotonic"
        );
    }

    #[test]
    fn every_blank_run_owns_one_row() {
        // The view is a markdown preview: a run of consecutive blanks
        // renders as ONE paragraph break (standard markdown collapses
        // them), so the run's head owns a display row and the later
        // blanks share it — the same shared-row treatment merged
        // paragraphs get. A lone blank still owns its own row.
        let source = full_source();
        let Rendered { source_starts, rows, .. } = render(&source, 80, &Highlighter::new(None, false));
        let blank = |i: usize| source.lines[i].trim().is_empty();
        let last_rendered = source
            .lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .unwrap();
        for i in 0..source.lines.len() {
            if !blank(i) || i == 0 {
                continue;
            }
            if blank(i - 1) {
                assert_eq!(
                    source_starts[i],
                    source_starts[i - 1],
                    "blank {i} is a run member: it shares the run's row"
                );
            } else if i < last_rendered {
                assert!(
                    source_starts[i] > source_starts[i - 1],
                    "blank {i} heads a run: it owns a row after its predecessor"
                );
                let t: String = rows[source_starts[i]].iter().map(|s| s.text.as_str()).collect();
                assert!(
                    t.trim().is_empty(),
                    "blank {i} (1-based {}) heads a run but its row {} has text: {t:?}",
                    i + 1,
                    source_starts[i]
                );
            }
        }
    }

    #[test]
    fn consecutive_blanks_collapse_to_one_row() {
        // para1 / blank / blank / blank / para2 — the preview shows one
        // gap row (what a browser shows), not three; all three source
        // blanks map onto it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "para1\n\n\n\npara2\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, .. } = render(&source, 60, &Highlighter::new(None, false));
        assert_eq!(rows.len(), 3, "para1, one gap row, para2");
        assert_eq!(source_starts, vec![0, 1, 1, 1, 2]);
    }

    #[test]
    fn segments_tile_the_attributed_text_without_gaps_or_overlaps() {
        let source = full_source();
        let Rendered { rows, row_segments, .. } = render(&source, 80, &Highlighter::new(None, false));
        for (r, (row, segs)) in rows.iter().zip(&row_segments).enumerate() {
            let len: usize = row.iter().map(|s| s.text.len()).sum();
            let mut prev_end = 0usize;
            for seg in segs {
                assert!(
                    seg.start <= seg.end && seg.end <= len,
                    "row {r}: segment {seg:?} out of range (row len {len})"
                );
                assert!(seg.start >= prev_end, "row {r}: overlapping segments");
                prev_end = seg.end;
            }
        }
    }

    #[test]
    fn invisible_line_sharing_a_blank_row_does_not_duplicate_it() {
        // blank / ref-def / blank / heading: the ref-def renders nothing and
        // shares the first blank's row BY DESIGN. The blank-row insertion
        // must not read that share as "the blank has no row" — the gap
        // between the paragraph and the heading is exactly the two source
        // blanks' rows, not three (regression: commenting the ref-def line
        // showed a spurious blank in the view).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "本文の段落です\n\n[ref1]: https://example.com \"タイトル\"\n\n## 見出し\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, .. } = render(&source, 60, &Highlighter::new(None, false));
        // The ref-def shares the first blank's row; each blank owns a row.
        assert_eq!(source_starts[2], source_starts[1], "ref-def rides the blank's row");
        assert!(source_starts[3] > source_starts[2], "second blank owns its row");
        let head = source_starts[4];
        let para = source_starts[0];
        let gap = (para + 1..head)
            .filter(|&r| rows[r].iter().all(|s| s.text.is_empty()))
            .count();
        assert_eq!(gap, 2, "exactly the two source blanks' rows, no spurious extra");
    }

    #[test]
    fn rules_attribute_their_own_lines() {
        // `---`/`***`/`___` render a visible rule row: the row must be
        // attributed to the rule's source line (regression: unattributed
        // rules were classed invisible — the ghost painted a gray "---"
        // next to the real rule, and the adjacent blank line's cursor band
        // landed on the rule glyphs).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "見出し前文\n\n---\n\n***\n\n___\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, row_segments, ghost } =
            render(&source, 60, &Highlighter::new(None, false));
        for &line in &[2usize, 4, 6] {
            assert_eq!(ghost[line], None, "a rule renders — no ghost");
            let r = source_starts[line];
            let text: String = rows[r].iter().map(|s| s.text.as_str()).collect();
            assert!(text.contains('-'), "line {line} maps to its rule row: {text:?}");
            assert!(
                row_segments[r].iter().any(|s| s.line == line),
                "the rule row is attributed to line {line}"
            );
        }
        // Each blank in between keeps its own (blank) row.
        for &line in &[1usize, 3, 5] {
            let r = source_starts[line];
            let text: String = rows[r].iter().map(|s| s.text.as_str()).collect();
            assert!(text.trim().is_empty(), "blank line {line} maps to a blank row");
        }
    }

    #[test]
    fn display_math_attributes_each_line() {
        // A $$ block renders its fences and content verbatim; every row
        // must be attributed to its own source line (regression: the whole
        // block collapsed onto the opening $$, so selecting just that line
        // highlighted all three rows).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "数式:\n\n$$\nx = 1\n$$\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, row_segments, ghost } =
            render(&source, 60, &Highlighter::new(None, false));
        for (line, needle) in [(2usize, "$$"), (3, "x = 1"), (4, "$$")] {
            assert_eq!(ghost[line], None, "math lines render — no ghost");
            let r = source_starts[line];
            let text: String = rows[r].iter().map(|s| s.text.as_str()).collect();
            assert!(text.contains(needle), "line {line} row shows {needle:?}: {text:?}");
            assert_eq!(
                row_segments[r].iter().map(|s| s.line).collect::<Vec<_>>(),
                vec![line],
                "row {r} is attributed to line {line} alone"
            );
        }
    }

    #[test]
    fn front_matter_fences_attribute_their_own_lines() {
        // The metadata block's `---` fences render; the opening fence is
        // the element range's first line, the closing one its last.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "---\ntitle: x\n---\n\n# H\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, row_segments, ghost } =
            render(&source, 60, &Highlighter::new(None, false));
        assert_eq!(ghost[0], None);
        assert_eq!(ghost[2], None);
        for line in [0usize, 2] {
            let r = source_starts[line];
            let text: String = rows[r].iter().map(|s| s.text.as_str()).collect();
            assert!(text.contains("---"), "line {line} maps to a fence row: {text:?}");
            assert!(
                row_segments[r].iter().any(|s| s.line == line),
                "fence row {r} attributed to line {line}"
            );
        }
        assert!(source_starts[2] > source_starts[1], "closing fence below the content");
    }

    #[test]
    fn ghost_marks_invisible_lines_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "本文\n\n[ref1]: https://example.com\n\n```rust\nfn main() {}\n```\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { ghost, .. } = render(&source, 60, &Highlighter::new(None, false));
        assert_eq!(ghost[0], None, "rendered text has no ghost");
        assert_eq!(ghost[1], None, "blank lines have no ghost");
        assert_eq!(ghost[2].as_deref(), Some("[ref1]: https://example.com"));
        assert_eq!(ghost[4].as_deref(), Some("```rust"));
        assert_eq!(ghost[5], None, "code lines render and have no ghost");
        assert_eq!(ghost[6].as_deref(), Some("```"));
    }

    #[test]
    fn render_produces_rows_and_monotonic_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "# 見出し\n\n日本語 **太字** [link](https://x.com)\n\n- リスト\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, .. } = render(&source, 60, &Highlighter::new(None, false));
        assert!(rows.len() >= 4);
        let all: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(all.contains("見出し"));
        assert!(all.contains("太字"));
        assert!(all.contains("link"));
        assert!(all.contains("リスト"));
        assert!(
            source_starts.windows(2).all(|w| w[0] <= w[1]),
            "source_starts must be monotonic"
        );
        assert_eq!(source_starts.len(), 5);
    }

    #[test]
    fn render_hides_code_fences_but_keeps_code() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "```rust\nfn main() {}\n```\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, source_starts, .. } = render(&source, 60, &Highlighter::new(None, false));
        let all: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(!all.contains("```"), "fences are hidden");
        assert!(all.contains("fn main() {}"));
        // The fence line renders no text: it keeps the previous row (0).
        assert_eq!(source_starts[0], 0);
        assert!(source_starts[1] >= source_starts[0]);
    }

    #[test]
    fn render_wraps_rows_to_width() {
        use unicode_width::UnicodeWidthStr;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "日本語の長い段落が幅を超えるときに正しく折り返されることを確認します\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, .. } = render(&source, 20, &Highlighter::new(None, false));
        for row in &rows {
            let w: usize = row.iter().map(|s| s.text.width()).sum();
            assert!(w <= 20, "row width {w} exceeds the pane width");
        }
    }

    #[test]
    fn block_level_styles_flow_into_rows() {
        // tui-markdown keeps heading/front-matter styles on the Line;
        // the conversion must merge them into the spans (ratatui renders
        // each span as `line.style.patch(span.style)`).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "---\ntitle: x\n---\n\n# H1\n\n## H2\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, .. } = render(&source, 60, &Highlighter::new(None, false));
        let find = |needle: &str| {
            rows.iter()
                .find(|r| {
                    let t: String = r.iter().map(|s| s.text.as_str()).collect();
                    t.contains(needle)
                })
                .expect(needle)
        };
        // Front matter carries the metadata (light yellow) style.
        let fm = find("title: x");
        assert_eq!(fm[0].style.fg, Some(Color::LightYellow));
        // H2 carries the theme's heading color (Catppuccin Mocha peach)
        // plus the structural bold.
        let h2 = find("H2");
        assert_eq!(h2[0].style.fg, Some(Color::Rgb(250, 179, 135)));
        assert!(h2[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unordered_list_markers_become_bullets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(
            &path,
            "- りんご\n- バナナ\n  - 子リスト\n\n1. 番号\n\n- [x] 完了\n",
        )
        .unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, .. } = render(&source, 60, &Highlighter::new(None, false));
        let all: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(all.contains("● りんご"), "bullet marker: {all:?}");
        assert!(all.contains("● バナナ"));
        assert!(all.contains("● 子リスト"), "nested items get bullets too");
        assert!(all.contains("1. 番号"), "ordered markers stay numbers");
        assert!(all.contains("● [x] 完了"), "task lists keep their checkbox");
        assert!(!all.contains("- りんご"), "no raw hyphen markers left");
    }

    #[test]
    fn code_lines_starting_with_hyphen_keep_their_text() {
        // A fenced code block whose first line starts with "-" must not get
        // the bullet treatment (its spans carry the code style).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "```\n- literal dash line\n```\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, .. } = render(&source, 60, &Highlighter::new(None, false));
        let all: String = rows.iter().flatten().map(|s| s.text.as_str()).collect();
        assert!(all.contains("- literal dash line"), "code kept verbatim: {all:?}");
        assert!(!all.contains("● literal dash line"));
    }

    #[test]
    fn theme_to_tmtheme_round_trips() {
        // The serialized theme must re-parse (the code theme falls back to
        // the built-in default when it does not) and keep its rules.
        let h = Highlighter::new(Some("Dracula"), false);
        let xml = super::theme_to_tmtheme(h.theme());
        assert!(
            tui_markdown::CodeTheme::from_textmate(&xml).is_ok(),
            "serialized theme must re-parse"
        );
        assert!(xml.contains("markup.heading"), "dracula heading rule survives");
    }

    #[test]
    fn scope_style_resolves_theme_colors() {
        // Dracula styles markdown headings with its cyan.
        let h = Highlighter::new(Some("Dracula"), false);
        let style = h.scope_style("markup.heading.2.markdown").expect("dracula colors headings");
        assert_eq!(style.fg, Some(Color::Rgb(139, 233, 253)));
        // A scope the theme has no rule for resolves to None.
        assert_eq!(h.scope_style("markup.table.definitely.not.a.scope"), None);
    }

    #[test]
    fn themed_style_sheet_reflects_the_theme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "## 見出し\n\n> 引用\n").unwrap();
        let source = Source::load(path).unwrap();
        let Rendered { rows, .. } = render(&source, 60, &Highlighter::new(Some("Dracula"), false));
        let find = |needle: &str| {
            rows.iter()
                .find(|r| {
                    let t: String = r.iter().map(|s| s.text.as_str()).collect();
                    t.contains(needle)
                })
                .expect(needle)
        };
        let h2 = find("見出し");
        assert_eq!(h2[0].style.fg, Some(Color::Rgb(139, 233, 253)), "dracula heading color");
        let quote = find("引用");
        assert_eq!(quote[0].style.fg, Some(Color::Rgb(98, 114, 164)), "dracula quote color");
    }

    #[test]
    fn code_blocks_use_the_themes_colors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "```rust\nfn main() {}\n```\n").unwrap();
        let source = Source::load(path).unwrap();
        // base16-ocean's keyword color is a muted pink; dracula's is bright.
        let ocean = render(&source, 60, &Highlighter::new(None, false));
        let dracula = render(&source, 60, &Highlighter::new(Some("Dracula"), false));
        let ocean_row = ocean
            .rows
            .iter()
            .find(|r| r.iter().any(|s| s.text.contains("fn")))
            .unwrap();
        let dracula_row = dracula
            .rows
            .iter()
            .find(|r| r.iter().any(|s| s.text.contains("fn")))
            .unwrap();
        let ocean_fg = ocean_row.iter().map(|s| s.style.fg).collect::<Vec<_>>();
        let dracula_fg = dracula_row.iter().map(|s| s.style.fg).collect::<Vec<_>>();
        assert_ne!(ocean_fg, dracula_fg, "code colors follow the theme");
        assert!(
            dracula_fg.contains(&Some(Color::Rgb(139, 233, 253))),
            "dracula fn (storage.type) cyan appears"
        );
    }




}
