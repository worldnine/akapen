//! Markdown history as a sequence of complete, renderable documents.
//!
//! Git is deliberately kept behind this module.  The UI consumes document
//! snapshots, not hunks: moving through time always produces a complete
//! Markdown source that can be rendered normally.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use similar::{DiffTag, TextDiff};

use crate::snapshot::{CachedFile, SnapshotCache};

/// How long one bounded diff may run before similar hands back its best
/// alignment so far: the appear-mask alignment in
/// [`crate::inserted_char_ranges`]. 8 ms completes typical paragraph
/// diffs (1 KB pairs cost 2–4 ms) and only truncates the genuinely huge
/// ones — the mask then animates a few extra or missing cells instead
/// of stalling the render.
pub(crate) const DIFF_DEADLINE: std::time::Duration = std::time::Duration::from_millis(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RevisionSource {
    Now,
    Local,
    Git,
}

/// One point on a document's timeline.
#[derive(Clone, Debug)]
pub(crate) struct Revision {
    /// Full commit id or stable `local:<content-id>`. `None` is NOW.
    pub(crate) id: Option<String>,
    pub(crate) short_id: String,
    pub(crate) summary: String,
    pub(crate) content: String,
    pub(crate) source: RevisionSource,
}

impl Revision {
    pub(crate) fn context(&self) -> Option<String> {
        match self.source {
            RevisionSource::Now => None,
            // LOCAL も Git と同じ「id — 説明文」形式。identity（「 — 」の左）は
            // `local:<内容ハッシュ>` のままで、右側に LLM 向けの説明文を添える
            // （同一性判定は左側のみで行う — 新旧形式の照合は [`same_revision`]）。
            RevisionSource::Local | RevisionSource::Git => Some(format!(
                "{} — {}",
                self.id.as_deref().unwrap_or(&self.short_id),
                self.summary
            )),
        }
    }
}

/// revision 文字列（`id — 説明文`）から identity（「 — 」の左側）を取り出す。
/// 旧形式（`local:<id>` 裸）には区切りが無いので全体が identity になる —
/// これが新旧形式の移行互換の要。identity 側に「 — 」が現れることは無い
/// （hex oid / `local:<hex>`）ので、先頭の区切りで切るだけで十分。
pub(crate) fn revision_id(context: &str) -> &str {
    context.split_once(" — ").map_or(context, |(id, _)| id)
}

/// 2 つの revision 文字列が同じ世代を指すか。同一性判定は「 — 」の左側
/// （機械用 identity）だけで行い、説明文（人間/LLM 用）は比較しない。
/// None ↔ None は一致（Revision 行の無い NOW 同士）、None ↔ Some は不一致。
pub(crate) fn same_revision(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => revision_id(a) == revision_id(b),
        (None, None) => true,
        _ => false,
    }
}

/// LOCAL 世代の説明文。受け取った LLM が `git show` などを空振りしないよう、
/// 「git オブジェクトではない」という否定情報を必ず含める。
/// キャッシュに固定保存された値（captured_ms / parent）だけから組み立てるので、
/// 同じスナップショットからは常に同じ文字列が再構成される（決定的）。
pub(crate) fn local_revision_summary(captured_ms: u64, parent: Option<&str>) -> String {
    let mut summary = format!(
        "akapen local snapshot: uncommitted state captured {}",
        format_captured_utc(captured_ms)
    );
    if let Some(parent) = parent {
        summary.push_str(&format!(
            ", on top of commit {}",
            parent.get(..7).unwrap_or(parent)
        ));
    }
    summary.push_str(" (not a git object)");
    summary
}

/// epoch ミリ秒を UTC の ISO 8601（秒精度、`2026-08-16T05:03:22Z`）に整形する。
/// chrono 等の依存を持ち込まず、Howard Hinnant の civil_from_days で日付を求める。
fn format_captured_utc(captured_ms: u64) -> String {
    let secs = captured_ms / 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Newest-first history. Position zero is always the live working tree.
#[derive(Clone, Debug, Default)]
pub(crate) struct DocumentHistory {
    pub(crate) revisions: Vec<Revision>,
    /// Revision currently selected by the lightweight timeline cursor.
    pub(crate) position: usize,
    /// Revision whose Markdown is actually displayed. During fast scrubbing
    /// this trails `position` until the render debounce settles.
    pub(crate) rendered_position: usize,
    /// Durable human-review checkpoint, independent of the displayed
    /// timeline position and of Git availability.
    pub(crate) reviewed_id: Option<String>,
    pub(crate) reviewed_content: Option<String>,
}

/// A block that existed in the previous revision but not the next one.
/// It is rendered dimly at `anchor` for a moment before being collapsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeletedBlock {
    pub(crate) anchor: usize,
    pub(crate) content: String,
}

/// Cumulative review marks from an acknowledged full document to NOW.
/// Markdown uses semantic blocks; every other text file uses source lines.
pub(crate) fn review_transition(
    _markdown: bool,
    reviewed: &str,
    now: &str,
) -> (HashSet<usize>, HashSet<usize>) {
    // Line-granular review marks: a one-cell edit marks one line, never
    // the whole block. The block-level "whole rendered block lights"
    // intent was dropped from the spec — the animation path and the
    // review marks now agree on per-line granularity.
    let old: Vec<String> = reviewed.lines().map(str::to_string).collect();
    let new: Vec<String> = now.lines().map(str::to_string).collect();
    let (changed, deleted) = line_transition(&old, &new);
    let last = new.len().saturating_sub(1);
    let deleted_before = deleted
        .into_iter()
        .map(|block| block.anchor.min(last))
        .collect();
    (changed, deleted_before)
}

/// Baseline-relative marks plus the baseline text of every change, in one
/// diff pass: the changed/deleted-position sets are identical to
/// [`review_transition`], and the returned blocks carry the OLD side of
/// every non-equal op — rewrites included, unlike the position set, which
/// counts only net deletions. Source mode renders these blocks inline as
/// red deleted rows (a unified-diff-style old side); view mode and the
/// review badge keep consuming only the sets.
///
/// Block anchors are NOT clamped: an anchor equal to the new line count
/// means "deleted after the last line" and renders below it.
pub(crate) fn comparison_transition(
    reviewed: &str,
    now: &str,
) -> (HashSet<usize>, HashSet<usize>, Vec<DeletedBlock>) {
    // Diff the raw texts, NOT a lines-rejoined copy: rejoining drops the
    // trailing newline, which turns a pure EOF deletion into a spurious
    // rewrite of the last line (its old text would render as a deleted
    // row of its own). (`review_transition` keeps the rejoined diff so
    // the badge count and its tests stay bit-identical.)
    let diff = TextDiff::from_lines(reviewed, now);
    let old: Vec<&str> = reviewed.lines().collect();
    let last = now.lines().count().saturating_sub(1);
    let mut changed = HashSet::new();
    let mut deleted_before = HashSet::new();
    let mut blocks = Vec::new();
    for op in diff.ops() {
        let old_range = op.old_range();
        let new_range = op.new_range();
        if op.tag() == DiffTag::Equal {
            continue;
        }
        if !new_range.is_empty() {
            changed.extend(new_range.clone());
        }
        if old_range.len() > new_range.len() {
            deleted_before.insert(new_range.start.min(last));
        }
        if !old_range.is_empty() {
            blocks.push(DeletedBlock {
                anchor: new_range.start,
                content: old
                    .get(old_range)
                    .map(|lines| lines.join("\n"))
                    .unwrap_or_default(),
            });
        }
    }
    (changed, deleted_before, blocks)
}

fn line_transition(old: &[String], new: &[String]) -> (HashSet<usize>, Vec<DeletedBlock>) {
    let old_text = old.join("\n");
    let new_text = new.join("\n");
    let diff = TextDiff::from_lines(&old_text, &new_text);
    let mut changed = HashSet::new();
    let mut deleted = Vec::new();
    for op in diff.ops() {
        let old_range = op.old_range();
        let new_range = op.new_range();
        if op.tag() != DiffTag::Equal && !new_range.is_empty() {
            changed.extend(new_range.clone());
        }
        if old_range.len() > new_range.len() {
            deleted.push(DeletedBlock {
                anchor: new_range.start,
                content: old
                    .get(old_range.clone())
                    .map(|lines| lines.join("\n"))
                    .unwrap_or_default(),
            });
        }
    }
    (changed, deleted)
}

impl DocumentHistory {
    /// Load up to `limit` committed versions of `path`, plus the live text.
    /// Failure is intentionally soft: a non-Git Markdown file still gets a
    /// one-point timeline and behaves like an ordinary viewer.
    pub(crate) fn load(path: &Path, live: &str, limit: usize) -> Self {
        Self::load_with_local(path, live, limit, CachedFile::default())
    }

    /// Load Git anchors and the bounded LOCAL cache into one newest-first
    /// document timeline. Cache failure is returned so the caller can fall
    /// back to Git-only history without making the file unreadable.
    pub(crate) fn load_cached(
        path: &Path,
        live: &str,
        limit: usize,
        cache: &SnapshotCache,
    ) -> anyhow::Result<Self> {
        let local = cache.record_with_parent(path, live, head_oid(path))?;
        Ok(Self::load_with_local(path, live, limit, local))
    }

    pub(crate) fn open_cached(
        path: &Path,
        live: &str,
        limit: usize,
        cache: &SnapshotCache,
    ) -> anyhow::Result<Self> {
        let local = cache.open_with_parent(path, live, head_oid(path))?;
        Ok(Self::load_with_local(path, live, limit, local))
    }

    fn load_with_local(path: &Path, live: &str, limit: usize, local: CachedFile) -> Self {
        let gits = load_git_revisions(path, limit);
        Self::assemble_timeline(live, gits, local)
    }

    /// gits（newest-first）と LOCAL スナップショットを一つの血統順タイムラインに
    /// 組み立てる純粋な配置。`load_with_local` の実体で、fixture テストは実 git を
    /// 立てずに fixture の gits / snapshots を直接渡して検証する。
    fn assemble_timeline(live: &str, git_revisions: Vec<Revision>, local: CachedFile) -> Self {
        let mut working = vec![Revision {
            id: None,
            short_id: "now".to_string(),
            summary: "working tree".to_string(),
            content: live.to_string(),
            source: RevisionSource::Now,
        }];

        // Git アンカーは newest-first。live と同内容のコミットは NOW に統合され、
        // 同じ content のコミットは newest だけ残す（重複除去は従来と同じ方針）。
        // ただし LOCAL の parent lookup はこの重複除去後の表示列ではなく除去前の
        // 完全列に対して行うため、各表示コミットの「完全列での index」も保持する。
        // （内容が重複して表示から落ちた parent に向く正しい LOCAL が、parent を
        //  解決できず orphan 化して先頭に浮く誤順序を防ぐ。）
        let mut seen = HashSet::new();
        let mut displayed: Vec<(usize, Revision)> = Vec::new();
        for (full_index, revision) in git_revisions.iter().enumerate() {
            if revision.content == live || !seen.insert(revision.content.clone()) {
                continue;
            }
            displayed.push((full_index, revision.clone()));
        }
        working.extend(
            displayed
                .iter()
                .map(|(_, revision)| revision.clone()),
        );

        // スナップショットは oldest-first で処理する。live または working 内の
        // いずれかと同内容なら LOCAL を生成しない（内容一致の LOCAL は COMMIT に
        // 統合され、そのコミットは自分の自然な位置に留まる — 古いコミットへ
        // 巻き戻した内容が「最新」に見える誤順序も同時に直る）。
        let mut orphans: Vec<Revision> = Vec::new();
        for snapshot in &local.snapshots {
            if snapshot.content == live
                || working.iter().any(|revision| revision.content == snapshot.content)
            {
                continue;
            }
            let local_revision = Revision {
                id: Some(format!("local:{}", snapshot.id)),
                short_id: snapshot.id.chars().take(7).collect(),
                // キャッシュに保存された固定値だけから組み立てる（決定的）。
                // parent が None（非 git 環境・旧メタデータ）なら説明文から
                // 「on top of commit」句を省略する。
                summary: local_revision_summary(
                    snapshot.captured_ms,
                    snapshot.parent.as_deref(),
                ),
                content: snapshot.content.clone(),
                source: RevisionSource::Local,
            };
            // parent（観測時点の HEAD oid）の位置を「重複除去前の完全な git 列」
            // から引く。表示済み commit に出てこない（内容重複で落ちた）parent
            // でも、LOCAL はその parent より新しいので「parent より新しい表示
            // commit のすぐ古い側」に挿入すれば正しいスロットになる。
            let anchor = snapshot.parent.as_deref().and_then(|parent| {
                git_revisions
                    .iter()
                    .position(|revision| revision.id.as_deref() == Some(parent))
            });
            match anchor {
                // working[0] は NOW。それ以降に「parent より新しい表示 commit」が
                // count_newer 個並ぶので、LOCAL はその直後（INDEX 1+count_newer）へ。
                // 同一 parent を共有する LOCAL は snapshots を oldest-first で処理し
                // 同じ slot へ insert するため、後から挿入した新しい方が前に来て
                // newest が前（NOW 寄り）に並ぶ。
                Some(parent_full_index) => {
                    let count_newer = displayed
                        .iter()
                        .filter(|(full_index, _)| *full_index < parent_full_index)
                        .count();
                    working.insert(1 + count_newer, local_revision);
                }
                // 非 git・rebase・別 ancestry の orphan は後回しにする。
                None => orphans.push(local_revision),
            }
        }

        // orphan は NOW 直後へ観測順（newest が前）で挿入し、従来の「LOCAL は
        // Git より新しい側・観測順」の劣化動作を維持する。
        if !orphans.is_empty() {
            orphans.reverse(); // oldest-first で集めたので newest-first に直す
            working.splice(1..1, orphans);
        }

        Self {
            revisions: working,
            position: 0,
            rendered_position: 0,
            reviewed_id: local.reviewed_id,
            reviewed_content: local.reviewed_content,
        }
    }

    pub(crate) fn replace_from_cache(
        &mut self,
        path: &Path,
        live: &str,
        limit: usize,
        cache: &SnapshotCache,
    ) -> anyhow::Result<()> {
        *self = Self::load_cached(path, live, limit, cache)?;
        Ok(())
    }

    pub(crate) fn acknowledge(
        &mut self,
        path: &Path,
        live: &str,
        cache: &SnapshotCache,
    ) -> anyhow::Result<()> {
        let local = cache.acknowledge(path, live)?;
        self.reviewed_id = local.reviewed_id;
        self.reviewed_content = local.reviewed_content;
        Ok(())
    }

    pub(crate) fn acknowledge_in_memory(&mut self, live: &str) {
        let id = crate::snapshot::content_id(live.as_bytes());
        self.reviewed_id = Some(id);
        self.reviewed_content = Some(live.to_string());
    }

    pub(crate) fn set_baseline(
        &mut self,
        path: &Path,
        content: &str,
        cache: &SnapshotCache,
    ) -> anyhow::Result<()> {
        let local = cache.set_baseline(path, content)?;
        self.reviewed_id = local.reviewed_id;
        self.reviewed_content = local.reviewed_content;
        Ok(())
    }

    pub(crate) fn pin_current(&self, path: &Path, cache: &SnapshotCache) -> anyhow::Result<()> {
        let Some(revision) = self.current() else {
            return Ok(());
        };
        if revision.source == RevisionSource::Git {
            return Ok(());
        }
        cache.pin(path, &revision.content)
    }

    pub(crate) fn at_now(&self) -> bool {
        self.position == 0
    }
}

fn load_git_revisions(path: &Path, limit: usize) -> Vec<Revision> {
    let mut revisions = Vec::new();

    let Some(root) = repo_root(path) else {
        return revisions;
    };
    let abs = absolutize(path);
    let Ok(rel) = abs.strip_prefix(&root) else {
        return revisions;
    };
    let format = "%H%x1f%h%x1f%s";
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["log", "--follow", &format!("--format={format}"), "--"])
        .arg(rel)
        .output();
    let Ok(output) = output else {
        return revisions;
    };
    if !output.status.success() {
        return revisions;
    }

    for line in String::from_utf8_lossy(&output.stdout).lines().take(limit) {
        let mut fields = line.splitn(3, '\x1f');
        let (Some(id), Some(short_id), Some(summary)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let spec = format!("{id}:{}", rel.to_string_lossy());
        let Ok(shown) = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["show", "--no-ext-diff", &spec])
            .output()
        else {
            continue;
        };
        if !shown.status.success() {
            continue;
        }
        let Ok(content) = String::from_utf8(shown.stdout) else {
            continue;
        };
        if revisions.last().is_some_and(|r| r.content == content) {
            continue;
        }
        revisions.push(Revision {
            id: Some(id.to_string()),
            short_id: short_id.to_string(),
            summary: summary.to_string(),
            content,
            source: RevisionSource::Git,
        });
    }
    revisions
}

impl DocumentHistory {
    pub(crate) fn current(&self) -> Option<&Revision> {
        self.revisions.get(self.position)
    }

    pub(crate) fn context_for_content(&self, content: &str) -> Option<String> {
        self.revisions
            .iter()
            .find(|revision| revision.content == content)
            .and_then(Revision::context)
    }

    pub(crate) fn move_by(&mut self, delta: isize) -> bool {
        if self.revisions.is_empty() {
            return false;
        }
        let next = (self.position as isize + delta)
            .clamp(0, self.revisions.len() as isize - 1) as usize;
        if next == self.position {
            return false;
        }
        self.position = next;
        true
    }

    pub(crate) fn label(&self) -> Option<String> {
        let revision = self.current()?;
        let chronological = self.revisions.len().saturating_sub(self.position);
        let provenance = match revision.source {
            RevisionSource::Now => "NOW",
            RevisionSource::Local => "LOCAL",
            RevisionSource::Git => "COMMIT",
        };
        let baseline_position = self.baseline_position();
        let is_baseline = baseline_position == Some(self.position);
        let mut label = if self.position == 0 {
            format!(
                "NOW · {}/{}",
                chronological,
                self.revisions.len()
            )
        } else {
            format!(
                "{provenance} · {}/{} · {} · {}",
                chronological,
                self.revisions.len(),
                revision.short_id,
                revision.summary
            )
        };
        if is_baseline {
            label = format!("BASELINE · {label}");
        } else if let Some(position) = baseline_position {
            let base_chronological = self.revisions.len().saturating_sub(position);
            label.push_str(&format!(
                " · base {}/{}",
                base_chronological,
                self.revisions.len()
            ));
        } else if self.reviewed_content.is_some() {
            label.push_str(" · base cached");
        }
        Some(label)
    }

    pub(crate) fn baseline_position(&self) -> Option<usize> {
        let reviewed = self.reviewed_content.as_deref()?;
        self.revisions
            .iter()
            .position(|revision| revision.content == reviewed)
    }
}

/// Keep the reader near the same semantic place after a revision change.
/// The nearest preceding Markdown heading is the anchor; the cursor's
/// distance from that heading is retained where the new section permits.
pub(crate) fn anchored_line(old: &[String], new: &[String], line: usize) -> usize {
    if new.is_empty() {
        return 0;
    }
    if old.is_empty() {
        // An empty old document has no heading to anchor to: the cursor
        // lands at the top of the new one (e.g. history browsing on an
        // empty working tree).
        return 0;
    }
    let old_line = line.min(old.len().saturating_sub(1));
    let heading = (0..=old_line)
        .rev()
        .find(|&i| heading_text(&old[i]).is_some());
    let Some(old_heading) = heading else {
        return old_line.min(new.len() - 1);
    };
    let Some(text) = heading_text(&old[old_heading]) else {
        return old_line.min(new.len() - 1);
    };
    let Some(new_heading) = new
        .iter()
        .position(|candidate| heading_text(candidate) == Some(text))
    else {
        return old_line.min(new.len() - 1);
    };
    let section_end = new
        .iter()
        .enumerate()
        .skip(new_heading + 1)
        .find(|(_, candidate)| heading_text(candidate).is_some())
        .map(|(i, _)| i)
        .unwrap_or(new.len());
    (new_heading + old_line.saturating_sub(old_heading)).min(section_end.saturating_sub(1))
}

fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    (hashes > 0 && hashes <= 6)
        .then(|| trimmed[hashes..].trim())
        .filter(|text| !text.is_empty())
}

fn repo_root(path: &Path) -> Option<PathBuf> {
    // Resolve first: a basename such as `README.md` has an empty lexical
    // parent, but its absolute form has the current directory as parent.
    let absolute = absolutize(path);
    let dir = absolute.parent()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    output.status.success().then(|| {
        let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        std::fs::canonicalize(&root).unwrap_or(root)
    })
}

/// 観測時点の HEAD commit oid。git 環境でなければ None（= 非 git は従来の観測順のまま）。
/// 失敗（非 git・git エラー・untracked）はすべて None に落とす（soft failure が既存の方針）。
pub(crate) fn head_oid(path: &Path) -> Option<String> {
    let root = repo_root(path)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // untracked なファイルは git の血統に載っていない。gits 側に一致する
    // コミットが無い以上 parent を付けても orphan 扱いになるだけなので、
    // 「観測時点の血統」という意味でも None が正直。
    let absolute = absolutize(path);
    let rel = absolute.strip_prefix(&root).ok()?;
    let tracked = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(rel)
        .output()
        .ok()?;
    if !tracked.status.success() {
        return None;
    }
    (!oid.is_empty()).then_some(oid)
}

fn absolutize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

#[cfg(test)]
mod tests {
    use super::{
        DocumentHistory, Revision, RevisionSource, anchored_line, head_oid,
        local_revision_summary, review_transition, revision_id, same_revision,
    };
    use crate::snapshot::{CachedFile, CachedSnapshot, SnapshotCache};
    use serde::Deserialize;
    use std::process::Command;

    #[test]
    fn local_summary_comes_from_fixed_cache_values() {
        // キャッシュに保存された固定値（captured_ms / parent）だけから説明文が
        // 組み立てられ、再構成しても決定的。タスク例のタイムスタンプ
        // （2026-08-16T05:03:22Z）で UTC 整形を検証する。
        let gits = Vec::new();
        let local = CachedFile {
            snapshots: vec![CachedSnapshot {
                id: "0123456789abcdef".repeat(4),
                captured_ms: 1_786_856_602_000,
                content: "old\n".into(),
                pinned: false,
                parent: Some("720a4450123456789abcdef0123456789abcdef".into()),
            }],
            reviewed_id: None,
            reviewed_content: None,
        };
        let history = DocumentHistory::assemble_timeline("new\n", gits, local);
        let local_revision = &history.revisions[1];
        assert_eq!(
            local_revision.summary,
            "akapen local snapshot: uncommitted state captured \
             2026-08-16T05:03:22Z, on top of commit 720a445 (not a git object)"
        );
        // context は「id — 説明文」の完全形（export にそのまま出る行）。
        assert_eq!(
            local_revision.context().as_deref(),
            Some(
                "local:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef — \
                 akapen local snapshot: uncommitted state captured 2026-08-16T05:03:22Z, \
                 on top of commit 720a445 (not a git object)"
            )
        );
    }

    #[test]
    fn local_summary_omits_the_parent_clause_when_unknown() {
        // parent が None（非 git 環境・旧メタデータ）なら「on top of commit」句を
        // 省略し、否定情報の句だけを残す。
        let gits = Vec::new();
        let local = CachedFile {
            snapshots: vec![CachedSnapshot {
                id: "0123456789abcdef".repeat(4),
                captured_ms: 0,
                content: "old\n".into(),
                pinned: false,
                parent: None,
            }],
            reviewed_id: None,
            reviewed_content: None,
        };
        let history = DocumentHistory::assemble_timeline("new\n", gits, local);
        assert_eq!(
            history.revisions[1].summary,
            "akapen local snapshot: uncommitted state captured 1970-01-01T00:00:00Z \
             (not a git object)"
        );
    }

    #[test]
    fn captured_ms_formats_as_utc_iso8601_with_seconds() {
        // epoch ミリ秒 → UTC 秒精度 ISO 8601。うるう日・年境界・遠い未来も含めて
        // civil_from_days の正しさを固定する。
        let cases = [
            (0u64, "1970-01-01T00:00:00Z"),
            (1_750_000_000_000, "2025-06-15T15:06:40Z"),
            (1_709_251_199_000, "2024-02-29T23:59:59Z"),
            (1_735_689_600_000, "2025-01-01T00:00:00Z"),
            (1_786_856_602_000, "2026-08-16T05:03:22Z"),
            (2_000_000_000_000, "2033-05-18T03:33:20Z"),
        ];
        for (ms, expected) in cases {
            assert_eq!(
                local_revision_summary(ms, None),
                format!(
                    "akapen local snapshot: uncommitted state captured {expected} \
                     (not a git object)"
                ),
                "captured_ms = {ms}"
            );
        }
    }

    #[test]
    fn same_revision_matches_on_the_identity_side_only() {
        // 新形式同士: 説明文が違っても identity（「 — 」の左）が同じなら一致。
        assert!(same_revision(
            Some(
                "local:abc — akapen local snapshot: uncommitted state captured \
                 2026-08-16T05:03:22Z (not a git object)"
            ),
            Some(
                "local:abc — akapen local snapshot: uncommitted state captured \
                 2026-08-16T05:03:22Z, on top of commit 720a445 (not a git object)"
            ),
        ));
        // 旧形式（裸の `local:<id>`）で保存済みのコメントが新形式 context に一致する。
        assert!(same_revision(
            Some("local:abc"),
            Some("local:abc — akapen local snapshot: … (not a git object)")
        ));
        assert!(!same_revision(
            Some("local:abc"),
            Some("local:abd — akapen local snapshot: … (not a git object)")
        ));
        // Git 世代は従来どおり `oid — subject` の oid 部で比較される。
        assert!(same_revision(
            Some("0123456789abcdef0123456789abcdef01234567 — simplify intro"),
            Some("0123456789abcdef0123456789abcdef01234567 — different subject"),
        ));
        assert!(!same_revision(
            Some("0123456789abcdef0123456789abcdef01234567 — simplify intro"),
            Some("ffffffffffffffffffffffffffffffffffffffff — simplify intro"),
        ));
        // None ↔ None は一致（Revision 行の無い NOW 同士）、None ↔ Some は不一致。
        assert!(same_revision(None, None));
        assert!(!same_revision(None, Some("local:abc — x")));
        assert!(!same_revision(Some("local:abc — x"), None));
    }

    #[test]
    fn revision_id_splits_only_the_first_separator() {
        // subject 側に「 — 」を含む Git 世代でも identity は先頭の oid のまま。
        assert_eq!(revision_id("abc — fix: a — b"), "abc");
        // 旧形式は区切りが無いので全体が identity。
        assert_eq!(revision_id("local:abc"), "local:abc");
    }

    #[test]
    fn timeline_clamps_at_both_ends() {
        let revision = |name: &str| Revision {
            id: None,
            short_id: name.into(),
            summary: name.into(),
            content: name.into(),
            source: RevisionSource::Git,
        };
        let mut history = DocumentHistory {
            revisions: vec![revision("now"), revision("old")],
            position: 0,
            rendered_position: 0,
            reviewed_id: None,
            reviewed_content: None,
        };
        assert!(!history.move_by(-1));
        assert!(history.move_by(1));
        assert!(!history.move_by(1));
        assert!(history.move_by(-1));
    }

    #[test]
    fn timeline_fraction_increases_from_oldest_toward_now() {
        let revision = |name: &str| Revision {
            id: None,
            short_id: name.into(),
            summary: name.into(),
            content: name.into(),
            source: RevisionSource::Git,
        };
        let mut history = DocumentHistory {
            revisions: vec![revision("now"), revision("middle"), revision("oldest")],
            position: 0,
            rendered_position: 0,
            reviewed_id: None,
            reviewed_content: None,
        };
        assert_eq!(history.label().as_deref(), Some("NOW · 3/3"));
        history.move_by(1);
        assert!(history.label().unwrap().starts_with("COMMIT · 2/3"));
        history.move_by(1);
        assert!(history.label().unwrap().starts_with("COMMIT · 1/3"));
    }

    #[test]
    fn timeline_label_always_identifies_the_baseline_position() {
        let revision = |name: &str| Revision {
            id: None,
            short_id: name.into(),
            summary: name.into(),
            content: name.into(),
            source: RevisionSource::Git,
        };
        let mut history = DocumentHistory {
            revisions: vec![revision("now"), revision("middle"), revision("oldest")],
            position: 0,
            rendered_position: 0,
            reviewed_id: Some("middle".into()),
            reviewed_content: Some("middle".into()),
        };

        assert!(history.label().unwrap().ends_with("base 2/3"));
        history.move_by(1);
        assert!(history.label().unwrap().starts_with("BASELINE · COMMIT · 2/3"));
    }

    #[test]
    fn cursor_follows_the_same_heading() {
        let old = ["# Intro", "a", "## Detail", "one", "two"]
            .map(str::to_string);
        let new = ["preface", "# Intro", "a", "## Detail", "changed", "two"]
            .map(str::to_string);
        assert_eq!(anchored_line(&old, &new, 4), 5);
    }

    #[test]
    fn anchored_line_survives_an_empty_old_document() {
        // History browsing on an empty working tree: no old lines, no
        // headings — the cursor lands at the top of the new document
        // instead of indexing out of bounds.
        let old: Vec<String> = Vec::new();
        let new = ["# New", "line"].map(str::to_string);
        assert_eq!(anchored_line(&old, &new, 3), 0);
        // And the reverse guard still holds: an empty new document.
        let new: Vec<String> = Vec::new();
        assert_eq!(anchored_line(&old, &new, 0), 0);
    }

    #[test]
    fn table_review_marks_only_the_row_containing_the_changed_cell() {
        let old = "| Key | Value |\n| --- | --- |\n| a | one |\n| b | two |\n";
        let new = "| Key | Value |\n| --- | --- |\n| a | one |\n| b | changed |\n";

        let (changed, deleted) = review_transition(true, old, new);

        assert_eq!(changed, [3].into_iter().collect());
        assert!(deleted.is_empty());
    }

    #[test]
    fn table_review_marks_added_and_deleted_rows_individually() {
        let base = "| Key | Value |\n| --- | --- |\n| a | one |\n| b | two |\n";
        let added = "| Key | Value |\n| --- | --- |\n| a | one |\n| new | row |\n| b | two |\n";
        let deleted = "| Key | Value |\n| --- | --- |\n| b | two |\n";

        let (changed, removed) = review_transition(true, base, added);
        assert_eq!(changed, [3].into_iter().collect());
        assert!(removed.is_empty());

        let (changed, removed) = review_transition(true, base, deleted);
        assert!(changed.is_empty());
        assert_eq!(removed, [2].into_iter().collect());
    }

    #[test]
    fn loads_complete_markdown_snapshots_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "akapen@example.invalid"]);
        git(&["config", "user.name", "akapen test"]);
        std::fs::write(&path, "# Doc\n\nfirst\n").unwrap();
        git(&["add", "doc.md"]);
        git(&["commit", "-qm", "first"]);
        std::fs::write(&path, "# Doc\n\nsecond\n").unwrap();
        git(&["commit", "-qam", "second"]);
        std::fs::write(&path, "# Doc\n\nworking\n").unwrap();

        let history = DocumentHistory::load(&path, "# Doc\n\nworking\n", 10);
        assert_eq!(history.revisions.len(), 3);
        assert_eq!(history.revisions[0].content, "# Doc\n\nworking\n");
        assert_eq!(history.revisions[1].summary, "second");
        assert_eq!(history.revisions[1].content, "# Doc\n\nsecond\n");
        assert_eq!(history.revisions[2].content, "# Doc\n\nfirst\n");
    }

    #[test]
    fn loads_previous_observation_as_local_without_git() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "first\n").unwrap();
        let cache = SnapshotCache::at(dir.path().join("cache"));
        cache.record(&path, "first\n").unwrap();

        let history = DocumentHistory::load_cached(&path, "second\n", 10, &cache).unwrap();

        assert_eq!(history.revisions.len(), 2);
        assert_eq!(history.revisions[0].source, RevisionSource::Now);
        assert_eq!(history.revisions[1].source, RevisionSource::Local);
        assert_eq!(history.revisions[1].content, "first\n");
    }

    #[test]
    fn content_equal_local_and_git_generations_are_shown_once_as_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "akapen@example.invalid"]);
        git(&["config", "user.name", "akapen test"]);
        std::fs::write(&path, "committed\n").unwrap();
        git(&["add", "doc.md"]);
        git(&["commit", "-qm", "committed"]);

        let cache = SnapshotCache::at(dir.path().join("cache"));
        cache.record(&path, "committed\n").unwrap();
        let history = DocumentHistory::load_cached(&path, "working\n", 10, &cache).unwrap();

        assert_eq!(history.revisions.len(), 2);
        assert_eq!(history.revisions[1].source, RevisionSource::Git);
        assert_eq!(history.revisions[1].summary, "committed");
        assert_eq!(history.reviewed_content.as_deref(), Some("committed\n"));
    }

    #[test]
    fn first_observation_remains_the_baseline_even_when_git_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "akapen@example.invalid"]);
        git(&["config", "user.name", "akapen test"]);
        std::fs::write(&path, "committed\n").unwrap();
        git(&["add", "doc.md"]);
        git(&["commit", "-qm", "committed"]);

        let cache = SnapshotCache::at(dir.path().join("cache"));
        let history = DocumentHistory::load_cached(&path, "working\n", 10, &cache).unwrap();

        assert_eq!(history.reviewed_content.as_deref(), Some("working\n"));
    }

    #[derive(Deserialize)]
    struct FixtureGit {
        id: String,
        short_id: String,
        summary: String,
        content: String,
    }

    #[derive(Deserialize)]
    struct FixtureSnapshot {
        id: String,
        captured_ms: u64,
        pinned: bool,
        parent: Option<String>,
        content: String,
    }

    #[derive(Deserialize)]
    struct FixtureScenario {
        name: String,
        live: String,
        gits_newest_first: Vec<FixtureGit>,
        snapshots_oldest_first: Vec<FixtureSnapshot>,
        expected_newest_first_ids: Vec<String>,
    }

    #[derive(Deserialize)]
    struct TimelineOrderFixtures {
        scenarios: Vec<FixtureScenario>,
    }

    #[test]
    fn timeline_order_fixtures_follow_the_parent_contract() {
        // fixture（正典）は実 git を立てずに配置ロジックを検証する。gits の id が
        // 擬似 oid（"A" / "B" / "R"）なので、git を呼ばない純粋な assemble_timeline
        // （load_with_local の実体）へ fixture の入力を直接渡す。
        let root = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(root).join("testdata/timeline-order-fixtures.json");
        let fixtures: TimelineOrderFixtures =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            fixtures.scenarios.len(),
            8,
            "fixture のシナリオ数が変わったら契約の見直しが必要"
        );

        for scenario in fixtures.scenarios {
            let gits: Vec<Revision> = scenario
                .gits_newest_first
                .iter()
                .map(|git| Revision {
                    id: Some(git.id.clone()),
                    short_id: git.short_id.clone(),
                    summary: git.summary.clone(),
                    content: git.content.clone(),
                    source: RevisionSource::Git,
                })
                .collect();
            let snapshots: Vec<CachedSnapshot> = scenario
                .snapshots_oldest_first
                .iter()
                .map(|snapshot| CachedSnapshot {
                    id: snapshot.id.clone(),
                    captured_ms: snapshot.captured_ms,
                    content: snapshot.content.clone(),
                    pinned: snapshot.pinned,
                    parent: snapshot.parent.clone(),
                })
                .collect();
            let local = CachedFile {
                snapshots,
                reviewed_id: None,
                reviewed_content: None,
            };
            let history = DocumentHistory::assemble_timeline(&scenario.live, gits, local);
            let ids: Vec<String> = history
                .revisions
                .iter()
                .map(|revision| revision.id.as_deref().unwrap_or("now").to_string())
                .collect();
            assert_eq!(
                ids, scenario.expected_newest_first_ids,
                "scenario: {}",
                scenario.name
            );
        }
    }

    #[test]
    fn head_oid_needs_a_tracked_file_with_a_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.md");
        std::fs::write(&path, "one\n").unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };

        // 非 git 環境 → None（従来の観測順のまま）。
        assert_eq!(head_oid(&path), None);

        // git 環境でもコミットが無ければ HEAD が存在しない → None。
        git(&["init", "-q"]);
        git(&["config", "user.email", "akapen@example.invalid"]);
        git(&["config", "user.name", "akapen test"]);
        git(&["add", "doc.md"]);
        assert_eq!(head_oid(&path), None);

        // untracked なファイルには血統アンカーが無い → None。
        let untracked = dir.path().join("untracked.md");
        std::fs::write(&untracked, "u\n").unwrap();
        assert_eq!(head_oid(&untracked), None);

        // tracked かつコミット済み → rev-parse HEAD のフル oid（%H と一致）。
        git(&["commit", "-qm", "first"]);
        assert_eq!(head_oid(&untracked), None, "コミット後も untracked は None");
        let head = head_oid(&path).expect("tracked file in a repo with commits");
        let expected = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(head, String::from_utf8(expected.stdout).unwrap().trim());
    }

}
