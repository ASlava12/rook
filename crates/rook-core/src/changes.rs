//! What a session actually changed on disk.
//!
//! The loop checkpoints every path a tool is about to touch, so the store
//! already holds the content of each file as it was before the agent first
//! touched it. Comparing that against the file now gives the exact effect of a
//! session — without a repository, without staging anything, and for files that
//! were never under version control.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rook_store::{EventKind, ObjectId};

use crate::error::Result;
use crate::fileset::FileSet;
use crate::service::Rook;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Added,
    Modified,
    Removed,
    /// Touched and then put back the way it was.
    Unchanged,
}

impl Change {
    pub fn sigil(self) -> char {
        match self {
            Change::Added => '+',
            Change::Modified => '~',
            Change::Removed => '-',
            Change::Unchanged => '=',
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub change: Change,
    pub lines_added: usize,
    pub lines_removed: usize,
    /// Absent when either side is binary or too large to diff.
    pub diff: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Changes {
    pub files: Vec<FileChange>,
}

impl Changes {
    pub fn touched(&self) -> usize {
        self.files.iter().filter(|f| f.change != Change::Unchanged).count()
    }

    pub fn summary(&self) -> String {
        let (added, removed): (usize, usize) =
            self.files.iter().fold((0, 0), |(a, r), f| (a + f.lines_added, r + f.lines_removed));
        format!("{} file(s), +{added} -{removed}", self.touched())
    }
}

/// A file this large is not worth rendering as a diff in a terminal.
const MAX_DIFF_BYTES: usize = 256 * 1024;

impl Rook {
    /// What `session` changed, comparing each file's earliest checkpoint against
    /// the file as it is now.
    pub fn changes(&self, session: u128, with_diff: bool) -> Result<Changes> {
        // Earliest wins: the first checkpoint of a path holds the state before
        // the agent touched it, and later ones are already its own work.
        let mut before: BTreeMap<PathBuf, Option<ObjectId>> = BTreeMap::new();

        for event in self.store.events(session, 0, usize::MAX)? {
            if event.record.kind != EventKind::Checkpoint {
                continue;
            }
            let Ok(set) = FileSet::load(&self.store, &event.record.body) else { continue };
            let root = PathBuf::from(&set.root);
            for (relative, hex) in &set.files {
                before.entry(root.join(relative)).or_insert_with(|| ObjectId::from_hex(hex));
            }
            for relative in &set.absent {
                before.entry(root.join(relative)).or_insert(None);
            }
        }

        // Both spellings, because a checkpoint records the root as it was given
        // while the workspace may also be reachable through a symlink — /tmp
        // against /private/tmp on macOS is the common case, and which one a path
        // carries depends on how it arrived.
        let roots = [
            self.workspace.canonicalize().unwrap_or_else(|_| self.workspace.clone()),
            self.workspace.clone(),
        ];
        let mut files = Vec::new();
        for (path, original) in before {
            let now = std::fs::read(&path).ok();
            let was = match original {
                Some(id) => Some(self.store.get(&id)?),
                None => None,
            };
            files.push(compare(&path, &roots, was.as_deref(), now.as_deref(), with_diff));
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Changes { files })
    }
}

fn compare(
    path: &Path,
    roots: &[PathBuf],
    was: Option<&[u8]>,
    now: Option<&[u8]>,
    with_diff: bool,
) -> FileChange {
    let change = match (was, now) {
        (None, Some(_)) => Change::Added,
        (Some(_), None) => Change::Removed,
        (Some(a), Some(b)) if a == b => Change::Unchanged,
        _ => Change::Modified,
    };

    // Relative to the workspace: that is how the user asked, and how they will
    // act on the answer.
    let relative = roots.iter().find_map(|root| path.strip_prefix(root).ok()).unwrap_or(path);
    let mut file = FileChange {
        path: relative.display().to_string(),
        change,
        lines_added: 0,
        lines_removed: 0,
        diff: None,
    };
    if change == Change::Unchanged {
        return file;
    }

    let (Some(old), Some(new)) = (readable(was), readable(now)) else {
        // One side is binary or oversized: report that it changed, but do not
        // try to render it.
        return file;
    };

    let diff = similar::TextDiff::from_lines(&old, &new);
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => file.lines_added += 1,
            similar::ChangeTag::Delete => file.lines_removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    if with_diff {
        file.diff = Some(diff.unified_diff().context_radius(3).header("before", "after").to_string());
    }
    file
}

/// Text small enough to diff. Absent content reads as empty, so an added or
/// removed file diffs against nothing rather than being skipped.
fn readable(content: Option<&[u8]>) -> Option<String> {
    let bytes = content.unwrap_or(&[]);
    if bytes.len() > MAX_DIFF_BYTES || bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}
