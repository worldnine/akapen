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
use std::path::Path;
use std::process::Command;

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
        let mut cmd = Command::new("git");
        cmd.arg("diff").arg(diff_ref).arg("--").arg(path);
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut diff = parse_diff(&stdout);
        // An untracked file has no HEAD side: `git diff` prints nothing for
        // it. `ls-files --error-unmatch` distinguishes that from a tracked
        // file without changes.
        let mut ls = Command::new("git");
        ls.args(["ls-files", "--error-unmatch", "--"]).arg(path);
        if let Some(dir) = dir {
            ls.current_dir(dir);
        }
        diff.untracked = !ls.output().map(|o| o.status.success()).unwrap_or(false);
        Some(diff)
    }

    /// 0-based new-file indices of the added/changed lines (every `+`
    /// line — a modified line is a delete+add pair, so the add side is the
    /// changed line in the new file). An untracked file marks every line.
    pub fn added(&self, new_len: usize) -> HashSet<usize> {
        if self.untracked {
            return (0..new_len).collect();
        }
        let mut out = HashSet::new();
        for h in &self.hunks {
            // `+0,0` (a whole-file deletion) has new_start == 0.
            let mut new_idx = h.new_start.saturating_sub(1) as usize;
            for l in &h.body {
                match l.tag {
                    Tag::Add => {
                        out.insert(new_idx);
                        new_idx += 1;
                    }
                    Tag::Delete => {}
                    Tag::Context => new_idx += 1,
                }
            }
        }
        out
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
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].old_lines(), vec!["one", "two", "three", "four"]);
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
        assert_eq!(diff.hunks[0].old_lines(), vec!["one", "two"], "HEAD~1 has no `three`");
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
