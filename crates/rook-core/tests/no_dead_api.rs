//! Public functions nothing calls.
//!
//! Every crate here is a library, so `pub` exempts an item from the dead-code
//! lint entirely. Three shipped that way and each was found by hand: a skill's
//! bundled files were listed by a function nobody called, `tool_call_done` was
//! written for ACP and sent from nowhere, and `reload_skills` could not be
//! called at all. A function that exists and is never reached is a claim about
//! an API that is not there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Names that carry no information: a trait method or a convention, whose call
/// sites are the trait's, not a name in the text.
const CONVENTIONAL: &[&str] =
    &["new", "default", "fmt", "from", "clone", "drop", "call", "name", "spec", "read", "write"];

#[test]
fn every_public_function_is_called_somewhere() {
    let root = repo_root();
    let sources: Vec<String> = ignore::WalkBuilder::new(&root)
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .filter(|p| !p.starts_with(root.join("references")))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .collect();
    let whole = sources.join("\n");

    let mut declared: BTreeMap<String, usize> = BTreeMap::new();
    for line in whole.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("pub fn ").or_else(|| line.strip_prefix("pub async fn ")) {
            let name = rest.split(['(', '<']).next().unwrap_or_default();
            if !name.is_empty() && !CONVENTIONAL.contains(&name) {
                *declared.entry(name.to_string()).or_default() += 1;
            }
        }
    }
    assert!(declared.len() > 100, "the parser found only {}, so it is broken", declared.len());

    let uncalled: Vec<&String> = declared
        .iter()
        .filter(|(name, declarations)| whole.matches(name.as_str()).count() <= **declarations)
        .map(|(name, _)| name)
        .collect();

    assert!(
        uncalled.is_empty(),
        "public but called by nothing, so the API it advertises does not exist: {uncalled:?}"
    );
}
