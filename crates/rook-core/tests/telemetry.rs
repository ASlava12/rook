//! The log file is bounded, because a daemon that runs for months writing to an
//! unbounded file is the shape of failure the storage design exists to avoid.

use std::io::Write;

use rook_core::telemetry::open_log;

struct Logs(tempfile::TempDir);

impl Logs {
    fn with(bytes: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join("rook.log")).unwrap();
        f.write_all(&vec![b'x'; bytes]).unwrap();
        Self(dir)
    }

    fn len(&self, name: &str) -> u64 {
        std::fs::metadata(self.0.path().join(name)).map(|m| m.len()).unwrap_or_default()
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(self.0.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }
}

#[test]
fn a_log_at_its_limit_rotates_instead_of_growing() {
    let logs = Logs::with(2048);

    drop(open_log(logs.0.path(), 1024).unwrap());

    assert_eq!(logs.len("rook.log"), 0, "the live log starts again");
    assert_eq!(logs.len("rook.log.1"), 2048, "and the previous run is still readable");
}

#[test]
fn rotating_repeatedly_does_not_keep_a_third_copy() {
    let logs = Logs::with(64);
    for _ in 0..3 {
        drop(open_log(logs.0.path(), 1));
        std::fs::write(logs.0.path().join("rook.log"), vec![b'x'; 64]).unwrap();
    }

    assert_eq!(logs.names(), ["rook.log", "rook.log.1"], "at most the live log and one before it");
}

#[test]
fn a_log_under_its_limit_is_appended_to() {
    let logs = Logs::with(10);

    drop(open_log(logs.0.path(), 1024).unwrap());

    assert_eq!(logs.len("rook.log"), 10, "nothing was lost");
}

/// A daemon's own stderr is where a panic lands — `panic = "abort"` is set for
/// release, so the process is gone before `tracing` can write a word. It used
/// to land in the file `tracing` writes to, which meant every line the daemon
/// logged was written twice: once by the file layer, once down the stderr that
/// was the same file. An evening spent reading that log found a dozen doubled
/// lines, which reads at first as two processes writing.
#[test]
fn a_daemons_own_stderr_is_not_the_file_tracing_writes_to() {
    use rook_core::telemetry::{PANICS, open_named};

    let logs = Logs::with(0);
    let mut tracing = open_log(logs.0.path(), u64::MAX).unwrap();
    let mut stderr = open_named(logs.0.path(), PANICS, u64::MAX).unwrap();
    tracing.write_all(b"a warning\n").unwrap();
    stderr.write_all(b"panicked at\n").unwrap();
    drop((tracing, stderr));

    assert_eq!(logs.names(), ["rook-stderr.log", "rook.log"], "two files, each with one job");
    assert_eq!(logs.len("rook.log"), 10, "the warning is written once");
    assert_eq!(logs.len(PANICS), 12, "and the panic is kept, apart from it");
}
