//! Markdown history as a sequence of complete, renderable documents.
//!
//! Git is deliberately kept behind this module.  The UI consumes document
//! snapshots, not hunks: moving through time always produces a complete
//! Markdown source that can be rendered normally.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use similar::TextDiff;

/// One point on a document's timeline.
#[derive(Clone, Debug)]
pub(crate) struct Revision {
    /// Full commit id. `None` is the live working-tree document.
    pub(crate) id: Option<String>,
    pub(crate) short_id: String,
    pub(crate) summary: String,
    pub(crate) content: String,
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
    let old_blocks = markdown_blocks(old);
    let new_blocks = markdown_blocks(new);
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
    for (old_index, old_block) in old_blocks.iter().enumerate() {
        if matched_old[old_index].is_some() {
            continue;
        }
        let best = new_blocks
            .iter()
            .enumerate()
            .filter(|(index, block)| {
                !used_new.contains(index) && block.kind == old_block.kind
            })
            .map(|(index, block)| {
                let ratio = TextDiff::from_chars(&old_block.content, &block.content).ratio();
                (index, ratio)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
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

fn markdown_blocks(lines: &[String]) -> Vec<MarkdownBlock> {
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
        let content = lines[start..=end].join("\n");
        let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut hasher = DefaultHasher::new();
        kind.hash(&mut hasher);
        normalized.hash(&mut hasher);
        blocks.push(MarkdownBlock {
            start,
            end,
            kind,
            id: hasher.finish(),
            content,
        });
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
        let mut revisions = vec![Revision {
            id: None,
            short_id: "now".to_string(),
            summary: "working tree".to_string(),
            content: live.to_string(),
        }];

        let Some(root) = repo_root(path) else {
            return Self { revisions, position: 0, rendered_position: 0 };
        };
        let abs = absolutize(path);
        let Ok(rel) = abs.strip_prefix(&root) else {
            return Self { revisions, position: 0, rendered_position: 0 };
        };
        let format = "%H%x1f%h%x1f%s";
        let output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["log", "--follow", &format!("--format={format}"), "--"])
            .arg(rel)
            .output();
        let Ok(output) = output else {
            return Self { revisions, position: 0, rendered_position: 0 };
        };
        if !output.status.success() {
            return Self { revisions, position: 0, rendered_position: 0 };
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
            // Do not create a visually empty step when HEAD and the working
            // tree are identical; the first Left press should visibly move.
            if revisions.last().is_some_and(|r| r.content == content) {
                continue;
            }
            revisions.push(Revision {
                id: Some(id.to_string()),
                short_id: short_id.to_string(),
                summary: summary.to_string(),
                content,
            });
        }
        Self { revisions, position: 0, rendered_position: 0 }
    }

    pub(crate) fn current(&self) -> Option<&Revision> {
        self.revisions.get(self.position)
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
        if self.position == 0 {
            Some(format!(
                "NOW · {}/{}",
                chronological,
                self.revisions.len()
            ))
        } else {
            Some(format!(
                "PAST · {}/{} · {} · {}",
                chronological,
                self.revisions.len(),
                revision.short_id,
                revision.summary
            ))
        }
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
    use super::{DocumentHistory, Revision, anchored_line, block_transition};
    use std::process::Command;

    #[test]
    fn timeline_clamps_at_both_ends() {
        let revision = |name: &str| Revision {
            id: None,
            short_id: name.into(),
            summary: name.into(),
            content: name.into(),
        };
        let mut history = DocumentHistory {
            revisions: vec![revision("now"), revision("old")],
            position: 0,
            rendered_position: 0,
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
        };
        let mut history = DocumentHistory {
            revisions: vec![revision("now"), revision("middle"), revision("oldest")],
            position: 0,
            rendered_position: 0,
        };
        assert_eq!(history.label().as_deref(), Some("NOW · 3/3"));
        history.move_by(1);
        assert!(history.label().unwrap().starts_with("PAST · 2/3"));
        history.move_by(1);
        assert!(history.label().unwrap().starts_with("PAST · 1/3"));
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

}
