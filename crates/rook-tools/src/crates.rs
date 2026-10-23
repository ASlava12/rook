//! What a crate offers, read from the copy already on this machine.
//!
//! A model that has forgotten a signature guesses at it, and a guess that
//! compiles is worse than one that does not. The answer is on disk: cargo
//! unpacks every dependency under its registry, and `Cargo.lock` says which
//! version this project resolved to.
//!
//! Not rustdoc JSON, which would be the right answer and is nightly-only, and
//! not docs.rs, which would be a network round trip for something already here.
//! The scanner is deliberately not a parser — the same trade as the HTML one:
//! what is wanted is the signature line, and a real parser costs more than the
//! rest of this binary.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::json;

use rook_llm::ToolSpec;

use crate::{Result, Tool, ToolContext, ToolOutcome, arg_str};
mod identity;

/// Signatures per answer. A crate the size of `syn` has thousands, and a list
/// nobody can read is not an answer.
const MOST_ITEMS: usize = 200;

pub struct CrateApi;

#[async_trait]
impl Tool for CrateApi {
    fn name(&self) -> &str {
        "crate_api"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "crate_api".into(),
            description: "Read a locked Rust package's local API; select version if ambiguous.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "crate": { "type": "string", "description": "package name in Cargo.lock" },
                    "version": { "type": "string", "description": "exact locked version" },
                    "entity": {
                        "type": "string",
                        "description": "a type or trait; its methods come with it"
                    }
                },
                "required": ["crate"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let wanted = arg_str(args, self.name(), "crate")?;
        let version = match args.get("version") {
            None => None,
            Some(serde_json::Value::String(value)) if !value.trim().is_empty() => Some(value.trim()),
            Some(_) => return Ok(ToolOutcome::error("version must be a nonempty exact locked version")),
        };
        let entity = args.get("entity").and_then(|e| e.as_str()).map(str::trim).filter(|e| !e.is_empty());

        let selected = match identity::resolve(&ctx.workspace, &wanted, version) {
            Ok(selected) => selected,
            Err(error) => return Ok(ToolOutcome::error(error)),
        };
        let version = selected.version;
        let source = match identity::unpacked(&wanted, &version, &selected.checksum) {
            Ok(source) => source,
            Err(error) => return Ok(ToolOutcome::error(error)),
        };

        let mut found = Vec::new();
        let incomplete = collect(&source, entity, &mut found);

        let full = found.len();
        found.truncate(MOST_ITEMS);
        let mut body = format!("{wanted} {version}\n{}\n\n{}", source.display(), found.join("\n"));
        if found.is_empty() {
            body.push_str("No public items found in the admitted source.");
        }
        if full > MOST_ITEMS {
            body.push_str("\n\n[More items may exist; narrow with `entity`]");
        }
        if incomplete {
            body.push_str("\n\n[Source scan incomplete: a read, traversal or output limit was reached]");
        }
        Ok(ToolOutcome {
            images: Vec::new(),
            full_bytes: body.len(),
            content: body,
            is_error: false,
            truncated: full > MOST_ITEMS || incomplete,
            meta: Default::default(),
        }
        .with("version", version)
        .with("package_checksum", selected.checksum)
        .with("scan_incomplete", incomplete)
        .with("items", full))
    }
}

/// Public signatures, in file order, with the `impl` they belong to.
fn collect(source: &Path, entity: Option<&str>, out: &mut Vec<String>) -> bool {
    let root = source.join("src");
    // Walkers follow their root even with follow_links disabled. Admit the
    // root separately so a symlink/junction cannot silently change the source.
    if !std::fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
        return true;
    }
    let mut incomplete = false;
    let mut files: Vec<PathBuf> = Vec::new();
    for (index, entry) in ignore::WalkBuilder::new(root).max_depth(Some(16)).build().enumerate() {
        if index == 4096 {
            incomplete = true;
            break;
        }
        let Ok(entry) = entry else {
            incomplete = true;
            continue;
        };
        if entry.depth() == 16 && entry.file_type().is_some_and(|t| t.is_dir()) {
            incomplete = true;
        }
        if !entry.path().extension().is_some_and(|e| e == "rs") {
            continue;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            incomplete = true;
            continue;
        }
        if files.len() == 512 {
            incomplete = true;
            break;
        }
        files.push(entry.into_path());
    }
    files.sort();
    let mut remaining = 8 * 1024 * 1024usize;
    let mut output_bytes = 0usize;
    for file in files {
        if remaining <= 1 {
            return true;
        }
        let maximum = (256 * 1024).min(remaining - 1);
        let bytes = match identity::limited(&file, maximum) {
            Ok(bytes) => bytes,
            Err(_) => {
                remaining = remaining.saturating_sub(maximum + 1);
                incomplete = true;
                continue;
            }
        };
        remaining -= bytes.len();
        let Ok(text) = std::str::from_utf8(&bytes) else {
            incomplete = true;
            continue;
        };
        let mut within: Option<String> = None;
        for line in text.lines() {
            let trimmed = line.trim();
            // An `impl` at the left margin opens a block; the methods below it
            // belong to whatever it named, which is the thing being asked about.
            if line.starts_with("impl ") {
                within = impl_target(trimmed);
            } else if line == "}" {
                within = None;
            }

            let Some(signature) = public_item(trimmed) else { continue };
            let named = match &within {
                Some(target) => format!("{target}::{signature}"),
                None => signature,
            };
            if entity.is_none_or(|e| named.contains(e)) {
                if named.len() > 4096 || output_bytes + named.len() > 256 * 1024 {
                    return true;
                }
                output_bytes += named.len();
                out.push(named);
                if out.len() > MOST_ITEMS {
                    return incomplete;
                }
            }
        }
    }
    incomplete
}

/// The type an `impl` block is for: `impl<T> Trait for Foo<T>` is about `Foo`.
fn impl_target(line: &str) -> Option<String> {
    let body = line.strip_prefix("impl")?.trim_start();
    let body = body
        .strip_prefix('<')
        .map_or(body, |rest| rest.split_once('>').map(|(_, after)| after.trim_start()).unwrap_or(rest));
    let target = body.rsplit(" for ").next().unwrap_or(body);
    let name: String = target.trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// The signature, if the line declares something public.
fn public_item(line: &str) -> Option<String> {
    const KINDS: [&str; 8] = [
        "pub fn ",
        "pub async fn ",
        "pub const fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "pub const ",
    ];
    if !KINDS.iter().any(|k| line.starts_with(k)) {
        return None;
    }
    // To the body or the end of the declaration, whichever comes first: a
    // signature is the useful half and a body is not.
    let cut = line.find(" {").or_else(|| line.find(';')).unwrap_or(line.len());
    Some(line[..cut].trim_end().to_string())
}
