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
///
/// It asks only of enums named `*Error`, and it counts mentions rather than
/// constructions — so a variant that is matched on but never built reads as
/// used. `SkillSource::Plugin` sat that way, ranked and labelled and made by
/// nothing. Widening it was tried: qualifying the name (`Enum::Variant`) to
/// separate a value from a pattern flags eighteen variants that serde builds
/// from the wire, and an exemption list that long proves less than it costs.
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

/// The same question asked of production alone.
///
/// Counting every `.rs` file lets a function that only tests reach pass as used,
/// which is a different thing from used: it is API the library advertises to
/// everyone and offers to nobody. A genuine test seam — one that exists because
/// the alternative is filling a context window or sleeping for an hour — says so
/// with `#[doc(hidden)]`, and that is the whole exemption.
#[test]
fn a_public_function_no_production_code_calls_says_it_is_a_test_seam() {
    let root = repo_root();
    let production: Vec<String> = ignore::WalkBuilder::new(&root)
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .filter(|p| !p.starts_with(root.join("references")))
        .filter(|p| p.components().any(|c| c.as_os_str() == "src"))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .map(|text| before_inline_tests(&text))
        .collect();
    let whole = production.join("\n");

    let mut unreachable: Vec<String> = Vec::new();
    for text in &production {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let Some(rest) =
                trimmed.strip_prefix("pub fn ").or_else(|| trimmed.strip_prefix("pub async fn "))
            else {
                continue;
            };
            let name = rest.split(['(', '<']).next().unwrap_or_default();
            if name.is_empty() || CONVENTIONAL.contains(&name) {
                continue;
            }
            // The attribute sits directly above, after any doc comment.
            let marked = lines[..i]
                .iter()
                .rev()
                .take_while(|l| {
                    let l = l.trim();
                    l.starts_with("///") || l.starts_with("#[") || l.is_empty()
                })
                .any(|l| l.trim() == "#[doc(hidden)]");
            let declarations = whole.matches(&format!("fn {name}")).count();
            if !marked && whole.matches(name).count() <= declarations {
                unreachable.push(name.to_string());
            }
        }
    }
    unreachable.sort();
    unreachable.dedup();

    assert!(
        unreachable.is_empty(),
        "no production code calls these, so they are API offered to nobody — wire one up, delete \
         it, or mark a real test seam `#[doc(hidden)]`: {unreachable:?}"
    );
}

/// A file without the inline `mod tests` at the end of it.
fn before_inline_tests(text: &str) -> String {
    match text.find("\n#[cfg(test)]") {
        Some(at) => text[..at].to_string(),
        None => text.to_string(),
    }
}
