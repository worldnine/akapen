//! Git-diff navigation and the old-side display: the changes list
//! entries ([`ChangesEntry`]), old-side substitution and toggling
//! (`o`), hunk jumping (F7 / `]c` / `n`), and the view gutter's
//! changed/deleted flags.

use std::collections::HashSet;

use ratatui::style::Style;

use crate::app::{App, DiffScope, Mode, OldSide};
use crate::comment::Selection;
use crate::git;
use crate::highlight::{Span as HiSpan, syntax_for};
use crate::source::Source;
use crate::replace_view_preserving_cursor;

/// Which diff a hunk reference points into (diff-scope step ②).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DiffSource {
    /// The last reload's synthesized diff.
    Last,
    /// The git snapshot vs `git_ref`.
    Git,
}

/// A hunk under the active scope: the diff it lives in, its index, and
/// its precomputed anchor (the new-file line the navigation compares
/// against — the first changed line, or the following line for a pure
/// deletion).
#[derive(Clone, Copy, Debug)]
pub(crate) struct HunkRef {
    pub(crate) source: DiffSource,
    pub(crate) index: usize,
    pub(crate) anchor: usize,
}

/// One selectable entry of the changes tab: a hunk of a file (under the
/// active scope), or an untracked file (every line is a change, no
/// hunks exist — git-side only).
#[derive(Clone, Copy, Debug)]
pub(crate) enum ChangesEntry {
    Hunk {
        file: usize,
        source: DiffSource,
        hunk: usize,
    },
    Untracked { file: usize },
}

impl ChangesEntry {
    pub(crate) fn file(&self) -> usize {
        match self {
            ChangesEntry::Hunk { file, .. } | ChangesEntry::Untracked { file } => *file,
        }
    }
}

/// The diff of `file` for a hunk source (the current file's diffs live
/// in the live App fields; the others in their FileState slots).
pub(crate) fn entry_diff(
    app: &App,
    file: usize,
    source: DiffSource,
) -> Option<&git::Diff> {
    match source {
        DiffSource::Last => last_diff_for(app, file),
        DiffSource::Git => git_diff_for(app, file),
    }
}

/// The git snapshot diff for `file`.
pub(crate) fn git_diff_for(app: &App, file: usize) -> Option<&git::Diff> {
    if file == app.current_file_index {
        app.git_diff.as_ref()
    } else {
        app.file_states.get(file).and_then(|fs| fs.git_diff.as_ref())
    }
}

/// The last-reload diff for `file`.
pub(crate) fn last_diff_for(app: &App, file: usize) -> Option<&git::Diff> {
    if file == app.current_file_index {
        app.last_diff.as_ref()
    } else {
        app.file_states.get(file).and_then(|fs| fs.last_diff.as_ref())
    }
}

/// The loaded line count of `file` (current file: the live source).
pub(crate) fn file_len(app: &App, file: usize) -> usize {
    if file == app.current_file_index {
        app.source.len()
    } else {
        app.file_states.get(file).map_or(0, |fs| fs.source.len())
    }
}

/// The hunks the active scope makes navigable, in anchor order
/// (diff-scope step ②): Last → the reload diff, Git → the git snapshot,
/// Both → both merged (a shared anchor resolves ONCE — last wins, so a
/// change visible in both diffs cannot stop navigation twice), Off →
/// none. The single resolution the change navigation and the changes
/// tab share, so "what is marked" and "what is reachable" always agree
/// with [`scoped_mark_sets`].
pub(crate) fn scoped_hunk_refs(app: &App) -> Vec<HunkRef> {
    scoped_hunk_refs_for(app, app.current_file_index)
}

/// [`scoped_hunk_refs`] for a specific file — the changes tab walks
/// every file's FileState this way.
pub(crate) fn scoped_hunk_refs_for(app: &App, file: usize) -> Vec<HunkRef> {
    let new_len = file_len(app, file);
    match app.scope {
        DiffScope::Last => last_diff_for(app, file)
            .map(|d| hunk_refs(DiffSource::Last, d, new_len))
            .unwrap_or_default(),
        DiffScope::Git => git_diff_for(app, file)
            .map(|d| hunk_refs(DiffSource::Git, d, new_len))
            .unwrap_or_default(),
        DiffScope::Both => {
            let mut out = last_diff_for(app, file)
                .map(|d| hunk_refs(DiffSource::Last, d, new_len))
                .unwrap_or_default();
            // The git side joins in anchor order, skipping anchors the
            // reload diff already covers (last wins).
            let mut seen: HashSet<usize> = out.iter().map(|r| r.anchor).collect();
            if let Some(d) = git_diff_for(app, file) {
                for (i, h) in d.hunks.iter().enumerate() {
                    let a = h.anchor(new_len);
                    if seen.insert(a) {
                        out.push(HunkRef {
                            source: DiffSource::Git,
                            index: i,
                            anchor: a,
                        });
                    }
                }
            }
            out.sort_by_key(|r| r.anchor);
            out
        }
        DiffScope::Off => Vec::new(),
    }
}

fn hunk_refs(source: DiffSource, diff: &git::Diff, new_len: usize) -> Vec<HunkRef> {
    diff.hunks
        .iter()
        .enumerate()
        .map(|(i, h)| HunkRef {
            source,
            index: i,
            anchor: h.anchor(new_len),
        })
        .collect()
}

/// Resolve a [`HunkRef`] to the hunk itself.
pub(crate) fn hunk_ref(app: &App, r: HunkRef) -> Option<&git::Hunk> {
    match r.source {
        DiffSource::Last => app.last_diff.as_ref()?.hunks.get(r.index),
        DiffSource::Git => app.git_diff.as_ref()?.hunks.get(r.index),
    }
}

/// The changes tab's entries: the active scope's hunks across every
/// session file (Last: reload diffs only — files never reloaded have no
/// entries; Git: the git hunks plus untracked files; Both: both merged
/// per file, untracked row from the git side; Off: none), in file order.
pub(crate) fn changes_entries(app: &App) -> Vec<ChangesEntry> {
    if app.scope == DiffScope::Off {
        return Vec::new();
    }
    let mut out = Vec::new();
    for fi in 0..app.files.len() {
        for r in scoped_hunk_refs_for(app, fi) {
            out.push(ChangesEntry::Hunk {
                file: fi,
                source: r.source,
                hunk: r.index,
            });
        }
        if matches!(app.scope, DiffScope::Git | DiffScope::Both)
            && git_diff_for(app, fi).is_some_and(|d| d.untracked)
            && file_len(app, fi) > 0
        {
            out.push(ChangesEntry::Untracked { file: fi });
        }
    }
    out
}

/// The changes tab's display rows: per file a group header, then one row
/// per entry — the same shape as the comments tab's rows.
pub(crate) fn changes_rows(app: &App) -> Vec<Option<usize>> {
    let entries = changes_entries(app);
    let mut rows = Vec::new();
    let mut last: Option<usize> = None;
    for (pos, e) in entries.iter().enumerate() {
        if last != Some(e.file()) {
            last = Some(e.file());
            rows.push(None); // group header
        }
        rows.push(Some(pos));
    }
    rows
}

/// Build the substituted document for the old-side view: the hunk's new
/// lines are replaced by the old lines, each old line blank-separated so
/// it keeps its own row (markdown would otherwise merge consecutive
/// lines into one paragraph — the old side is a diff-like view) — except
/// consecutive table rows (`|`-leading), which stay adjacent or the
/// table breaks. Returns the substituted [`Source`] and the block's
/// geometry: `a` = the substituted index of the block's first line,
/// `span` = the replaced new-line count (0 for a pure deletion, which
/// inserts before `owner`), `block_len` = the block's line count.
pub(crate) fn substitute_old_side(app: &App, os: &OldSide) -> (Source, usize, usize, usize) {
    let (a, b) = match os.range {
        Some((a, b)) => (a, b),
        None => (os.owner, os.owner),
    };
    let span = os.range.map_or(0, |(a, b)| b - a + 1);
    let mut block: Vec<String> = Vec::with_capacity(os.old_lines.len() * 2);
    for (i, line) in os.old_lines.iter().enumerate() {
        if i > 0 {
            let prev = &os.old_lines[i - 1];
            let table_row = prev.starts_with('|') && line.starts_with('|');
            // An indented line continues the previous block (a list
            // item's wrapped continuation, a code line) — separating it
            // would break a bullet into orphan fragments.
            let continuation = line.starts_with(' ') || line.starts_with('\t');
            if !table_row && !continuation {
                block.push(String::new());
            }
        }
        block.push(line.clone());
    }
    let block_len = block.len();
    let mut lines: Vec<String> = Vec::with_capacity(app.source.len() + block_len);
    lines.extend(app.source.lines[..a].iter().cloned());
    lines.extend(block);
    lines.extend(app.source.lines[b + 1..].iter().cloned());
    let content = lines.join("\n") + "\n";
    let source = Source {
        path: app.current_file_path().to_path_buf(),
        content,
        gutter_width: lines.len().to_string().len().max(1),
        lines,
    };
    (source, a, span, block_len)
}

/// Map a substituted-source line index back to the NEW-file line index
/// (the substitution replaced new lines `[a, a+span)` with a block of
/// `block_len` lines that all belong to new-file line `a`).
pub(crate) fn remapped_line(i: usize, a: usize, span: usize, block_len: usize) -> usize {
    if i < a {
        i
    } else if i < a + block_len {
        a
    } else {
        i + span - block_len
    }
}

/// The substituted-source index for NEW-file line `j` (the inverse of
/// [`remapped_line`] on the non-block lines; the hunk's lines all point
/// at the block's first line).
pub(crate) fn sub_index(j: usize, a: usize, span: usize, block_len: usize) -> usize {
    if j < a {
        j
    } else if j < a + span {
        a
    } else {
        j + block_len - span
    }
}

/// Rebuild the source-mode layout and the rendered view after the `o`
/// toggle (3-2): `base_rows` folds the block in (navigation math),
/// `line_rows` re-attaches the bars onto the block, and the view splices
/// the block's rows in place of the hunk. On and off converge on the same
/// path, so toggling never leaves stale layout.
pub(crate) fn apply_old_side_state(app: &mut App) {
    app.rebuild_base_rows();
    app.refresh_line_rows();
    replace_view_preserving_cursor(app);
    app.keep_cursor_visible(app.source_viewport_rows() as u16);
    app.offset = app.offset.min(app.max_offset(app.source_viewport_rows() as u16));
}

/// `o`: toggle the hunk under the cursor between old and new display
/// (3-2). The old-side base follows the scope (spec 3-4): Last compares
/// against the last loaded content (the synthesized reload diff), Git /
/// Both against `git_ref` (HEAD today; the loader takes the ref as an
/// argument, so a future generation shift only changes what is passed
/// there). Off disables the toggle.
pub(crate) fn toggle_old_side(app: &mut App) {
    let new_len = app.source.len();
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    let (base, base_label) = match app.scope {
        DiffScope::Off => {
            app.flash_err("marks off — m: cycle scopes");
            return;
        }
        DiffScope::Last => (app.last_diff.clone(), "last load"),
        DiffScope::Git | DiffScope::Both => (app.git_diff.clone(), "HEAD"),
    };
    let Some(diff) = base else {
        match app.scope {
            DiffScope::Last => {
                app.flash_err("no reload diff yet — r reloads");
                return;
            }
            _ => {
                app.flash_err("not in a git repository — o disabled");
                return;
            }
        }
    };
    // The untracked guard applies to the git-based scopes only: the
    // reload diff is usable for an untracked file (Last has no ref side
    // to miss).
    if app.scope != DiffScope::Last && diff.untracked {
        app.flash_err("untracked file — nothing to compare against HEAD");
        return;
    }
    // A second `o` with the cursor still on the toggled hunk returns to
    // the new side; with the cursor elsewhere it toggles that hunk.
    if let Some(os) = &app.old_side
        && diff.hunk_at(line, new_len) == Some(os.hunk)
    {
        app.old_side = None;
        apply_old_side_state(app);
        app.flash("new side");
        return;
    }
    let Some(h) = diff.hunk_at(line, new_len) else {
        app.flash_err("no git changes on this line — F7: next change");
        return;
    };
    let hunk = &diff.hunks[h];
    let old_lines = hunk.old_lines();
    let range = hunk.new_range();
    let owner = range.map_or_else(|| hunk.owner(new_len), |(a, _)| a);
    let old_numbers: Vec<u32> = (hunk.old_start..).take(old_lines.len()).collect();
    // Tokenize the old block as one unit so cross-line constructs (code
    // fences, block comments) keep their context; pad when a trailing
    // empty line collapses in the join.
    let old_content = old_lines.join("\n");
    let mut old_spans = app
        .highlight
        .highlight_with(&old_content, syntax_for(app.current_file_path()));
    while old_spans.len() < old_lines.len() {
        old_spans.push(vec![HiSpan {
            text: String::new(),
            style: Style::default(),
        }]);
    }
    app.old_side = Some(OldSide {
        hunk: h,
        old_lines,
        old_spans,
        row_counts: Vec::new(),
        range,
        owner,
        old_numbers,
    });
    apply_old_side_state(app);
    let where_ = match range {
        Some((a, b)) => format!("L{}-{}", a + 1, b + 1),
        None => format!("before L{}", owner + 1),
    };
    app.flash(format!("old side vs {base_label} · {where_}"));
}

/// 1-based location label for a hunk: `L5-9`, or `before L5` for a pure
/// deletion (mirrors the `o` toggle's message).
pub(crate) fn hunk_label(hunk: &git::Hunk, new_len: usize) -> String {
    match hunk.new_range() {
        Some((a, b)) => format!("L{}-{}", a + 1, b + 1),
        None => format!("before L{}", hunk.owner(new_len) + 1),
    }
}

/// Land the cursor on hunk `h` of the current file's diff, with a
/// selection covering the hunk. Shared by F7 / `]c` jumping and the
/// changes tab's Enter. The cursor sits on the hunk's first CHANGED line
/// (`anchor` — the review position), while the selection covers the
/// whole hunk range (the owner line for a pure deletion).
pub(crate) fn land_on_hunk(app: &mut App, h: usize) {
    let Some(r) = scoped_hunk_refs(app).get(h).copied() else { return };
    let Some(hunk) = hunk_ref(app, r) else { return };
    let new_len = app.source.len();
    let (start, end) = match hunk.new_range() {
        Some((a, b)) => (a, b),
        None => {
            let o = hunk.owner(new_len);
            (o, o)
        }
    };
    let line = hunk.anchor(new_len);
    app.selection = Some(Selection {
        anchor: start,
        cursor: end,
    });
    if app.mode == Mode::View {
        app.view.goto_source_line(line);
        app.view.keep_cursor_visible(app.view_viewport_rows());
    } else {
        app.cursor = line;
        app.keep_cursor_visible(app.source_viewport_rows() as u16);
    }
}

/// Jump to the next (`dir > 0`) or previous change hunk: `F7` /
/// `Shift+F7`, or the `]c` / `[c` chord. From a line outside any hunk
/// the jump lands on the nearest hunk in `dir`; at the ends it flashes
/// instead of wrapping (like `n`/`N`).
pub(crate) fn jump_hunk(app: &mut App, dir: isize) {
    let refs = scoped_hunk_refs(app);
    if refs.is_empty() {
        // Scope-specific explanations; the git-based scopes keep the
        // pre-scope wording.
        let msg = match app.scope {
            DiffScope::Off => "marks off — m: cycle scopes",
            DiffScope::Last => "no reload diff yet — r reloads",
            DiffScope::Git if app.git_diff.as_ref().is_some_and(|d| d.untracked) => {
                "untracked file — nothing to compare"
            }
            DiffScope::Git => "no git changes",
            DiffScope::Both => "no changes",
        };
        app.flash_err(msg);
        return;
    }
    let n = refs.len();
    let new_len = app.source.len();
    let line = if app.mode == Mode::View {
        app.view.cursor
    } else {
        app.cursor
    };
    // Stepping compares ANCHORS (each hunk's first changed line — or,
    // for a pure deletion, its owner line): the next hunk is the one
    // whose first change lies beyond the cursor. A cursor on a hunk's
    // leading context lines still lands on THAT hunk; a cursor on its
    // trailing lines (or on the owner line of a deletion-only hunk,
    // where the jump itself lands) steps past it. The old "is the
    // cursor on a changed line" check rewound to the first hunk there.
    let target = if dir > 0 {
        refs.iter().position(|r| r.anchor > line)
    } else {
        refs.iter().rposition(|r| r.anchor < line)
    };
    let Some(target) = target else {
        app.flash_err(if dir > 0 { "no changes below" } else { "no changes above" });
        return;
    };
    let hunk = hunk_ref(app, refs[target]).expect("refs resolve");
    let label = hunk_label(hunk, new_len);
    app.flash(format!("change {}/{} · {label}", target + 1, n));
    land_on_hunk(app, target);
}

/// The mark sets the active scope selects (diff-scope step ①) — the
/// single place that turns the scope into concrete per-line mark sets,
/// read by both the view gutter and the source gutter so they can never
/// disagree. Returns `(added, deleted_before)` as 0-based new-file
/// indices.
/// An untracked file contributes NO marks under the Git scope (git's
/// all-lines-added mark is noise); under Both only the reload marks show.
pub(crate) fn scoped_mark_sets(app: &App) -> (HashSet<usize>, HashSet<usize>) {
    let untracked = app.git_diff.as_ref().is_some_and(|d| d.untracked);
    match app.scope {
        DiffScope::Last => (app.last_added.clone(), app.last_deleted_before.clone()),
        DiffScope::Git if untracked => (HashSet::new(), HashSet::new()),
        DiffScope::Git => (app.git_added.clone(), app.git_deleted_before.clone()),
        DiffScope::Both => {
            let mut added = app.last_added.clone();
            let mut deleted = app.last_deleted_before.clone();
            if !untracked {
                added.extend(app.git_added.iter().copied());
                deleted.extend(app.git_deleted_before.iter().copied());
            }
            (added, deleted)
        }
        DiffScope::Off => (HashSet::new(), HashSet::new()),
    }
}

/// Per-line flags: which source lines are in `marks` (the scoped mark
/// set — see [`scoped_mark_sets`]). The view gutter shows a green `▌`
/// for changed lines.
pub(crate) fn view_changed_flags(marks: &HashSet<usize>, n_lines: usize) -> Vec<bool> {
    let mut changed = vec![false; n_lines];
    for &i in marks {
        if i < n_lines {
            changed[i] = true;
        }
    }
    changed
}

/// Per-line flags: which source lines are in `marks` (the scoped mark
/// set — see [`scoped_mark_sets`]). The view gutter shows a red `▌` for
/// the lines following a deletion block.
pub(crate) fn view_deleted_flags(marks: &HashSet<usize>, n_lines: usize) -> Vec<bool> {
    let mut deleted = vec![false; n_lines];
    for &i in marks {
        if i < n_lines {
            deleted[i] = true;
        }
    }
    deleted
}

#[cfg(test)]
mod git_tests {
    use super::{DiffSource, scoped_hunk_refs};
    use crate::app::{App, DiffScope, FileState, Mode};
    use crate::comment::Selection;
    use crate::config::{Config, EscQuit};
    use crate::export;
    use crate::highlight::{Highlighter, syntax_for};
    use crate::overlay::{Overlay, OverlayTab};
    use crate::ime::ImeMode;
    use crate::reload::{git_snapshot, reload_source};
    use crate::source::Source;
    use crate::state_tests;
    use crate::view::ViewState;
    use crate::{
        activate_first_file, build_rows, draw, on_input_key, on_key, on_overlay_key,
        on_source_key, on_view_key,
    };
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::style::{Color, Style};
    use std::collections::HashSet;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// Init a git repo in `dir` and commit `doc.md` with `lines`.
    /// Returns the file path.
    fn init_repo(dir: &std::path::Path, lines: &[&str]) -> std::path::PathBuf {
        let status = Command::new("git").arg("init").arg("-q").arg(dir).status().unwrap();
        assert!(status.success(), "git init failed — is git installed?");
        let path = dir.join("doc.md");
        let content = lines.join("\n") + "\n";
        std::fs::write(&path, content).unwrap();
        git(dir, &["add", "."]);
        git(dir, &[
            "-c", "user.name=test", "-c", "user.email=test@test", "commit", "-q", "-m", "init",
        ]);
        path
    }

    fn overwrite(path: &std::path::Path, lines: &[&str]) {
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    /// An app over `path` with the git snapshot loaded, exactly like
    /// run() builds one.
    fn git_app(path: std::path::PathBuf, mode: Mode) -> App {
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(path).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = mode;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        let (diff, added, deleted) = git_snapshot(&app.git_ref, &app.files[0], app.source.len());
        app.git_diff = diff;
        app.git_added = added;
        app.git_deleted_before = deleted;
        // run() と同じ初期スコープ (diff-scope step ①): リポジトリ内は
        // Git、外は Last。
        app.scope = if app.git_diff.is_some() { DiffScope::Git } else { DiffScope::Last };
        app
    }

    /// The rendered source rows as plain strings (gutter included).
    fn source_rows(app: &App) -> Vec<String> {
        let (text, _) = build_rows(app, 100, 75);
        text.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn git_marks_render_in_the_source_gutter() {
        // 3-1: lines changed vs HEAD get the green `+` gutter (merged with
        // the reload-diff marks); lines after a deletion block the red `-`.
        // The Git scope (run()'s in-repo default, diff-scope step ①)
        // selects the git marks.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"]);
        overwrite(&path, &["one", "two", "three", "CHANGED", "five", "six", "seven", "eight", "nine", "ten"]);
        let mut app = git_app(path, Mode::Source);
        assert_eq!(app.scope, DiffScope::Git, "in-repo default");
        let rows = source_rows(&app);
        assert!(rows[3].starts_with("+ 4 "), "changed line gets the + mark: {}", rows[3]);
        assert!(rows[3].contains("CHANGED"));
        assert!(rows[0].starts_with("> 1 "), "the cursor row keeps the > mark");
        assert!(!rows[9].starts_with("+"), "unchanged lines stay clean");
        // Both merges the reload-diff marks with the git marks: a line
        // the session itself changed is marked too.
        app.scope = DiffScope::Both;
        app.last_added.insert(8);
        let rows = source_rows(&app);
        assert!(rows[8].starts_with("+ 9 "), "reload mark joins the git mark");
    }

    #[test]
    fn c_on_the_old_side_comments_the_whole_hunk() {
        // Old side: the whole hunk IS the display — `c` with the cursor
        // inside that hunk comments the hunk (raw diff snippet), not a
        // single new-side line. Outside the hunk, `c` stays a line
        // comment.
        let lines: Vec<String> = (1..=20).map(|i| format!("line{i:02}")).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        let mut work = lines.clone();
        work[4] = "CHANGED-A".into();
        work[14] = "CHANGED-B".into();
        overwrite(&path, &work.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let mut app = git_app(path.clone(), Mode::View);
        app.view.goto_source_line(4);
        // o: hunk 0 を old 表示に。
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some());
        assert_eq!(app.old_side.as_ref().unwrap().hunk, 0);
        // その hunk 内で c (選択なし) → hunk 全体が対象。
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (1, 7), "the hunk range");
        assert!(
            app.composer_hunk.is_some(),
            "the snippet becomes the hunk's raw diff"
        );
        for ch in "note".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 1);
        assert!(app.comments[0].hunk, "flagged as a hunk comment");
        assert_eq!((app.comments[0].start, app.comments[0].end), (2, 8));
        // old 表示中でも hunk 外の行ではカーソル行コメント。
        app.view.goto_source_line(10);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (10, 10));
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // source モードでも同じ。
        let mut app = git_app(path, Mode::Source);
        app.cursor = 4;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some());
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (1, 7), "source old side: the hunk");
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    }
    #[test]
    fn o_toggles_the_hunk_to_old_side_in_source_mode() {
        // 3-2: `o` on a changed line replaces the hunk's new lines with
        // the HEAD content — old line numbers and a faint `~` mark; a
        // second `o` returns to the new side.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"]);
        overwrite(&path, &["one", "two", "three", "CHANGED", "five", "six", "seven", "eight", "nine", "ten"]);
        let mut app = git_app(path, Mode::Source);
        app.cursor = 3; // on the changed line
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let os = app.old_side.as_ref().expect("the hunk is toggled");
        // Change at line 4 with 3 context lines: the hunk is old 1-7.
        assert_eq!(os.old_lines, vec!["one", "two", "three", "four", "five", "six", "seven"]);
        assert_eq!(os.old_numbers, vec![1, 2, 3, 4, 5, 6, 7], "HEAD line numbers");
        assert_eq!(os.range, Some((0, 6)), "the hunk's new-line range");
        let rows = source_rows(&app);
        assert!(rows[0].starts_with("> 1 "), "the block opens with the cursor mark: {}", rows[0]);
        assert!(rows[3].starts_with("~ 4 "), "old line 4 with a faint mark: {}", rows[3]);
        assert!(rows[3].contains("four"), "the old content shows: {}", rows[3]);
        assert!(!rows.iter().any(|r| r.contains("CHANGED")), "the new content is replaced");
        assert!(rows[7].starts_with("  8 "), "the hunk's trailing context follows: {}", rows[7]);
        // A second `o` with the cursor still on the hunk returns to new.
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none(), "toggled back");
        let rows = source_rows(&app);
        assert!(rows[3].starts_with("> 4 "), "the new side is back (cursor row): {}", rows[3]);
        assert!(rows[3].contains("CHANGED"));
    }

    #[test]
    fn o_moves_with_the_cursor_and_flashes_on_unchanged_lines() {
        // A 20-line file so the hunk (change at line 10) has context
        // outside its range: the ends are untouched by the diff.
        let lines: Vec<String> = (1..=20).map(|i| format!("line{i:02}")).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let mut work = lines.clone();
        work[9] = "CHANGED".into();
        overwrite(&path, &work.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let mut app = git_app(path, Mode::Source);
        // Unchanged line: nothing to toggle.
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none());
        let (msg, _, is_error) = app.status.as_ref().expect("a flash explains");
        assert!(*is_error, "it is an error toast: {msg}");
        assert!(msg.contains("no git changes"), "message names the miss: {msg}");
        // Toggle the hunk, move the cursor OUT of it, and `o` there
        // flashes instead of untoggling — `o` always means the cursor's
        // hunk.
        app.cursor = 9;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some());
        app.cursor = 0;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some(), "cursor outside the hunk: no untoggle");
        assert!(app.status.is_some());
    }

    #[test]
    fn o_toggles_a_pure_deletion_hunk() {
        // A hunk whose new side is empty — the form git emits for a
        // whole-file deletion. The block renders before the following
        // line (the deletion position, marked red by 3-1). The diff is
        // injected directly: with default context git only produces
        // `+c,0` hunks for whole-file deletions, and an empty new file
        // has no source rows to render the block into.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["a", "b", "c", "d", "e"]);
        overwrite(&path, &["a", "e"]);
        let mut app = git_app(path, Mode::Source);
        app.git_diff = Some(crate::git::Diff {
            hunks: vec![crate::git::Hunk {
                old_start: 2,
                old_len: 3,
                new_start: 2,
                new_len: 0,
                body: vec![
                    crate::git::HunkLine { tag: crate::git::Tag::Delete, text: "b".into() },
                    crate::git::HunkLine { tag: crate::git::Tag::Delete, text: "c".into() },
                    crate::git::HunkLine { tag: crate::git::Tag::Delete, text: "d".into() },
                ],
            }],
            untracked: false,
        });
        app.git_deleted_before =
            app.git_diff.as_ref().unwrap().deleted_before(app.source.len());
        assert!(app.git_deleted_before.contains(&1), "the following line is marked");
        let rows = source_rows(&app);
        assert!(
            rows[1].starts_with("-2 e"),
            "deletion position mark on the following line: {}",
            rows[1]
        );
        app.cursor = 1; // the following line — the deletion position
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let os = app.old_side.as_ref().expect("the deletion hunk toggles");
        assert_eq!(os.range, None, "pure deletion has no new-line range");
        assert_eq!(os.old_lines, vec!["b", "c", "d"]);
        assert_eq!(os.old_numbers, vec![2, 3, 4], "the deleted lines' HEAD numbers");
        let rows = source_rows(&app);
        assert!(rows[1].starts_with(">2 b"), "the block opens with the cursor mark: {}", rows[1]);
        assert!(rows[2].contains("c") && rows[3].contains("d"), "the deleted lines show");
        // The owner row (cursor on it) shows the `>` marker; the red `-`
        // deletion mark was asserted above, before the toggle.
        assert!(rows[4].starts_with(">2 e"), "the following line keeps its place: {}", rows[4]);
    }

    #[test]
    fn o_works_in_view_mode_and_restores() {
        // 3-2 in the rendered view: the block is spliced in place of the
        // hunk's rows as raw dim lines with a `~` marker; the cursor's
        // extent spans the block; `o` restores the render exactly.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["# one", "# two", "# three", "# four", "# five", "# six", "# seven", "# eight", "# nine", "# ten"]);
        overwrite(&path, &["# one", "# two", "# three", "# CHANGED", "# five", "# six", "# seven", "# eight", "# nine", "# ten"]);
        let mut app = git_app(path, Mode::View);
        let before_len = app.view.rows.len();
        let before_starts = app.view.source_starts.clone();
        app.view.goto_source_line(3); // the changed heading
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let rows = &app.view.rows;
        let range = app.view.old_side_rows.clone();
        assert!(!range.is_empty(), "the block occupies rows");
        let block_text: String = rows[range.clone()]
            .iter()
            .flatten()
            .map(|s| s.text.as_str())
            .collect();
        assert!(
            block_text.contains("four"),
            "old content spliced in (rendered): {block_text}"
        );
        assert!(
            !block_text.contains("# four"),
            "rendered, not raw source: {block_text}"
        );
        assert!(!block_text.contains("CHANGED"), "new content replaced");
        assert_eq!(
            app.view.source_starts[3], range.start,
            "the hunk's lines map to the block's first row"
        );
        assert_eq!(
            app.view.old_side_lines,
            Some((0, 6)),
            "the hunk's new lines 1-7 (1-based)"
        );
        // The cursor (on a range line) sits on the block and spans it.
        assert_eq!(app.view.cursor_row(), range.start);
        assert_eq!(app.view.cursor_end_row(), range.end - 1);
        // The marker column shows `~` on the block and `>` on the cursor.
        let (_, gutter) = app.view.visible_text(
            100,
            &[],
            &[],
            &[],
            None,
            Color::Rgb(88, 91, 112),
            Style::default(),
        );
        assert_eq!(gutter[range.start].glyph, ">", "cursor on the block");
        assert_eq!(gutter[range.start + 1].glyph, "~", "old-side rows carry ~");
        assert_eq!(gutter[range.end].glyph, "│", "rows after the block keep the border");
        // Tab to source: the toggle is shared, the old lines show there.
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        let rows = source_rows(&app);
        assert!(rows[3].contains("four") && !rows[3].contains("CHANGED"));
        // Tab back and toggle off: the render is restored exactly.
        on_source_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::View);
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none());
        assert_eq!(app.view.rows.len(), before_len, "rows restored");
        assert_eq!(app.view.source_starts, before_starts, "mapping restored");
    }

    #[test]
    fn old_side_keeps_list_item_continuations_together() {
        // An old bullet split across INDENTED continuation lines must
        // stay one list item — blank-separating the continuations
        // rendered them as orphan fragments ("、"/"o で確認します）").
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &[
                "- **変更行マーク（3-1）**: 追加・変更行は再読込 diff と同じ緑 `+` ガター / `▌` マーカーで",
                "  表示（両者はマージされ、セッション内の変更と git の変更が同じ信号に統合）。",
                "  削除行は**位置に薄いマークのみ**（削除ブロック直後の行に赤 `-`。内容は出さず、",
                "  `o` で確認します）",
                "",
                "次の段落。",
            ],
        );
        overwrite(
            &path,
            &[
                "- **変更行マーク（3-1）**: 追加・変更行は再読込 diff と同じ緑 `+` ガター / `▌` マーカーで",
                "  表示（両者はマージされ、セッション内の変更と git の変更が同じ信号に統合）。",
                "  削除行は**位置に薄いマークのみ**（削除ブロック直後の行に赤 `-`。内容は出さず、",
                "  `o` で確認できます）",
                "",
                "次の段落。",
            ],
        );
        let mut app = git_app(path, Mode::View);
        app.view.goto_source_line(3); // the changed line (bullet's tail)
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let range = app.view.old_side_rows.clone();
        assert!(!range.is_empty());
        let rows: Vec<String> = app.view.rows[range]
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect();
        let start = rows
            .iter()
            .position(|r| r.contains("変更行マーク"))
            .expect("the bullet's first row");
        let end = rows
            .iter()
            .position(|r| r.contains("確認します"))
            .expect("the bullet's last row");
        assert!(
            start < end,
            "the bullet spans rows {start}..={end}: {rows:?}"
        );
        assert!(
            rows[start..=end].iter().all(|r| !r.trim().is_empty()),
            "no blank rows inside the bullet: {rows:?}"
        );
    }

    #[test]
    fn old_side_in_view_renders_tables_as_tables() {
        // A table in the old side must render as a table (box frames),
        // not raw pipe text — the blank-line join would break it (the
        // regression: the key table showed `| ? | キーリファレンス |`).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &[
                "| Key | 動作 |",
                "|---|---|",
                "| j / k | 移動 |",
                "| o | old side トグル |",
            ],
        );
        // A row added to the table: the hunk covers the table's end.
        overwrite(
            &path,
            &[
                "| Key | 動作 |",
                "|---|---|",
                "| j / k | 移動 |",
                "| o | old side トグル |",
                "| F7 | 次の変更へ |",
            ],
        );
        let mut app = git_app(path, Mode::View);
        app.view.goto_source_line(3);
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let range = app.view.old_side_rows.clone();
        assert!(!range.is_empty());
        let rows: Vec<String> = app.view.rows[range]
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect();
        assert!(
            rows.iter().any(|r| r.contains('┌') || r.contains('├')),
            "the table renders with box frames: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.trim_start().starts_with("| ")),
            "no raw pipe text: {rows:?}"
        );
        assert!(rows.iter().any(|r| r.contains("old side トグル")));
        // The USER's case: the hunk cuts the table mid-body (the header
        // and delimiter are outside it) — the body-only fragment must
        // still render as a table (the first row becomes the header).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &[
                "| Key | 動作 |",
                "|---|---|",
                "| ? | キーリファレンス |",
                "| r / i | 再読込 / 無視 |",
                "| e | 編集 |",
                "| y | コピー |",
            ],
        );
        overwrite(
            &path,
            &[
                "| Key | 動作 |",
                "|---|---|",
                "| ? | キーリファレンス |",
                "| r / i | 再読込 / 無視 |",
                "| o | old side トグル |",
                "| F7 | 次の変更へ |",
                "| e | 編集 |",
                "| y | コピー |",
            ],
        );
        let mut app = git_app(path, Mode::View);
        app.view.goto_source_line(3); // 挿入位置直前のコンテキスト行
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let range = app.view.old_side_rows.clone();
        assert!(!range.is_empty());
        let rows: Vec<String> = app.view.rows[range]
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect();
        assert!(
            rows.iter().any(|r| r.contains('┌') || r.contains('├')),
            "body-only fragment still renders as a table: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.trim_start().starts_with("| ")),
            "no raw pipe text: {rows:?}"
        );
        assert!(rows.iter().any(|r| r.contains("キーリファレンス")));
    }

    #[test]
    fn old_side_in_view_keeps_each_line_separate() {
        // The fragment render joins the old lines with blanks: consecutive
        // lines must NOT merge into one run-on paragraph (the regression:
        // three context lines rendered as a single merged row, hiding the
        // hunk's line structure).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["line one", "line two", "line three"]);
        overwrite(&path, &["line one CHANGED", "line two", "line three"]);
        let mut app = git_app(path, Mode::View);
        app.view.goto_source_line(0);
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let range = app.view.old_side_rows.clone();
        assert!(!range.is_empty());
        let rows: Vec<String> = app.view.rows[range]
            .iter()
            .map(|r| r.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect();
        assert!(rows.iter().any(|r| r.contains("line one")));
        assert!(rows.iter().any(|r| r.contains("line three")));
        assert!(
            !rows.iter().any(|r| r.contains("line one") && r.contains("line three")),
            "each old line stays its own row: {rows:?}"
        );
        assert!(!rows.iter().any(|r| r.contains("CHANGED")));
    }

    #[test]
    fn old_side_in_view_renders_markdown_not_raw_source() {
        // View mode's old side renders like the view itself: `**bold**`
        // and `# ` syntax is consumed, not shown raw (the fragment render
        // keeps the display closed within the hunk).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["# Title", "", "**old** text"]);
        overwrite(&path, &["# Title", "", "**new** text"]);
        let mut app = git_app(path, Mode::View);
        app.view.goto_source_line(2);
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some());
        let range = app.view.old_side_rows.clone();
        assert!(!range.is_empty());
        let block_text: String = app.view.rows[range]
            .iter()
            .flatten()
            .map(|s| s.text.as_str())
            .collect();
        assert!(block_text.contains("old"), "the old text shows: {block_text}");
        assert!(
            !block_text.contains("**"),
            "markdown syntax is consumed: {block_text}"
        );
        assert!(!block_text.contains("new"), "the new content is replaced");
        // Source mode keeps the RAW old lines (it is the raw pane).
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        let rows = source_rows(&app);
        assert!(
            rows.iter().any(|r| r.contains("**old**")),
            "source mode shows the raw old line"
        );
    }

    #[test]
    fn drawing_with_the_old_side_toggled_does_not_panic() {
        // Smoke: the full draw path (title, view/source panes, footer)
        // runs with the block spliced in, and the old content actually
        // reaches the terminal buffer in both modes.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &["# one", "# two", "# three", "# four", "# five", "# six", "# seven", "# eight", "# nine", "# ten"],
        );
        overwrite(
            &path,
            &["# one", "# two", "# three", "# CHANGED", "# five", "# six", "# seven", "# eight", "# nine", "# ten"],
        );
        let mut app = git_app(path, Mode::View);
        app.view.goto_source_line(3);
        on_view_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some());
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .filter(|&c| c != ' ')
                .collect()
        };
        let view = capture(&mut app);
        assert!(view.contains("four"), "old content rendered in the view: {view}");
        assert!(!view.contains("CHANGED"), "new content hidden: {view}");
        assert!(view.contains('~'), "the old-side marker is drawn");
        // Same pane in source mode: the block renders with old numbers.
        on_view_key(&mut app, KeyCode::Tab, KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Source);
        let source = capture(&mut app);
        assert!(source.contains("four"), "old content in source mode: {source}");
        assert!(!source.contains("CHANGED"));
        assert!(source.contains('~'));
    }

    #[test]
    fn o_without_git_is_a_no_op_with_a_flash() {
        // P1: outside a repository the `o` key changes nothing about the
        // existing behavior — it just explains why.
        let dir = tempfile::tempdir().unwrap(); // no repo
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut app = git_app(path, Mode::Source);
        assert!(app.git_diff.is_none(), "no git snapshot outside a repo");
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none());
        assert!(app.status.is_some(), "a flash explains the disabled key");
        assert_eq!(app.mode, Mode::Source, "nothing else changed");
    }

    #[test]
    fn untracked_files_disable_o_and_hide_git_marks() {
        // An untracked file has no HEAD side: the old-side toggle has
        // nothing to compare, and (diff-scope step ①) the Git scope shows
        // NO marks — git's all-lines-added mark is noise. Both shows only
        // the reload marks.
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path(), &["tracked"]);
        let untracked = dir.path().join("new.md");
        std::fs::write(&untracked, "fresh\nnew\n").unwrap();
        let mut app = git_app(untracked, Mode::Source);
        assert_eq!(app.git_added.len(), 2, "every line is new in the snapshot");
        assert_eq!(app.scope, DiffScope::Git, "untracked is still inside a repo");
        let rows = source_rows(&app);
        assert!(
            !rows[1].starts_with("+2 "),
            "Git scope hides the all-lines mark: {}",
            rows[1]
        );
        app.last_added.insert(1);
        app.scope = DiffScope::Both;
        let rows = source_rows(&app);
        assert!(rows[1].starts_with("+2 "), "Both shows the reload marks: {}", rows[1]);
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none());
        let (msg, _, _) = app.status.as_ref().expect("a flash explains");
        assert!(msg.contains("untracked"), "message names the case: {msg}");
    }

    #[test]
    fn c_on_a_hunk_line_comments_the_whole_hunk() {
        // The hunk-wide comment is a VISIBLE act: `n`/`F7` jumps select
        // the whole hunk (highlight band), and `c` on that selection
        // targets the hunk. A bare `c` (no selection) comments the exact
        // cursor line — a hunk's inside line included — so single-line
        // notes stay possible anywhere.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four", "five"]);
        overwrite(&path, &["one", "two", "CHANGED", "four", "five"]);
        let mut app = git_app(path.clone(), Mode::Source);
        // Bare c on the changed line: the exact line, not the hunk.
        app.cursor = 2; // the changed line, no selection
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (2, 2), "the cursor line");
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // Bare c on a CONTEXT line of the hunk: also the exact line.
        app.cursor = 0;
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (0, 0));
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // n (or F7) selects the whole hunk first — the target is visible
        // as the highlight band — and c then comments the HUNK.
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection { anchor: 0, cursor: 4 }));
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (0, 4), "the hunk range");
        for ch in "note".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.comments.len(), 1);
        // The comment carries the hunk's raw diff as its snippet: the
        // agent sees the change itself, deletions included.
        let c = &app.comments[0];
        assert_eq!((c.start, c.end), (1, 5), "the hunk range");
        assert!(c.hunk, "flagged as a hunk comment");
        assert!(
            c.lines.contains("@@ -1,5 +1,5 @@"),
            "the raw diff header: {}",
            c.lines
        );
        assert!(c.lines.contains("-three"), "deleted lines ride along: {}", c.lines);
        assert!(c.lines.contains("+CHANGED"), "added lines ride along: {}", c.lines);
        let out = export::format_all(&app.comments);
        assert!(out.contains("doc.md:1-5"), "the hunk range location: {out}");
        assert!(out.contains("@@ -1,5 +1,5 @@"), "exported as-is: {out}");
        assert!(!out.contains("1: @"), "no line numbering on the diff: {out}");
        // Reply mode blockquotes the raw diff the same way.
        let reply = export::format_all_reply(&app.comments);
        assert!(reply.contains("> @@ -1,5 +1,5 @@"), "blockquoted diff: {reply}");
        assert!(reply.contains("> -three"), "blockquoted deletions: {reply}");
        // A v-selection of an arbitrary range comments that range (not
        // the hunk) — and a non-hunk range is NOT flagged as a hunk.
        app.comments.clear();
        app.selection = None;
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE, None);
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection { anchor: 1, cursor: 2 }));
        on_source_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (1, 2));
        for ch in "line note".chars() {
            on_input_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        on_input_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let c = &app.comments[0];
        assert!(!c.hunk, "a partial selection is a line comment, not a hunk");
        assert_eq!((c.start, c.end), (2, 3));
        // View mode: bare c comments the exact view line; c after an n
        // jump (hunk selected) comments the hunk.
        let mut app = git_app(path.clone(), Mode::View);
        app.view.goto_source_line(2);
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!((app.input_start, app.input_end), (2, 2), "view c: the exact line");
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // n from OUTSIDE the hunk (cursor 0) selects the whole hunk.
        app.view.goto_source_line(0);
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.selection, Some(Selection { anchor: 0, cursor: 4 }));
        on_view_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!((app.input_start, app.input_end), (0, 4), "view c on the hunk selection");
        on_input_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    }

    #[test]
    fn f7_jumps_between_hunks_and_selects_them() {
        // A 20-line file with two changes: F7 from an unchanged line
        // lands on the first hunk's first CHANGED line, F7 steps to the
        // second, Shift+F7 back. The selection covers each hunk.
        let lines: Vec<String> = (1..=20).map(|i| format!("line{i:02}")).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        let mut work = lines.clone();
        work[4] = "CHANGED-A".into();
        work[14] = "CHANGED-B".into();
        overwrite(&path, &work.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let mut app = git_app(path, Mode::Source);
        assert_eq!(app.git_diff.as_ref().unwrap().hunks.len(), 2);
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 4, "lands on the first changed line");
        // The selection covers the whole hunk (context included), like
        // the `o` toggle's replacement range.
        assert_eq!(app.selection, Some(Selection { anchor: 1, cursor: 7 }));
        let (msg, _, _) = app.status.as_ref().unwrap();
        assert!(msg.contains("change 1/2"), "toast reports the position: {msg}");
        // F7 again: the second hunk.
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 14);
        assert_eq!(app.selection, Some(Selection { anchor: 11, cursor: 17 }));
        assert!(app.status.as_ref().unwrap().0.contains("change 2/2"));
        // Shift+F7: back to the first.
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::SHIFT, None);
        assert_eq!(app.cursor, 4);
        // n / N: the same jumps — the standard diff-tool keys, on the
        // base layer of any keyboard layout.
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 14, "n: next hunk");
        on_source_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 4, "N: previous hunk");
        // Some terminals report Shift+N as 'N' WITH the SHIFT flag set:
        // the jump must not be dropped by a modifier guard.
        on_source_key(&mut app, KeyCode::Char('N'), KeyModifiers::SHIFT, None);
        assert_eq!(app.cursor, 4, "N with the SHIFT flag still jumps back");
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::SHIFT, None);
        assert_eq!(app.cursor, 14, "n with the SHIFT flag still jumps forward");
        // Alt+j / Alt+k: the same jumps, for layouts where the terminal
        // eats Alt and layouts without F7/`[`/`]` on the base layer.
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::ALT, None);
        assert_eq!(app.cursor, 14, "Alt+j: next hunk");
        on_source_key(&mut app, KeyCode::Char('k'), KeyModifiers::ALT, None);
        assert_eq!(app.cursor, 4, "Alt+k: previous hunk");
        // Plain j/k still move (no modifier confusion) — after the hunk
        // jump cleared its selection first.
        app.selection = None;
        on_source_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 5, "plain j still moves");
        // Same keys work in view mode (from the first hunk onward).
        app.view.goto_source_line(4);
        app.mode = Mode::View;
        on_view_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 14);
        on_view_key(&mut app, KeyCode::F(7), KeyModifiers::SHIFT, None);
        assert_eq!(app.view.cursor, 4);
        on_view_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 14, "view n: next hunk");
        on_view_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.view.cursor, 4, "view N: previous hunk");
        on_view_key(&mut app, KeyCode::Char('j'), KeyModifiers::ALT, None);
        assert_eq!(app.view.cursor, 14, "view Alt+j: next hunk");
        on_view_key(&mut app, KeyCode::Char('k'), KeyModifiers::ALT, None);
        assert_eq!(app.view.cursor, 4, "view Alt+k: previous hunk");
    }

    #[test]
    fn n_stepping_never_rewinds_to_the_first_hunk() {
        // The step target is measured by hunk POSITION, not "is the
        // cursor on a changed line": landing on a deletion-only hunk
        // (no added lines — the cursor sits on a context line there)
        // used to make the next `n` rewind to the first hunk.
        let lines: Vec<String> = (1..=30).map(|i| format!("line{i:02}")).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        // Delete lines 6-10 (a hunk with NO added lines) and change
        // line 24: two hunks (7 blank lines apart — git keeps them
        // separate), the first deletion-only.
        let mut work: Vec<String> = (1..=5).map(|i| format!("line{i:02}")).collect();
        work.extend((11..=23).map(|i| format!("line{i:02}")));
        work.push("CHANGED-B".into());
        work.extend((25..=30).map(|i| format!("line{i:02}")));
        overwrite(&path, &work.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let mut app = git_app(path, Mode::Source);
        {
            let hunks = &app.git_diff.as_ref().unwrap().hunks;
            assert_eq!(hunks.len(), 2, "deletion-only hunk + one change");
            assert!(
                !hunks[0].body.iter().any(|l| l.tag == crate::git::Tag::Add),
                "hunk 0 has no added lines"
            );
        }
        // n lands on the deletion-only hunk's owner line (a context
        // line — NOT a changed line), the whole hunk range selected.
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        let landed = app.cursor;
        let h0 = app.git_diff.as_ref().unwrap().hunks[0].new_range().unwrap();
        assert_eq!(app.selection, Some(Selection { anchor: h0.0, cursor: h0.1 }));
        // n again: the NEXT hunk — never a rewind to the top.
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 18, "steps to the second hunk's changed line");
        assert!(
            app.status.as_ref().unwrap().0.contains("change 2/2"),
            "reports hunk 2 of 2: {}",
            app.status.as_ref().unwrap().0
        );
        // At the end: a flash, not a wrap.
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        let (msg, _, is_error) = app.status.as_ref().unwrap();
        assert!(*is_error && msg.contains("no changes below"), "{msg}");
        assert_eq!(app.cursor, 18, "stays put");
        // From the blank gap between the hunks, n goes FORWARD to the
        // next hunk (the old code rewound to the first hunk).
        app.selection = None;
        app.cursor = 10; // between hunk 0 and hunk 1
        on_source_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 18, "from the gap: the next hunk, not the first");
        // N from the same gap goes BACK to hunk 0.
        app.selection = None;
        on_source_key(&mut app, KeyCode::Char('N'), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, landed, "N from the gap: the previous hunk");
    }

    #[test]
    fn f7_at_the_ends_flashes_without_wrapping() {
        let lines: Vec<String> = (1..=20).map(|i| format!("line{i:02}")).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        let mut work = lines.clone();
        work[4] = "CHANGED-A".into();
        work[14] = "CHANGED-B".into();
        overwrite(&path, &work.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let mut app = git_app(path, Mode::Source);
        // Cursor on the LAST hunk already: F7 flashes, does not wrap.
        app.cursor = 14;
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        let (msg, _, is_error) = app.status.as_ref().unwrap();
        assert!(*is_error && msg.contains("no changes below"), "{msg}");
        assert_eq!(app.cursor, 14, "stays put");
        // Shift+F7 from the first hunk flashes the other way.
        app.cursor = 4;
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::SHIFT, None);
        let (msg, _, is_error) = app.status.as_ref().unwrap();
        assert!(*is_error && msg.contains("no changes above"), "{msg}");
        assert_eq!(app.cursor, 4);
        // No hunks at all: a clear error.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three"]);
        let mut app = git_app(path, Mode::Source);
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert!(app.status.as_ref().unwrap().0.contains("no git changes"));
    }

    #[test]
    fn bracket_c_chord_jumps_instead_of_switching_files() {
        // `]c` within the chord window is a change jump (the F7
        // fallback) — the file does NOT switch. A 2-file session: a.md
        // has a change, b.rs does not.
        let dir = tempfile::tempdir().unwrap();
        let a = init_repo(dir.path(), &["one", "two", "three"]);
        overwrite(&a, &["one", "CHANGED", "three"]);
        let b = dir.path().join("b.rs");
        std::fs::write(&b, "fn main() {}\n// second line\n").unwrap();
        let config = Config {
            files: vec![a, b],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        // file_states[1] needs a real source: the chord's default action
        // switches into it.
        let b_source = Source::load(app.files[1].clone()).unwrap();
        let b_spans = app
            .highlight
            .highlight_with(&b_source.content, syntax_for(&app.files[1]));
        let b_view = ViewState::render(&b_source, 75, &app.highlight);
        app.file_states = vec![
            FileState::default(),
            FileState {
                source: b_source,
                spans: b_spans,
                view: b_view,
                mode: Mode::Source,
                ..Default::default()
            },
        ];
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        let (diff, added, deleted) = git_snapshot(&app.git_ref, &app.files[0], app.source.len());
        app.git_diff = diff;
        app.git_added = added;
        app.git_deleted_before = deleted;
        app.scope = DiffScope::Git; // run() のリポジトリ内初期スコープ
        // `]` then `c`: the chord completes into a jump. The tests go
        // through on_key — the chord resolution lives in the dispatcher,
        // not in the per-mode handlers.
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 0, "no file switch");
        assert!(app.pending_chord.is_none(), "the chord was consumed");
        assert_eq!(app.cursor, 1, "landed on the changed line");
        assert_eq!(app.selection, Some(Selection { anchor: 0, cursor: 2 }));
        assert_eq!(app.mode, Mode::Source, "c was the chord, not the composer");
        // `[c` jumps back to the previous hunk (none above: flashes).
        on_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        on_key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE, None);
        assert!(app.status.as_ref().unwrap().0.contains("no changes above"));
        // A plain `]` alone still switches files once the window expires.
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        state_tests::expire_chord(&mut app);
        assert_eq!(app.current_file_index, 1, "the default action fires");
    }

    #[test]
    fn other_keys_resolve_the_bracket_to_a_file_switch() {
        // `]` followed by any non-chord key falls back to the file
        // switch and the key is processed normally; Esc cancels instead.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "# a\n\nline2\n").unwrap();
        std::fs::write(&b, "fn main() {}\n// second line\n").unwrap();
        let config = Config {
            files: vec![a, b],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: None,
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(None, false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        // file_states[1] needs a real source: switching into an empty
        // slot would make `j` a no-op.
        let b_source = Source::load(app.files[1].clone()).unwrap();
        let b_spans = app
            .highlight
            .highlight_with(&b_source.content, syntax_for(&app.files[1]));
        let b_view = ViewState::render(&b_source, 75, &app.highlight);
        app.file_states = vec![
            FileState {
                mode: Mode::View,
                ..Default::default()
            },
            FileState {
                source: b_source,
                spans: b_spans,
                view: b_view,
                mode: Mode::Source,
                ..Default::default()
            },
        ];
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        // `]` then `j`: file switch + move (through on_key, the real
        // dispatcher).
        let cur = app.cursor;
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        on_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 1, "] fell back to the switch");
        assert_eq!(app.cursor, cur + 1, "j processed normally afterwards");
        // `[` then Esc: cancelled — no switch, Esc proceeds (clears a
        // selection).
        on_key(&mut app, KeyCode::Char('['), KeyModifiers::NONE, None);
        app.selection = Some(Selection::new(1));
        on_key(&mut app, KeyCode::Esc, KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 1, "Esc cancels the bracket");
        assert!(app.selection.is_none(), "Esc itself is still processed");
        assert!(app.pending_chord.is_none());
        // Mashing `]` `]`: the second press resolves the first into a
        // file switch and arms itself.
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        on_key(&mut app, KeyCode::Char(']'), KeyModifiers::NONE, None);
        assert_eq!(app.current_file_index, 1, "the first ] resolved");
        assert!(app.pending_chord.is_some(), "the second ] is pending");
    }

    #[test]
    fn l_changes_tab_lists_hunks_and_enter_jumps() {
        // `l` → Tab: the changes tab lists every hunk across the session
        // files with location, +N/-M, and a preview; Enter jumps to it.
        let dir = tempfile::tempdir().unwrap();
        let a = init_repo(dir.path(), &["one", "two", "three", "four", "five"]);
        overwrite(&a, &["one", "CHANGED", "three", "four", "five"]);
        let b = dir.path().join("b.rs");
        std::fs::write(&b, "fn main() {}\n// second line\n").unwrap();
        let config = Config {
            files: vec![a, b],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        let (diff, added, deleted) = git_snapshot(&app.git_ref, &app.files[0], app.source.len());
        app.git_diff = diff;
        app.git_added = added;
        app.git_deleted_before = deleted;
        app.scope = DiffScope::Git; // run() のリポジトリ内初期スコープ
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        assert_eq!(app.overlay, Some(Overlay::Comments));
        on_overlay_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.overlay_tab, OverlayTab::Changes);
        // Draw: the changes tab shows the hunk's location and preview.
        let capture = |app: &mut App| -> String {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .unwrap();
            t.draw(|f| draw(f, app)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol().chars().next().unwrap_or(' '))
                .collect()
        };
        let frame = capture(&mut app);
        assert!(frame.contains("changes (1)"), "tab count: {frame}");
        assert!(frame.contains("+1/-1"), "hunk counts: {frame}");
        assert!(frame.contains("CHANGED"), "preview line: {frame}");
        assert!(!frame.contains("d:delete"), "no delete hint on the changes tab");
        // Enter jumps to the hunk and closes the overlay.
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
        assert_eq!(app.cursor, 1, "landed on the changed line");
        assert_eq!(app.selection, Some(Selection { anchor: 0, cursor: 4 }));
        // Tab back to comments.
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.overlay_tab, OverlayTab::Comments);
    }

    #[test]
    fn changes_tab_shows_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path(), &["tracked"]);
        let untracked = dir.path().join("new.md");
        std::fs::write(&untracked, "fresh\nnew\n").unwrap();
        let config = Config {
            files: vec![untracked],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let source = Source::load(config.files[0].clone()).unwrap();
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        let view = ViewState::render(&source, 75, &highlight);
        let mut app = App::new(config, source, highlight, view, false);
        app.spans = app
            .highlight
            .highlight_with(&app.source.content, syntax_for(&app.files[0]));
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        let (diff, added, deleted) = git_snapshot(&app.git_ref, &app.files[0], app.source.len());
        app.git_diff = diff;
        app.git_added = added;
        app.git_deleted_before = deleted;
        app.scope = DiffScope::Git; // run() のリポジトリ内初期スコープ
        on_source_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE, None);
        on_overlay_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        let frame: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(frame.contains("untracked"), "untracked row: {frame}");
        assert!(frame.contains("all new"), "the explanation: {frame}");
        // Enter lands at the top.
        on_overlay_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.overlay, None);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn f7_without_git_flashes() {
        // P1: outside a repository the session runs on the Last scope —
        // the jump keys explain themselves (nothing reloaded yet) and
        // change nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let mut app = git_app(path, Mode::Source);
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        let (msg, _, _) = app.status.as_ref().expect("a flash explains");
        assert!(msg.contains("no reload diff yet"), "{msg}");
        assert!(app.selection.is_none());
    }

    #[test]
    fn o_last_scope_shows_the_reload_diffs_old_side() {
        // Diff-scope step ②: under Last, `o` compares against the last
        // loaded content — the synthesized reload diff, usable outside a
        // repository too (P1 untouched: git is not required for it).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut app = git_app(path.clone(), Mode::Source); // no repo → Last scope
        overwrite(&path, &["one", "CHANGED", "three"]);
        assert!(reload_source(&mut app, false).is_ok());
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let os = app.old_side.as_ref().expect("the reload hunk is toggled");
        assert_eq!(os.old_lines, vec!["two"], "the pre-reload content");
        assert_eq!(os.old_numbers, vec![2], "old numbers from the synthesized diff");
        assert_eq!(os.range, Some((1, 1)));
        let (msg, _, _) = app.status.as_ref().expect("a flash names the base");
        assert!(msg.contains("old side vs last load"), "{msg}");
        // A second `o` on the same hunk returns to the new side.
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none(), "toggled back");
    }

    #[test]
    fn o_last_scope_without_a_reload_flashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let mut app = git_app(path, Mode::Source);
        assert!(app.last_diff.is_none(), "nothing reloaded yet");
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none());
        let (msg, _, is_error) = app.status.as_ref().expect("a flash explains");
        assert!(*is_error, "it is an error toast: {msg}");
        assert!(msg.contains("no reload diff yet"), "{msg}");
    }

    #[test]
    fn o_off_scope_flashes() {
        // Off disables the toggle even inside a repository with changes.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three"]);
        overwrite(&path, &["one", "CHANGED", "three"]);
        let mut app = git_app(path, Mode::Source);
        app.scope = DiffScope::Off;
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_none());
        let (msg, _, _) = app.status.as_ref().expect("a flash explains");
        assert!(msg.contains("marks off"), "{msg}");
    }

    #[test]
    fn o_untracked_last_scope_uses_the_reload_diff() {
        // The untracked guard applies to the git-based scopes only: Last
        // has a usable reload diff for an untracked file.
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path(), &["tracked"]);
        let untracked = dir.path().join("new.md");
        std::fs::write(&untracked, "one\ntwo\nthree\n").unwrap();
        let mut app = git_app(untracked.clone(), Mode::Source); // in repo → Git scope
        assert!(app.git_diff.as_ref().unwrap().untracked);
        // The agent edits; reload auto-transitions Git → Last.
        overwrite(&untracked, &["one", "CHANGED", "three"]);
        assert!(reload_source(&mut app, false).is_ok());
        assert_eq!(app.scope, DiffScope::Last);
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        let os = app.old_side.as_ref().expect("the reload hunk is toggled");
        assert_eq!(os.old_lines, vec!["two"], "old side works despite untracked");
    }

    #[test]
    fn m_closes_the_toggled_old_side() {
        // A scope switch invalidates the open old side's hunk index (it
        // pointed into the previous scope's diff): `m` closes it and
        // rebuilds the layout.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four"]);
        overwrite(&path, &["one", "CHANGED", "three", "four"]);
        let mut app = git_app(path, Mode::Source);
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some());
        on_source_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE, None);
        assert_eq!(app.scope, DiffScope::Both);
        assert!(app.old_side.is_none(), "the scope switch closes the old side");
        let rows = source_rows(&app);
        assert!(rows[1].contains("CHANGED"), "the new side renders again: {}", rows[1]);
    }

    #[test]
    fn jump_follows_the_scope() {
        // The navigable hunk list switches with the scope: Git walks the
        // snapshot's two hunks, Last the reload diff's single hunk. The
        // changes sit far enough apart (line 2 and line 10 of a 12-line
        // file) that git's 3 context lines keep them in separate hunks.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(
            dir.path(),
            &[
                "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
                "eleven", "twelve",
            ],
        );
        overwrite(
            &path,
            &["one", "CHANGED1", "three", "four", "five", "six", "seven", "eight", "nine", "CHANGED2", "eleven", "twelve"],
        );
        let mut app = git_app(path.clone(), Mode::Source);
        assert_eq!(app.scope, DiffScope::Git);
        assert_eq!(app.git_diff.as_ref().unwrap().hunks.len(), 2);
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 1, "first git change");
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 9, "second git change");
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert!(app.status.as_ref().unwrap().0.contains("no changes below"));
        // The agent rewrites only the second change away; reload
        // auto-transitions to Last, whose diff has one hunk.
        overwrite(
            &path,
            &["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "CHANGED2", "eleven", "twelve"],
        );
        assert!(reload_source(&mut app, false).is_ok());
        assert_eq!(app.scope, DiffScope::Last);
        assert_eq!(app.last_diff.as_ref().unwrap().hunks.len(), 1);
        // The reload diff describes THIS reload's delta — line 2 being
        // reverted — not the pre-existing CHANGED2.
        app.cursor = 0;
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert_eq!(app.cursor, 1, "the reload diff's change (line 2 reverted)");
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert!(app.status.as_ref().unwrap().0.contains("no changes below"));
    }

    #[test]
    fn jump_off_scope_flashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three"]);
        overwrite(&path, &["one", "CHANGED", "three"]);
        let mut app = git_app(path, Mode::Source);
        app.scope = DiffScope::Off;
        on_source_key(&mut app, KeyCode::F(7), KeyModifiers::NONE, None);
        assert!(app.selection.is_none());
        let (msg, _, _) = app.status.as_ref().expect("a flash explains");
        assert!(msg.contains("marks off"), "{msg}");
    }

    #[test]
    fn both_scope_merges_hunks_by_anchor() {
        // The Both scope walks both diffs as ONE anchor-ordered list; a
        // shared anchor resolves once (last wins) so a change visible in
        // both diffs cannot stop navigation twice.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four"]);
        let mut app = git_app(path, Mode::Source);
        let hunk_at = |a: u32| crate::git::Hunk {
            old_start: a,
            old_len: 0,
            new_start: a + 1,
            new_len: 1,
            body: vec![crate::git::HunkLine {
                tag: crate::git::Tag::Add,
                text: "x".into(),
            }],
        };
        app.last_diff = Some(crate::git::Diff {
            hunks: vec![hunk_at(1), hunk_at(4)],
            untracked: false,
        });
        app.git_diff = Some(crate::git::Diff {
            hunks: vec![hunk_at(1), hunk_at(7)],
            untracked: false,
        });
        app.scope = DiffScope::Both;
        let refs = scoped_hunk_refs(&app);
        let anchors: Vec<usize> = refs.iter().map(|r| r.anchor).collect();
        assert_eq!(anchors, vec![1, 4, 7], "merged and anchor-ordered");
        assert_eq!(refs[0].source, DiffSource::Last, "the shared anchor resolves as last");
        assert_eq!(refs[2].source, DiffSource::Git);
        // Single-scope resolutions are plain pass-throughs.
        app.scope = DiffScope::Last;
        assert_eq!(scoped_hunk_refs(&app).len(), 2);
        app.scope = DiffScope::Git;
        assert_eq!(scoped_hunk_refs(&app).len(), 2);
        app.scope = DiffScope::Off;
        assert!(scoped_hunk_refs(&app).is_empty());
    }

    #[test]
    fn startup_activation_carries_the_git_snapshot_into_the_live_fields() {
        // Regression: run() moves the first file's per-file state into the
        // live App fields via activate_first_file — the git snapshot must
        // ride along. Before the fix the first file opened with empty
        // marks and `o` claimed "not in a git repository" until a file
        // switch (the tests built App directly and never exercised run()'s
        // wiring).
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three"]);
        overwrite(&path, &["one", "CHANGED", "three"]);
        let config = Config {
            files: vec![path.clone()],
            send_cmd: None,
            send_agent: false,
            reply: false,
            theme: Some("base16-ocean.dark".into()),
            ime: ImeMode::Off,
            light: None,
            callback: None,
            esc_quit: EscQuit::Auto,
        };
        let highlight = Highlighter::new(config.theme.as_deref(), false);
        // Build FileState exactly like run() does.
        let source = Source::load(path.clone()).unwrap();
        let spans = highlight.highlight_with(&source.content, syntax_for(&path));
        let view = ViewState::render(&source, 75, &highlight);
        let mut fs = FileState {
            source,
            spans,
            view,
            mode: Mode::Source,
            ..Default::default()
        };
        let (diff, added, deleted) = git_snapshot("HEAD", &config.files[0], fs.source.len());
        fs.git_diff = diff;
        fs.git_added = added;
        fs.git_deleted_before = deleted;
        // run() の初期スコープ設定 (diff-scope step ①): リポジトリ内は Git。
        fs.scope = if fs.git_diff.is_some() { DiffScope::Git } else { DiffScope::Last };
        // Activate like run() does.
        let mut app =
            App::new(config, Source::default(), highlight, ViewState::default(), false);
        app.file_states = vec![fs];
        activate_first_file(&mut app);
        assert_eq!(
            app.source.lines,
            ["one", "CHANGED", "three"],
            "the first file's source is live"
        );
        assert!(app.git_diff.is_some(), "the snapshot rides along");
        assert_eq!(app.git_added, HashSet::from([1]), "the marks ride along");
        assert_eq!(app.scope, DiffScope::Git, "the initial scope rides along");
        assert!(!app.scope_manual, "the startup default is not user-pinned");
        // `o` works on the first file right away (it used to flash
        // "not in a git repository").
        app.mode = Mode::Source;
        app.gutter_cols = 3;
        app.ensure_row_cache(75);
        app.refresh_line_rows();
        app.cursor = 1;
        on_source_key(&mut app, KeyCode::Char('o'), KeyModifiers::NONE, None);
        assert!(app.old_side.is_some(), "the toggle sees the snapshot");
    }

    #[test]
    fn reload_refetches_the_git_snapshot() {
        // 論点 5: a manual reload takes the diff again, so the marks
        // describe the CURRENT working tree, and the toggle resets.
        let dir = tempfile::tempdir().unwrap();
        let path = init_repo(dir.path(), &["one", "two", "three", "four", "five"]);
        overwrite(&path, &["one", "two", "three", "four", "five", "six"]);
        let mut app = git_app(path, Mode::Source);
        assert!(app.git_added.contains(&5), "the appended line is marked");
        // The agent rewrites the file; `r` refetches the diff.
        overwrite(&app.files[0], &["one", "two", "CHANGED", "four"]);
        assert!(reload_source(&mut app, false).is_ok());
        assert!(app.git_added.contains(&2), "the new change is marked: {:?}", app.git_added);
        assert!(!app.git_added.contains(&5), "stale marks are gone");
        assert!(app.old_side.is_none());
        // A content-changing reload under the unpinned Git scope
        // auto-transitions to Last (diff-scope step ①).
        assert_eq!(app.scope, DiffScope::Last, "the new edit is reviewed via the reload diff");
    }
}
