//! Skills that exist somewhere else, and putting one of them on this machine.
//!
//! A source is a git repository or a directory. Its skills are read from disk —
//! a `SKILL.md` under `skills/`, or anywhere in a plain directory — so a source
//! is just a place with skills in it and needs no index, no API and no
//! agreement beyond the format everything here already speaks.
//!
//! Nothing is fetched on its own. A search or an install reaches the network;
//! opening the store, starting a turn and listing what is installed do not.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::paths;

/// A skill a source offers, before anything is installed.
#[derive(Clone, Debug)]
pub struct Offered {
    pub name: String,
    pub description: String,
    /// The source it came from, as written in configuration.
    pub source: String,
    /// Where it sits in the fetched copy.
    pub dir: PathBuf,
}

/// Everything the configured sources offer, and what went wrong with the ones
/// that could not be read.
///
/// `refresh` decides whether the network is touched: a search that has just run
/// costs nothing the second time.
pub fn offered(sources: &[String], refresh: bool) -> (Vec<Offered>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for source in sources {
        match fetch(source, refresh) {
            Ok(root) => found.extend(read_skills(&root, source)),
            Err(e) => errors.push(format!("{source}: {e}")),
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    (found, errors)
}

/// The ones whose name or description answer `query`, best first.
pub fn matching<'a>(offered: &'a [Offered], query: &str) -> Vec<&'a Offered> {
    // The words that carry meaning, by the same rule memory ranks facts with:
    // scoring on "a" and "the" ranks whichever description is longest.
    let wanted = crate::memory::terms_of(query);
    let score = |o: &Offered| {
        let name = o.name.to_lowercase();
        let haystack = format!("{} {}", o.name, o.description).to_lowercase();
        // A name match is the strong signal and a word in the description the
        // weak one, which is why they are not worth the same.
        wanted.iter().map(|w| usize::from(name.contains(w.as_str())) * 4).sum::<usize>()
            + wanted.iter().filter(|w| haystack.contains(w.as_str())).count()
    };
    let mut ranked: Vec<(usize, &Offered)> =
        offered.iter().map(|o| (score(o), o)).filter(|(n, _)| wanted.is_empty() || *n > 0).collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    ranked.into_iter().map(|(_, o)| o).collect()
}

/// Copy it in, replacing whatever was there under that name.
///
/// The whole directory, because a skill's scripts and references are the half
/// that does the work — `SKILL.md` alone is instructions for tools that are
/// not there.
pub fn install(skill: &Offered, into: &Path) -> Result<PathBuf> {
    let target = into.join(&skill.name);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| CoreError::Io { path: target.clone(), source: e })?;
    }
    copy_tree(&skill.dir, &target)?;
    Ok(target)
}

fn fetch(source: &str, refresh: bool) -> Result<PathBuf> {
    let local = Path::new(source);
    if local.is_dir() {
        return Ok(local.to_path_buf());
    }
    if !source.starts_with("http://") && !source.starts_with("https://") && !source.contains('@') {
        return Err(CoreError::Other(format!("{source:?} is neither a directory nor a repository")));
    }

    let into = paths::sources_cache().join(cache_name(source));
    if into.join(".git").is_dir() {
        if refresh {
            git(&["fetch", "--depth", "1", "origin", "HEAD"], Some(&into))?;
            git(&["reset", "--hard", "FETCH_HEAD"], Some(&into))?;
        }
        return Ok(into);
    }
    if let Some(parent) = into.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io { path: parent.into(), source: e })?;
    }
    git(&["clone", "--depth", "1", source, &into.display().to_string()], None)?;
    Ok(into)
}

/// One directory per source, named after it rather than hashed: a cache you
/// cannot read is one you cannot clear with any confidence.
fn cache_name(source: &str) -> String {
    source
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut command = std::process::Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let out = command
        .output()
        .map_err(|e| CoreError::Other(format!("git is needed to fetch a skill source: {e}")))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(CoreError::Other(String::from_utf8_lossy(&out.stderr).trim().to_string())),
    }
}

fn read_skills(root: &Path, source: &str) -> Vec<Offered> {
    let mut found = Vec::new();
    for entry in ignore::WalkBuilder::new(root).max_depth(Some(4)).require_git(false).build().flatten() {
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        let Some(dir) = entry.path().parent() else { continue };
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok((manifest, _)) = rook_skills::parse_manifest(&text, entry.path()) else { continue };
        found.push(Offered {
            name: manifest.name,
            description: manifest.description,
            source: source.to_string(),
            dir: dir.to_path_buf(),
        });
    }
    found
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| CoreError::Io { path: to.into(), source: e })?;
    for entry in std::fs::read_dir(from).map_err(|e| CoreError::Io { path: from.into(), source: e })? {
        let entry = entry.map_err(|e| CoreError::Io { path: from.into(), source: e })?;
        let target = to.join(entry.file_name());
        match entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            true => copy_tree(&entry.path(), &target)?,
            false => {
                std::fs::copy(entry.path(), &target)
                    .map_err(|e| CoreError::Io { path: target.clone(), source: e })?;
            }
        }
    }
    Ok(())
}
