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

/// The same question of an error's variants. `ContextOverflow` was declared for
/// a case nothing checked, so a prompt too large for the window went to the
/// provider whole; `NoSuchSymbol` existed while two call sites formatted the
/// same sentence by hand.
#[test]
fn every_error_variant_is_constructed_somewhere() {
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
    let mut inside = false;
    for line in whole.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub enum ") && trimmed.contains("Error") {
            inside = true;
            continue;
        }
        if inside && trimmed == "}" {
            inside = false;
        }
        if !inside {
            continue;
        }
        // A variant opens a line and is immediately followed by its shape or a
        // comma. Without that, a word inside a multi-line `#[error(...)]` string
        // reads as one.
        let name: String = trimmed.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let shape = trimmed[name.len()..].trim_start().chars().next();
        if !name.is_empty()
            && name.starts_with(char::is_uppercase)
            && matches!(shape, Some('(') | Some('{') | Some(','))
        {
            *declared.entry(name).or_default() += 1;
        }
    }
    assert!(declared.len() > 20, "the parser found only {}, so it is broken", declared.len());

    // One mention per declaration is the declaration alone; anything more is
    // somebody raising it, including a generated `From`.
    let unraised: Vec<&String> = declared
        .iter()
        .filter(|(name, declarations)| whole.matches(name.as_str()).count() <= **declarations)
        .map(|(name, _)| name)
        .collect();

    assert!(
        unraised.is_empty(),
        "declared but raised by nothing, so the case it names is not handled: {unraised:?}"
    );
}

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
