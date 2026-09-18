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
    // Cut at an offset taken from the text rather than reconstructed from it.
    // This summed the lengths `lines()` reports and added one byte for each
    // newline — which is short by one on every line of a Windows checkout,
    // where the separator is two bytes. The cut then landed a byte or two
    // early, and on the run that found it that was the middle of an em dash:
    // the test that forbids panics panicked, and only on the platform this
    // repository cannot check before pushing. `split_inclusive` keeps the
    // separator, so the offset is real and is always a character boundary.
    let mut upto = 0usize;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("#[cfg(") && t.contains("test") && !t.contains("not(test)") {
            return &text[..upto];
        }
        upto += line.len();
    }
    text
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
        // Asked of the path rather than spelled into it: `contains("/src/")` is
        // true of every file here and of none on Windows, where the runner read
        // nothing at all and said so — which is the only reason this was not a
        // test that passed by looking at an empty workspace.
        let in_src = path.components().any(|c| c.as_os_str() == "src");
        if path.extension().is_none_or(|e| e != "rs") || !in_src {
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

/// Where the production part ends cannot depend on how the file was checked
/// out. It did: the offset was rebuilt by summing what `lines()` reports and
/// adding one byte per newline, which is short by one on every line of a
/// Windows checkout — so the cut fell a byte or two early, and on the run that
/// found it that was the middle of an em dash. The test that forbids panics
/// panicked, on the one platform the gate here cannot reach.
#[test]
fn where_the_production_part_ends_does_not_depend_on_the_line_endings() {
    let unix = "//! — a dash, two bytes wide\nfn ships() {}\n#[cfg(test)]\nmod tests {}\n";
    let windows = unix.replace('\n', "\r\n");

    let kept = production(unix);
    assert!(kept.contains("fn ships()"), "what ships is kept: {kept:?}");
    assert!(!kept.contains("mod tests"), "and the test module is not: {kept:?}");
    assert_eq!(
        production(&windows).replace("\r\n", "\n"),
        kept,
        "and a checkout with two-byte line endings cuts in the same place"
    );
}
