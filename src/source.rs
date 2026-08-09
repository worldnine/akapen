//! The markdown file under review: loaded once, split into lines, and queried
//! for line numbers, snippets, and width computations.
//!
//! Line indices inside the TUI are 0-based; `location()`-style output and the
//! `start`/`end` of a `Comment` are 1-based (human-facing, reviewr-compatible).

use std::path::PathBuf;

use std::default::Default;

use anyhow::{Context, Result};

/// The loaded markdown source.
#[derive(Debug, Default)]
pub struct Source {
    /// The path as given on the command line, used for output locations.
    #[allow(dead_code)]
    pub path: PathBuf,
    /// File content verbatim.
    pub content: String,
    /// Lines without trailing newlines. Empty file => empty vec, never crashes.
    pub lines: Vec<String>,
    /// Width of the line-number gutter: digits of the last line number (>= 1).
    pub gutter_width: usize,
}

impl Source {
    /// Build a source from text that did not come from the working tree
    /// (for example a historical Git revision).
    pub fn from_content(path: PathBuf, content: String) -> Self {
        let lines: Vec<String> = content.lines().map(str::to_owned).collect();
        let gutter_width = lines.len().to_string().len().max(1);
        Self {
            path,
            content,
            lines,
            gutter_width,
        }
    }

    /// Load `path`, split into lines, and compute the gutter width.
    ///
    /// Deliberately STRICT UTF-8 (no `from_utf8_lossy`): a binary file or
    /// a Shift-JIS/mid-write edit must fail loudly instead of becoming
    /// mojibake that the reviewer would happily attach line-numbered
    /// comments to. The reload path toasts the error and keeps the old
    /// content; startup's eager load failure is handled separately.
    pub fn load(path: PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(Self::from_content(path, content))
    }

    /// Number of source lines (0 for an empty file).
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The snippet a comment anchors to: 1-based inclusive `start..=end`.
    /// Out-of-range indices clamp; an empty file yields an empty string.
    pub fn snippet(&self, start: u32, end: u32) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let lo = (start.saturating_sub(1) as usize).min(self.lines.len() - 1);
        let hi = (end.saturating_sub(1) as usize).min(self.lines.len() - 1);
        self.lines[lo..=hi].join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::Source;

    fn load(text: &str) -> Source {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, text).unwrap();
        Source::load(path).unwrap()
    }

    #[test]
    fn splits_lines_and_numbers_them_from_one() {
        let s = load("a\nb\nc\n");
        assert_eq!(s.len(), 3);
        assert_eq!(s.lines[0], "a");
        assert_eq!(s.lines[2], "c");
        assert_eq!(s.snippet(1, 2), "a\nb");
    }

    #[test]
    fn no_trailing_newline_still_splits() {
        let s = load("a\nb");
        assert_eq!(s.len(), 2);
        assert_eq!(s.snippet(2, 2), "b");
    }

    #[test]
    fn empty_file_is_not_a_crash() {
        let s = load("");
        assert_eq!(s.len(), 0);
        assert_eq!(s.snippet(1, 1), "");
    }

    #[test]
    fn snippet_clamps_out_of_range() {
        let s = load("x\ny\n");
        assert_eq!(s.snippet(1, 999), "x\ny");
        assert_eq!(s.snippet(0, 0), "x");
    }

    #[test]
    fn gutter_width_follows_line_count() {
        assert_eq!(load("a\n").gutter_width, 1);
        assert_eq!(load(&"x\n".repeat(100)).gutter_width, 3);
    }
}
