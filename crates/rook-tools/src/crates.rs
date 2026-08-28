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
            description: "List what a Rust dependency offers, read from the source this project \
                          resolved to. Use it instead of recalling a signature."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "crate": { "type": "string", "description": "as named in Cargo.toml" },
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
        let entity = args.get("entity").and_then(|e| e.as_str()).map(str::trim).filter(|e| !e.is_empty());

        let Some(version) = resolved(&ctx.workspace, &wanted) else {
            return Ok(ToolOutcome::error(format!(
                "{wanted:?} is not in this project's Cargo.lock — check the name, or add it first"
            )));
        };
        let Some(source) = unpacked(&wanted, &version) else {
            return Ok(ToolOutcome::error(format!(
                "{wanted} {version} is not unpacked in the cargo registry — `cargo fetch` puts it \
                 there. A path or git dependency is not there at all."
            )));
        };

        let mut found = Vec::new();
        collect(&source, entity, &mut found);
        if found.is_empty() {
            let what = entity.map(|e| format!("{e} in ")).unwrap_or_default();
            return Ok(ToolOutcome::ok(format!("nothing public found for {what}{wanted} {version}")));
        }

        let full = found.len();
        found.truncate(MOST_ITEMS);
        let mut body = format!("{wanted} {version}\n{}\n\n{}", source.display(), found.join("\n"));
        if full > MOST_ITEMS {
            body.push_str(&format!("\n\n[{} more; narrow it with `entity`]", full - MOST_ITEMS));
        }
        Ok(ToolOutcome {
            content: body,
            is_error: false,
            truncated: full > MOST_ITEMS,
            full_bytes: 0,
            meta: Default::default(),
        }
        .with("version", version)
        .with("items", full))
    }
}

/// The version this project resolved to, from the nearest `Cargo.lock`.
///
/// The lock rather than the manifest: the manifest says `1`, and what is on disk
/// is `1.0.229`. Walked upward because a crate in a workspace has its own
/// manifest and the workspace's lock.
fn resolved(from: &Path, wanted: &str) -> Option<String> {
    let mut here = Some(from);
    while let Some(dir) = here {
        if let Ok(lock) = std::fs::read_to_string(dir.join("Cargo.lock"))
            && let Some(version) = version_in(&lock, wanted)
        {
            return Some(version);
        }
        here = dir.parent();
    }
    None
}

fn version_in(lock: &str, wanted: &str) -> Option<String> {
    let mut named = false;
    for line in lock.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix("name = ") {
            named = name.trim_matches('"') == wanted;
        } else if named && let Some(version) = line.strip_prefix("version = ") {
            return Some(version.trim_matches('"').to_string());
        }
    }
    None
}

/// Where cargo put the source. Registries are named by a hash, so the directory
/// is searched rather than computed.
fn unpacked(name: &str, version: &str) -> Option<PathBuf> {
    let home = std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let user = ["HOME", "USERPROFILE"]
            .into_iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_default();
        PathBuf::from(user).join(".cargo")
    });
    let wanted = format!("{name}-{version}");
    std::fs::read_dir(home.join("registry").join("src"))
        .ok()?
        .flatten()
        .map(|registry| registry.path().join(&wanted))
        .find(|candidate| candidate.is_dir())
}

/// Public signatures, in file order, with the `impl` they belong to.
fn collect(source: &Path, entity: Option<&str>, out: &mut Vec<String>) {
    let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(source.join("src"))
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.sort();

    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
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
                out.push(named);
            }
        }
    }
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
