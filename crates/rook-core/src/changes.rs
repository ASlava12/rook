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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Changes {
    pub files: Vec<FileChange>,
    /// Files a command wrote. Nothing holds what they were before — a command
    /// declares no paths, so none was checkpointed — so they cannot be diffed
    /// and `session rewind` cannot put them back. Naming them is all there is,
    /// and it is the difference between a wrong answer and a partial one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub written_by_commands: Vec<String>,
    /// False when the workspace was too large to walk, so the list above is
    /// short for a reason that is not "nothing was written".
    #[serde(default = "watched_by_default")]
    pub watched: bool,
}

fn watched_by_default() -> bool {
    true
}

impl Default for Changes {
    fn default() -> Self {
        Self { files: Vec::new(), written_by_commands: Vec::new(), watched: true }
    }
}

impl Changes {
    pub fn touched(&self) -> usize {
        self.files.iter().filter(|f| f.change != Change::Unchanged).count() + self.written_by_commands.len()
    }

    pub fn summary(&self) -> String {
        let (added, removed): (usize, usize) =
            self.files.iter().fold((0, 0), |(a, r), f| (a + f.lines_added, r + f.lines_removed));
        let mut said = format!("{} file(s), +{added} -{removed}", self.touched());
        // Counted in the total and named apart from it: they have no line
        // counts to add, and saying so is the difference between a partial
        // answer and one that looks whole.
        if !self.written_by_commands.is_empty() {
            said.push_str(&format!(
                ", {} written by commands and not diffable",
                self.written_by_commands.len()
            ));
        }
        if !self.watched {
            said.push_str(", and the workspace was too large to watch for more");
        }
        said
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

        // What a command wrote, which no checkpoint holds: the loop records it
        // by name and this is where it is read back.
        let mut written: std::collections::BTreeSet<String> = Default::default();
        let mut watched = true;
        for event in self.store.events(session, 0, usize::MAX)? {
            if event.record.kind == EventKind::Note && event.record.label == crate::agent::WROTE {
                let Ok(body) = self.store.get(&event.record.body) else { continue };
                let said: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                watched &= said["complete"].as_bool().unwrap_or(true);
                for path in said["paths"].as_array().into_iter().flatten() {
                    if let Some(path) = path.as_str() {
                        written.insert(path.to_string());
                    }
                }
                continue;
            }
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
            let was = match original {
                Some(id) => Some(self.store.get(&id)?),
                None => None,
            };
            files.push(match on_disk(&path) {
                OnDisk::Text(now) => compare(&path, &roots, was.as_deref(), Some(&now), with_diff),
                OnDisk::Gone => compare(&path, &roots, was.as_deref(), None, with_diff),
                // Too large to hold, let alone to diff. Whether it changed is
                // still answerable: the id a capture is stored under is the hash
                // of its content, so hashing the file says which without either
                // of them being in memory at once.
                OnDisk::Large(id) => {
                    let same = original.is_some_and(|was| was == id);
                    named(&path, &roots, if same { Change::Unchanged } else { Change::Modified })
                }
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Changes { files, written_by_commands: written.into_iter().collect(), watched })
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

enum OnDisk {
    Gone,
    Text(Vec<u8>),
    /// Past what is worth holding: its content hash, which is what a capture is
    /// stored under.
    Large(ObjectId),
}

/// Read a file only as far as it is worth reading.
///
/// The cap used to be applied after the bytes were all in memory, which is not a
/// cap: a session that wrote a two-gigabyte file made `session diff` read two
/// gigabytes in order to decide it would not diff them. Its length answers that
/// first, and for one too large to hold, its hash answers whether it changed —
/// which is the id a capture is stored under, so the two are comparable without
/// either being in memory.
fn on_disk(path: &Path) -> OnDisk {
    let Ok(meta) = std::fs::metadata(path) else { return OnDisk::Gone };
    if meta.len() as usize <= MAX_DIFF_BYTES {
        return std::fs::read(path).map(OnDisk::Text).unwrap_or(OnDisk::Gone);
    }
    match std::fs::File::open(path).and_then(ObjectId::of_reader) {
        Ok(id) => OnDisk::Large(id),
        Err(_) => OnDisk::Gone,
    }
}

/// A change with nothing to say about its contents.
/// Relative to the workspace, however the two paths spell it.
///
/// That is how the user asked and how they will act on the answer — and
/// `strip_prefix` alone did not get there on Windows, where the same directory
/// arrives as `C:\x` from one side and `\\?\C:\x` from the other, spelled with
/// either separator. Forward slashes out, as `list_dir` already prints them.
fn relative_to(path: &Path, roots: &[PathBuf]) -> String {
    let plain = |p: &Path| {
        let text = p.display().to_string().replace('\\', "/");
        text.trim_start_matches("//?/").trim_end_matches('/').to_string()
    };
    let shown = plain(path);
    roots
        .iter()
        .find_map(|root| shown.strip_prefix(&plain(root))?.strip_prefix('/'))
        .unwrap_or(&shown)
        .to_string()
}

fn named(path: &Path, roots: &[PathBuf], change: Change) -> FileChange {
    FileChange { path: relative_to(path, roots), change, lines_added: 0, lines_removed: 0, diff: None }
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

#[cfg(test)]
mod tests {
    use super::relative_to;
    use std::path::{Path, PathBuf};

    /// Both sides are absolute and neither controls how the other is spelled: a
    /// capture records the root it was handed, and the workspace is whatever the
    /// engine was opened with. On Windows one of them is a verbatim path.
    #[test]
    fn a_path_is_named_relatively_however_the_two_sides_spell_it() {
        let roots = |root: &str| vec![PathBuf::from(root)];

        assert_eq!(relative_to(Path::new("/w/src/a.rs"), &roots("/w")), "src/a.rs");
        assert_eq!(relative_to(Path::new("/w/src/a.rs"), &roots("/w/")), "src/a.rs");
        assert_eq!(
            relative_to(Path::new(r"\\?\C:\w\src\a.rs"), &roots(r"C:\w")),
            "src/a.rs",
            "verbatim on one side only"
        );
        assert_eq!(relative_to(Path::new("//?/C:/w/src/a.rs"), &roots(r"\\?\C:\w")), "src/a.rs");
        // Nothing matched: the whole path beats a relative name that is wrong.
        assert_eq!(relative_to(Path::new("/elsewhere/a.rs"), &roots("/w")), "/elsewhere/a.rs");
    }
}
