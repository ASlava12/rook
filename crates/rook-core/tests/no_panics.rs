//! Panicking constructs in production code.
//!
//! `panic = "abort"` is set for release, so a panic anywhere in `rookd` ends the
//! whole daemon and every turn it was holding — and it does so without writing a
//! word into any session, because the process is gone before the loop can. One
//! did: a turn a thousand steps deep stopped mid-sentence at 00:22, its session
//! said nothing about why, and what was left of the reason was an address in a
//! crash report.
//!
//! Surviving a panic costs a quarter of both binaries, measured. Not having one
//! costs a rule, which is this. The five `.unwrap()` calls that were here are
//! gone — a poisoned lock is read through rather than died on, a serialisation
//! of two literals falls back rather than asserts, and a daemon that cannot
//! install a SIGTERM handler says so and keeps running.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The part of a file that ships. A test module is not production code, and
/// `#[cfg(test)]` is spelled several ways — `#[cfg(all(test, unix))]` hid eleven
/// of these from a first count.
fn production(text: &str) -> &str {
    text.lines()
        .position(|line| {
            let t = line.trim();
            t.starts_with("#[cfg(") && t.contains("test") && !t.contains("not(test)")
        })
        .map(|at| {
            let upto: usize = text.lines().take(at).map(|l| l.len() + 1).sum();
            &text[..upto.min(text.len())]
        })
        .unwrap_or(text)
}

/// `.unwrap()` says "this cannot fail" without saying why, and is wrong exactly
/// when nobody is watching. `expect` and `unreachable!` are allowed because they
/// carry the reason they are safe in the text that prints when they are not —
/// and are required to carry one.
#[test]
fn nothing_that_ships_can_panic_without_saying_why() {
    let root = repo_root();
    let mut found: Vec<String> = Vec::new();
    let mut read = (0usize, 0usize);

    for entry in ignore::WalkBuilder::new(root.join("crates")).build().flatten() {
        let path = entry.into_path();
        if path.extension().is_none_or(|e| e != "rs") || !path.to_string_lossy().contains("/src/") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let shown = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
        read = (read.0 + 1, read.1 + production(&text).lines().count());
        for (i, line) in production(&text).lines().enumerate() {
            let code = line.trim();
            if code.starts_with("//") {
                continue;
            }
            let at = format!("{shown}:{}", i + 1);
            if code.contains(".unwrap()") {
                found.push(format!("{at}: `.unwrap()` — return the error, or say why it cannot fail"));
            }
            for named in ["panic!(", "todo!(", "unimplemented!("] {
                if code.contains(named) {
                    found.push(format!("{at}: `{named}` in code that ships"));
                }
            }
            // A message is the whole of what makes these two acceptable: it is
            // what a crash report would otherwise not contain.
            for named in ["expect(", "unreachable!("] {
                if code.contains(&format!("{named})")) || code.contains(&format!("{named}\"\")")) {
                    found.push(format!("{at}: `{named}` with nothing to say when it fires"));
                }
            }
        }
    }

    // The precondition, because a walk that found nothing and a walk that
    // looked at nothing pass the same way — and `production` cutting at the
    // wrong line is exactly how the second happens.
    assert!(
        read.0 > 40 && read.1 > 20_000,
        "this must have read the workspace, and read {} lines of {} files",
        read.1,
        read.0
    );
    assert!(found.is_empty(), "code that ships must not panic:\n  {}", found.join("\n  "));
}
