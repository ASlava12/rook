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
    // Never more than the cap, whatever the file's size: a cap applied to bytes
    // already in memory is not a cap, and this one exists precisely because the
    // project's copy is written by whoever sends the pull request. The length
    // comes from the metadata, since what was left out is no longer read.
    let full = std::fs::metadata(path).ok()?.len() as usize;
    let text = read_head(path, max_bytes)?;
    if text.trim().is_empty() {
        return None;
    }
    // On a character boundary, or the slice panics: a limit that a file somebody
    // else wrote can crash the agent with is not a limit.
    let cut = crate::context::floor_char_boundary(text.as_bytes(), max_bytes);
    if full <= cut {
        return Some(Instructions { from: path.to_path_buf(), elided: 0, text });
    }
    // Both ends of a file that is too long, not the first half of it. A
    // conventions file is written like one: the subject at the top and the
    // sharpest rules at the bottom, which is where "never do X" lives — and
    // cutting at the ceiling dropped exactly that, silently. It is the rule
    // command output already follows here for the same reason.
    let head = crate::context::floor_char_boundary(text.as_bytes(), max_bytes * 2 / 3);
    let want = max_bytes - head;
    let tail = read_tail(path, want).unwrap_or_default();
    let from = tail.len().saturating_sub(want);
    let tail = &tail[crate::context::ceil_char_boundary(tail.as_bytes(), from)..];
    let elided = full.saturating_sub(head + tail.len());
    Some(Instructions {
        from: path.to_path_buf(),
        elided,
        text: format!(
            "{}\n[{elided} bytes from the middle not read — past `[agent] max_instructions_bytes`]\n{tail}",
            &text[..head]
        ),
    })
}

/// The last `bytes` of a file, as text. Seeked to rather than read up to: the
/// point of the ceiling is that the file is never held whole.
fn read_tail(path: &Path, bytes: usize) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(bytes as u64))).ok()?;
    let mut tail = Vec::with_capacity(bytes);
    file.take(bytes as u64).read_to_end(&mut tail).ok()?;
    Some(String::from_utf8_lossy(&tail).into_owned())
}

/// The first `bytes` of a file, as text.
///
/// Lossy, not strict: an instructions file with one stray byte in it is a file
/// somebody wrote by hand, and dropping the whole of it over that byte would
/// leave the agent following nothing and saying nothing about why. One character
/// may straddle the cut; the caller trims to a boundary.
fn read_head(path: &Path, bytes: usize) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::with_capacity(bytes + 1);
    std::fs::File::open(path).ok()?.take(bytes as u64 + 1).read_to_end(&mut head).ok()?;
    Some(String::from_utf8_lossy(&head).into_owned())
}
