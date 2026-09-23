//! Line selection and review comments.
//!
//! Selection is 0-based internally (source line indices); `Comment` stores
//! 1-based human-facing locations, reviewr-compatible.

/// A range selection over source lines (0-based), anchored where `v` was
/// pressed and extended with j/k.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    /// The line the selection was anchored on (`v`, or the cursor line
    /// when `J`/`K` / Shift+↓↑ started it).
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
    /// **accept した候補との結び目**（`a` / `A` で作ったときだけ `Some`）。
    /// 人が書いたコメントは `None`。同じ行に同じルールの候補が 2 本あっても、
    /// どちらのコメントかをこれで見分ける（`docs/design/marks-only-and-review-mode.md`
    /// 4 節「コメントと候補の結び目」）。
    pub anchor: Option<ReviewAnchor>,
    /// The comment body.
    pub text: String,
}

/// コメントが結ばれている候補 — ルールと、候補の**文書全体でのバイト範囲**
/// （[`crate::review::Candidate::range`] と同じもの）。
///
/// `column` と `excerpt` は送る文面に書く位置で、`range` と本文から作る
/// （[`Self::new`]。`Comment::lines` と同じく本文の写しである）。作り直すのは
/// accept したときと、見届けた reload で付け直したときだけ。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReviewAnchor {
    /// 候補のルール（lint なら `<source>/<code>`）。
    pub rule: String,
    /// 候補のバイト範囲（文書の頭から）。
    pub range: std::ops::Range<usize>,
    /// 範囲の始まりが、その行の何**文字**目か（1 始まり。バイトではない）。
    pub column: usize,
    /// 範囲とその前後を少し含む 1 行の抜粋。範囲は `【】` で括る。前後は
    /// 同じ行の中から 12 文字まで、切ったところに `…`。
    pub excerpt: String,
}

/// 抜粋に含める前後の文字数。
const EXCERPT_CONTEXT: usize = 12;

/// 抜粋の範囲がこれより長ければ、頭と尻だけを残して中を `…` で詰める。
const EXCERPT_RANGE: usize = 24;

impl ReviewAnchor {
    /// `content`（範囲を切り出した版の本文）から、位置と抜粋を作る。
    /// 範囲が本文の外や字の途中を指していれば、抜粋は空で桁は 1 である。
    pub fn new(rule: String, range: std::ops::Range<usize>, content: &str) -> Self {
        let (column, excerpt) = place_of(content, &range).unwrap_or((1, String::new()));
        Self {
            rule,
            range,
            column,
            excerpt,
        }
    }
}

fn place_of(content: &str, range: &std::ops::Range<usize>) -> Option<(usize, String)> {
    let before = content.get(..range.start)?;
    let inside = content.get(range.clone())?;
    let after = content.get(range.end..)?;
    // 前後の文脈は**範囲の行の中だけ**から取る（段落の境目を跨いで見出しを
    // 拾わない）。畳むのは範囲そのものが行を跨ぐときの改行である。
    let line_head = before.rfind('\n').map_or(0, |at| at + 1);
    let before = &before[line_head..];
    let column = before.chars().count() + 1;
    let after = after.split('\n').next().unwrap_or_default();
    let after = after.strip_suffix('\r').unwrap_or(after);

    let before_chars: Vec<char> = before.chars().collect();
    let lead_from = before_chars.len().saturating_sub(EXCERPT_CONTEXT);
    let lead: String = before_chars[lead_from..].iter().collect();
    let after_chars: Vec<char> = after.chars().collect();
    let tail_to = after_chars.len().min(EXCERPT_CONTEXT);
    let tail: String = after_chars[..tail_to].iter().collect();
    let inside_chars: Vec<char> = inside.chars().collect();
    let inside = if inside_chars.len() > EXCERPT_RANGE {
        let half = EXCERPT_RANGE / 2 - 2;
        let head: String = inside_chars[..half].iter().collect();
        let end: String = inside_chars[inside_chars.len() - half..].iter().collect();
        format!("{head}…{end}")
    } else {
        inside.to_string()
    };
    let mut excerpt = String::new();
    if lead_from > 0 {
        excerpt.push('…');
    }
    excerpt.push_str(&lead);
    excerpt.push('【');
    excerpt.push_str(&inside);
    excerpt.push('】');
    excerpt.push_str(&tail);
    if tail_to < after_chars.len() {
        excerpt.push('…');
    }
    Some((column, fold_breaks(&excerpt)))
}

/// 改行を空白 1 つに畳む（`\r` は捨てる）。改行の前後の空白は 1 つにまとめる。
fn fold_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut broke = false;
    for c in text.chars() {
        match c {
            '\r' => {}
            '\n' => {
                while out.ends_with([' ', '\t']) {
                    out.pop();
                }
                broke = true;
            }
            ' ' | '\t' if broke => {}
            _ => {
                if broke {
                    out.push(' ');
                    broke = false;
                }
                out.push(c);
            }
        }
    }
    if broke {
        out.push(' ');
    }
    out
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
            anchor: None,
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
