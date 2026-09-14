//! The one-way dependency graph, checked rather than described.
//!
//! [`CLAUDE.md`](../../../CLAUDE.md) states it as a rule — "do not add an edge
//! that reverses them" — and nothing held it, so the table beside the rule had
//! already drifted from the manifests it described. A layer that only exists in
//! prose is a layer somebody crosses without noticing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How far from the bottom each crate sits. A dependency must be strictly
/// lower, which is the whole rule; ranks say it without enumerating every edge,
/// so an edge that is merely new does not need this file edited.
fn layers() -> BTreeMap<&'static str, u8> {
    BTreeMap::from([
        // Nothing internal: each is a thing in itself, and the store must never
        // learn what a skill or a checkpoint is.
        // Below everything: platform glue with no dependencies of its own, and
        // the one place Win32 lives. Anything may reach for it — a language
        // server and a toolchain probe both start processes, and starting one
        // without a console window is its answer — and it reaches for nothing.
        ("rook-contain", 0),
        ("rook-llm", 1),
        ("rook-lsp", 1),
        ("rook-proto", 1),
        ("rook-skills", 1),
        ("rook-store", 1),
        // Speaks to somebody else's tools, in the shapes `rook-llm` defines.
        ("rook-mcp", 2),
        ("rook-tools", 3),
        // The engine. Everything above it is a way of driving it.
        ("rook-core", 4),
        ("rook-acp", 5),
        ("rookd", 5),
        ("rook-cli", 6),
    ])
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Internal dependencies, however they are spelled: `rook-core.workspace = true`
/// and `rook-mcp = { path = … }` both begin the line with the name.
fn depends_on(manifest: &str) -> Vec<String> {
    manifest
        .lines()
        .filter_map(|line| line.split(['=', ' ', '.']).next())
        .map(str::trim)
        .filter(|name| name.starts_with("rook-") || *name == "rookd")
        .map(str::to_string)
        .collect()
}

#[test]
fn no_crate_depends_on_one_above_it() {
    let layers = layers();
    let mut checked = 0;

    for entry in std::fs::read_dir(crates_dir()).unwrap().flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        let rank = *layers
            .get(name.as_str())
            .unwrap_or_else(|| panic!("{name} is not placed in the layering above — put it in one"));
        checked += 1;

        for dep in depends_on(&text) {
            let below = *layers
                .get(dep.as_str())
                .unwrap_or_else(|| panic!("{name} depends on {dep}, which is not placed"));
            assert!(
                below < rank,
                "{name} (layer {rank}) depends on {dep} (layer {below}) — that edge runs the \
                 wrong way, or the layering above is out of date"
            );
        }
    }

    assert_eq!(checked, layers.len(), "every crate is checked, and only crates that exist");
}

/// An internal crate is depended on by every platform or by none.
///
/// `rook-tools` had `rook-contain` under `[target.'cfg(windows)'.dependencies]`,
/// from when the only thing wanted from it was a job object. The moment
/// something outside a `#[cfg(windows)]` called into it — one function turning
/// a command's bytes into text, which every platform needs — the crate stopped
/// existing on Linux and macOS. It compiled on the machine it was written on,
/// the gate there was green, and four CI jobs failed on the push.
///
/// The rule costs nothing: an internal crate cfg-gates its own contents, has no
/// dependencies of its own where it does nothing, and `rook-cli` and `rookd`
/// already declare this one outright. A per-target edge buys no compile time
/// and hides a whole platform's build from the gate.
#[test]
fn an_internal_crate_is_never_a_dependency_of_one_platform_only() {
    let mut checked = 0;

    for entry in std::fs::read_dir(crates_dir()).unwrap().flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        checked += 1;

        let mut section = "";
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                section = if line.starts_with("[target.") { "target" } else { "" };
                continue;
            }
            let Some(dep) = line.split(['=', ' ', '.']).next().map(str::trim) else { continue };
            let internal = dep.starts_with("rook-") || dep == "rookd";
            assert!(
                !(section == "target" && internal),
                "{name} depends on {dep} for one platform only. Declare it under `[dependencies]`: \
                 the first use of it from code that is not itself `#[cfg]`-gated compiles on that \
                 platform and on no other, and the gate runs on one platform"
            );
        }
    }

    assert_eq!(checked, layers().len(), "every crate is checked, and only crates that exist");
}

/// A file that starts a process says how it is to be started.
///
/// Windows hands every process it starts a console unless told otherwise, and a
/// console is a window. Nothing here wants one — every child is handed pipes —
/// so each was a window that opened and shut on somebody's desktop: sixteen at
/// startup for the toolchain probes, more per turn for the shell, the language
/// server and the MCP server. Reported from a real desktop as a swarm of them,
/// and invisible to every test, because a window is not something a test sees.
///
/// So it is asked of the source instead: a file that spawns says, somewhere in
/// it, which flags it spawns with. Coarse on purpose — what this catches is the
/// next file that starts a process and says nothing, which is how this got here.
#[test]
fn every_file_that_starts_a_process_says_how() {
    // `DETACHED_PROCESS` for the daemon, which wants no console at all and its
    // own process group; `quietly` or `NO_WINDOW` for everything else.
    const SAYS_HOW: [&str; 3] = ["creation_flags", "quietly", "NO_WINDOW"];
    let mut checked = 0;

    for entry in walk(&crates_dir()) {
        let Ok(text) = std::fs::read_to_string(&entry) else { continue };
        if !text.contains("Command::new") {
            continue;
        }
        checked += 1;
        assert!(
            SAYS_HOW.iter().any(|said| text.contains(said)),
            "{} starts a process and never says with what flags — on Windows that is a console \
             window opening and shutting for each one. `rook_contain::quietly` is the answer for \
             a `std` command, `creation_flags(rook_contain::NO_WINDOW)` for a `tokio` one",
            entry.display()
        );
    }
    // Or the loop found nothing and proved nothing.
    assert!(checked >= 6, "only {checked} files were found to start a process, which is too few");
}

/// Every `src` file under `crates/`, without pulling in a directory walker.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        // `src` only: a test may spawn a bare command to check something about
        // spawning, and what is being guarded is what ships.
        if path.is_dir() && path.file_name().is_some_and(|n| n == "tests") {
            continue;
        }
        match path.is_dir() {
            true => out.extend(walk(&path)),
            false if path.extension().is_some_and(|e| e == "rs") => out.push(path),
            false => {}
        }
    }
    out
}
