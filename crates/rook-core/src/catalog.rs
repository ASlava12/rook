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
    std::fs::create_dir_all(into).map_err(|e| CoreError::Io { path: into.into(), source: e })?;
    let stage = tempfile::tempdir_in(into).map_err(|e| CoreError::Io { path: into.into(), source: e })?;
    let prepared = stage.path().join("prepared");
    copy_tree(&skill.dir, &prepared, &mut Budget::new())?;
    let card = prepared.join("SKILL.md");
    let text = std::fs::read_to_string(&card).map_err(|e| CoreError::Io { path: card.clone(), source: e })?;
    let (manifest, _) = rook_skills::parse_manifest(&text, &card)?;
    if manifest.name != skill.name {
        return Err(CoreError::Other(
            "source changed its skill identity during installation; retry discovery".into(),
        ));
    }
    // The previous directory survives every preparation failure. Renames stay
    // on one filesystem; a failed final rename restores the previous copy.
    let backup = stage.path().join("previous");
    let existed = target.symlink_metadata().is_ok();
    if existed {
        std::fs::rename(&target, &backup).map_err(|e| CoreError::Io { path: target.clone(), source: e })?;
    }
    if let Err(error) = std::fs::rename(&prepared, &target) {
        if existed && let Err(restore) = std::fs::rename(&backup, &target) {
            let kept = stage.keep();
            return Err(CoreError::Other(format!(
                "skill replacement failed ({error}); restore failed ({restore}); previous files retained in {}",
                kept.display()
            )));
        }
        return Err(CoreError::Io { path: target, source: error });
    }
    Ok(target)
}

/// Equivalent local paths share identity; remote addresses remain exact.
pub(crate) fn source_identity(source: &str) -> String {
    let path = Path::new(source);
    if path.is_dir() || path.is_absolute() || source.starts_with('.') {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf()).display().to_string();
    }
    // An explicit origin or a saved installation origin must not match a
    // different configured HTTP repository merely because `.git` was trimmed.
    source.to_string()
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
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // Leave room for Git's configuration/bootstrap paths. Long object
        // names in shorter roots are handled by the command-local opt-in.
        if into.join(".git/config").as_os_str().encode_wide().count() >= 240 {
            if refresh || !into.join(".git").is_dir() {
                if let Some(parent) = into.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|source| CoreError::Io { path: parent.into(), source })?;
                }
                fetch_windows(source, &into)?;
            }
            return Ok(into);
        }
    }
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

/// Git for Windows can fail to read .git/config before core.longpaths takes
/// effect. Keep Git's work in private temporary storage, then publish the
/// complete bounded cache on its own filesystem. A failed refresh keeps the
/// previous cache; source addresses and directory identities stay unchanged.
#[cfg(windows)]
fn fetch_windows(source: &str, into: &Path) -> Result<()> {
    let existed = match into.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if !into.join(".git").is_dir()
                && std::fs::read_dir(into)
                    .map_err(|source| CoreError::Io { path: into.into(), source })?
                    .next()
                    .is_some()
            {
                return Err(CoreError::Other(format!(
                    "skill cache destination is not a Git cache or empty directory: {}",
                    into.display()
                )));
            }
            true
        }
        Ok(_) => {
            return Err(CoreError::Other(format!(
                "skill cache destination is not a real directory: {}",
                into.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => return Err(CoreError::Io { path: into.into(), source }),
    };
    let temporary = tempfile::Builder::new()
        .prefix("rk-git-")
        .tempdir()
        .map_err(|source| CoreError::Io { path: std::env::temp_dir(), source })?;
    let repository = temporary.path().join("repo");
    git(&["clone", "--depth", "1", "--", source, &repository.display().to_string()], None)?;
    let parent = into.parent().ok_or_else(|| CoreError::Other("skill cache has no parent".into()))?;
    let stage =
        tempfile::tempdir_in(parent).map_err(|source| CoreError::Io { path: parent.into(), source })?;
    let prepared = stage.path().join("prepared");
    copy_tree(&repository, &prepared, &mut Budget::new())?;
    publish_cache(stage, &prepared, into, existed)
}

#[cfg(windows)]
fn publish_cache(stage: tempfile::TempDir, prepared: &Path, into: &Path, existed: bool) -> Result<()> {
    let backup = stage.path().join("previous");
    if existed {
        std::fs::rename(into, &backup).map_err(|source| CoreError::Io { path: into.into(), source })?;
    }
    if let Err(error) = std::fs::rename(prepared, into) {
        if existed && let Err(restore) = std::fs::rename(&backup, into) {
            let kept = stage.keep();
            return Err(CoreError::Other(format!(
                "skill cache replacement failed ({error}); restore failed ({restore}); previous files retained in {}",
                kept.display()
            )));
        }
        return Err(CoreError::Io { path: into.into(), source: error });
    }
    Ok(())
}

/// The readable label is only a hint; the complete configured source owns the
/// cache identity. Lossy punctuation mapping alone aliased `/a/b` with `/a-b`,
/// so a search attributed the first repository's skills to the second URL.
/// Legacy directories remain untouched and are never treated as identity proof.
fn cache_name(source: &str) -> String {
    use sha2::{Digest, Sha256};

    // Keep a short repository label, excluding URL query/fragment and userinfo.
    // The label is not parsed or used to equate repository addresses.
    let address = source.split(['?', '#']).next().unwrap_or(source).trim_end_matches('/');
    let label: String = address
        .rsplit(['/', ':', '@'])
        .next()
        .unwrap_or("source")
        .trim_end_matches(".git")
        .chars()
        // No dots: CON.anything is still a reserved Windows device name,
        // even with a digest appended after its apparent extension.
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .take(24)
        .collect();
    let label = match label.trim_matches('-') {
        "" => "source",
        label => label,
    };
    // Do not assume `.git`, trailing slash, case or URL escaping are aliases:
    // arbitrary configured HTTP servers may map them to different repositories.
    format!("{label}-{:x}", Sha256::digest(source.as_bytes()))
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut command = std::process::Command::new("git");
    rook_contain::quietly(&mut command);
    // Our hashed cache name can push a nested Windows home past MAX_PATH.
    // Scope the Git-for-Windows opt-in to this operation, without rewriting
    // the user's Git configuration or changing source identity.
    #[cfg(windows)]
    command.args(["-c", "core.longpaths=true"]);
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
    let boundary = rook_contain::files::RecoveryFiles::open(from, Path::new("."), false)
        .map_err(|source| CoreError::Io { path: from.into(), source })?;
    let mut pending = vec![PathBuf::from(".")];
    std::fs::create_dir_all(to).map_err(|source| CoreError::Io { path: to.into(), source })?;
    while let Some(dir) = pending.pop() {
        let entries = boundary
            .entries(&dir, Budget::MOST_FILES)
            .map_err(|source| CoreError::Io { path: from.join(&dir), source })?;
        for (name, is_file, is_dir) in entries {
            if !is_file && !is_dir {
                continue;
            }
            let relative = dir.join(name);
            let target = to.join(&relative);
            if is_dir {
                budget.charge(&relative, 0)?;
                std::fs::create_dir(&target).map_err(|source| CoreError::Io { path: target, source })?;
                pending.push(relative);
                continue;
            }
            let room = (Budget::MOST_BYTES - budget.bytes).min(8 * 1024 * 1024) as usize;
            let bytes = boundary
                .read(&relative, room)
                .map_err(|source| CoreError::Io { path: from.join(&relative), source })?;
            budget.charge(&relative, bytes.len() as u64)?;
            let permissions = boundary
                .metadata(&relative)
                .map_err(|source| CoreError::Io { path: from.join(&relative), source })?
                .permissions();
            std::fs::write(&target, bytes)
                .map_err(|source| CoreError::Io { path: target.clone(), source })?;
            std::fs::set_permissions(&target, permissions)
                .map_err(|source| CoreError::Io { path: target, source })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Offered, cache_name, matching, source_identity};

    #[test]
    #[cfg(windows)]
    fn a_failed_cache_publication_restores_the_previous_cache() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("cache");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("sentinel"), b"previous cache").unwrap();
        let stage = tempfile::tempdir_in(root.path()).unwrap();
        let missing = stage.path().join("missing-prepared-directory");
        assert!(super::publish_cache(stage, &missing, &target, true).is_err());
        assert_eq!(std::fs::read(target.join("sentinel")).unwrap(), b"previous cache");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    }

    #[test]
    #[cfg(windows)]
    fn a_failed_first_publication_does_not_leave_a_ready_cache() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("cache");
        let stage = tempfile::tempdir_in(root.path()).unwrap();
        let missing = stage.path().join("missing-prepared-directory");
        assert!(super::publish_cache(stage, &missing, &target, false).is_err());
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

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

    #[test]
    fn distinct_repository_addresses_do_not_share_a_lossy_label_as_identity() {
        for (first, second) in [
            ("https://example.invalid/a/b", "https://example.invalid/a-b"),
            ("https://example.invalid/repo", "https://example.invalid/repo.git"),
            ("https://example.invalid/repo", "https://example.invalid/repo/"),
            ("https://example.invalid/Repo", "https://example.invalid/repo"),
            ("https://example.invalid/a%2Fb", "https://example.invalid/a-b"),
            ("https://example.invalid/repo?ref=first", "https://example.invalid/repo?ref=second"),
        ] {
            assert_ne!(cache_name(first), cache_name(second), "{first} and {second}");
            assert_ne!(
                source_identity(first),
                source_identity(second),
                "origin selection: {first} and {second}"
            );
        }
        assert_eq!(cache_name("https://example.invalid/repo"), cache_name("https://example.invalid/repo"));
    }

    #[test]
    fn a_cache_name_is_one_bounded_readable_directory_even_for_long_or_unusual_sources() {
        for source in [
            "..@".to_string(),
            "https://...".into(),
            "https://../../x".into(),
            format!("https://example.invalid/{}", "длинный-адрес".repeat(500)),
            format!("https://example.invalid/{}", "long-name".repeat(500)),
        ] {
            let name = cache_name(&source);
            assert!(!name.is_empty() && name.len() <= 89, "{name}");
            assert!(name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
            assert_ne!(name, ".");
            assert_ne!(name, "..");
            assert_eq!(std::path::Path::new(&name).components().count(), 1);
        }
        assert!(cache_name("https://example.invalid/readable.git").starts_with("readable-"));
        assert!(
            !cache_name("https://person:password@example.invalid/repo?token=secret").contains("password")
        );
        assert!(!cache_name("https://person:password@example.invalid/repo?token=secret").contains("secret"));
    }

    #[test]
    fn repository_labels_cannot_create_windows_device_names() {
        let directory = tempfile::tempdir().unwrap();
        for label in ["CON.foo", "COM1.anything", "LPT1.txt", "NUL", "PRN"] {
            let name = cache_name(&format!("https://example.invalid/{label}"));
            assert!(!name.contains('.'), "a Windows device name cannot retain an apparent extension");
            std::fs::create_dir(directory.path().join(name)).unwrap();
        }
    }

    #[test]
    fn local_origin_selection_still_recognizes_equivalent_directories() {
        let directory = tempfile::tempdir().unwrap();
        let direct = directory.path().display().to_string();
        let equivalent = directory.path().join(".").display().to_string();
        assert_eq!(source_identity(&direct), source_identity(&equivalent));
    }
}
