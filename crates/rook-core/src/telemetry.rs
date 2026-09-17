//! Where the logs go.
//!
//! Both binaries set this up the same way, because a level that means one thing
//! in the CLI and another in the daemon is a support question.

use std::fs::File;
use std::io::IsTerminal;

use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::config::TelemetryConfig;
use crate::paths;

/// Log to `$ROOK_HOME/logs/rook.log`, and to stderr unless something is drawing
/// on it.
///
/// `to_terminal` is false for the window, which owns the screen: a warning
/// written to stderr under the alternate screen lands in the middle of the
/// drawing and stays there until something redraws over it. Every sandbox rule
/// that will not compile, every lock read through after a panic, every note
/// about a log that could not be written — all of it went onto the interface.
/// The file gets it either way, which is where anyone looks afterwards.
///
/// `ROOK_LOG` overrides the configured level, because the moment you need more
/// detail is the moment you do not want to edit a file first.
pub fn init(config: &TelemetryConfig, to_terminal: bool) {
    let filter = std::env::var("ROOK_LOG").unwrap_or_else(|_| config.log_level.clone());
    let stderr = std::io::stderr;
    let ansi = to_terminal && std::io::stderr().is_terminal();
    let file = open_log(&paths::logs_dir(), config.max_log_bytes);

    match (file, to_terminal) {
        (Some(file), true) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(ansi)
            .with_writer(stderr.and(file))
            .init(),
        (Some(file), false) => {
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(file).init()
        }
        // No file to write to. Stderr is all there is, and a window that has to
        // choose between a marked screen and losing the reason outright keeps
        // the reason: the screen redraws, and the reason does not come back.
        (None, _) => {
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(ansi).with_writer(stderr).init()
        }
    }
}

/// Where a spawned daemon's own stderr goes.
///
/// Its own file, and not the one `tracing` writes to. It was that one, so every
/// line the daemon logged was written twice — once by the file layer and once
/// down the stderr that was the same file — and the comment that said so called
/// it a line a month. An evening of reading a daemon that could not reach its
/// own network found a dozen, doubled, which is twice as long to read and reads
/// at first as two processes writing.
///
/// Kept rather than dropped, because what comes out here is what `tracing`
/// cannot write: `panic = "abort"` is set for release, so a panic ends the
/// daemon and every turn it was holding, and the reason is on stderr or it is
/// nowhere. This file being anything other than empty is itself the news.
pub const PANICS: &str = "rook-stderr.log";

/// Rotate once at the limit, so the logs cost at most twice it and the previous
/// run is still readable. Returns nothing if the directory is not writable —
/// losing the file log is not a reason to refuse to start.
///
/// `name` is [`PANICS`] for a daemon's raw stderr and `rook.log` for the lines
/// `tracing` writes.
pub fn open_log(dir: &std::path::Path, max_bytes: u64) -> Option<File> {
    open_named(dir, "rook.log", max_bytes)
}

/// The same, for a file that is not the one everything logs to.
pub fn open_named(dir: &std::path::Path, name: &str, max_bytes: u64) -> Option<File> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(name);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= max_bytes) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    File::options().create(true).append(true).open(&path).ok()
}
