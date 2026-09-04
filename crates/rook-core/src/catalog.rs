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
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    // Whole words, by the rule memory ranks facts with, and not substrings:
    // `contains` had "port" matching "supports", so a search for a port offered
    // five unrelated skills and told the model to install one by name. It did.
    let score = |o: &Offered| {
        let name = crate::memory::terms_of(&o.name.replace('-', " "));
        let described = crate::memory::terms_of(&format!("{} {}", o.name.replace('-', " "), o.description));
        let like = |terms: &std::collections::BTreeSet<String>, w: &String| {
            terms.iter().any(|t| crate::memory::akin(t, w))
        };
        // A name match is the strong signal and a word in the description the
        // weak one, which is why they are not worth the same.
        wanted.iter().filter(|w| like(&name, w)).count() * 4
            + wanted.iter().filter(|w| like(&described, w)).count()
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
    // The name comes from a `SKILL.md` in somebody else's repository and becomes
    // a directory here. `parse` already refuses one that cannot be a directory,
    // and this is the check that guards the `remove_dir_all` below: the parser
    // is one caller away, and what is about to be deleted recursively deserves
    // to be proven inside the skills directory rather than assumed.
    if !rook_skills::usable_name(&skill.name) {
        return Err(CoreError::Other(format!(
            "{:?} from {} cannot be a skill name — it would install outside the skills directory",
            skill.name, skill.source
        )));
    }
    let target = into.join(&skill.name);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| CoreError::Io { path: target.clone(), source: e })?;
    }
    copy_tree(&skill.dir, &target, &mut Budget::new())?;
    Ok(target)
}

/// What one install may copy.
///
/// A source is a repository someone else controls, and `git clone` brings all of
/// it. Without this the size of the skills directory is decided there — the same
/// reason `CaptureLimits` exists for a checkpoint.
struct Budget {
    files: usize,
    bytes: u64,
}

impl Budget {
    const MOST_FILES: usize = 2_000;
    const MOST_BYTES: u64 = 64 << 20;

    fn new() -> Self {
        Self { files: 0, bytes: 0 }
    }

    fn charge(&mut self, path: &Path, len: u64) -> Result<()> {
        self.files += 1;
        self.bytes += len;
        match self.files > Self::MOST_FILES || self.bytes > Self::MOST_BYTES {
            true => Err(CoreError::CaptureTooBig {
                what: format!("{} files / {} bytes at {}", self.files, self.bytes, path.display()),
                limit: format!("a skill may bring {} files and {} bytes", Self::MOST_FILES, Self::MOST_BYTES),
            }),
            false => Ok(()),
        }
    }
}

fn fetch(source: &str, refresh: bool) -> Result<PathBuf> {
    let local = Path::new(source);
    if local.is_dir() {
        return Ok(local.to_path_buf());
    }
    let repository =
        (source.starts_with("http://") || source.starts_with("https://") || source.contains('@'))
            && !source.starts_with('-');
    if !repository {
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
    // `--` because `source` is configuration: a value beginning with `-` would
    // otherwise reach git in the position where it reads options.
    git(&["clone", "--depth", "1", "--", source, &into.display().to_string()], None)?;
    Ok(into)
}

/// One directory per source, named after it rather than hashed: a cache you
/// cannot read is one you cannot clear with any confidence.
fn cache_name(source: &str) -> String {
    let name: String = source
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect();
    // Dots are kept because they are half of what makes a host readable, and
    // trimmed at the ends because a name of nothing but them is `..` — a
    // directory built from configuration that points outside the cache. No
    // separator survives the mapping, so this is the only way out.
    match name.trim_matches(['-', '.']) {
        "" => "source".into(),
        trimmed => trimmed.to_string(),
    }
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

fn copy_tree(from: &Path, to: &Path, budget: &mut Budget) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| CoreError::Io { path: to.into(), source: e })?;
    for entry in std::fs::read_dir(from).map_err(|e| CoreError::Io { path: from.into(), source: e })? {
        let entry = entry.map_err(|e| CoreError::Io { path: from.into(), source: e })?;
        let Ok(kind) = entry.file_type() else { continue };
        // Skipped, not followed: `read_dir` reports a symlink as itself while
        // `fs::copy` reads through it, so a link in the source repository would
        // copy whatever it points at on this machine into the skills directory.
        if kind.is_symlink() {
            continue;
        }
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, budget)?;
            continue;
        }
        budget.charge(&entry.path(), entry.metadata().map(|m| m.len()).unwrap_or(0))?;
        std::fs::copy(entry.path(), &target)
            .map_err(|e| CoreError::Io { path: target.clone(), source: e })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Offered, cache_name, matching};

    fn offering(name: &str, description: &str) -> Offered {
        Offered {
            name: name.into(),
            description: description.into(),
            source: "somewhere".into(),
            dir: std::path::PathBuf::new(),
        }
    }

    /// Matching on substrings had "port" inside "supports", so a search for a
    /// port answered with five unrelated skills and an invitation to install
    /// one by name. A model that already had the answer took it, and ran out of
    /// steps installing and loading skills about spreadsheets.
    #[test]
    fn a_search_matches_words_rather_than_the_middles_of_them() {
        let offered = vec![
            offering("webapp-testing", "Supports verifying frontend behaviour."),
            offering("port-forwarding", "Forward a port to a local process."),
        ];

        let found = matching(&offered, "port");

        assert_eq!(found.len(), 1, "only the one that is about ports: {:?}", found);
        assert_eq!(found[0].name, "port-forwarding");
    }

    /// The prefix rule is what makes a search usable, and it is memory's, so
    /// the two rank by the same idea of what a word is.
    #[test]
    fn a_plural_still_finds_the_singular_and_a_hyphenated_name_its_parts() {
        let offered = vec![offering("theme-factory", "Styling for artifacts.")];

        assert_eq!(matching(&offered, "styling").len(), 1, "a description word");
        assert_eq!(matching(&offered, "factory").len(), 1, "and half a hyphenated name");
        assert_eq!(matching(&offered, "artifact").len(), 1, "singular against a plural");
        assert!(matching(&offered, "database").is_empty(), "and nothing that shares no word");
    }

    /// The name becomes a directory under the cache, and it is built from
    /// configuration. No separator survives the mapping, so the only way out was
    /// a name of nothing but dots.
    #[test]
    fn a_cache_name_is_always_one_directory_inside_the_cache() {
        assert_eq!(cache_name("https://github.com/rook/skills.git"), "https---github.com-rook-skills");
        assert_eq!(cache_name("git@github.com:rook/skills"), "git-github.com-rook-skills");
        assert_eq!(cache_name("..@"), "source", "not the cache's parent");
        assert_eq!(cache_name("https://..."), "https", "and never only dots");
        assert!(!cache_name("https://../../x").contains('/'), "and never a path of its own");
    }
}
