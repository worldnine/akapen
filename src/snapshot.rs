//! Bounded, Git-independent document snapshot cache.
//!
//! The UI always consumes complete documents. Persistence follows the same
//! model: gzip-compressed, content-addressed blobs plus a small per-file JSON
//! index. The history layer promotes content-equal snapshots to Git revisions;
//! redundant local blobs remain eligible for bounded garbage collection.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FORMAT_VERSION: u32 = 1;
const MAX_FILE_SNAPSHOTS: usize = 32;
const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct SnapshotCache {
    root: PathBuf,
    max_file_snapshots: usize,
    max_cache_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedSnapshot {
    pub(crate) id: String,
    pub(crate) captured_ms: u64,
    pub(crate) content: String,
    pub(crate) pinned: bool,
    /// 観測時点の HEAD commit oid（git 環境）。タイムラインを血統順に並べる
    /// ための親アンカー。非 git 環境・不明・pin 由来の追加では None。
    pub(crate) parent: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CachedFile {
    /// Oldest first. The newest entry is the most recently observed text.
    pub(crate) snapshots: Vec<CachedSnapshot>,
    pub(crate) reviewed_id: Option<String>,
    pub(crate) reviewed_content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileIndex {
    version: u32,
    path: String,
    reviewed: Option<String>,
    snapshots: Vec<SnapshotMeta>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotMeta {
    id: String,
    captured_ms: u64,
    #[serde(default)]
    pinned: bool,
    /// 観測時点の HEAD commit oid（git 環境）。旧キャッシュでは無いので
    /// `#[serde(default)]` で None として読む。
    #[serde(default)]
    parent: Option<String>,
}

impl SnapshotCache {
    /// Resolve the per-user cache without touching it. Tests leave this off
    /// unless they explicitly construct a cache under a TempDir.
    pub(crate) fn discover() -> Option<Self> {
        let base = std::env::var_os("AKAPEN_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".cache"))
            })?;
        Some(Self::at(base.join("akapen").join("snapshots")))
    }

    pub(crate) fn at(root: PathBuf) -> Self {
        Self {
            root,
            max_file_snapshots: MAX_FILE_SNAPSHOTS,
            max_cache_bytes: MAX_CACHE_BYTES,
        }
    }

    #[cfg(test)]
    fn with_limits(root: PathBuf, per_file: usize, total_bytes: u64) -> Self {
        Self {
            root,
            max_file_snapshots: per_file,
            max_cache_bytes: total_bytes,
        }
    }

    /// Observe `content`, creating a LOCAL generation only when its content
    /// differs from the latest cached generation. The first observation is
    /// also the initial review baseline.
    pub(crate) fn record(&self, path: &Path, content: &str) -> Result<CachedFile> {
        self.ensure_dirs()?;
        let canonical = canonical_path(path);
        let mut index = self.load_index(&canonical)?.unwrap_or_else(|| FileIndex {
            version: FORMAT_VERSION,
            path: canonical.to_string_lossy().into_owned(),
            reviewed: None,
            snapshots: Vec::new(),
        });
        dedupe_snapshots(&mut index);
        let id = content_id(content.as_bytes());
        if index.snapshots.last().is_none_or(|last| last.id != id) {
            self.write_blob(&id, content)?;
            let mut snapshot = index
                .snapshots
                .iter()
                .position(|snapshot| snapshot.id == id)
                .map(|position| index.snapshots.remove(position))
                .unwrap_or(SnapshotMeta {
                    id: id.clone(),
                    captured_ms: now_ms(),
                    pinned: false,
                    parent: None,
                });
            // Re-observing known content does not create a duplicate
            // generation, but it does make that content the newest LOCAL.
            snapshot.captured_ms = now_ms();
            index.snapshots.push(snapshot);
        }
        if index.reviewed.is_none() {
            index.reviewed = Some(id);
        }
        trim_file_index(&mut index, self.max_file_snapshots);
        self.save_index(&canonical, &index)?;
        self.gc_global()?;
        self.materialize(&index)
    }

    // STUB: feat/snapshot-parent の本実装にマージ時に置き換わる（parent 引数は捨てる）。
    // history 側が参照する次のコミットまでは未使用なので allow を付ける。
    #[allow(dead_code)]
    pub(crate) fn record_with_parent(
        &self,
        path: &Path,
        content: &str,
        _parent: Option<String>,
    ) -> Result<CachedFile> {
        self.record(path, content)
    }

    /// Start a new UI session for a file. Comment pins belong to the
    /// process that created them (comments themselves are not persisted),
    /// so stale pins from an earlier or crashed session are released here.
    pub(crate) fn open(&self, path: &Path, content: &str) -> Result<CachedFile> {
        let _ = self.record(path, content)?;
        let canonical = canonical_path(path);
        let mut index = self
            .load_index(&canonical)?
            .context("snapshot index disappeared while opening")?;
        for snapshot in &mut index.snapshots {
            snapshot.pinned = false;
        }
        trim_file_index(&mut index, self.max_file_snapshots);
        self.save_index(&canonical, &index)?;
        self.gc_global()?;
        self.materialize(&index)
    }

    // STUB: 同上
    #[allow(dead_code)]
    pub(crate) fn open_with_parent(
        &self,
        path: &Path,
        content: &str,
        _parent: Option<String>,
    ) -> Result<CachedFile> {
        self.open(path, content)
    }

    #[cfg(test)]
    pub(crate) fn load(&self, path: &Path) -> Result<CachedFile> {
        let canonical = canonical_path(path);
        let Some(index) = self.load_index(&canonical)? else {
            return Ok(CachedFile::default());
        };
        self.materialize(&index)
    }

    /// Move the durable review baseline to `content`.
    pub(crate) fn acknowledge(&self, path: &Path, content: &str) -> Result<CachedFile> {
        let _ = self.record(path, content)?;
        let canonical = canonical_path(path);
        let mut index = self
            .load_index(&canonical)?
            .context("snapshot index disappeared while acknowledging")?;
        index.reviewed = Some(content_id(content.as_bytes()));
        trim_file_index(&mut index, self.max_file_snapshots);
        self.save_index(&canonical, &index)?;
        self.gc_global()?;
        self.materialize(&index)
    }

    /// Select an already-viewed historical generation as the durable
    /// review baseline without making it the newest LOCAL observation.
    pub(crate) fn set_baseline(&self, path: &Path, content: &str) -> Result<CachedFile> {
        self.ensure_dirs()?;
        let canonical = canonical_path(path);
        let mut index = self.load_index(&canonical)?.unwrap_or_else(|| FileIndex {
            version: FORMAT_VERSION,
            path: canonical.to_string_lossy().into_owned(),
            reviewed: None,
            snapshots: Vec::new(),
        });
        dedupe_snapshots(&mut index);
        let id = content_id(content.as_bytes());
        // A Git baseline need not become a LOCAL timeline entry, but keep a
        // compressed body so marks remain reproducible if Git later moves.
        self.write_blob(&id, content)?;
        index.reviewed = Some(id);
        self.save_index(&canonical, &index)?;
        self.gc_global()?;
        self.materialize(&index)
    }

    /// A comment makes its exact document generation non-evictable.
    pub(crate) fn pin(&self, path: &Path, content: &str) -> Result<()> {
        self.ensure_dirs()?;
        let canonical = canonical_path(path);
        let mut index = self.load_index(&canonical)?.unwrap_or_else(|| FileIndex {
            version: FORMAT_VERSION,
            path: canonical.to_string_lossy().into_owned(),
            reviewed: None,
            snapshots: Vec::new(),
        });
        dedupe_snapshots(&mut index);
        let id = content_id(content.as_bytes());
        if let Some(snapshot) = index.snapshots.iter_mut().find(|s| s.id == id) {
            snapshot.pinned = true;
        } else {
            self.write_blob(&id, content)?;
            index.snapshots.push(SnapshotMeta {
                id: id.clone(),
                captured_ms: now_ms(),
                pinned: true,
                parent: None,
            });
        }
        if index.reviewed.is_none() {
            index.reviewed = Some(id);
        }
        trim_file_index(&mut index, self.max_file_snapshots);
        self.save_index(&canonical, &index)?;
        self.gc_global()?;
        Ok(())
    }

    fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.root.join("files"))
            .with_context(|| format!("create {}", self.root.display()))?;
        fs::create_dir_all(self.root.join("blobs"))
            .with_context(|| format!("create {}", self.root.display()))?;
        Ok(())
    }

    fn index_path(&self, path: &Path) -> PathBuf {
        self.root
            .join("files")
            .join(format!("{}.json", content_id(path.to_string_lossy().as_bytes())))
    }

    fn blob_path(&self, id: &str) -> PathBuf {
        self.root.join("blobs").join(format!("{id}.gz"))
    }

    fn load_index(&self, path: &Path) -> Result<Option<FileIndex>> {
        let index_path = self.index_path(path);
        let bytes = match fs::read(&index_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", index_path.display())),
        };
        let index: FileIndex = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", index_path.display()))?;
        if index.version != FORMAT_VERSION {
            anyhow::bail!("unsupported snapshot index version {}", index.version);
        }
        Ok(Some(index))
    }

    fn save_index(&self, path: &Path, index: &FileIndex) -> Result<()> {
        self.ensure_dirs()?;
        let target = self.index_path(path);
        let temp = target.with_extension(format!("json.tmp-{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(index)?;
        fs::write(&temp, bytes).with_context(|| format!("write {}", temp.display()))?;
        fs::rename(&temp, &target)
            .with_context(|| format!("replace {}", target.display()))?;
        Ok(())
    }

    fn write_blob(&self, id: &str, content: &str) -> Result<()> {
        let target = self.blob_path(id);
        if target.exists() {
            return Ok(());
        }
        let temp = target.with_extension(format!("gz.tmp-{}", std::process::id()));
        let file = File::create(&temp).with_context(|| format!("create {}", temp.display()))?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(content.as_bytes())?;
        encoder.finish()?.sync_all()?;
        match fs::rename(&temp, &target) {
            Ok(()) => Ok(()),
            // Another akapen process may have won the content-address race.
            Err(_) if target.exists() => {
                let _ = fs::remove_file(temp);
                Ok(())
            }
            Err(e) => Err(e).with_context(|| format!("replace {}", target.display())),
        }
    }

    fn read_blob(&self, id: &str) -> Result<String> {
        let path = self.blob_path(id);
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let mut decoder = GzDecoder::new(file);
        let mut content = String::new();
        decoder
            .read_to_string(&mut content)
            .with_context(|| format!("decompress {}", path.display()))?;
        Ok(content)
    }

    fn materialize(&self, index: &FileIndex) -> Result<CachedFile> {
        let mut snapshots = Vec::with_capacity(index.snapshots.len());
        for snapshot in &index.snapshots {
            // A damaged/missing old blob should not make the live document
            // unusable; skip that generation while retaining the others.
            let Ok(content) = self.read_blob(&snapshot.id) else {
                continue;
            };
            snapshots.push(CachedSnapshot {
                id: snapshot.id.clone(),
                captured_ms: snapshot.captured_ms,
                content,
                pinned: snapshot.pinned,
                parent: snapshot.parent.clone(),
            });
        }
        let reviewed_content = index
            .reviewed
            .as_deref()
            .and_then(|id| snapshots.iter().find(|snapshot| snapshot.id == id))
            .map(|snapshot| snapshot.content.clone())
            .or_else(|| index.reviewed.as_deref().and_then(|id| self.read_blob(id).ok()));
        Ok(CachedFile {
            snapshots,
            reviewed_id: index.reviewed.clone(),
            reviewed_content,
        })
    }

    /// Enforce the global byte limit by deleting the oldest reviewed,
    /// unpinned generations. NOW, the review baseline, and every generation
    /// newer than that baseline are protected.
    fn gc_global(&self) -> Result<()> {
        let files_dir = self.root.join("files");
        let Ok(entries) = fs::read_dir(&files_dir) else {
            return Ok(());
        };
        let mut indices: Vec<(PathBuf, FileIndex)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let Ok(index) = serde_json::from_slice::<FileIndex>(&bytes) else { continue };
            indices.push((path, index));
        }

        // Per-file trimming removes index entries first. Sweep blobs that
        // no remaining file references, otherwise the on-disk cache could
        // grow even though every JSON index stayed within its limit.
        let referenced: HashSet<String> = indices
            .iter()
            .flat_map(|(_, index)| {
                index
                    .snapshots
                    .iter()
                    .map(|snapshot| snapshot.id.clone())
                    .chain(index.reviewed.iter().cloned())
            })
            .collect();
        if let Ok(blobs) = fs::read_dir(self.root.join("blobs")) {
            for entry in blobs.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("gz") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if !referenced.contains(id) {
                    let _ = fs::remove_file(path);
                }
            }
        }

        let mut blob_sizes = HashMap::new();
        for (_, index) in &indices {
            for id in index
                .snapshots
                .iter()
                .map(|snapshot| &snapshot.id)
                .chain(index.reviewed.iter())
            {
                blob_sizes.entry(id.clone()).or_insert_with(|| {
                    fs::metadata(self.blob_path(id))
                        .map(|m| m.len())
                        .unwrap_or(0)
                });
            }
        }
        let mut total: u64 = blob_sizes.values().sum();
        if total <= self.max_cache_bytes {
            return Ok(());
        }

        let mut candidates = Vec::new();
        for (file_index, (_, index)) in indices.iter().enumerate() {
            let reviewed_pos = index
                .reviewed
                .as_ref()
                .and_then(|id| index.snapshots.iter().position(|s| &s.id == id));
            for (snapshot_index, snapshot) in index.snapshots.iter().enumerate() {
                let latest = snapshot_index + 1 == index.snapshots.len();
                let unreviewed = reviewed_pos.is_none_or(|pos| snapshot_index >= pos);
                if !latest && !unreviewed && !snapshot.pinned {
                    candidates.push((snapshot.captured_ms, file_index, snapshot_index));
                }
            }
        }
        candidates.sort_by_key(|candidate| candidate.0);

        let mut removals: HashMap<usize, HashSet<usize>> = HashMap::new();
        for (_, file_index, snapshot_index) in candidates {
            if total <= self.max_cache_bytes {
                break;
            }
            let id = indices[file_index].1.snapshots[snapshot_index].id.clone();
            removals.entry(file_index).or_default().insert(snapshot_index);
            // Charge the blob only if no other still-retained entry refers to it.
            let other_ref = indices.iter().enumerate().any(|(fi, (_, index))| {
                index.reviewed.as_deref() == Some(&id)
                    || index.snapshots.iter().enumerate().any(|(si, snapshot)| {
                        snapshot.id == id
                            && !(fi == file_index && si == snapshot_index)
                            && !removals.get(&fi).is_some_and(|set| set.contains(&si))
                    })
            });
            if !other_ref {
                total = total.saturating_sub(blob_sizes.get(&id).copied().unwrap_or(0));
            }
        }

        for (file_index, remove) in removals {
            let (index_path, index) = &mut indices[file_index];
            let removed_ids: Vec<String> = index
                .snapshots
                .iter()
                .enumerate()
                .filter(|(i, _)| remove.contains(i))
                .map(|(_, snapshot)| snapshot.id.clone())
                .collect();
            index.snapshots = index
                .snapshots
                .drain(..)
                .enumerate()
                .filter_map(|(i, snapshot)| (!remove.contains(&i)).then_some(snapshot))
                .collect();
            let bytes = serde_json::to_vec_pretty(index)?;
            let temp = index_path.with_extension(format!("json.tmp-{}", std::process::id()));
            fs::write(&temp, bytes)?;
            fs::rename(temp, &*index_path)?;
            for id in removed_ids {
                let still_used = indices.iter().any(|(_, other)| {
                    other.reviewed.as_deref() == Some(&id)
                        || other.snapshots.iter().any(|snapshot| snapshot.id == id)
                });
                if !still_used {
                    let _ = fs::remove_file(self.blob_path(&id));
                }
            }
        }
        Ok(())
    }
}

/// Old development builds could append the same content again when a
/// historical comment pinned it. Keep the last observation, merge the pin,
/// and restore the one-content/one-LOCAL invariant while reading the index.
fn dedupe_snapshots(index: &mut FileIndex) {
    let pinned: HashSet<String> = index
        .snapshots
        .iter()
        .filter(|snapshot| snapshot.pinned)
        .map(|snapshot| snapshot.id.clone())
        .collect();
    let mut seen = HashSet::new();
    let mut unique: Vec<SnapshotMeta> = index
        .snapshots
        .drain(..)
        .rev()
        .filter(|snapshot| seen.insert(snapshot.id.clone()))
        .collect();
    unique.reverse();
    for snapshot in &mut unique {
        snapshot.pinned = pinned.contains(&snapshot.id);
    }
    index.snapshots = unique;
}

fn trim_file_index(index: &mut FileIndex, limit: usize) {
    while index.snapshots.len() > limit {
        let reviewed_pos = index
            .reviewed
            .as_ref()
            .and_then(|id| index.snapshots.iter().position(|s| &s.id == id));
        let removable = index
            .snapshots
            .iter()
            .enumerate()
            .find(|(i, snapshot)| {
                let latest = *i + 1 == index.snapshots.len();
                let unreviewed = reviewed_pos.is_none_or(|pos| *i >= pos);
                !latest && !unreviewed && !snapshot.pinned
            })
            .map(|(i, _)| i);
        let Some(removable) = removable else { break };
        index.snapshots.remove(removable);
    }
}

fn canonical_path(path: &Path) -> PathBuf {
    if let Ok(path) = fs::canonicalize(path) {
        return path;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Stable content address used by blobs, per-file indices, review baselines,
/// and LOCAL revision references.
pub(crate) fn content_id(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_deduplicates_and_restores_review_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        fs::write(&file, "one\n").unwrap();
        let cache = SnapshotCache::at(dir.path().join("cache"));

        let first = cache.record(&file, "one\n").unwrap();
        assert_eq!(first.snapshots.len(), 1);
        assert_eq!(first.reviewed_content.as_deref(), Some("one\n"));
        let same = cache.record(&file, "one\n").unwrap();
        assert_eq!(same.snapshots.len(), 1);
        let second = cache.record(&file, "two\n").unwrap();
        assert_eq!(second.snapshots.len(), 2);
        assert_eq!(second.reviewed_content.as_deref(), Some("one\n"));

        let reopened = cache.load(&file).unwrap();
        assert_eq!(reopened, second);
    }

    #[test]
    fn reobserving_or_pinning_known_content_does_not_duplicate_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        fs::write(&file, "one").unwrap();
        let cache = SnapshotCache::at(dir.path().join("cache"));
        cache.record(&file, "one").unwrap();
        cache.record(&file, "two").unwrap();
        cache.record(&file, "one").unwrap();
        cache.pin(&file, "two").unwrap();

        let loaded = cache.load(&file).unwrap();
        assert_eq!(loaded.snapshots.len(), 2);
        assert_eq!(loaded.snapshots[0].content, "two");
        assert!(loaded.snapshots[0].pinned);
        assert_eq!(loaded.snapshots[1].content, "one");
    }

    #[test]
    fn acknowledge_moves_the_durable_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        fs::write(&file, "base\n").unwrap();
        let cache = SnapshotCache::at(dir.path().join("cache"));
        cache.record(&file, "base\n").unwrap();
        cache.record(&file, "changed\n").unwrap();
        let acknowledged = cache.acknowledge(&file, "changed\n").unwrap();
        assert_eq!(acknowledged.reviewed_content.as_deref(), Some("changed\n"));
    }

    #[test]
    fn pinned_and_unreviewed_generations_survive_small_per_file_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        fs::write(&file, "zero").unwrap();
        let cache = SnapshotCache::with_limits(dir.path().join("cache"), 2, u64::MAX);
        cache.record(&file, "zero").unwrap();
        cache.acknowledge(&file, "zero").unwrap();
        cache.record(&file, "one").unwrap();
        cache.pin(&file, "one").unwrap();
        cache.record(&file, "two").unwrap();
        let loaded = cache.load(&file).unwrap();
        // The limit is soft while every generation is protected.
        assert_eq!(loaded.snapshots.len(), 3);
        assert!(loaded.snapshots[1].pinned);
    }

    #[test]
    fn opening_a_new_session_releases_old_comment_pins() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        fs::write(&file, "zero").unwrap();
        let cache = SnapshotCache::at(dir.path().join("cache"));
        cache.record(&file, "zero").unwrap();
        cache.pin(&file, "zero").unwrap();
        assert!(cache.load(&file).unwrap().snapshots[0].pinned);

        let reopened = cache.open(&file, "zero").unwrap();
        assert!(!reopened.snapshots[0].pinned);
    }

    #[test]
    fn global_limit_evicts_only_old_acknowledged_generations() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        fs::write(&file, "zero").unwrap();
        let cache = SnapshotCache::with_limits(dir.path().join("cache"), 32, 1);
        cache.record(&file, "zero").unwrap();
        cache.record(&file, "one").unwrap();
        cache.acknowledge(&file, "one").unwrap();

        let loaded = cache.load(&file).unwrap();
        assert_eq!(loaded.reviewed_content.as_deref(), Some("one"));
        assert_eq!(loaded.snapshots.len(), 1);
        assert_eq!(loaded.snapshots[0].content, "one");
    }

    #[test]
    fn per_file_trimming_also_removes_orphaned_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.md");
        fs::write(&file, "zero").unwrap();
        let root = dir.path().join("cache");
        let cache = SnapshotCache::with_limits(root.clone(), 2, u64::MAX);
        cache.record(&file, "zero").unwrap();
        cache.record(&file, "one").unwrap();
        cache.record(&file, "two").unwrap();
        cache.acknowledge(&file, "two").unwrap();
        cache.record(&file, "three").unwrap();

        assert_eq!(cache.load(&file).unwrap().snapshots.len(), 2);
        assert_eq!(
            fs::read_dir(root.join("blobs")).unwrap().count(),
            2,
            "unreferenced compressed bodies are swept"
        );
    }

    #[test]
    fn content_ids_are_stable_and_content_sensitive() {
        assert_eq!(content_id(b"abc"), content_id(b"abc"));
        assert_ne!(content_id(b"abc"), content_id(b"abd"));
        assert_ne!(content_id(b"abc"), content_id(b"abc\n"));
    }
}
