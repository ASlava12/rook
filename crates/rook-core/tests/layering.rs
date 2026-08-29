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
        ("rook-llm", 0),
        ("rook-lsp", 0),
        ("rook-proto", 0),
        ("rook-skills", 0),
        ("rook-store", 0),
        // Speaks to somebody else's tools, in the shapes `rook-llm` defines.
        ("rook-mcp", 1),
        ("rook-tools", 2),
        // The engine. Everything above it is a way of driving it.
        ("rook-core", 3),
        ("rook-acp", 4),
        ("rookd", 4),
        ("rook-cli", 5),
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
