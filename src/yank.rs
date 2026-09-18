//! `y`: copy the selection (or the cursor line) as it is displayed.
//!
//! The rule is one sentence — *what you see is what you copy*: source
//! mode yields the raw Markdown lines, view mode yields the rendered
//! text, so a reviewer who wants the Markdown presses Tab first. Wrapped
//! rows are unwrapped again (one line per rendered row of an unbounded
//! render), because a paste target wants logical lines, not the pane
//! width's fold points. The joined text carries no trailing newline: the
//! usual paste target is a chat box, not a file.

use crate::highlight::Highlighter;
use crate::render::{self, Rendered};
use crate::source::Source;

/// The width view text is re-rendered at so nothing wraps. Bounded (not
/// `usize::MAX`) so the table layout's budget arithmetic stays sane.
const UNWRAPPED_WIDTH: usize = 4096;

/// Source mode: the raw lines `start..=end` joined by `\n`. `None` when
/// the range lies outside the document.
pub fn source_text(source: &Source, start: usize, end: usize) -> Option<String> {
    let lines = source.lines.get(start..=end)?;
    Some(lines.join("\n"))
}

/// View mode: the rendered text of source lines `start..=end`, exactly
/// the phrases the view highlights for that range (a merged paragraph row
/// contributes only the selected lines' phrases; a row with no
/// attribution — a table border, a rule — contributes its whole text
/// when the range covers it). Rows are trimmed on the right and blank
/// rows at both ends are dropped. `None` when nothing renders for the
/// range (a fence marker, a reference definition, a blank line).
pub fn view_text(
    source: &Source,
    highlighter: &Highlighter,
    start: usize,
    end: usize,
) -> Option<String> {
    if start >= source.lines.len() {
        return None;
    }
    let rendered = render::render(source, UNWRAPPED_WIDTH, highlighter);
    let text = collect_rows(&rendered, start, end);
    if text.is_empty() { None } else { Some(text) }
}

/// The unwrapped rows a line range occupies, cut to the range's phrases.
/// Exposed for tests (the render is deterministic given a theme).
pub(crate) fn collect_rows(rendered: &Rendered, start: usize, end: usize) -> String {
    let n = rendered.source_starts.len();
    if n == 0 || start >= n {
        return String::new();
    }
    let end = end.min(n - 1);
    let mut first_row = rendered.source_starts[start];
    // A table's top border is a synthesized row (no attribution) that
    // sits directly above the header's row; the bottom border falls
    // inside the range by construction (before the next line's start).
    // Pull the top border in too, so a copied table is whole: blocks are
    // separated by blank rows, so a NON-blank unattributed row right
    // above the first attributed row always belongs to the same block.
    while first_row > 0 {
        let above = first_row - 1;
        let unattributed = rendered.row_segments.get(above).is_none_or(|s| s.is_empty());
        let text: String = rendered.rows[above].iter().map(|s| s.text.as_str()).collect();
        if unattributed && !text.trim().is_empty() {
            first_row = above;
        } else {
            break;
        }
    }
    let last_row = rendered
        .source_starts
        .get(end + 1)
        .copied()
        .unwrap_or(rendered.rows.len());
    // A merged paragraph row: `source_starts[start] == source_starts[end+1]`
    // is possible, so the row the range starts on is always included.
    let last_row = last_row.max(first_row + 1).min(rendered.rows.len());
    let mut out: Vec<String> = Vec::new();
    for row in first_row..last_row {
        let full: String = rendered.rows[row].iter().map(|s| s.text.as_str()).collect();
        let segs = rendered
            .row_segments
            .get(row)
            .map(|s| s.as_slice())
            .unwrap_or(&[]);
        let line = if segs.is_empty() {
            full.clone()
        } else {
            // Keep only the phrases attributed to lines inside the range;
            // if the row is entirely someone else's (a paragraph row that
            // only *starts* the range's neighbour), it contributes nothing.
            let mut picked = String::new();
            let mut any_in_range = false;
            let all_in_range = segs.iter().all(|s| (start..=end).contains(&s.line));
            for s in segs {
                if (start..=end).contains(&s.line) {
                    any_in_range = true;
                    if let Some(t) = full.get(s.start..s.end) {
                        picked.push_str(t);
                    }
                }
            }
            if !any_in_range {
                continue;
            }
            // The whole row is ours: keep its prefixes (list marker,
            // quote bar) too — that is what the reader sees.
            if all_in_range { full.clone() } else { picked }
        };
        out.push(line.trim_end().to_string());
    }
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn src(content: &str) -> Source {
        Source::from_content(PathBuf::from("/tmp/yank_test.md"), content.to_string())
    }

    fn hl() -> Highlighter {
        Highlighter::new(Some("base16-ocean.dark"), false)
    }

    #[test]
    fn source_text_joins_lines_without_trailing_newline() {
        let s = src("a\nb\nc\n");
        assert_eq!(source_text(&s, 0, 1).as_deref(), Some("a\nb"));
        assert_eq!(source_text(&s, 2, 2).as_deref(), Some("c"));
        assert_eq!(source_text(&s, 2, 3), None, "past the end is nothing");
    }

    #[test]
    fn view_text_strips_markup_and_unwraps() {
        // A heading loses its `#`, emphasis loses its `*`, and a paragraph
        // written on two source lines comes back as ONE line (the renderer
        // joins soft breaks), never folded at a pane width.
        let s = src("# Title\n\nsome *long* text\nthat continues here\n");
        let h = hl();
        assert_eq!(view_text(&s, &h, 0, 0).as_deref(), Some("Title"));
        assert_eq!(
            view_text(&s, &h, 2, 3).as_deref(),
            Some("some long text that continues here")
        );
    }

    #[test]
    fn view_text_cuts_a_merged_row_to_the_selected_phrase() {
        // Selecting only the second line of a merged paragraph yields just
        // that line's phrase — what the view highlights in gray.
        let s = src("first part\nsecond part\n");
        let h = hl();
        assert_eq!(view_text(&s, &h, 1, 1).as_deref(), Some("second part"));
        assert_eq!(view_text(&s, &h, 0, 0).as_deref(), Some("first part"));
    }

    #[test]
    fn view_text_keeps_list_markers_and_code_without_fences() {
        let s = src("- one\n- two\n\n```sh\necho hi\n```\n");
        let h = hl();
        assert_eq!(view_text(&s, &h, 0, 1).as_deref(), Some("- one\n- two"));
        // The fence lines render nothing; the code line is its own text.
        assert_eq!(view_text(&s, &h, 4, 4).as_deref(), Some("echo hi"));
    }

    #[test]
    fn view_text_copies_a_whole_table_including_its_top_border() {
        let s = src("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let h = hl();
        let t = view_text(&s, &h, 0, 2).unwrap();
        let rows: Vec<&str> = t.lines().collect();
        assert_eq!(rows.len(), 5, "top border, header, rule, row, bottom border: {t}");
        assert!(rows[0].starts_with('┌'), "{t}");
        assert!(rows[4].starts_with('└'), "{t}");
        assert!(rows[1].contains('a') && rows[3].contains('1'), "{t}");
    }

    #[test]
    fn view_text_is_none_where_nothing_renders() {
        let s = src("para\n\n[ref]: https://example.com\n");
        let h = hl();
        assert_eq!(view_text(&s, &h, 1, 1), None, "blank line");
        assert_eq!(view_text(&s, &h, 2, 2), None, "reference definition");
        assert_eq!(view_text(&s, &h, 9, 9), None, "out of range");
    }
}
