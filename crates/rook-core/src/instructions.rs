//! Standing instructions a project keeps beside its code.
//!
//! `AGENTS.md` is the convention codex, opencode and others already read, and
//! Rook read none of them: a project's conventions had to be repeated in every
//! prompt or hidden in a skill, which is loaded on demand and so is not standing
//! instruction at all.
//!
//! Two files, most general first: `$ROOK_HOME/AGENTS.md` applies everywhere and
//! the workspace's applies here, so the project has the last word on anything
//! both mention. Bounded, because a file in a repository is written by whoever
//! sends the pull request and is paid for on every single request.

use std::path::{Path, PathBuf};

/// Read in this order; the last one wins where they disagree, and the model is
/// told which is which.
pub const FILENAME: &str = "AGENTS.md";

pub struct Instructions {
    pub from: PathBuf,
    pub text: String,
    /// Bytes past the limit that were not read.
    pub elided: usize,
}

/// What applies in `workspace`, most general first.
///
/// A file that is unreadable is one that is not there: an instruction nobody
/// can read must not be the reason a turn does not start.
pub fn applying_in(workspace: &Path, max_bytes: usize) -> Vec<Instructions> {
    [crate::paths::home().join(FILENAME), workspace.join(FILENAME)]
        .into_iter()
        .filter(|path| path.is_file())
        .filter_map(|path| read_bounded(&path, max_bytes))
        .collect()
}

fn read_bounded(path: &Path, max_bytes: usize) -> Option<Instructions> {
    let text = std::fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    // On a character boundary, or the slice panics: a limit that a file somebody
    // else wrote can crash the agent with is not a limit.
    let cut = crate::context::floor_char_boundary(text.as_bytes(), max_bytes);
    Some(Instructions {
        from: path.to_path_buf(),
        elided: text.len().saturating_sub(cut),
        text: text[..cut].to_string(),
    })
}
