//! Bounded streaming removal of routine Cargo progress, before the display cap.
//! The caller retains raw bytes separately. Unknown lines are emitted unchanged.

const MAX_LINE: usize = 16 * 1024;

/// A conservative presentation selector, not a shell parser or approval rule.
/// Pipelines, substitutions, compound commands and unknown options stay raw.
pub(super) fn eligible(command: &str) -> bool {
    if command.bytes().any(|b| b"\r\n|&;<>()`$%!".contains(&b)) {
        return false;
    }
    let command = command.trim_ascii();
    let (program, rest) = if let Some(quote @ ('\'' | '"')) = command.chars().next() {
        let after = &command[1..];
        let Some(end) = after.find(quote) else { return false };
        let rest = &after[end + 1..];
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        (&after[..end], rest)
    } else {
        let Some((program, rest)) = command.split_once(char::is_whitespace) else { return false };
        (program, rest)
    };
    let name = program.rsplit(['/', '\\']).next().unwrap_or_default();
    if name != "cargo" && !name.eq_ignore_ascii_case("cargo.exe") {
        return false;
    }
    let mut arguments = rest.split_ascii_whitespace().peekable();
    if arguments.peek().is_some_and(|a| a.starts_with('+')) {
        arguments.next();
    }
    while let Some(argument) = arguments.next() {
        match argument {
            "--locked" | "--offline" | "--frozen" | "--quiet" | "-q" | "--verbose" | "-v" | "-vv" => {}
            "--color" => {
                if !matches!(arguments.next(), Some("auto" | "always" | "never")) {
                    return false;
                }
            }
            "build" | "b" | "check" | "c" | "test" | "t" | "clippy" | "doc" | "bench" => return true,
            _ => return false,
        }
    }
    false
}

#[derive(Clone, Copy, Default)]
pub(super) struct Removed {
    pub progress: usize,
    pub passed: usize,
    pub bytes: usize,
}

impl Removed {
    pub fn plus(self, other: Self) -> Self {
        Self {
            progress: self.progress.saturating_add(other.progress),
            passed: self.passed.saturating_add(other.passed),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }

    pub fn note(self) -> String {
        if self.bytes == 0 {
            return String::new();
        }
        format!(
            "\n[Cargo output condensed: {} progress lines and {} passing-test lines ({} bytes) \
             omitted. Other output still uses the normal display cap. `read_result` with \
             source=output reads the original capture; output=\"raw\" keeps progress in the reply.]",
            self.progress, self.passed, self.bytes
        )
    }

    pub fn annotate(self, outcome: crate::ToolOutcome) -> crate::ToolOutcome {
        if self.bytes == 0 {
            return outcome;
        }
        outcome
            .with("cargo_output_condensed", true)
            .with("cargo_output_removed_bytes", self.bytes)
            .with("cargo_output_progress_lines", self.progress)
            .with("cargo_output_passing_test_lines", self.passed)
    }
}

#[derive(Default)]
pub(super) struct Filter {
    pending: Vec<u8>,
    passthrough: bool,
    pub removed: Removed,
}

impl Filter {
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(&[u8])) {
        for part in bytes.split_inclusive(|&byte| byte == b'\n') {
            let complete = part.last() == Some(&b'\n');
            if self.passthrough {
                emit(part);
                self.passthrough = !complete;
            } else if part.len() > MAX_LINE.saturating_sub(self.pending.len()) {
                // An oversized or unterminated line is data, not removable noise.
                emit(&self.pending);
                emit(part);
                self.pending.clear();
                self.passthrough = !complete;
            } else {
                self.pending.extend_from_slice(part);
                if complete {
                    let kind = classify(&self.pending);
                    match kind {
                        Some(true) => self.removed.progress = self.removed.progress.saturating_add(1),
                        Some(false) => self.removed.passed = self.removed.passed.saturating_add(1),
                        None => emit(&self.pending),
                    }
                    if kind.is_some() {
                        self.removed.bytes = self.removed.bytes.saturating_add(self.pending.len());
                    }
                    self.pending.clear();
                }
            }
        }
    }

    pub fn finish(&mut self, mut emit: impl FnMut(&[u8])) {
        // Preserve partial lines at EOF, timeout or pipe retirement.
        emit(&self.pending);
        self.pending.clear();
    }
}

/// Only ordinary SGR colors are ignored for classification. Kept lines retain
/// every original byte; unknown controls and non-UTF8 data are passed through.
fn classify(line: &[u8]) -> Option<bool> {
    let mut plain = Vec::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        if line[at] == 0x1b {
            if line.get(at + 1) != Some(&b'[') {
                return None;
            }
            at += 2;
            while line.get(at).is_some_and(|b| b.is_ascii_digit() || *b == b';' || *b == b':') {
                at += 1;
            }
            if line.get(at) != Some(&b'm') {
                return None;
            }
        } else {
            plain.push(line[at]);
        }
        at += 1;
    }
    let text = std::str::from_utf8(&plain).ok()?.trim_ascii();
    for verb in ["Compiling ", "Checking ", "Fresh "] {
        if let Some(rest) = text.strip_prefix(verb) {
            let mut words = rest.split_ascii_whitespace();
            let package = words.next()?;
            let version = words.next()?;
            if !package.is_empty()
                && version.strip_prefix('v').is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
            {
                return Some(true);
            }
        }
    }
    if text
        .strip_prefix("test ")
        .is_some_and(|rest| rest.strip_suffix(" ... ok").is_some_and(|name| !name.trim_ascii().is_empty()))
    {
        return Some(false);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(bytes: &[u8], chunk: usize) -> (Vec<u8>, Removed) {
        let mut filter = Filter::default();
        let mut kept = Vec::new();
        for piece in bytes.chunks(chunk) {
            filter.feed(piece, |b| kept.extend_from_slice(b));
            assert!(filter.pending.len() <= MAX_LINE);
        }
        filter.finish(|b| kept.extend_from_slice(b));
        (kept, filter.removed)
    }

    #[test]
    fn command_selection_does_not_guess_at_shell_programs() {
        for command in [
            "cargo check",
            "cargo +stable test",
            "cargo --locked -q build",
            "\"C:\\compiler tools\\cargo.exe\" clippy",
            "'/tools/my cargo/cargo' test",
            "cargo --color always check",
        ] {
            assert!(eligible(command), "{command}");
        }
        for command in [
            "echo cargo check",
            "cargo run",
            "cargo check | head",
            "cd project && cargo check",
            "cargo check; echo ok",
            "cargo check\necho next",
            "cargo check > output",
            "cargo check $(echo foo)",
            "cargo --unknown check",
            "\"cargo\"check",
            "cargo",
            "env cargo check",
        ] {
            assert!(!eligible(command), "{command}");
        }
    }

    #[test]
    fn split_colored_lines_are_removed_but_diagnostics_are_exact() {
        let input = b"\x1b[1;32m   Compiling\x1b[0m owned v1.0.0\r\ntest owned ... ok\nwarning: keep every byte\r\nerror[E0502]: exact\n --> src/lib.rs:4:2\ntest result: ok. 1 passed; 0 failed\n";
        let expected = b"warning: keep every byte\r\nerror[E0502]: exact\n --> src/lib.rs:4:2\ntest result: ok. 1 passed; 0 failed\n";
        for chunk in [1, 3, 17, 65536] {
            let (kept, removed) = filtered(input, chunk);
            assert_eq!(kept, expected);
            assert_eq!(removed.progress, 1);
            assert_eq!(removed.passed, 1);
            assert_eq!(removed.bytes, input.len() - expected.len());
        }
    }

    #[test]
    fn partial_long_non_utf8_and_unknown_control_lines_are_data() {
        let mut input = b"Compiling partial v1.0.0".to_vec();
        let (kept, removed) = filtered(&input, 1);
        assert_eq!(kept, input);
        assert_eq!(removed.bytes, 0);
        input.extend(vec![b'x'; MAX_LINE * 4]);
        input.extend_from_slice(
            b"\ntest \xff ... ok\n\x1b[2K   Checking owned v1.0.0\nBlocking waiting for file lock\n",
        );
        for chunk in [1, 65536] {
            let (kept, removed) = filtered(&input, chunk);
            assert_eq!(kept, input);
            assert_eq!(removed.bytes, 0);
        }
    }

    #[test]
    fn a_huge_chunk_and_newline_free_output_do_not_accumulate() {
        let input = vec![b'x'; 8 * MAX_LINE];
        let mut filter = Filter::default();
        let mut emitted = 0;
        filter.feed(&input, |b| emitted += b.len());
        assert_eq!(emitted, input.len());
        assert!(filter.pending.len() <= MAX_LINE);
        filter.feed(b"\n   Checking owned v1.0.0\n", |b| emitted += b.len());
        assert_eq!(filter.removed.progress, 1);
        assert_eq!(emitted, input.len() + 1);
    }
}
