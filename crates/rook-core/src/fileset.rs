//! Snapshots of a set of files, addressed by content.
//!
//! One type serves both jobs that need it: versioning a skill (its `SKILL.md`
//! plus bundled scripts and references) and checkpointing a workspace before a
//! risky edit.
//!
//! The guards are the point. A well-known agent takes workspace checkpoints by
//! running `git add .`, which on a 45 GB data-science directory pins the CPU and
//! stages tens of thousands of files nobody asked to version. Here a capture
//! declares its budget up front and refuses rather than thrashing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rook_store::{Kind, ObjectId, Store};

use crate::error::{CoreError, Result};

/// A content-addressed manifest: relative path -> object id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileSet {
    /// `skill` or `checkpoint`.
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub captured_at: i64,
    /// Absolute root the paths are relative to, for restore and display.
    pub root: String,
    /// Relative path -> hex object id.
    pub files: BTreeMap<String, String>,
    /// Paths that did not exist when this was captured. A rewind deletes them,
    /// which is the only way to undo a file the agent created.
    #[serde(default)]
    pub absent: Vec<String>,
    pub total_bytes: u64,
    #[serde(default)]
    pub note: Option<String>,
}

/// Budget for a capture. Exceeding any of these is an error, not a slow path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureLimits {
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
    /// Skip paths matching these substrings. Applied before the counters, so a
    /// `target/` or `node_modules/` never counts against the budget.
    pub exclude: Vec<String>,
    /// Honour `.gitignore` and friends while walking.
    pub respect_ignore_files: bool,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            max_files: 5_000,
            max_total_bytes: 256 << 20, // 256 MiB
            max_file_bytes: 16 << 20,
            exclude: [
                ".git/",
                "target/",
                "node_modules/",
                ".venv/",
                "__pycache__/",
                "dist/",
                "build/",
                ".rook/store/",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            respect_ignore_files: true,
        }
    }
}

impl CaptureLimits {
    /// A skill directory is small by construction; no need for workspace-sized
    /// budgets, and a runaway one should be caught early.
    pub fn for_skill() -> Self {
        Self { max_files: 500, max_total_bytes: 32 << 20, max_file_bytes: 8 << 20, ..Default::default() }
    }

    fn excluded(&self, rel: &str) -> bool {
        let normalized = rel.replace('\\', "/");
        self.exclude.iter().any(|e| normalized.contains(e.as_str()))
    }
}

impl FileSet {
    /// Walk `root`, store every eligible file, and return the manifest.
    pub fn capture(
        store: &Store,
        kind: &str,
        name: &str,
        version: &str,
        root: &Path,
        limits: &CaptureLimits,
        note: Option<String>,
    ) -> Result<(Self, ObjectId)> {
        let mut files = BTreeMap::new();
        let mut total = 0u64;

        let mut walker = ignore::WalkBuilder::new(root);
        walker
            .hidden(false)
            .git_ignore(limits.respect_ignore_files)
            .git_global(limits.respect_ignore_files)
            .git_exclude(limits.respect_ignore_files)
            .follow_links(false);

        for entry in walker.build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else { continue };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if limits.excluded(&rel_str) {
                continue;
            }

            let meta = entry.metadata().map_err(|e| CoreError::Capture(e.to_string()))?;
            if meta.len() > limits.max_file_bytes {
                return Err(CoreError::CaptureTooBig {
                    what: format!("{rel_str} is {} bytes", meta.len()),
                    limit: format!("max_file_bytes = {}", limits.max_file_bytes),
                });
            }
            if files.len() >= limits.max_files {
                return Err(CoreError::CaptureTooBig {
                    what: format!("more than {} files under {}", limits.max_files, root.display()),
                    limit: "max_files".into(),
                });
            }
            total += meta.len();
            if total > limits.max_total_bytes {
                return Err(CoreError::CaptureTooBig {
                    what: format!("{total} bytes under {}", root.display()),
                    limit: format!("max_total_bytes = {}", limits.max_total_bytes),
                });
            }

            let data = std::fs::read(entry.path())
                .map_err(|e| CoreError::Io { path: entry.path().to_path_buf(), source: e })?;
            let id = store.put(Kind::FileBlob, &data)?;
            files.insert(rel_str, id.to_hex());
        }

        let set = FileSet {
            kind: kind.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            captured_at: rook_store::now_unix(),
            root: root.display().to_string(),
            files,
            absent: Vec::new(),
            total_bytes: total,
            note,
        };
        let encoded = serde_json::to_vec(&set)?;
        let object_kind = if kind == "skill" { Kind::Skill } else { Kind::Snapshot };
        let id = store.put(object_kind, &encoded)?;
        Ok((set, id))
    }

    pub fn load(store: &Store, id: &ObjectId) -> Result<Self> {
        let raw = store.get(id)?;
        Ok(serde_json::from_slice(&raw)?)
    }

    /// Write the captured files back under `dest`.
    pub fn restore(&self, store: &Store, dest: &Path) -> Result<usize> {
        let mut written = 0;
        for (rel, hex) in &self.files {
            let Some(id) = ObjectId::from_hex(hex) else {
                return Err(CoreError::Capture(format!("manifest holds a bad object id for {rel}")));
            };
            let data = store.get(&id)?;
            let path = dest.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| CoreError::Io { path: parent.to_path_buf(), source: e })?;
            }
            std::fs::write(&path, data).map_err(|e| CoreError::Io { path: path.clone(), source: e })?;
            written += 1;
        }
        Ok(written)
    }

    /// Paths that differ between two captures, as (path, change).
    pub fn diff(&self, other: &FileSet) -> Vec<(String, Change)> {
        let mut out = Vec::new();
        for (path, id) in &self.files {
            match other.files.get(path) {
                None => out.push((path.clone(), Change::Removed)),
                Some(o) if o != id => out.push((path.clone(), Change::Modified)),
                Some(_) => {}
            }
        }
        for path in other.files.keys() {
            if !self.files.contains_key(path) {
                out.push((path.clone(), Change::Added));
            }
        }
        out.sort();
        out
    }

    /// Object ids this manifest keeps alive, for the store's GC expander.
    pub fn referenced_objects(&self) -> Vec<ObjectId> {
        self.files.values().filter_map(|h| ObjectId::from_hex(h)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Change {
    Added,
    Modified,
    Removed,
}

impl Change {
    pub fn sigil(self) -> char {
        match self {
            Change::Added => '+',
            Change::Modified => '~',
            Change::Removed => '-',
        }
    }
}

/// The GC expander: teaches [`rook_store::Store::gc`] that a manifest keeps its
/// files alive.
pub fn gc_expander(_kind: Kind, body: &[u8]) -> Vec<ObjectId> {
    match serde_json::from_slice::<FileSet>(body) {
        Ok(set) => set.referenced_objects(),
        Err(_) => Vec::new(),
    }
}

/// Capture an explicit list of paths. A path that does not exist is recorded in
/// `absent` rather than failing, so a later rewind can remove it.
pub fn capture_paths(
    store: &Store,
    kind: &str,
    name: &str,
    root: &Path,
    paths: &[PathBuf],
    limits: &CaptureLimits,
) -> Result<(FileSet, ObjectId)> {
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/");
    let mut files = BTreeMap::new();
    let mut absent = Vec::new();
    let mut total = 0u64;
    for path in paths {
        let Ok(meta) = std::fs::metadata(path) else {
            absent.push(rel(path));
            continue;
        };
        // A directory has no content to keep, and recording it absent would
        // have a rewind delete it. Skipped, because the alternative is what
        // happened: a model naming a directory where a file goes turned the
        // whole capture into "no checkpoint was taken" — an alarm that is
        // meant to be rare and serious, raised by an ordinary bad argument.
        if meta.is_dir() {
            continue;
        }
        if meta.len() > limits.max_file_bytes {
            return Err(CoreError::CaptureTooBig {
                what: format!("{} is {} bytes", path.display(), meta.len()),
                limit: format!("max_file_bytes = {}", limits.max_file_bytes),
            });
        }
        total += meta.len();
        if total > limits.max_total_bytes || files.len() >= limits.max_files {
            return Err(CoreError::CaptureTooBig {
                what: format!("{} files / {total} bytes", files.len()),
                limit: "capture limits".into(),
            });
        }
        let data = std::fs::read(path).map_err(|e| CoreError::Io { path: path.clone(), source: e })?;
        let id = store.put(Kind::FileBlob, &data)?;
        files.insert(rel(path), id.to_hex());
    }
    let set = FileSet {
        kind: kind.into(),
        name: name.into(),
        version: String::new(),
        captured_at: rook_store::now_unix(),
        root: root.display().to_string(),
        files,
        absent,
        total_bytes: total,
        note: None,
    };
    let id = store.put(Kind::Snapshot, &serde_json::to_vec(&set)?)?;
    Ok((set, id))
}
