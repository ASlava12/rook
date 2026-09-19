//! Updating rook itself from the releases it is published as.
//!
//! The same rules as the language-server installer, because it is the same
//! kind of act: a digest the publisher listed is checked against what arrived
//! before anything is written, redirects go only where a release is kept, and
//! nothing on disk changes until the bytes have matched.
//!
//! What is different is the destination. This replaces the binary that is
//! running, so every replacement is a rename rather than a write over the top:
//! the previous one is kept beside the new one, an interrupted step can be
//! undone by hand, and on Windows — where a running `.exe` can be renamed but
//! not overwritten — it is the only spelling that works at all.

use std::path::{Path, PathBuf};

use crate::install::{self, Asset};
use crate::paths;

/// Where rook is published. `ROOK_UPDATE_REPO` moves it, which is how a fork
/// updates from its own releases rather than from this one.
pub fn repo() -> String {
    std::env::var("ROOK_UPDATE_REPO").ok().filter(|r| !r.is_empty()).unwrap_or_else(|| REPO.into())
}

const REPO: &str = "ASlava12/rook";

/// The version this binary was built as.
pub fn running() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// What the latest release is, and whether it is worth doing anything about.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Check {
    pub running: String,
    pub latest: String,
    pub tag: String,
    /// The release page, so a person can read what changed before agreeing.
    pub notes: String,
    /// `true` only when the published version is strictly newer. A build ahead
    /// of the last release — anyone working in the repository — is not behind.
    pub newer: bool,
    pub target: String,
    /// What would be fetched. Absent where the release has nothing for this
    /// platform, which is a thing to say rather than a thing to fail on.
    pub asset: Option<Published>,
}

/// One release asset, as the API describes it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Published {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    #[serde(skip)]
    url: String,
}

/// What an update did, in the words the person needs to undo it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Applied {
    pub from: String,
    pub to: String,
    /// Each destination that was replaced, with where its predecessor went.
    pub replaced: Vec<(PathBuf, PathBuf)>,
    /// Parts of the archive that were deliberately not placed, and why.
    pub left: Vec<String>,
    pub verified: String,
    /// What has to happen before the new version is the one running.
    pub restart: String,
}

/// The three places an update writes to, worked out from the running binary.
///
/// Asked of `current_exe` rather than spelled, and carried as a value rather
/// than read again inside each step: an update that replaced the binary and
/// then asked where the binary is would answer about the new one.
#[derive(Debug, Clone)]
pub struct Layout {
    pub bin: PathBuf,
    /// Where the shipped skills are, when they are somewhere this may replace.
    pub skills: Option<PathBuf>,
    /// Why they are not, when they are not.
    pub skills_left: Option<String>,
}

impl Layout {
    /// From the binary that is running.
    pub fn here() -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("could not work out which binary is running: {e}"))?;
        let bin = exe
            .parent()
            .ok_or_else(|| format!("{} has no directory to install into", exe.display()))?
            .to_path_buf();
        Ok(Self::beside(&bin))
    }

    /// From a directory holding `rook`, which is what a test can point at
    /// without replacing the test binary.
    pub fn beside(bin: &Path) -> Self {
        // An explicit directory is somebody's own arrangement, and an update
        // that overwrote it would be replacing skills it never installed.
        if std::env::var("ROOK_BUILTIN_SKILLS").is_ok_and(|v| !v.is_empty()) {
            return Self {
                bin: bin.to_path_buf(),
                skills: None,
                skills_left: Some(
                    "ROOK_BUILTIN_SKILLS names where the shipped skills come from, so they were left \
                     alone"
                        .into(),
                ),
            };
        }
        // The two shapes `builtin_skills_dir` looks in, in the same order, and
        // the release layout where neither is there yet: a release without its
        // skills is an agent with none, and nothing would say so.
        let flat = bin.join("skills");
        // `<bin>/../share/…` rather than the parent's `share/…` only where
        // there is no parent to ask: the path is printed, and one carrying a
        // `..` reads as a bug in the thing printing it.
        let shared = match bin.parent() {
            Some(prefix) => prefix.join("share/rook/skills"),
            None => bin.join("../share/rook/skills"),
        };
        let skills = match (flat.is_dir(), shared.is_dir()) {
            (true, _) => flat,
            (_, true) => shared,
            _ => shared,
        };
        Self { bin: bin.to_path_buf(), skills: Some(skills), skills_left: None }
    }
}

/// The target triple this build's release asset is named after.
///
/// Derived from what the compiler knows rather than from a build script: the
/// five published targets are the five the release workflow builds, and a
/// platform that has none is told so by name instead of being handed a
/// download for another one. Linux is published as musl, which runs on glibc
/// systems too.
pub fn target() -> Result<String, String> {
    let arch = std::env::consts::ARCH;
    match (std::env::consts::OS, arch) {
        ("macos", "aarch64" | "x86_64") => Ok(format!("{arch}-apple-darwin")),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc".into()),
        ("linux", "aarch64" | "x86_64") => Ok(format!("{arch}-unknown-linux-musl")),
        (os, arch) => Err(format!(
            "no release is published for {os} {arch}, so there is nothing to fetch — \
             `cargo install --path crates/rook-cli` builds it from source instead"
        )),
    }
}

/// Ask the release API what the newest version is.
pub async fn check(proxy: &rook_llm::Proxy) -> Result<Check, String> {
    let api = std::env::var("ROOK_RELEASE_API").unwrap_or_else(|_| "https://api.github.com".into());
    let client = install::release_client(&api, proxy)?;
    let repo = repo();
    let url = format!("{api}/repos/{repo}/releases/latest");
    let response = client
        .get(&url)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {} — no version could be read", response.status()));
    }
    let (body, _) = install::read_bounded(response, install::MOST_API_BYTES).await?;
    let release: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("{url} did not answer with a release: {e}"))?;

    let tag = release["tag_name"].as_str().unwrap_or_default().to_string();
    let latest = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    let running = running().to_string();
    // Compared as versions, not as text: "0.10.0" sorts before "0.9.0" as a
    // string, and that reads as an update being available forever after.
    let newer = match (semver::Version::parse(&latest), semver::Version::parse(&running)) {
        (Ok(there), Ok(here)) => there > here,
        // A tag nobody can parse is not a reason to claim an update: say the
        // versions and let the person decide.
        _ => false,
    };
    let target = target()?;
    let asset = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.contains(&target)))
        .and_then(|found| {
            Some(Published {
                name: found["name"].as_str()?.to_string(),
                size: found["size"].as_u64().unwrap_or(0),
                sha256: install::listed_sha256(found)?,
                url: found["browser_download_url"].as_str()?.to_string(),
            })
        });
    Ok(Check {
        running,
        latest,
        tag,
        notes: release["html_url"].as_str().unwrap_or_default().to_string(),
        newer,
        target,
        asset,
    })
}

/// Fetch what the check found and put it in place.
pub async fn apply(check: &Check, layout: &Layout, proxy: &rook_llm::Proxy) -> Result<Applied, String> {
    let Some(asset) = &check.asset else {
        return Err(format!(
            "release {} has nothing named for {} — nothing was fetched",
            check.tag, check.target
        ));
    };
    let api = std::env::var("ROOK_RELEASE_API").unwrap_or_else(|_| "https://api.github.com".into());
    let client = install::release_client(&api, proxy)?;
    let bytes = install::fetch_asset(
        &client,
        &Asset {
            tag: check.tag.clone(),
            name: asset.name.clone(),
            url: asset.url.clone(),
            sha256: asset.sha256.clone(),
            size: asset.size,
        },
    )
    .await?;

    // Unpacked beside the state rather than beside the binaries: a half-opened
    // archive must never be somewhere that looks like an installation.
    let staging = paths::home().join("update").join(&check.tag);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("could not create {}: {e}", staging.display()))?;
    let unpacked = match asset.name.ends_with(".zip") {
        true => install::unpack_zip(&bytes, &staging, true),
        false => install::unpack_tar_gz(&bytes, &staging, true),
    };
    if let Err(why) = unpacked {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(why);
    }

    let done = place_all(check, layout, &staging);
    // The staging tree is the copy nobody reads; the previous version is the
    // one kept, and it is kept beside what it was replaced with.
    let _ = std::fs::remove_dir_all(&staging);
    let (replaced, left) = done?;

    Ok(Applied {
        from: check.running.clone(),
        to: check.latest.clone(),
        replaced,
        left,
        verified: format!("sha256 {} as release {} lists it", asset.sha256, check.tag),
        // Said rather than implied: on unix the running processes keep the
        // bytes they started with, so an update reports success while the
        // thing reporting it is still the old version.
        restart: "the `rook` and `rookd` already running are still the old ones until they are \
                  started again — `rook daemon restart` for the daemon"
            .into(),
    })
}

/// Put back what the last update replaced.
///
/// The same swap in the other direction, which makes it a toggle rather than a
/// one-way door: what is running now becomes the kept one, so a rollback taken
/// by mistake is undone by running it again. That matters more than it sounds
/// — the reason to roll back is usually that the new version is doing
/// something surprising, and finding out it was not the version is a normal
/// outcome of looking.
///
/// Told in prose it was a rename somebody had to get right three times, on
/// Windows with an extension in the middle of the name. A promise that the
/// previous version is one rename away is worth a command, or it is a promise
/// each person keeps for themselves.
pub fn rollback(layout: &Layout) -> Result<RolledBack, String> {
    let mut back = Vec::new();
    let mut left = Vec::new();
    let mut destinations = vec![layout.bin.join(exe_name("rook"))];
    let daemon = layout.bin.join(exe_name("rookd"));
    if daemon.exists() || with_suffix(&daemon, KEPT).exists() {
        destinations.push(daemon);
    }
    destinations.extend(layout.skills.clone());

    for to in destinations {
        let kept = with_suffix(&to, KEPT);
        if !kept.exists() {
            left.push(format!("{} — nothing kept, so there is nothing to go back to", to.display()));
            continue;
        }
        // `swap_in` copies the source aside before it touches the
        // destination, so naming the kept one as the source is safe even
        // though the same call is about to overwrite it.
        let (at, now_kept) = swap_in(&kept, &to)?;
        back.push((at, now_kept));
    }
    if back.is_empty() {
        return Err(format!(
            "nothing to go back to beside {} — a rollback restores what an update kept, and no \
             update has run here. `rook update` fetches a version instead",
            layout.bin.display()
        ));
    }
    Ok(RolledBack { back, left })
}

/// What a rollback put back, and what it could not.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RolledBack {
    /// Each destination restored, with where what had been running went.
    pub back: Vec<(PathBuf, PathBuf)>,
    pub left: Vec<String>,
}

#[allow(clippy::type_complexity)]
fn place_all(
    check: &Check,
    layout: &Layout,
    staging: &Path,
) -> Result<(Vec<(PathBuf, PathBuf)>, Vec<String>), String> {
    let mut replaced = Vec::new();
    let mut left = Vec::new();

    let rook = exe_name("rook");
    let staged_bin = staging.join("bin");
    let from = staged_bin.join(&rook);
    if !from.is_file() {
        return Err(format!("release {} unpacked without a {rook} in it — nothing was replaced", check.tag));
    }
    replaced.push(swap_in(&from, &layout.bin.join(&rook))?);

    // Only what is already there: a machine with no `rookd` beside `rook` was
    // arranged that way, and an update is not the moment to add a daemon
    // nobody asked for.
    let daemon = exe_name("rookd");
    let to = layout.bin.join(&daemon);
    match (staged_bin.join(&daemon).is_file(), to.exists()) {
        (true, true) => replaced.push(swap_in(&staged_bin.join(&daemon), &to)?),
        (true, false) => left.push(format!(
            "{daemon} is in the release but not beside {rook} here, so it was not added — copy it in \
             by hand to run the daemon"
        )),
        (false, _) => left.push(format!("release {} has no {daemon} in it", check.tag)),
    }

    match (&layout.skills, &layout.skills_left) {
        (Some(skills), _) => {
            let staged = staging.join("share/rook/skills");
            match staged.is_dir() {
                true => replaced.push(swap_in(&staged, skills)?),
                false => left.push(format!(
                    "release {} carries no shipped skills, so {} was left alone",
                    check.tag,
                    skills.display()
                )),
            }
        }
        (None, Some(why)) => left.push(why.clone()),
        (None, None) => {}
    }
    Ok((replaced, left))
}

/// `rook` or `rook.exe`, asked of the platform rather than spelled per branch.
fn exe_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

/// What is kept of what was replaced. Beside the new one, so restoring it is a
/// rename and needs nothing else to still exist.
const KEPT: &str = "previous";

/// Put `from` where `to` is, keeping whatever was there.
///
/// Copy first, then two renames. Copying over the destination would be the
/// obvious spelling and is wrong twice: macOS caches a code signature against
/// the inode, so a binary whose bytes changed underneath is killed with signal
/// 9 and no message anywhere, and Windows refuses to write over an `.exe` that
/// is running at all. Renaming a running binary aside is allowed on both.
fn swap_in(from: &Path, to: &Path) -> Result<(PathBuf, PathBuf), String> {
    let incoming = with_suffix(to, "incoming");
    let kept = with_suffix(to, KEPT);
    let _ = remove(&incoming);
    match from.is_dir() {
        true => install::copy_tree(from, &incoming)?,
        false => {
            std::fs::copy(from, &incoming)
                .map_err(|e| format!("could not copy to {}: {e}", incoming.display()))?;
            install::executable(&incoming)?;
        }
    }

    if to.exists() {
        // A previous kept from an earlier update has done its job: the one
        // being replaced now is the one worth being able to go back to.
        remove(&kept).map_err(|e| {
            format!(
                "could not clear {} to keep the current version there: {e}. Nothing was replaced; \
                 remove it by hand and run this again",
                kept.display()
            )
        })?;
        std::fs::rename(to, &kept).map_err(|e| {
            let _ = remove(&incoming);
            format!(
                "could not move {} aside: {e}. Nothing was replaced — if rook was installed by a \
                 package manager, update it with that instead",
                to.display()
            )
        })?;
    }
    if let Err(e) = std::fs::rename(&incoming, to) {
        // Halfway is the one state nobody can diagnose: put the old one back
        // rather than leave the destination missing.
        let back = std::fs::rename(&kept, to);
        let _ = remove(&incoming);
        return Err(match back {
            Ok(()) => format!("could not put {} in place: {e}. The previous version is back", to.display()),
            Err(also) => format!(
                "could not put {} in place: {e}, and the previous version could not be restored \
                 either: {also}. It is at {}",
                to.display(),
                kept.display()
            ),
        });
    }
    Ok((to.to_path_buf(), kept))
}

/// `rook.exe` keeps its extension: `rook.previous` next to `rook.exe` is a
/// name Windows will not run, and the point of keeping it is being able to.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

fn remove(path: &Path) -> std::io::Result<()> {
    match path.is_dir() {
        true => std::fs::remove_dir_all(path),
        false => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_platform_with_no_release_is_named_rather_than_handed_another_ones() {
        // Whatever this is compiled for, one of the two has to be true.
        match target() {
            Ok(triple) => assert!(
                triple.contains(std::env::consts::ARCH),
                "the asset is named for this machine: {triple}"
            ),
            Err(why) => assert!(
                why.contains(std::env::consts::OS) && why.contains("from source"),
                "it says which platform and what to do instead: {why}"
            ),
        }
    }

    #[test]
    fn what_is_kept_can_still_be_run_on_windows() {
        let kept = with_suffix(Path::new("/opt/rook/bin/rook.exe"), KEPT);
        assert_eq!(kept.file_name().unwrap(), "rook.exe.previous", "the extension survives: {kept:?}");
        let unix = with_suffix(Path::new("/opt/rook/bin/rook"), KEPT);
        assert_eq!(unix.file_name().unwrap(), "rook.previous", "{unix:?}");
    }

    #[test]
    fn replacing_a_binary_keeps_the_one_it_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("rook");
        std::fs::write(&live, b"old").unwrap();
        let new = dir.path().join("staged");
        std::fs::write(&new, b"new").unwrap();

        let (at, kept) = swap_in(&new, &live).unwrap();
        assert_eq!(std::fs::read(&at).unwrap(), b"new", "the new one is in place");
        assert_eq!(std::fs::read(&kept).unwrap(), b"old", "and the old one is beside it");
        assert!(!dir.path().join("rook.incoming").exists(), "nothing half-copied is left behind");
    }

    #[test]
    fn a_second_update_keeps_the_version_it_replaced_not_the_one_before_that() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("rook");
        std::fs::write(&live, b"v1").unwrap();
        for version in ["v2", "v3"] {
            let staged = dir.path().join("staged");
            std::fs::write(&staged, version.as_bytes()).unwrap();
            swap_in(&staged, &live).unwrap();
        }
        assert_eq!(std::fs::read(&live).unwrap(), b"v3");
        assert_eq!(
            std::fs::read(dir.path().join("rook.previous")).unwrap(),
            b"v2",
            "going back once goes back one version, not to the first ever installed"
        );
    }

    #[test]
    fn a_rollback_puts_back_what_the_update_replaced_and_is_itself_undoable() {
        let _alone = paths::alone();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let rook = bin.join(exe_name("rook"));
        std::fs::write(&rook, b"the old one").unwrap();
        let staged = dir.path().join("staged");
        std::fs::write(&staged, b"the new one").unwrap();
        swap_in(&staged, &rook).unwrap();
        assert_eq!(std::fs::read(&rook).unwrap(), b"the new one");

        let layout = Layout::beside(&bin);
        let done = rollback(&layout).unwrap();
        assert_eq!(std::fs::read(&rook).unwrap(), b"the old one", "the version before the update is back");
        assert_eq!(done.back.len(), 1, "and only the binary that was there: {done:?}");

        // The reason to roll back is usually a guess, and finding out it was
        // wrong must not cost the version you were on.
        rollback(&layout).unwrap();
        assert_eq!(std::fs::read(&rook).unwrap(), b"the new one", "rolling back again undoes the rollback");
    }

    #[test]
    fn a_rollback_with_nothing_kept_says_so_rather_than_reporting_a_success() {
        let _alone = paths::alone();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(exe_name("rook")), b"never updated").unwrap();

        let why = rollback(&Layout::beside(&bin)).unwrap_err();
        assert!(why.contains("no update has run here"), "{why}");
        assert!(why.contains("rook update"), "and says what to do instead: {why}");
    }

    #[test]
    fn a_whole_directory_is_swapped_rather_than_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("skills");
        std::fs::create_dir_all(live.join("gone")).unwrap();
        std::fs::write(live.join("gone/SKILL.md"), b"withdrawn upstream").unwrap();
        let staged = dir.path().join("staged");
        std::fs::create_dir_all(staged.join("kept")).unwrap();
        std::fs::write(staged.join("kept/SKILL.md"), b"still published").unwrap();

        swap_in(&staged, &live).unwrap();
        assert!(live.join("kept/SKILL.md").is_file(), "what the release carries is there");
        assert!(
            !live.join("gone/SKILL.md").exists(),
            "and a skill the release stopped carrying is not still sitting there: {:?}",
            std::fs::read_dir(&live).unwrap().flatten().map(|e| e.file_name()).collect::<Vec<_>>()
        );
        assert!(
            dir.path().join("skills.previous/gone/SKILL.md").is_file(),
            "it is kept, because somebody may have been using it"
        );
    }

    #[test]
    fn where_the_shipped_skills_go_is_a_path_somebody_can_read() {
        let _alone = paths::alone();
        let layout = Layout::beside(Path::new("/opt/rook/bin"));
        let skills = layout.skills.unwrap();
        assert_eq!(skills, Path::new("/opt/rook/share/rook/skills"), "{skills:?}");
    }

    #[test]
    fn an_explicit_skills_directory_is_somebody_elses_and_is_left_alone() {
        let _alone = paths::alone();
        // Read at the moment the layout is worked out, so set it here.
        unsafe { std::env::set_var("ROOK_BUILTIN_SKILLS", "/somewhere/of/their/own") };
        let layout = Layout::beside(Path::new("/opt/rook/bin"));
        unsafe { std::env::remove_var("ROOK_BUILTIN_SKILLS") };
        assert!(layout.skills.is_none(), "{layout:?}");
        assert!(
            layout.skills_left.is_some_and(|why| why.contains("ROOK_BUILTIN_SKILLS")),
            "and it says why rather than saying nothing"
        );
    }

    /// The name field written directly, because `tar::Builder` refuses to
    /// produce one of these — which is the point: an archive worth defending
    /// against was not made with our builder.
    fn tar_gz_named(name: &[u8], kind: tar::EntryType, link: Option<&str>) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_size(if kind == tar::EntryType::file() { 3 } else { 0 });
        header.set_mode(0o644);
        header.set_entry_type(kind);
        if let Some(link) = link {
            header.set_link_name(link).unwrap();
        }
        if let Some(gnu) = header.as_gnu_mut() {
            gnu.name[..name.len()].copy_from_slice(name);
        }
        header.set_cksum();
        let mut tar = tar::Builder::new(Vec::new());
        let body: &[u8] = if kind == tar::EntryType::file() { b"bad" } else { b"" };
        tar.append(&header, body).unwrap();
        let bytes = tar.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn a_tar_that_names_a_path_outside_itself_is_refused() {
        let gz = tar_gz_named(b"rook-0.0.0/../../escaped", tar::EntryType::file(), None);
        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("into");
        std::fs::create_dir_all(&into).unwrap();

        let why = install::unpack_tar_gz(&gz, &into, true).unwrap_err();
        assert!(why.contains("outside itself"), "{why}");
        assert!(!dir.path().join("escaped").exists(), "and nothing was written there");
    }

    #[test]
    fn a_tar_that_carries_a_link_is_refused_rather_than_skipped() {
        // A link is how an archive writes through a path it never named: the
        // entry lands inside, and the next entry writes through it.
        let gz = tar_gz_named(b"rook-0.0.0/bin", tar::EntryType::symlink(), Some("/etc"));
        let dir = tempfile::tempdir().unwrap();

        let why = install::unpack_tar_gz(&gz, dir.path(), true).unwrap_err();
        assert!(why.contains("not a file"), "it says what it refused and why: {why}");
        assert!(!dir.path().join("bin").exists(), "and left nothing behind");
    }
}
