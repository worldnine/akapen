//! git diff integration (仕様 3 章): changed-line marks (3-1) and the
//! old-side hunk toggle (3-2).
//!
//! Everything is a startup (or manual-reload) snapshot: `git diff` is run
//! once per file and parsed into hunks; there is no live tracking (P4).
//! Outside a git repository [`Diff::load`] returns `None` and the app keeps
//! its non-git behavior (P1).
//!
//! The diff base is a plain `ref` argument (`HEAD` by default), so a future
//! generation shift (`HEAD^`, a tag, …) only needs to pass a different ref
//! (3-2: 世代移動は後付け可能にしておく).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use similar::{ChangeTag, TextDiff};

/// One side of a unified-diff hunk body line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    /// ` ` — present in both files.
    Context,
    /// `+` — only in the new (working-tree) file.
    Add,
    /// `-` — only in the old (ref) file.
    Delete,
}

/// One body line of a parsed hunk.
#[derive(Clone, Debug)]
pub struct HunkLine {
    pub tag: Tag,
    /// The line text without the diff prefix.
    pub text: String,
}

/// A parsed `@@ -a,b +c,d @@` hunk.
#[derive(Clone, Debug)]
pub struct Hunk {
    /// 1-based first line in the old (ref) file; `old_len` lines.
    pub old_start: u32,
    /// The old side's extent — kept for the model's completeness (the
    /// ref side of a future generation shift reads it); the old-side
    /// display itself derives its lines from the body.
    #[allow(dead_code)]
    pub old_len: u32,
    /// 1-based first line in the new (working-tree) file; `new_len` lines.
    pub new_start: u32,
    pub new_len: u32,
    /// The hunk body in order: context / add / delete lines.
    pub body: Vec<HunkLine>,
}

impl Hunk {
    /// The 0-based new-file line range this hunk covers (inclusive), or
    /// `None` for a pure-deletion hunk (it covers no new lines).
    pub fn new_range(&self) -> Option<(usize, usize)> {
        if self.new_len == 0 {
            return None;
        }
        let a = self.new_start as usize - 1;
        Some((a, a + self.new_len as usize - 1))
    }

    /// The new-file line the hunk's position anchors to: the first line of
    /// its new range, or — for a pure deletion — the line FOLLOWING the
    /// deleted block (the last new line when the deletion runs to EOF).
    /// This is where the deletion-position mark (3-1) and the old-side
    /// block of a pure-deletion toggle (3-2) sit.
    pub fn owner(&self, new_len: usize) -> usize {
        self.new_range()
            .map(|(a, _)| a)
            .unwrap_or_else(|| {
                // `+0,0` (a whole-file deletion) has new_start == 0.
                (self.new_start.saturating_sub(1) as usize)
                    .min(new_len.saturating_sub(1))
            })
    }

    /// The old-side lines of the hunk: context + deleted lines in order —
    /// exactly the ref's lines this hunk covers.
    pub fn old_lines(&self) -> Vec<String> {
        self.body
            .iter()
            .filter(|l| l.tag != Tag::Add)
            .map(|l| l.text.clone())
            .collect()
    }

    /// The new-file line a hunk jump lands on: the first added/changed
    /// line of the hunk (the first `+` — context lines are skipped), or
    /// the following line for a pure deletion.
    pub fn anchor(&self, new_len: usize) -> usize {
        let mut new_idx = self.new_start.saturating_sub(1) as usize;
        for l in &self.body {
            match l.tag {
                Tag::Add => return new_idx,
                Tag::Delete => {}
                Tag::Context => new_idx += 1,
            }
        }
        self.owner(new_len)
    }

    /// `(added, deleted)` line counts inside the hunk — the `+N/-M` the
    /// changes list shows per hunk.
    pub fn counts(&self) -> (usize, usize) {
        let mut added = 0usize;
        let mut deleted = 0usize;
        for l in &self.body {
            match l.tag {
                Tag::Add => added += 1,
                Tag::Delete => deleted += 1,
                Tag::Context => {}
            }
        }
        (added, deleted)
    }

    /// The hunk as raw unified-diff text — the snippet for hunk-targeted
    /// comments (the agent reads the change itself, deletions included).
    pub fn diff_text(&self) -> String {
        let mut out =
            format!("@@ -{},{} +{},{} @@", self.old_start, self.old_len, self.new_start, self.new_len);
        for l in &self.body {
            let prefix = match l.tag {
                Tag::Context => " ",
                Tag::Add => "+",
                Tag::Delete => "-",
            };
            out.push('\n');
            out.push_str(prefix);
            out.push_str(&l.text);
        }
        out
    }

    /// The preview line for a changes-list row: the first ADDED line of
    /// the hunk (what the file now contains), else the first deleted
    /// line for a pure deletion.
    pub fn preview_line(&self) -> Option<&str> {
        self.body
            .iter()
            .find(|l| l.tag == Tag::Add)
            .or_else(|| self.body.iter().find(|l| l.tag != Tag::Context))
            .map(|l| l.text.as_str())
    }
}

/// The parsed `git diff <ref> -- <file>` for one file.
#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub hunks: Vec<Hunk>,
    /// The file is untracked (no ref side exists): every line counts as
    /// added, and the old-side toggle has nothing to show.
    pub untracked: bool,
}

impl Diff {
    /// Run `git diff <diff_ref> -- <path>` in the file's directory and
    /// parse the output. `None` when the file is not inside a git
    /// repository (or git is missing, or `diff_ref` does not resolve —
    /// e.g. HEAD on a fresh repo): the caller keeps the non-git behavior.
    pub fn load(diff_ref: &str, path: &Path) -> Option<Diff> {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
        // Run git with the file's directory as cwd (a relative launch
        // path must resolve from wherever the user started akapen) but
        // pass an ABSOLUTE path: with a relative path git would resolve
        // it against the cwd and look for `src/src/main.rs` — missing —
        // marking a tracked file untracked (and diffing nothing).
        let abs = absolutize(path);
        let mut cmd = Command::new("git");
        // -U0: zero context lines. With the default 3 context lines,
        // nearby distinct changes fuse into one giant hunk — `o` then
        // flips half the screen — and the hunk boundaries become
        // unreadable from the marks. With 0 context, a run of adjacent
        // mark rows is ALWAYS one hunk, so the boundaries line up with
        // the mark gaps. `synthesize_diff` (the reload path) already
        // produces this shape, so both sides of the app now agree.
        cmd.arg("diff").arg("-U0").arg(diff_ref).arg("--").arg(&abs);
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut diff = parse_diff(&stdout);
        // Normalize git's raw zero-context output to the internal hunk
        // convention (the one `synthesize_diff` produces and every
        // consumer — owner/deleted_before/hunk_at — expects). This is
        // the ONLY conversion point from git's raw shape; parse_diff
        // stays a pure parser (its unit tests feed it internal-format
        // text directly, so normalizing there would double-count).
        normalize_zero_context(&mut diff);
        // An untracked file has no HEAD side: `git diff` prints nothing for
        // it. `ls-files --error-unmatch` distinguishes that from a tracked
        // file without changes.
        let mut ls = Command::new("git");
        ls.args(["ls-files", "--error-unmatch", "--"]).arg(&abs);
        if let Some(dir) = dir {
            ls.current_dir(dir);
        }
        diff.untracked = !ls.output().map(|o| o.status.success()).unwrap_or(false);
        Some(diff)
    }

    /// 0-based new-file indices of the added/changed lines (every `+`
    /// line — a modified line is a delete+add pair, so the add side is the
    /// changed line in the new file). An untracked file marks every line.
    /// The union of [`Diff::added_modified`] — kept for callers that only
    /// need "changed or added" without the origin split.
    pub fn added(&self, new_len: usize) -> HashSet<usize> {
        let (added, modified) = self.added_modified(new_len);
        added.union(&modified).copied().collect()
    }

    /// 0-based new-file indices of the added/changed lines, split by
    /// origin (diff-scope step ③): `added` = pure additions (lines with
    /// no old counterpart), `modified` = the add lines that pair with a
    /// deletion in the same hunk — a rewritten line, whose old content
    /// the `o` toggle can show. Per hunk the first `min(deleted, added)`
    /// add lines count as rewrites (gitgutter's rule), the rest as pure
    /// additions. `added ∪ modified` equals what [`Diff::added`]
    /// returns. An untracked file: every line is a pure addition.
    pub fn added_modified(&self, new_len: usize) -> (HashSet<usize>, HashSet<usize>) {
        if self.untracked {
            return ((0..new_len).collect(), HashSet::new());
        }
        let mut added = HashSet::new();
        let mut modified = HashSet::new();
        for h in &self.hunks {
            // `+0,0` (a whole-file deletion) has new_start == 0.
            let mut new_idx = h.new_start.saturating_sub(1) as usize;
            let (deleted, inserted) = h.counts();
            let paired = deleted.min(inserted);
            let mut adds_seen = 0usize;
            for l in &h.body {
                match l.tag {
                    Tag::Add => {
                        if adds_seen < paired {
                            modified.insert(new_idx);
                        } else {
                            added.insert(new_idx);
                        }
                        adds_seen += 1;
                        new_idx += 1;
                    }
                    Tag::Delete => {}
                    Tag::Context => new_idx += 1,
                }
            }
        }
        (added, modified)
    }

    /// 0-based new-file indices of the lines immediately following a
    /// deletion block — the thin position mark for deleted lines (3-1):
    /// the line that now sits where the deleted lines were. Matches the
    /// reload-diff convention (`last_deleted_before`).
    pub fn deleted_before(&self, new_len: usize) -> HashSet<usize> {
        let mut out = HashSet::new();
        if new_len == 0 {
            return out;
        }
        for h in &self.hunks {
            let mut new_idx = h.new_start.saturating_sub(1) as usize;
            for l in &h.body {
                match l.tag {
                    Tag::Add | Tag::Context => new_idx += 1,
                    Tag::Delete => {
                        out.insert(new_idx.min(new_len - 1));
                    }
                }
            }
        }
        out
    }

    /// The hunk whose CHANGED region contains new-file `line`: an added
    /// line, or the owner line of a pure deletion. Context-only positions
    /// return `None` — the jump keys treat them as "not on a change" and
    /// land on the nearest hunk instead of stepping.
    /// The index of the hunk under new-file `line` (0-based): the hunk
    /// whose new range contains the line, or — for a pure-deletion hunk —
    /// the hunk whose following line (`owner`) is `line`.
    pub fn hunk_at(&self, line: usize, new_len: usize) -> Option<usize> {
        self.hunks.iter().position(|h| match h.new_range() {
            Some((a, b)) => line >= a && line <= b,
            None => line == h.owner(new_len),
        })
    }
}

/// Build a zero-context [`Diff`] from two file contents — the reload
/// path's analogue of `git diff -U0` (the diff-scope step 1: the reload
/// diff is normalized into the same structure the git snapshot uses, so
/// marks, counts, old-side and navigation can share one consumer).
/// The git snapshot side ([`Diff::load`]) now ALSO runs `git diff -U0`
/// and normalizes its pure-deletion hunks to this same internal
/// convention, so both sides of the app produce identical hunk shapes. The
/// runs of inserted/deleted lines TextDiff finds become hunks; a
/// modification (delete run immediately followed by an insert run) is
/// ONE hunk, its body deletes-then-adds, no context lines.
///
/// Header conventions (what [`Hunk::owner`], [`Hunk::anchor`],
/// [`Hunk::new_range`], [`Hunk::hunk_at`], [`Diff::added`] and
/// [`Diff::deleted_before`] expect):
/// - the non-empty side is 1-based: `old_start` = first deleted old
///   line, `new_start` = first added new line (git's `-2,2 +2,2`);
/// - a pure insertion carries its 0-based position on the zero-length
///   old side (git's `-2,0 +3,2`);
/// - a pure deletion carries the 1-based position of the deleted block
///   on the zero-length new side — `new_start` = deletion position + 1,
///   and 0 for a start-of-file deletion, exactly git's `+0,0`. Note
///   that git's own zero-context output uses the BARE position (`+4,0`
///   for a deletion after new line 4): that only lines up with
///   `deleted_before`/`owner` because its default 3 context lines walk
///   `new_idx` forward first — with zero context the bare form would
///   put the deletion mark on the line BEFORE the deleted block (off
///   by one from the reload convention). The +1 form is what the
///   formulas expect when there are no context lines.
///
/// The returned hunks are sorted by new position (old position breaks
/// ties) — same file order as real git diffs — so position-ordered
/// navigation (step ②) can walk them directly.
pub fn synthesize_diff(old: &str, new: &str) -> Diff {
    // (old position, new position, hunk) — the positions are the sort
    // key: similar's Myers can emit the insert of a same-position
    // insert/delete pair before its delete, while real git diffs are
    // always in file order. Navigation (step ②) depends on hunk order,
    // so the synthesized hunks are sorted by new position (old position
    // breaks ties) before returning.
    let mut hunks: Vec<(usize, usize, Hunk)> = Vec::new();
    // The open hunk as (old position, new position, delete lines, add
    // lines): positions are 0-based, captured when the hunk opens. Equal
    // lines close it — a hunk is a maximal run of changes.
    let mut open: Option<(usize, usize, Vec<HunkLine>, Vec<HunkLine>)> = None;
    let mut old_idx = 0usize;
    let mut new_idx = 0usize;
    for change in TextDiff::from_lines(old, new).iter_all_changes() {
        let value = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                let n = value.lines().count();
                old_idx += n;
                new_idx += n;
                if let Some(h) = open.take() {
                    hunks.push((h.0, h.1, finish_hunk(h)));
                }
            }
            ChangeTag::Delete => {
                let slot = open.get_or_insert_with(|| (old_idx, new_idx, Vec::new(), Vec::new()));
                for text in value.lines() {
                    slot.2.push(HunkLine { tag: Tag::Delete, text: text.to_owned() });
                }
                old_idx += value.lines().count();
            }
            ChangeTag::Insert => {
                let slot = open.get_or_insert_with(|| (old_idx, new_idx, Vec::new(), Vec::new()));
                for text in value.lines() {
                    slot.3.push(HunkLine { tag: Tag::Add, text: text.to_owned() });
                }
                new_idx += value.lines().count();
            }
        }
    }
    if let Some(h) = open.take() {
        hunks.push((h.0, h.1, finish_hunk(h)));
    }
    hunks.sort_by_key(|&(old_pos, new_pos, _)| (new_pos, old_pos));
    Diff {
        hunks: hunks.into_iter().map(|(_, _, h)| h).collect(),
        untracked: false,
    }
}

/// Normalize git's raw `-U0` output to the internal hunk convention:
/// git writes the zero-length side of a pure-deletion hunk as the bare
/// 0-based position (`+4,0` = "after new line 4"), while the internal
/// convention (and every consumer: [`Hunk::owner`], [`Diff::deleted_before`],
/// [`Hunk::hunk_at`]) expects `new_start - 1` to be the deletion
/// position — i.e. `new_start` 1-based, 0 for a start-of-file deletion
/// (git's `+0,0`, which is already internal-form).
fn normalize_zero_context(diff: &mut Diff) {
    for h in &mut diff.hunks {
        if h.new_len == 0 && h.new_start != 0 {
            h.new_start += 1;
        }
    }
}

/// Close an open hunk: the body in unified-diff order (deletes before
/// adds) and the 1-based headers described on [`synthesize_diff`].
fn finish_hunk(
    (old_pos, new_pos, deletes, adds): (usize, usize, Vec<HunkLine>, Vec<HunkLine>),
) -> Hunk {
    let old_len = deletes.len() as u32;
    let new_len = adds.len() as u32;
    let mut body = deletes;
    body.extend(adds);
    Hunk {
        old_start: if old_len > 0 { old_pos as u32 + 1 } else { old_pos as u32 },
        old_len,
        // Pure deletions (new_len == 0) carry the 1-based position of
        // the deleted block — except a start-of-file deletion, which
        // keeps git's `+0,0`.
        new_start: if new_len > 0 || new_pos > 0 {
            new_pos as u32 + 1
        } else {
            0
        },
        new_len,
        body,
    }
}

/// Resolve a launch path against the process cwd: git calls receive an
/// absolute path, so a relative argument like `src/main.rs` cannot be
/// mis-resolved against the file's own directory.
fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

/// Parse unified-diff text into hunks. Tolerates the noise git emits
/// around the hunks (`diff --git` headers, `Binary files … differ`,
/// `\ No newline at end of file` markers, multiple files).
fn parse_diff(text: &str) -> Diff {
    let mut hunks = Vec::new();
    let mut lines = text.lines().peekable();
    loop {
        // Scan for the next hunk header (everything else is header noise).
        while let Some(line) = lines.peek() {
            if line.starts_with("@@") {
                break;
            }
            lines.next();
        }
        let Some(header) = lines.next() else { break };
        if !header.starts_with("@@") {
            break;
        }
        let Some(mut hunk) = parse_hunk_header(header) else {
            continue; // malformed header: skip it, the scan advances
        };
        while let Some(line) = lines.peek() {
            let Some(c) = line.chars().next() else { break };
            let tag = match c {
                ' ' => Tag::Context,
                '+' => Tag::Add,
                '-' => Tag::Delete,
                // `\ No newline at end of file`: not a content line.
                '\\' => {
                    lines.next();
                    continue;
                }
                _ => break, // the next hunk header or file
            };
            hunk.body.push(HunkLine {
                tag,
                text: line[1..].to_string(),
            });
            lines.next();
        }
        hunks.push(hunk);
    }
    Diff {
        hunks,
        untracked: false,
    }
}

/// Parse `@@ -a[,b] +c[,d] @@ …` into a [`Hunk`]. Both counts default to
/// 1 when omitted (single-line hunks).
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@")?;
    let ranges = rest.split("@@").next()?;
    let mut parts = ranges.split_whitespace();
    let (old_start, old_len) = parse_side(parts.next()?)?;
    let (new_start, new_len) = parse_side(parts.next()?)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        body: Vec::new(),
    })
}

/// Parse `-a[,b]` / `+a[,b]` into (start, len).
fn parse_side(s: &str) -> Option<(u32, u32)> {
    let s = s.trim_start_matches(['-', '+']);
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunk_headers_with_and_without_counts() {
        let d = parse_diff("@@ -1,3 +1,3 @@\n a\n b\n c\n");
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 3, 1, 3));
        // Single-line hunks omit the count: `-1 +1`.
        let d = parse_diff("@@ -5 +7 @@\n x\n");
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (5, 1, 7, 1));
        // Zero-count sides (pure additions/deletions).
        let d = parse_diff("@@ -0,0 +3,2 @@\n+a\n+b\n");
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (0, 0, 3, 2));
        // The trailing section text (function context) is ignored.
        let d = parse_diff("@@ -1,2 +1,2 @@ fn main() {\n x\n");
        assert_eq!(d.hunks.len(), 1);
    }

    #[test]
    fn parses_body_lines_and_skips_no_newline_markers() {
        let d = parse_diff(
            "@@ -1,4 +1,4 @@\n keep\n-old\n+new\n keep2\n\\ No newline at end of file\n",
        );
        let h = &d.hunks[0];
        let tags: Vec<Tag> = h.body.iter().map(|l| l.tag).collect();
        assert_eq!(
            tags,
            vec![Tag::Context, Tag::Delete, Tag::Add, Tag::Context],
            "the no-newline marker is not a body line"
        );
        let texts: Vec<&str> = h.body.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["keep", "old", "new", "keep2"]);
    }

    #[test]
    fn skips_binary_and_multifile_noise() {
        // Binary files and unrelated headers contribute no hunks.
        let d = parse_diff(
            "diff --git a/x b/x\nindex 1..2 100644\nBinary files a/x and b/x differ\n\
             diff --git a/y b/y\n@@ -1 +1 @@\n-a\n+b\n",
        );
        assert_eq!(d.hunks.len(), 1, "only the text file's hunk survives");
        assert_eq!(d.hunks[0].old_lines(), vec!["a"]);
    }

    #[test]
    fn old_lines_are_context_and_deletes_in_order() {
        let d = parse_diff("@@ -1,5 +1,4 @@\n c1\n-old1\n-old2\n+new1\n c2\n c3\n");
        assert_eq!(d.hunks[0].old_lines(), vec!["c1", "old1", "old2", "c2", "c3"]);
    }

    #[test]
    fn anchor_lands_on_the_first_changed_line() {
        // Context lines are skipped; the first `+` is the landing line.
        let d = parse_diff("@@ -1,5 +1,5 @@\n c1\n-old1\n+new1\n c2\n c3\n");
        let h = &d.hunks[0];
        assert_eq!(h.anchor(10), 1, "the changed line (0-based)");
        // A pure addition anchors at the first added line.
        let d = parse_diff("@@ -1,2 +1,4 @@\n c1\n c2\n+new3\n+new4\n");
        assert_eq!(d.hunks[0].anchor(10), 2);
        // A pure deletion anchors at the following line.
        let d = parse_diff("@@ -2,3 +2,0 @@\n-b\n-c\n-d\n");
        assert_eq!(d.hunks[0].anchor(5), 1);
    }

    #[test]
    fn counts_and_preview_line_describe_the_hunk() {
        let d = parse_diff("@@ -1,5 +1,6 @@\n c1\n-old1\n+new1\n+new2\n c2\n c3\n");
        let h = &d.hunks[0];
        assert_eq!(h.counts(), (2, 1), "+2/-1");
        assert_eq!(h.preview_line(), Some("new1"), "the first changed line");
        let d = parse_diff("@@ -2,3 +2,0 @@\n-b\n-c\n-d\n");
        assert_eq!(d.hunks[0].counts(), (0, 3));
        assert_eq!(d.hunks[0].preview_line(), Some("b"));
    }

    #[test]
    fn new_range_and_owner() {
        let h = Hunk {
            old_start: 1,
            old_len: 3,
            new_start: 2,
            new_len: 2,
            body: vec![],
        };
        assert_eq!(h.new_range(), Some((1, 2)));
        assert_eq!(h.owner(10), 1);
        // Pure deletion: the following line anchors it.
        let h = Hunk {
            old_start: 5,
            old_len: 3,
            new_start: 4,
            new_len: 0,
            body: vec![],
        };
        assert_eq!(h.new_range(), None);
        assert_eq!(h.owner(10), 3, "the line after the deleted block");
        // Deletion to EOF: the last new line anchors it.
        let h = Hunk {
            old_start: 4,
            old_len: 2,
            new_start: 3,
            new_len: 0,
            body: vec![],
        };
        assert_eq!(h.owner(3), 2, "clamped to the last new line");
        assert_eq!(h.owner(0), 0, "empty new file clamps to 0");
    }

    /// A modification of line 3 (old `three` → new `changed`) plus an
    /// appended line, with the default 3 context lines.
    fn modification_diff() -> Diff {
        parse_diff(
            "@@ -1,4 +1,5 @@\n one\n two\n-three\n+changed\n four\n+five\n",
        )
    }

    #[test]
    fn added_and_deleted_before_track_new_file_indices() {
        let d = modification_diff();
        let mut added: Vec<usize> = d.added(5).into_iter().collect();
        added.sort_unstable();
        assert_eq!(added, vec![2, 4], "the changed line and the appended line");
        let mut del: Vec<usize> = d.deleted_before(5).into_iter().collect();
        del.sort_unstable();
        assert_eq!(del, vec![2], "the line that replaced the deleted one");
    }

    #[test]
    fn untracked_marks_every_line() {
        let d = Diff {
            untracked: true,
            ..Default::default()
        };
        assert_eq!(d.added(3).len(), 3);
        assert_eq!(d.added(0).len(), 0);
    }

    #[test]
    fn hunk_at_resolves_ranges_and_pure_deletions() {
        // A 10-line file with line 5 changed: with the default 3 context
        // lines the hunk covers new lines 2-8 (0-based), not the ends.
        let d = parse_diff(
            "@@ -2,7 +2,7 @@\n two\n three\n four\n-five\n+changed\n six\n seven\n eight\n",
        );
        assert_eq!(d.hunk_at(4, 10), Some(0));
        assert_eq!(d.hunk_at(7, 10), Some(0));
        assert_eq!(d.hunk_at(0, 10), None, "context outside the range");
        assert_eq!(d.hunk_at(9, 10), None);
        // Pure deletion: the following line resolves to the hunk.
        let d = parse_diff("@@ -2,3 +2,0 @@\n-b\n-c\n-d\n");
        assert_eq!(d.hunk_at(1, 5), Some(0), "the following line");
        assert_eq!(d.hunk_at(0, 5), None);
        // A whole-file deletion (`+0,0`, as git emits it) resolves at the
        // empty file's single cursor position.
        let d = parse_diff("@@ -1,5 +0,0 @@\n-a\n-b\n-c\n-d\n-e\n");
        assert_eq!(d.hunks[0].new_range(), None);
        assert_eq!(d.hunks[0].owner(0), 0, "empty new file anchors at 0");
        assert_eq!(d.hunk_at(0, 0), Some(0));
        assert!(d.added(0).is_empty() && d.deleted_before(0).is_empty());
    }

    /// Runs `git` for real: init a repo in `dir`, commit `doc.md`, return
    /// its path. Skipped implicitly when git is unavailable (the tests
    /// panic loudly — this feature is git integration, so the dev
    /// environment is expected to have git).
    fn init_repo(dir: &std::path::Path, lines: &[&str]) -> std::path::PathBuf {
        let git = |args: &[&str]| {
            let status = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        let status = Command::new("git").arg("init").arg("-q").arg(dir).status().unwrap();
        assert!(status.success(), "git init failed — is git installed?");
        let path = dir.join("doc.md");
        let content = lines.join("\n") + "\n";
        std::fs::write(&path, content).unwrap();
        git(&["add", "."]);
        git(&[
            "-c", "user.name=test", "-c", "user.email=test@test", "commit", "-q", "-m", "init",
        ]);
        path
    }

    #[test]
    fn load_reads_the_diff_vs_head() {
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four"]);
        std::fs::write(&path, "one\ntwo\nchanged\nfour\nfive\n").unwrap();
        let diff = Diff::load("HEAD", &path).expect("inside a git repo");
        assert!(!diff.untracked);
        // -U0: the rewrite and the appended line are separate hunks, each
        // with zero context lines.
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!(diff.hunks[0].old_lines(), vec!["three"], "the rewritten line");
        assert_eq!(diff.hunks[0].new_range(), Some((2, 2)));
        assert!(diff.hunks[1].old_lines().is_empty(), "a pure addition");
        assert_eq!(diff.hunks[1].new_range(), Some((4, 4)));
    }

    #[test]
    fn load_absolutizes_relative_paths() {
        // Launching `akapen src/main.rs` from the repo root passes a
        // relative path; the git calls must receive it ABSOLUTE, or git
        // resolves it against the file's own directory (cwd) and looks
        // for `src/src/main.rs` — missing — marking a tracked file
        // untracked. (The full git round-trip is covered by
        // load_reads_the_diff_vs_head with an absolute path.)
        let abs = absolutize(Path::new("sub/doc.md"));
        assert!(abs.is_absolute(), "{abs:?}");
        assert!(abs.ends_with("sub/doc.md"), "{abs:?}");
        // An absolute path passes through untouched.
        let p = Path::new("/tmp/x.md");
        assert_eq!(absolutize(p), p.to_path_buf());
    }

    #[test]
    fn load_accepts_a_ref_argument_for_generation_shifts() {
        // The ref is a plain argument: diffing against HEAD~1 (or any
        // future generation) needs no structural change (3-2).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two"]);
        let git = |args: &[&str]| {
            let status = Command::new("git").arg("-C").arg(dir.path()).args(args).status().unwrap();
            assert!(status.success());
        };
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        git(&["add", "."]);
        git(&[
            "-c", "user.name=test", "-c", "user.email=test@test", "commit", "-q", "-m", "second",
        ]);
        std::fs::write(&path, "one\nCHANGED\nthree\n").unwrap();
        let diff = Diff::load("HEAD~1", &path).expect("HEAD~1 resolves");
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].old_lines(), vec!["two"], "HEAD~1 has no `three` — the rewritten line");
    }

    #[test]
    fn load_marks_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path(), &["tracked"]);
        let untracked = dir.path().join("untracked.md");
        std::fs::write(&untracked, "new file\n").unwrap();
        let diff = Diff::load("HEAD", &untracked).expect("inside the repo");
        assert!(diff.untracked, "?? files have no HEAD side");
        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn load_returns_none_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap(); // no git init
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "x\n").unwrap();
        assert!(Diff::load("HEAD", &path).is_none(), "no repo, no diff");
    }

    /// The pre-normalization reload-diff scan (the old reload_source
    /// walk): frozen here as the oracle the synthesized diff must
    /// reproduce — the diff-scope step 1 promise was that marks and
    /// counts stay exactly as before.
    fn legacy_reload_scan(old: &str, new: &str) -> (HashSet<usize>, HashSet<usize>) {
        use similar::{ChangeTag, TextDiff};
        let diff = TextDiff::from_lines(old, new);
        let mut changed: HashSet<usize> = HashSet::new();
        let mut deleted_before: HashSet<usize> = HashSet::new();
        let mut new_idx = 0usize;
        let new_len = new.lines().count();
        for change in diff.iter_all_changes() {
            let n = change.value().lines().count();
            match change.tag() {
                ChangeTag::Equal => new_idx += n,
                ChangeTag::Insert => {
                    for i in 0..n {
                        changed.insert(new_idx + i);
                    }
                    new_idx += n;
                }
                ChangeTag::Delete => {
                    // The line at `new_idx` (or the last line for an
                    // end-of-file deletion) follows this deletion block.
                    let mark = new_idx.min(new_len.saturating_sub(1));
                    if new_len > 0 {
                        deleted_before.insert(mark);
                    }
                }
            }
        }
        (changed, deleted_before)
    }

    /// Case table: (old, new, exact +N/-M) covering every shape the
    /// reload diff can take.
    const DIFF_CASES: &[(&str, &str, (usize, usize))] = &[
        // 挿入のみ (mid-file): two lines spliced after old line 2.
        ("one\ntwo\nthree\nfour\n", "one\ntwo\nX\nY\nthree\nfour\n", (2, 0)),
        // 挿入のみ (start of file).
        ("one\ntwo\nthree\n", "X\none\ntwo\nthree\n", (1, 0)),
        // 削除のみ (mid-file): drop five,six,seven.
        (
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
            "one\ntwo\nthree\nfour\neight\nnine\nten\n",
            (0, 3),
        ),
        // 削除のみ (EOF).
        ("one\ntwo\nthree\nfour\n", "one\ntwo\n", (0, 2)),
        // 削除のみ (start of file).
        ("one\ntwo\nthree\n", "three\n", (0, 2)),
        // 削除のみ (whole file).
        ("one\ntwo\nthree\n", "", (0, 3)),
        // 変更 (delete+insert 隣接): one old line → two new lines.
        ("one\ntwo\nthree\nfour\nfive\n", "one\nX\nY\nthree\nfour\nfive\n", (2, 1)),
        // 変更: two old lines → one new line.
        ("one\ntwo\nthree\nfour\n", "one\nX\nfour\n", (1, 2)),
        // 複数離れた変更: a replacement, a deletion, an append — the
        // append merges into the last hunk (no equal lines between).
        ("a\nb\nc\nd\ne\nf\ng\n", "A\nb\nC\nD\ne\nf\nG\nH\n", (5, 4)),
        // 末尾改行なし (no trailing newline on either side).
        ("a\nb", "a\nc", (1, 1)),
        // 同一内容: no hunks at all.
        ("a\nb\n", "a\nb\n", (0, 0)),
    ];

    #[test]
    fn synthesize_diff_matches_the_legacy_reload_scan() {
        // The important property (diff-scope step 1): the synthesized
        // diff's mark sets must equal what the old direct TextDiff walk
        // produced, case by case — marks are displayed behavior, so a
        // mismatch would show up as changed gutters.
        for (old, new, _) in DIFF_CASES {
            let diff = synthesize_diff(old, new);
            let new_len = new.lines().count();
            let (want_added, want_deleted) = legacy_reload_scan(old, new);
            assert_eq!(
                diff.added(new_len),
                want_added,
                "added() diverges from the legacy scan — old:\n{old}new:\n{new}"
            );
            assert_eq!(
                diff.deleted_before(new_len),
                want_deleted,
                "deleted_before() diverges from the legacy scan — old:\n{old}new:\n{new}"
            );
        }
    }

    #[test]
    fn added_modified_splits_pure_additions_from_rewrites() {
        // 純追加のみ: two appended lines.
        let d = synthesize_diff("a\nb\n", "a\nb\nX\nY\n");
        let (added, modified) = d.added_modified(4);
        assert_eq!(added, HashSet::from([2, 3]));
        assert!(modified.is_empty(), "no deletes, no rewrites");
        // 書き換えのみ: delete 1 + add 1.
        let d = synthesize_diff("a\nb\nc\n", "a\nX\nc\n");
        let (added, modified) = d.added_modified(3);
        assert!(added.is_empty());
        assert_eq!(modified, HashSet::from([1]));
        // 混在 hunk: delete 1 + add 3 — the first min(1,3) add is the
        // rewrite, the rest are pure additions.
        let d = synthesize_diff("a\nb\nc\n", "a\nX\nY\nZ\nc\n");
        let (added, modified) = d.added_modified(5);
        assert_eq!(modified, HashSet::from([1]));
        assert_eq!(added, HashSet::from([2, 3]));
        // 複数の離れた書き換え: 2 hunks, one rewrite each.
        let d = synthesize_diff("a\nb\nc\nd\ne\nf\n", "a\nX\nc\nd\nY\nf\n");
        let (added, modified) = d.added_modified(6);
        assert!(added.is_empty());
        assert_eq!(modified, HashSet::from([1, 4]));
        // `added()` stays the union (caller compatibility).
        let d = synthesize_diff("a\nb\nc\n", "a\nX\nY\nZ\nc\n");
        let (added, modified) = d.added_modified(5);
        let union: HashSet<usize> = added.union(&modified).copied().collect();
        assert_eq!(d.added(5), union);
        // An untracked diff: every line is a pure addition.
        let d = Diff { untracked: true, ..Default::default() };
        let (added, modified) = d.added_modified(3);
        assert_eq!(added, HashSet::from([0, 1, 2]));
        assert!(modified.is_empty());
    }

    #[test]
    fn added_modified_works_on_real_git_diffs() {
        // The same split must hold for a parsed real git diff: a
        // rewritten line plus appended lines (separate -U0 hunks).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three"]);
        std::fs::write(&path, "one\nCHANGED\nthree\nfour\nfive\n").unwrap();
        let diff = Diff::load("HEAD", &path).expect("inside a git repo");
        assert_eq!(diff.hunks.len(), 2, "-U0 splits the rewrite from the append");
        let (added, modified) = diff.added_modified(5);
        assert_eq!(modified, HashSet::from([1]), "the rewritten line");
        assert_eq!(added, HashSet::from([3, 4]), "the appended lines");
    }

    #[test]
    fn u0_normalization_keeps_the_mark_positions() {
        // The -U0 switch changes the hunk STRUCTURE (nearby changes
        // split apart), but the marks — added/modified/deleted_before —
        // must land on exactly the same lines as the pre-U0 3-context
        // snapshots: the marks are what the gutters paint, so they are
        // the behavior contract. The old shape is reproduced by parsing
        // git's explicit `-U3` output.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &["one", "two", "three", "four", "five", "six", "seven", "eight"],
        );
        // two rewritten, four deleted, seven rewritten, NINE appended.
        std::fs::write(&path, "one\nCHANGED\nthree\nfive\nsix\nSEVEN\neight\nNINE\n").unwrap();
        let new_len = 8;
        let abs = absolutize(&path);
        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["diff", "-U3", "HEAD", "--"])
            .arg(&abs)
            .output()
            .unwrap();
        assert!(out.status.success(), "git -U3 runs");
        let old = parse_diff(&String::from_utf8_lossy(&out.stdout));
        assert!(old.hunks.len() < 4, "3 context fuses the changes: {}", old.hunks.len());
        let new = Diff::load("HEAD", &path).expect("inside the repo");
        assert!(new.hunks.len() > old.hunks.len(), "-U0 splits them apart");
        assert_eq!(new.added(new_len), old.added(new_len), "added marks unchanged");
        assert_eq!(
            new.deleted_before(new_len),
            old.deleted_before(new_len),
            "deletion marks unchanged"
        );
        // The -U0 split also makes the add/rewrite classification EXACT:
        // with 3 context lines the fused hunk's min(D,A) rule swallowed
        // the appended line into `modified`; the separated hunks classify
        // it as a pure addition.
        let (na, nm) = new.added_modified(new_len);
        assert_eq!(na, HashSet::from([7]), "NINE is a pure addition");
        assert_eq!(nm, HashSet::from([1, 5]), "the two rewrites");
    }

    #[test]
    fn u0_pure_deletion_hunks_normalize_to_the_internal_convention() {
        // git's raw -U0 output writes a pure-deletion hunk's new_start as
        // the bare 0-based position; the internal convention (what
        // owner/deleted_before/hunk_at expect) is position+1, with 0 for
        // a start-of-file deletion. The normalization in load must land
        // the deletion marks on the same lines as before.
        let dir = tempfile::tempdir().unwrap();
        // Mid-file deletion: four dropped from an 8-line file.
        let path = init_repo(
            dir.path(),
            &["one", "two", "three", "four", "five", "six", "seven", "eight"],
        );
        std::fs::write(&path, "one\ntwo\nthree\nfive\nsix\nseven\neight\n").unwrap();
        let diff = Diff::load("HEAD", &path).expect("inside the repo");
        assert_eq!(diff.hunks.len(), 1);
        let h = &diff.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (4, 1, 4, 0));
        assert_eq!(h.owner(7), 3, "the following line (0-based)");
        assert_eq!(diff.deleted_before(7), HashSet::from([3]));
        // Start-of-file deletion: git's `+0,0` is already internal-form.
        std::fs::write(&path, "two\nthree\nfour\nfive\nsix\nseven\neight\n").unwrap();
        let diff = Diff::load("HEAD", &path).expect("inside the repo");
        assert_eq!((diff.hunks[0].new_start, diff.hunks[0].new_len), (0, 0));
        assert_eq!(diff.deleted_before(7), HashSet::from([0]));
        // EOF deletion: the mark clamps to the last new line.
        std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
        let diff = Diff::load("HEAD", &path).expect("inside the repo");
        assert_eq!((diff.hunks[0].new_start, diff.hunks[0].new_len), (7, 0));
        assert_eq!(diff.deleted_before(6), HashSet::from([5]), "clamped to the last line");
    }

    #[test]
    fn u0_splits_nearby_changes_into_separate_hunks() {
        // With the default 3 context lines, two changes separated by a
        // single unchanged line fuse into one giant hunk; -U0 keeps them
        // apart, so a run of adjacent mark rows is always exactly one
        // hunk (the hunk boundaries read off the mark gaps).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &["one", "two", "three", "four", "five", "six", "seven"],
        );
        // two rewritten, four rewritten — one unchanged line (three)
        // between them.
        std::fs::write(&path, "one\nCHANGED\nthree\nFOUR\nfive\nsix\nseven\n").unwrap();
        let diff = Diff::load("HEAD", &path).expect("inside the repo");
        assert_eq!(diff.hunks.len(), 2, "one unchanged line keeps them apart");
        assert_eq!(diff.hunks[0].new_range(), Some((1, 1)));
        assert_eq!(diff.hunks[1].new_range(), Some((3, 3)));
    }

    #[test]
    fn synthesize_diff_hunks_are_sorted_by_position() {
        // similar's Myers splits a same-position insert/delete pair
        // across an equal line and can emit the insert first — the old
        // [line1, line2, line3] → [line1, line3, X] case yields the
        // insertion at new 1 before the deletion at new 2. Real git
        // diffs are always in file order, and navigation (step ②) walks
        // hunks in order, so the synthesized hunks must be position-
        // sorted: new position, old position as tie-break.
        let d = synthesize_diff("line1\nline2\nline3\n", "line1\nline3\nX\n");
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[0].counts(), (0, 1), "the insertion at new 1");
        assert_eq!(d.hunks[1].counts(), (1, 0), "the deletion at new 2");
        assert!(d.hunks[0].new_start <= d.hunks[1].new_start);
        // Every diff-shape case keeps the (new, old) position order.
        for (old, new, _) in DIFF_CASES {
            let d = synthesize_diff(old, new);
            let mut prev = (0u32, 0u32);
            for h in &d.hunks {
                let key = (h.new_start, h.old_start);
                assert!(key >= prev, "hunks out of order — old:\n{old}new:\n{new}");
                prev = key;
            }
        }
    }

    #[test]
    fn synthesize_diff_counts_are_exact() {
        // The +N/-M badge is the sum of the hunks' counts() — it must
        // equal the actual added/deleted line counts of each case.
        for (old, new, want) in DIFF_CASES {
            let diff = synthesize_diff(old, new);
            let got = diff.hunks.iter().fold((0, 0), |(a, d), h| {
                let (x, y) = h.counts();
                (a + x, d + y)
            });
            assert_eq!(got, *want, "old:\n{old}new:\n{new}");
        }
    }

    #[test]
    fn synthesize_diff_emits_conventional_hunk_headers() {
        // Pure insertion: git's own zero-length-side convention
        // (`@@ -2,0 +3,2 @@`), body all adds.
        let d = synthesize_diff("one\ntwo\nthree\nfour\n", "one\ntwo\nX\nY\nthree\nfour\n");
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (2, 0, 3, 2));
        assert_eq!(h.body.iter().map(|l| l.tag).collect::<Vec<_>>(), vec![Tag::Add, Tag::Add]);
        assert_eq!(h.body[0].text, "X");
        // Pure deletion mid-file: new_start is the 1-based position of
        // the deleted block (the form owner/deleted_before need with
        // zero context — git's bare `+4,0` assumes its 3 context lines).
        let d = synthesize_diff(
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
            "one\ntwo\nthree\nfour\neight\nnine\nten\n",
        );
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (5, 3, 5, 0));
        assert_eq!(h.owner(7), 4, "the line following the deleted block");
        assert_eq!(h.anchor(7), 4, "a jump lands on that line");
        assert_eq!(d.hunk_at(4, 7), Some(0));
        assert_eq!(d.hunk_at(3, 7), None, "the line before is not on the hunk");
        assert_eq!(h.old_lines(), vec!["five", "six", "seven"]);
        // Start-of-file deletion keeps git's `+0,0`.
        let d = synthesize_diff("one\ntwo\nthree\n", "three\n");
        assert_eq!((d.hunks[0].old_start, d.hunks[0].new_start), (1, 0));
        // Modification: both sides 1-based, deletes before adds.
        let d = synthesize_diff("one\ntwo\nthree\nfour\nfive\n", "one\nX\nY\nthree\nfour\nfive\n");
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (2, 1, 2, 2));
        let tags: Vec<Tag> = h.body.iter().map(|l| l.tag).collect();
        assert_eq!(tags, vec![Tag::Delete, Tag::Add, Tag::Add]);
        assert!(!d.untracked, "a synthesized diff is never untracked");
    }

    #[test]
    fn load_returns_none_when_head_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "x\n").unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git").arg("-C").arg(dir.path()).args(args).status().unwrap();
            assert!(status.success());
        };
        let status = Command::new("git").arg("init").arg("-q").arg(dir.path()).status().unwrap();
        assert!(status.success());
        git(&["add", "."]);
        // No commit yet: HEAD does not resolve.
        assert!(Diff::load("HEAD", &path).is_none());
    }
}
