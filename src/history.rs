//! Markdown history as a sequence of complete, renderable documents.
//!
//! Git is deliberately kept behind this module.  The UI consumes document
//! snapshots, not hunks: moving through time always produces a complete
//! Markdown source that can be rendered normally.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use similar::{DiffTag, TextDiff, TextDiffConfig};

use crate::snapshot::{CachedFile, SnapshotCache};

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
            RevisionSource::Local => self.id.clone(),
            RevisionSource::Git => Some(format!(
                "{} — {}",
                self.id.as_deref().unwrap_or(&self.short_id),
                self.summary
            )),
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BlockKind {
    Heading,
    Paragraph,
    List,
    Quote,
    Code,
    Table,
}

#[derive(Clone, Debug)]
struct MarkdownBlock {
    start: usize,
    end: usize,
    kind: BlockKind,
    /// Keeps row matching inside the same table. Zero for non-table blocks.
    scope: u64,
    id: u64,
    content: String,
}

/// A block that existed in the previous revision but not the next one.
/// It is rendered dimly at `anchor` for a moment before being collapsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeletedBlock {
    pub(crate) anchor: usize,
    pub(crate) content: String,
}

/// Semantic block transition: exact block IDs keep moved/unchanged blocks
/// stable, fuzzy matches identify rewrites, and unmatched old blocks become
/// short-lived deletion ghosts.
pub(crate) fn block_transition(
    old: &[String],
    new: &[String],
) -> (HashSet<usize>, Vec<DeletedBlock>) {
    semantic_transition(old, new, false)
}

/// Review marks use the same semantic matching as history animation, except
/// that a Markdown table is split into source rows. A one-cell edit should
/// send the reviewer to its row, while time travel may still animate the
/// rendered table as one layout block.
fn review_block_transition(
    old: &[String],
    new: &[String],
) -> (HashSet<usize>, Vec<DeletedBlock>) {
    semantic_transition(old, new, true)
}

fn semantic_transition(
    old: &[String],
    new: &[String],
    split_table_rows: bool,
) -> (HashSet<usize>, Vec<DeletedBlock>) {
    let old_blocks = markdown_blocks(old, split_table_rows);
    let new_blocks = markdown_blocks(new, split_table_rows);
    let mut matched_old = vec![None; old_blocks.len()];
    let mut used_new = HashSet::new();
    let mut by_id: HashMap<u64, Vec<usize>> = HashMap::new();
    for (index, block) in new_blocks.iter().enumerate() {
        by_id.entry(block.id).or_default().push(index);
    }

    // Stable IDs first: an unchanged block remains the same object even if
    // another block was inserted above it or it moved within the document.
    for (old_index, block) in old_blocks.iter().enumerate() {
        if let Some(candidates) = by_id.get(&block.id)
            && let Some(&new_index) = candidates.iter().find(|index| !used_new.contains(*index))
        {
            matched_old[old_index] = Some(new_index);
            used_new.insert(new_index);
        }
    }

    let mut changed = HashSet::new();
    // Rewritten blocks keep their identity when kind and text remain
    // recognizably similar. The whole rendered block lights, not just the
    // individual source lines selected by a line diff.
    //
    // The fuzzy pass is the expensive part of a transition: each
    // candidate pair runs a full Myers diff, which is quadratic in the
    // block lengths. Two exact pre-filters keep the pass linear-ish on
    // documents where a rewrite reshuffles long blocks (a 1 KB pair
    // costs milliseconds; thousands of pairs stall the render).
    //
    // The similarity ratio is at most 2·min(a,b)/(a+b), so a pair that
    // cannot reach the 0.55 threshold on that bound is skipped. Tighter
    // still: the longest common subsequence can use each character at
    // most as often as either text contains it, so
    //   LCS ≤ Σ_c min(freq_a(c), freq_b(c)) =: inter,
    // giving the bound ratio ≤ 2·inter/(a+b). The character
    // histograms are computed once per block (not per pair), so the
    // filter costs a few map lookups per candidate — and it is exact:
    // a pair that passes both bounds is diffed in full and judged on
    // its true ratio. Lengths are CHARACTER counts, matching similar's
    // ratio denominator (byte counts would underestimate the bound on
    // CJK text and wrongly reject real rewrites).
    let old_hist: Vec<(usize, HashMap<char, u32>)> = old_blocks
        .iter()
        .map(|b| char_histogram(&b.content))
        .collect();
    let new_hist: Vec<(usize, HashMap<char, u32>)> = new_blocks
        .iter()
        .map(|b| char_histogram(&b.content))
        .collect();
    let fuzzy_start = std::time::Instant::now();
    for (old_index, old_block) in old_blocks.iter().enumerate() {
        if matched_old[old_index].is_some() {
            continue;
        }
        let candidates: Vec<usize> = new_blocks
            .iter()
            .enumerate()
            .filter(|(index, block)| {
                // The budget is checked PER CANDIDATE, not just per
                // block: on CJK documents every paragraph shares the
                // same character distribution, the histogram bound lets
                // most candidates through, and a single block's
                // candidate list could otherwise burn the whole budget
                // (and then some — the loop below breaks only between
                // blocks).
                if fuzzy_start.elapsed() > FUZZY_TOTAL_BUDGET {
                    return false;
                }
                if used_new.contains(index)
                    || block.kind != old_block.kind
                    || block.scope != old_block.scope
                {
                    return false;
                }
                let (a_len, a_hist) = &old_hist[old_index];
                let (b_len, b_hist) = &new_hist[*index];
                let bound = histogram_ratio_bound(a_hist, b_hist, *a_len, *b_len);
                bound >= 0.55
            })
            .map(|(index, _)| index)
            .collect();
        // Length-proximity order: closest-size candidates first, so the
        // genuine rewrite (if any) is evaluated before the budget runs
        // out. On CJK documents the histogram bound admits most
        // same-kind candidates (every paragraph shares the
        // kana/kanji distribution), so the order — not the filter — is
        // what keeps the pass within budget there. The full candidate
        // list is still scanned for the best match while the budget
        // allows — ordering never changes which match wins, only which
        // candidates get a chance at all.
        let mut candidates: Vec<usize> = candidates;
        candidates.sort_by_key(|&index| {
            let (a_chars, _) = &old_hist[old_index];
            let (b_chars, _) = &new_hist[index];
            a_chars.abs_diff(*b_chars)
        });
        let mut best: Option<(usize, f32)> = None;
        for index in candidates {
            if fuzzy_start.elapsed() > FUZZY_TOTAL_BUDGET {
                break;
            }
            // A full Myers diff is quadratic in the block lengths, and
            // the surviving pairs can be multi-KB tables whose
            // character histograms overlap heavily. The deadline
            // bounds the per-pair cost: similar returns its best
            // alignment found so far, which for genuinely similar
            // blocks is already representative. Genuinely different
            // blocks rarely outrun it, so the rewrite decision
            // barely moves — and a missed rewrite degrades to a
            // brief deletion ghost, never to a stall.
            let block = &new_blocks[index];
            let ratio = TextDiffConfig::default()
                .timeout(DIFF_DEADLINE)
                .diff_chars(&old_block.content, &block.content)
                .ratio();
            if ratio >= 0.55 {
                // The candidates run in length-proximity order, so the
                // first pass is effectively the best pass: taking it
                // immediately (instead of scanning the whole list for
                // the max) keeps the budget for the remaining blocks.
                // A slightly weaker match may win in rare reshuffled
                // documents; the alternative — full scans — starves
                // later blocks entirely on CJK texts.
                best = Some((index, ratio));
                break;
            }
        }
        if let Some((new_index, ratio)) = best
            && ratio >= 0.55
        {
            matched_old[old_index] = Some(new_index);
            used_new.insert(new_index);
            changed.extend(new_blocks[new_index].start..=new_blocks[new_index].end);
        }
    }

    for (index, block) in new_blocks.iter().enumerate() {
        if !used_new.contains(&index) {
            changed.extend(block.start..=block.end);
        }
    }

    let mut deleted = Vec::new();
    for (old_index, block) in old_blocks.iter().enumerate() {
        if matched_old[old_index].is_some() {
            continue;
        }
        let next = matched_old
            .iter()
            .enumerate()
            .skip(old_index + 1)
            .find_map(|(_, mapped)| *mapped)
            .map(|index| new_blocks[index].start);
        let previous = matched_old[..old_index]
            .iter()
            .rev()
            .find_map(|mapped| *mapped)
            .map(|index| new_blocks[index].end.saturating_add(1));
        let anchor = next.or(previous).unwrap_or(0).min(new.len());
        deleted.push(DeletedBlock {
            anchor,
            content: block.content.clone(),
        });
    }
    (changed, deleted)
}

/// Cumulative review marks from an acknowledged full document to NOW.
/// Markdown uses semantic blocks; every other text file uses source lines.
pub(crate) fn review_transition(
    markdown: bool,
    reviewed: &str,
    now: &str,
) -> (HashSet<usize>, HashSet<usize>) {
    let old: Vec<String> = reviewed.lines().map(str::to_string).collect();
    let new: Vec<String> = now.lines().map(str::to_string).collect();
    let (changed, deleted) = if markdown {
        review_block_transition(&old, &new)
    } else {
        line_transition(&old, &new)
    };
    let last = new.len().saturating_sub(1);
    let deleted_before = deleted
        .into_iter()
        .map(|block| block.anchor.min(last))
        .collect();
    (changed, deleted_before)
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

fn markdown_blocks(lines: &[String], split_table_rows: bool) -> Vec<MarkdownBlock> {
    let mut blocks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        if lines[start].trim().is_empty() {
            start += 1;
            continue;
        }
        let trimmed = lines[start].trim_start();
        let kind = if heading_text(&lines[start]).is_some() {
            BlockKind::Heading
        } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            BlockKind::Code
        } else if is_list_line(trimmed) {
            BlockKind::List
        } else if trimmed.starts_with('>') {
            BlockKind::Quote
        } else if looks_like_table(lines, start) {
            BlockKind::Table
        } else {
            BlockKind::Paragraph
        };
        let mut end = start;
        match kind {
            BlockKind::Heading => {}
            BlockKind::Code => {
                let fence = if trimmed.starts_with("~~~") { "~~~" } else { "```" };
                while end + 1 < lines.len() {
                    end += 1;
                    if lines[end].trim_start().starts_with(fence) {
                        break;
                    }
                }
            }
            BlockKind::Table => {
                while end + 1 < lines.len()
                    && !lines[end + 1].trim().is_empty()
                    && lines[end + 1].contains('|')
                {
                    end += 1;
                }
            }
            BlockKind::List => {
                while end + 1 < lines.len()
                    && !lines[end + 1].trim().is_empty()
                    && (is_list_line(lines[end + 1].trim_start())
                        || lines[end + 1].starts_with(' '))
                {
                    end += 1;
                }
            }
            BlockKind::Quote => {
                while end + 1 < lines.len() && lines[end + 1].trim_start().starts_with('>') {
                    end += 1;
                }
            }
            BlockKind::Paragraph => {
                while end + 1 < lines.len()
                    && !lines[end + 1].trim().is_empty()
                    && heading_text(&lines[end + 1]).is_none()
                    && !lines[end + 1].trim_start().starts_with("```")
                    && !lines[end + 1].trim_start().starts_with("~~~")
                {
                    end += 1;
                }
            }
        }
        let table_scope = if split_table_rows && kind == BlockKind::Table {
            let structural_end = (start + 1).min(end);
            let structure = lines[start..=structural_end]
                .join("\n")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let mut hasher = DefaultHasher::new();
            structure.hash(&mut hasher);
            hasher.finish()
        } else {
            0
        };
        let ranges: Vec<(usize, usize)> = if split_table_rows && kind == BlockKind::Table {
            (start..=end).map(|row| (row, row)).collect()
        } else {
            vec![(start, end)]
        };
        for (block_start, block_end) in ranges {
            let content = lines[block_start..=block_end].join("\n");
            let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut hasher = DefaultHasher::new();
            kind.hash(&mut hasher);
            table_scope.hash(&mut hasher);
            normalized.hash(&mut hasher);
            blocks.push(MarkdownBlock {
                start: block_start,
                end: block_end,
                kind,
                scope: table_scope,
                id: hasher.finish(),
                content,
            });
        }
        start = end + 1;
    }
    blocks
}

fn is_list_line(line: &str) -> bool {
    line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("+ ")
        || line
            .split_once(". ")
            .is_some_and(|(number, _)| number.chars().all(|c| c.is_ascii_digit()))
}

fn looks_like_table(lines: &[String], start: usize) -> bool {
    lines[start].contains('|')
        && lines.get(start + 1).is_some_and(|line| {
            let cells = line.trim().trim_matches('|').split('|');
            cells.into_iter().all(|cell| {
                let cell = cell.trim().trim_matches(':');
                cell.len() >= 3 && cell.chars().all(|c| c == '-')
            })
        })
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
        let local = cache.record(path, live)?;
        Ok(Self::load_with_local(path, live, limit, local))
    }

    pub(crate) fn open_cached(
        path: &Path,
        live: &str,
        limit: usize,
        cache: &SnapshotCache,
    ) -> anyhow::Result<Self> {
        let local = cache.open(path, live)?;
        Ok(Self::load_with_local(path, live, limit, local))
    }

    fn load_with_local(path: &Path, live: &str, limit: usize, local: CachedFile) -> Self {
        let mut revisions = vec![Revision {
            id: None,
            short_id: "now".to_string(),
            summary: "working tree".to_string(),
            content: live.to_string(),
            source: RevisionSource::Now,
        }];

        let mut git_revisions = load_git_revisions(path, limit);
        let mut used_git = HashSet::new();
        for snapshot in local.snapshots.iter().rev() {
            if snapshot.content == live
                || revisions.iter().any(|revision| revision.content == snapshot.content)
            {
                continue;
            }
            if let Some((index, revision)) = git_revisions
                .iter()
                .enumerate()
                .find(|(index, revision)| {
                    !used_git.contains(index) && revision.content == snapshot.content
                })
            {
                used_git.insert(index);
                revisions.push(revision.clone());
            } else {
                revisions.push(Revision {
                    id: Some(format!("local:{}", snapshot.id)),
                    short_id: snapshot.id.chars().take(7).collect(),
                    summary: "local snapshot".to_string(),
                    content: snapshot.content.clone(),
                    source: RevisionSource::Local,
                });
            }
        }

        for (index, revision) in git_revisions.drain(..).enumerate() {
            if used_git.contains(&index)
                || revisions.iter().any(|existing| existing.content == revision.content)
            {
                continue;
            }
            revisions.push(revision);
        }

        Self {
            revisions,
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

/// (character count, per-character frequencies) of `text`. The count is
/// the histogram's value sum — the same length similar's ratio divides
/// by, so the bound below is exact for CJK text too.
/// How long one bounded diff may run before similar hands back its best
/// alignment so far: the fuzzy rewrite match in [`semantic_transition`]
/// and the appear-mask alignment in [`crate::inserted_char_ranges`].
/// See those call sites for the trade-off.
/// How long one bounded diff may run before similar hands back its best
/// alignment so far: the fuzzy rewrite match in [`semantic_transition`]
/// and the appear-mask alignment in [`crate::inserted_char_ranges`].
/// See those call sites for the trade-off. 15 ms completes typical
/// paragraph diffs (1 KB pairs cost 2–4 ms) and only truncates the
/// genuinely huge ones.
pub(crate) const DIFF_DEADLINE: std::time::Duration = std::time::Duration::from_millis(8);
/// The whole fuzzy pass stops after this much work, so a pathological
/// document (thousands of similar-length, similar-alphabet blocks, e.g.
/// reordered task lists) can delay a render by seconds at most. Blocks
/// the budget leaves unmatched degrade to deletion ghosts — visible but
/// transient — never to a stall.
const FUZZY_TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_millis(60);

fn char_histogram(text: &str) -> (usize, HashMap<char, u32>) {
    let mut hist: HashMap<char, u32> = HashMap::new();
    for ch in text.chars() {
        *hist.entry(ch).or_insert(0) += 1;
    }
    let count = hist.values().sum::<u32>() as usize;
    (count, hist)
}

/// The tightest cheap upper bound on the similarity ratio of two texts:
/// 2·Σ_c min(freq_a(c), freq_b(c)) / (a+b), where a and b are character
/// counts. The intersection sums the smaller histogram, so a candidate
/// pair costs a handful of lookups.
fn histogram_ratio_bound<K: Eq + std::hash::Hash>(
    a: &HashMap<K, u32>,
    b: &HashMap<K, u32>,
    a_len: usize,
    b_len: usize,
) -> f64 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let intersection: u32 = small
        .iter()
        .map(|(c, n)| (*n).min(large.get(c).copied().unwrap_or(0)))
        .sum();
    2.0 * intersection as f64 / (a_len + b_len) as f64
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
        DocumentHistory, Revision, RevisionSource, anchored_line, block_transition,
        review_transition,
    };
    use crate::snapshot::SnapshotCache;
    use std::process::Command;

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
    fn block_identity_ignores_unchanged_blocks_moved_by_an_insertion() {
        let old = ["# Doc", "", "stable paragraph"].map(str::to_string);
        let new = ["# Doc", "", "new paragraph", "", "stable paragraph"]
            .map(str::to_string);
        let (changed, deleted) = block_transition(&old, &new);
        assert_eq!(changed, [2].into_iter().collect());
        assert!(deleted.is_empty());
    }

    #[test]
    fn rewritten_paragraph_lights_the_whole_block() {
        let old = ["# Doc", "", "This is the old", "paragraph text."]
            .map(str::to_string);
        let new = ["# Doc", "", "This is the newer", "paragraph text."]
            .map(str::to_string);
        let (changed, deleted) = block_transition(&old, &new);
        assert_eq!(changed, [2, 3].into_iter().collect());
        assert!(deleted.is_empty());
    }

    #[test]
    fn deleted_block_becomes_a_ghost_at_the_following_block() {
        let old = ["# Doc", "", "remove me", "", "## Next", "text"]
            .map(str::to_string);
        let new = ["# Doc", "", "## Next", "text"].map(str::to_string);
        let (_, deleted) = block_transition(&old, &new);
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].anchor, 2);
        assert_eq!(deleted[0].content, "remove me");
    }

    #[test]
    fn cjk_rewritten_paragraph_matches_through_the_histogram_bound() {
        // The pre-filter's denominator must count CHARACTERS, not bytes:
        // a CJK paragraph is ~3 bytes per char, and a byte-based bound
        // would wrongly reject a genuine rewrite (the fuzzy match would
        // degrade to a deletion ghost).
        let old = [
            "# Doc",
            "",
            "これはとても長い日本語の段落です。内容はほぼ同じですが、",
            "一部の文字だけが変更されています。",
        ]
        .map(str::to_string);
        let new = [
            "# Doc",
            "",
            "これはとても長い日本語の段落です。内容はほぼ同じですが、",
            "ごく一部の文字だけが変更されています。",
        ]
        .map(str::to_string);
        let (changed, deleted) = block_transition(&old, &new);
        assert!(deleted.is_empty(), "the rewrite keeps its identity");
        assert!(
            changed.contains(&3),
            "the edited line lights: {changed:?}"
        );
    }

    #[test]
    fn dissimilar_blocks_are_filtered_before_the_full_diff() {
        // Same-kind blocks with disjoint character multisets cannot
        // reach the 0.55 ratio; the histogram bound skips the expensive
        // diff and the block is treated as deleted + added.
        let old = ["# Doc", "", "aaaa aaaa aaaa aaaa"].map(str::to_string);
        let new = ["# Doc", "", "bbbb bbbb bbbb bbbb"].map(str::to_string);
        let (changed, deleted) = block_transition(&old, &new);
        assert_eq!(deleted.len(), 1, "the old block ghosts: {deleted:?}");
        assert_eq!(changed, [2].into_iter().collect());
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
    fn table_time_travel_still_treats_the_rendered_table_as_one_block() {
        let old = ["| Key | Value |", "| --- | --- |", "| a | one |", "| b | two |"]
            .map(str::to_string);
        let new = [
            "| Key | Value |",
            "| --- | --- |",
            "| a | one |",
            "| b | changed |",
        ]
        .map(str::to_string);

        let (changed, deleted) = block_transition(&old, &new);

        assert_eq!(changed, [0, 1, 2, 3].into_iter().collect());
        assert!(deleted.is_empty());
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

}
