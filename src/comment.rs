//! Line selection and review comments.
//!
//! Selection is 0-based internally (source line indices); `Comment` stores
//! 1-based human-facing locations, reviewr-compatible.

/// A range selection over source lines (0-based), anchored where `v` was
/// pressed and extended with j/k.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    /// The line where `v` was pressed.
    pub anchor: usize,
    /// The current extension line (equals the source-mode cursor).
    pub cursor: usize,
}

impl Selection {
    pub fn new(anchor: usize) -> Self {
        Self {
            anchor,
            cursor: anchor,
        }
    }

    /// The inclusive 0-based range, normalized so `anchor <= end`.
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    /// Whether `line` (0-based) falls inside the selection.
    pub fn contains(&self, line: usize) -> bool {
        let (a, b) = self.range();
        a <= line && line <= b
    }
}

/// A reviewer comment anchored to a run of source lines, carrying the
/// snippet. Locations are 1-based, reviewr-compatible. Comments never touch
/// the file on disk — output is a separate channel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    /// The file path as given on the command line.
    pub file_path: std::path::PathBuf,
    /// 1-based first line.
    pub start: u32,
    /// 1-based last line.
    pub end: u32,
    /// The verbatim source snippet the comment anchors to.
    pub lines: String,
    /// Exact historical document revision this comment was made against.
    /// `None` means the live working tree.
    pub revision: Option<String>,
    /// The comment body.
    pub text: String,
}

impl Comment {
    /// The `path:start-end` (or `path:line`) location header.
    pub fn location(&self) -> String {
        let path = self.file_path.display();
        if self.start == self.end {
            format!("{path}:{}", self.start)
        } else {
            format!("{path}:{}-{}", self.start, self.end)
        }
    }

    /// `start-end` or `start` (1-based) — the inline card title, which omits
    /// the file (the app title bar already shows it).
    pub fn range_label(&self) -> String {
        if self.start == self.end {
            format!("{}", self.start)
        } else {
            format!("{}-{}", self.start, self.end)
        }
    }

    /// Whether 0-based source `line` falls inside the anchored range.
    pub fn covers(&self, line: usize) -> bool {
        let start = self.start.saturating_sub(1) as usize;
        let end = self.end.saturating_sub(1) as usize;
        start <= line && line <= end
    }
}

#[cfg(test)]
mod tests {
    use super::{Comment, Selection};

    #[test]
    fn selection_normalizes_direction() {
        let s = Selection::new(5);
        assert_eq!(s.range(), (5, 5));
        let up = Selection {
            anchor: 5,
            cursor: 2,
        };
        assert_eq!(up.range(), (2, 5));
        let down = Selection {
            anchor: 2,
            cursor: 7,
        };
        assert_eq!(down.range(), (2, 7));
    }

    #[test]
    fn selection_contains_is_inclusive() {
        let s = Selection {
            anchor: 3,
            cursor: 5,
        };
        assert!(s.contains(3));
        assert!(s.contains(4));
        assert!(s.contains(5));
        assert!(!s.contains(2));
        assert!(!s.contains(6));
    }

    fn comment(start: u32, end: u32) -> Comment {
        Comment {
            file_path: "doc.md".into(),
            start,
            end,
            lines: "snippet".into(),
            revision: None,
            text: "text".into(),
        }
    }

    #[test]
    fn location_formats_range_and_single_line() {
        assert_eq!(comment(12, 14).location(), "doc.md:12-14");
        assert_eq!(comment(31, 31).location(), "doc.md:31");
    }

    #[test]
    fn range_label_omits_the_file() {
        assert_eq!(comment(12, 14).range_label(), "12-14");
        assert_eq!(comment(31, 31).range_label(), "31");
    }

    #[test]
    fn covers_maps_1_based_range_to_0_based_lines() {
        let c = comment(3, 5);
        assert!(c.covers(2));
        assert!(c.covers(3));
        assert!(c.covers(4));
        assert!(!c.covers(1));
        assert!(!c.covers(5));
    }
}
