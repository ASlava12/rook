//! Where the logs go.
//!
//! Both binaries set this up the same way, because a level that means one thing
//! in the CLI and another in the daemon is a support question.

use std::fs::File;
use std::io::IsTerminal;

use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::config::TelemetryConfig;
use crate::paths;

/// Log to stderr and to `$ROOK_HOME/logs/rook.log`.
///
/// `ROOK_LOG` overrides the configured level, because the moment you need more
/// detail is the moment you do not want to edit a file first.
pub fn init(config: &TelemetryConfig) {
    let filter = std::env::var("ROOK_LOG").unwrap_or_else(|_| config.log_level.clone());
    let stderr = std::io::stderr;

    match open_log(&paths::logs_dir(), config.max_log_bytes) {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(std::io::stderr().is_terminal())
            .with_writer(stderr.and(file))
            .init(),
        None => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(std::io::stderr().is_terminal())
            .with_writer(stderr)
            .init(),
    }
}

/// Rotate once at the limit, so the logs cost at most twice it and the previous
/// run is still readable. Returns nothing if the directory is not writable —
/// losing the file log is not a reason to refuse to start.
pub fn open_log(dir: &std::path::Path, max_bytes: u64) -> Option<File> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join("rook.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= max_bytes) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    File::options().create(true).append(true).open(&path).ok()
}
