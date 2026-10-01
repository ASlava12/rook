//! Bounded, private retry identities for Stops whose socket delivery is uncertain.
//! The daemon owns whether a Stop was applied; this file only lets a new window
//! repeat the same caller ID and observed turn to ask it safely.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::StopAttempt;

const FILE: &str = "tui-stop-retries.json";
const LOCK: &str = "tui-stop-retries.lock";
const MAX_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 64;
const MAX_WORKSPACE_BYTES: usize = 1024;

#[derive(Clone)]
pub(super) struct Journal {
    home: PathBuf,
    workspace: String,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    workspace: String,
    session: String,
    id: String,
    generation: Option<String>,
    turn: Option<String>,
}

impl Journal {
    pub(super) fn new(home: PathBuf, workspace: &Path) -> io::Result<Self> {
        if workspace.as_os_str().len() > MAX_WORKSPACE_BYTES {
            return Err(io::Error::other("workspace path exceeds Stop retry limit"));
        }
        let workspace = workspace.to_str().ok_or_else(|| io::Error::other("workspace path is not UTF-8"))?;
        Ok(Self { home, workspace: workspace.to_owned() })
    }

    pub(super) fn load(&self, session: u128) -> io::Result<Option<StopAttempt>> {
        let named = rook_store::format_session_id(session);
        Ok(self
            .read()?
            .into_iter()
            .find(|entry| entry.workspace == self.workspace && entry.session == named)
            .map(|entry| StopAttempt {
                session,
                id: entry.id,
                generation: entry.generation,
                turn: entry.turn,
            }))
    }

    pub(super) fn save(&self, attempt: &StopAttempt) -> io::Result<()> {
        validate_attempt(attempt)?;
        let _lock = self.lock()?;
        let mut entries = self.read()?;
        let session = rook_store::format_session_id(attempt.session);
        entries.retain(|entry| entry.workspace != self.workspace || entry.session != session);
        if entries.len() >= MAX_ENTRIES {
            return Err(io::Error::other("Stop retry file is full; clear an old retry first"));
        }
        entries.push(Entry {
            workspace: self.workspace.clone(),
            session,
            id: attempt.id.clone(),
            generation: attempt.generation.clone(),
            turn: attempt.turn.clone(),
        });
        self.write(&entries)
    }

    /// Never erase another window's newer retry for the same session.
    pub(super) fn clear(&self, attempt: &StopAttempt) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut entries = self.read()?;
        let before = entries.len();
        let session = rook_store::format_session_id(attempt.session);
        entries.retain(|entry| {
            entry.workspace != self.workspace || entry.session != session || entry.id != attempt.id
        });
        if entries.len() != before {
            self.write(&entries)?;
        }
        Ok(())
    }

    fn lock(&self) -> io::Result<std::fs::File> {
        rook_core::paths::private_dir(&self.home)?;
        let file = rook_contain::files::lock_file(&self.home, Path::new(LOCK))?;
        file.try_lock().map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "Stop retry file is busy"))?;
        Ok(file)
    }

    fn read(&self) -> io::Result<Vec<Entry>> {
        let text = match rook_contain::files::read_text(&self.home, Path::new(FILE), MAX_BYTES) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let entries: Vec<Entry> = serde_json::from_str(&text).map_err(io::Error::other)?;
        if entries.len() > MAX_ENTRIES || entries.iter().any(|entry| !valid_entry(entry)) {
            return Err(io::Error::other("invalid or oversized Stop retry entry"));
        }
        let mut keys = std::collections::HashSet::new();
        if entries.iter().any(|entry| !keys.insert((&entry.workspace, &entry.session))) {
            return Err(io::Error::other("duplicate Stop retry session"));
        }
        Ok(entries)
    }

    fn write(&self, entries: &[Entry]) -> io::Result<()> {
        struct Bounded(Vec<u8>);
        impl Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
                    return Err(io::Error::other("Stop retry file exceeds byte limit"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut encoded = Bounded(Vec::new());
        serde_json::to_writer(&mut encoded, entries).map_err(io::Error::other)?;
        rook_contain::files::write_private(&self.home, Path::new(FILE), &encoded.0)
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 26 && rook_store::parse_session_id(id).is_some()
}

fn valid_entry(entry: &Entry) -> bool {
    !entry.workspace.is_empty()
        && entry.workspace.len() <= MAX_WORKSPACE_BYTES
        && valid_id(&entry.session)
        && valid_id(&entry.id)
        && entry.generation.as_deref().is_none_or(valid_id)
        && entry.turn.as_deref().is_none_or(valid_id)
        && (entry.generation.is_some() != entry.turn.is_some())
}

fn validate_attempt(attempt: &StopAttempt) -> io::Result<()> {
    if valid_id(&rook_store::format_session_id(attempt.session))
        && valid_id(&attempt.id)
        && attempt.generation.as_deref().is_none_or(valid_id)
        && attempt.turn.as_deref().is_none_or(valid_id)
        && (attempt.generation.is_some() != attempt.turn.is_some())
    {
        Ok(())
    } else {
        Err(io::Error::other("Stop retry identity is invalid"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_stop_survives_reopen_and_cannot_clear_a_newer_id() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let journal = Journal::new(home.path().to_path_buf(), workspace.path()).unwrap();
        let session = rook_store::new_session_id();
        let turn = rook_store::format_session_id(rook_store::new_session_id());
        let first = StopAttempt {
            session,
            id: rook_store::format_session_id(rook_store::new_session_id()),
            generation: None,
            turn: Some(turn.clone()),
        };
        journal.save(&first).unwrap();
        let reopened = Journal::new(home.path().to_path_buf(), workspace.path()).unwrap();
        assert_eq!(reopened.load(session).unwrap().unwrap().id, first.id);
        let newer =
            StopAttempt { id: rook_store::format_session_id(rook_store::new_session_id()), ..first.clone() };
        reopened.save(&newer).unwrap();
        journal.clear(&first).unwrap();
        assert_eq!(reopened.load(session).unwrap().unwrap().id, newer.id);
        reopened.clear(&newer).unwrap();
        assert!(journal.load(session).unwrap().is_none());
    }

    #[test]
    fn oversized_retry_file_is_refused_before_decoding() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(FILE), vec![b' '; MAX_BYTES + 1]).unwrap();
        let journal = Journal::new(home.path().to_path_buf(), workspace.path()).unwrap();
        assert!(journal.load(rook_store::new_session_id()).is_err());
    }

    #[test]
    fn saved_stops_are_scoped_to_workspace_and_invalid_ids_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let first_workspace = tempfile::tempdir().unwrap();
        let second_workspace = tempfile::tempdir().unwrap();
        let first = Journal::new(home.path().to_path_buf(), first_workspace.path()).unwrap();
        let second = Journal::new(home.path().to_path_buf(), second_workspace.path()).unwrap();
        let session = rook_store::new_session_id();
        let attempt = StopAttempt {
            session,
            id: rook_store::format_session_id(rook_store::new_session_id()),
            generation: None,
            turn: Some(rook_store::format_session_id(rook_store::new_session_id())),
        };
        first.save(&attempt).unwrap();
        assert!(second.load(session).unwrap().is_none());
        second.save(&attempt).unwrap();
        first.clear(&attempt).unwrap();
        assert!(first.load(session).unwrap().is_none());
        assert_eq!(second.load(session).unwrap().unwrap().id, attempt.id);

        let invalid = StopAttempt { id: "not-a-valid-id".into(), ..attempt };
        assert!(second.save(&invalid).is_err());
        assert!(second.load(session).unwrap().is_some(), "invalid input must not replace the saved ID");
    }
}
