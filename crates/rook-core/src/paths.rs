//! Where Rook keeps things, on every supported platform.
//!
//! One root directory, overridable with `ROOK_HOME`. Deliberately not the
//! platform-idiomatic split across config/data/cache directories: an agent's
//! state is one thing users back up, sync and inspect together, and scattering
//! it makes "where did my agent's memory go" a support question.
//!
//! Where that one root goes is a platform question, and on Windows the answer
//! was wrong: a dotted directory in the profile root is a unix habit, and
//! `C:\Users\<user>\.rook` is what somebody complained about. It is
//! `%LOCALAPPDATA%\rook` now — Local rather than Roaming, because a roaming
//! profile is copied to a server at every logon and this directory holds
//! sessions, caches and downloaded language servers, which is gigabytes nobody
//! asked to have synchronised.

use std::path::{Path, PathBuf};

/// `$ROOK_HOME`, else the platform's place for a program's own state.
///
/// On Windows that is `%LOCALAPPDATA%\rook`; on unix `~/.rook`, which is where
/// it has always been and where every dotfile of its kind lives.
pub fn home() -> PathBuf {
    if let Ok(explicit) = std::env::var("ROOK_HOME")
        && !explicit.is_empty()
    {
        return PathBuf::from(explicit);
    }
    default_home(&user_home(), local_app_data().as_deref(), |p| p.is_dir())
}

/// The root to use when nothing names one, as a function of what exists.
///
/// Its own function so the choice can be tested from any platform, which is
/// this repository's rule about a path that differs by platform: ask the code
/// that makes it rather than spelling one answer.
///
/// The legacy directory wins when it is there, and that is the whole of the
/// migration. Somebody upgrading has their sessions, their memory and their
/// installed language servers under `%USERPROFILE%\.rook`; moving gigabytes
/// under them — possibly across volumes, possibly while a daemon holds the
/// store lock — to tidy a path is not a trade worth making, and leaving them
/// pointing at an empty new directory would read as the agent having forgotten
/// everything. New installs get the right place; old ones keep working and
/// `rook doctor` says where they are.
fn default_home(user: &Path, local: Option<&Path>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let legacy = user.join(".rook");
    match local {
        Some(local) if !exists(&legacy) => local.join("rook"),
        _ => legacy,
    }
}

/// A line about where the state is, when that is worth saying, and nothing
/// when it is the ordinary place.
///
/// Three cases are worth a line and the fourth is not: told explicitly, kept
/// where an older install put it, or — the one that reads as data loss — an
/// old directory sitting beside the one now in use, which happens when
/// somebody sets `ROOK_HOME`, or copies a profile, and then wonders which of
/// the two the agent is reading.
pub fn where_the_state_is() -> Option<String> {
    let home = home();
    let legacy = user_home().join(".rook");
    if std::env::var("ROOK_HOME").is_ok_and(|v| !v.is_empty()) {
        return Some(format!("{} — named by ROOK_HOME", home.display()));
    }
    if cfg!(windows) && home == legacy {
        return Some(format!(
            "{} — where an earlier version put it, and still in use. A fresh install \
             would use {}; moving it is a copy you make when you want to.",
            home.display(),
            local_app_data().unwrap_or_default().join("rook").display()
        ));
    }
    (home != legacy && legacy.is_dir())
        .then(|| format!("{} — and there is another at {}", home.display(), legacy.display()))
}

/// `%LOCALAPPDATA%`, and nothing on a platform that has no such idea.
fn local_app_data() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    std::env::var("LOCALAPPDATA")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        // Windows without the variable: the documented location under the
        // profile, rather than falling back to the path this moved away from.
        .or_else(|| {
            let user = user_home();
            (user != Path::new(".")).then(|| user.join("AppData").join("Local"))
        })
}

pub fn user_home() -> PathBuf {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key)
            && !v.is_empty()
        {
            return PathBuf::from(v);
        }
    }
    // Windows without USERPROFILE: fall back to the drive-relative pair.
    if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        return PathBuf::from(format!("{drive}{path}"));
    }
    PathBuf::from(".")
}

pub fn store_dir() -> PathBuf {
    home().join("store")
}

/// Where secrets live: beside the config and not in the store, which is
/// content-addressed, searchable and copied by a fork — every property that
/// makes it good is the wrong one here.
pub fn secrets_file() -> PathBuf {
    home().join("secrets.toml")
}

/// Where `config.toml` is read and written.
///
/// `ROOK_CONFIG_DIR` moves it and nothing else, which is the point: a person
/// who keeps their configuration in a dotfiles repository wants that one file
/// there, not the store, not the logs, and not the sessions. `ROOK_HOME` is
/// still how to move the lot.
///
/// `secrets.toml` deliberately does not follow it. It is the one file here that
/// is nothing but credentials, and the directory somebody points this at is by
/// construction a directory they sync, share or commit.
pub fn config_dir() -> PathBuf {
    match std::env::var("ROOK_CONFIG_DIR") {
        Ok(explicit) if !explicit.is_empty() => PathBuf::from(explicit),
        _ => home(),
    }
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn user_skills_dir() -> PathBuf {
    home().join("skills")
}

/// Where the whole of a runaway command's output is kept.
///
/// Under the home rather than in the workspace: it is the agent's record of what
/// a command printed, not a file the project has, and putting it in the
/// workspace would put it in every checkpoint and every `git status`.
pub fn output_dir() -> PathBuf {
    home().join("output")
}

pub fn user_plugins_dir() -> PathBuf {
    home().join("plugins")
}

/// Where a running `rookd` records the address it is listening on, so the CLI
/// can reach it instead of guessing a port from config that may not be the one
/// in use. Absent when no daemon is running — it is removed on shutdown.
pub fn daemon_address_file() -> PathBuf {
    home().join("rookd.addr")
}

pub fn logs_dir() -> PathBuf {
    home().join("logs")
}

/// One file per turn in flight, removed when the turn ends.
///
/// Outside the store on purpose: the store takes one writer, and what is kept
/// here has to outlive that writer's death. A file left behind is a turn whose
/// process did not get to finish it.
pub fn running_dir() -> PathBuf {
    home().join("running")
}

/// Where a skill source is kept between searches, so asking twice does not
/// fetch twice. Nothing here is authoritative — deleting it costs a download.
pub fn sources_cache() -> PathBuf {
    home().join("cache").join("sources")
}

pub fn project_skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".rook").join("skills")
}

pub fn project_plugins_dir(workspace: &Path) -> PathBuf {
    workspace.join(".rook").join("plugins")
}

/// Skills shipped with the binary, if an install laid them down next to it.
///
/// `ROOK_BUILTIN_SKILLS` overrides the search, which is how you point a
/// `cargo run` build at the repository's own `skills/` directory.
pub fn builtin_skills_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("ROOK_BUILTIN_SKILLS")
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("skills"), dir.join("../share/rook/skills")].into_iter().find(|c| c.is_dir())
}

/// Language servers `rook lsp install` fetched: one directory per server, one
/// per version under it, and `current` holding the one in use. Under the state
/// directory rather than anywhere on `PATH`, so installing one changes nothing
/// about the machine that removing this directory does not undo.
pub fn servers_dir() -> PathBuf {
    home().join("servers")
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in state_dirs() {
        private_dir(&d)?;
    }
    Ok(())
}

/// Everything the agent keeps, in one list, so the two questions asked of it —
/// create these, and check these — cannot come to different answers.
pub fn state_dirs() -> [PathBuf; 6] {
    [home(), store_dir(), user_skills_dir(), logs_dir(), servers_dir(), running_dir()]
}

/// State directories any other account on this machine can read, with the mode
/// that lets them.
///
/// [`private_dir`] creates them shut and leaves an existing one alone, which is
/// right — a mode its owner chose is not this program's to change — but silent:
/// a directory made before that code existed, or by a shell, keeps handing every
/// transcript to every account and nothing says so. Reported rather than fixed,
/// with the mode, so the answer is one `chmod` away and is the owner's to give.
#[cfg(unix)]
pub fn readable_by_others() -> Vec<(PathBuf, u32)> {
    use std::os::unix::fs::PermissionsExt;
    state_dirs()
        .into_iter()
        .filter_map(|dir| {
            let mode = std::fs::metadata(&dir).ok()?.permissions().mode() & 0o777;
            (mode & 0o077 != 0).then_some((dir, mode))
        })
        .collect()
}

/// Windows has no mode; a directory under the user's profile inherits an ACL
/// that is already the user's.
#[cfg(not(unix))]
pub fn readable_by_others() -> Vec<(PathBuf, u32)> {
    Vec::new()
}

/// Create a directory readable only by its owner.
///
/// What accumulates under here is every transcript the agent has ever written —
/// the files it read, the commands it ran, what it was told to remember — and,
/// in `config.toml`, whatever header or environment variable an MCP server needs
/// to authenticate. On a machine with more than one account the default mode
/// hands all of that to every other one.
///
/// Applied on creation only: a directory that already exists keeps the mode its
/// owner chose, because changing it under them is not this function's business.
#[cfg(unix)]
pub fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if path.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

/// As above. Windows inherits the parent's ACL, which for a directory under the
/// user's profile is already the user's.
#[cfg(not(unix))]
pub fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// `ROOK_HOME` is process-wide, so the tests that set it take this in turn.
/// Two of them in two modules is already enough to have them read each other's
/// value and fail on a machine that runs them in parallel, which is every one.
#[cfg(test)]
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod where_it_goes {
    use super::*;

    /// The complaint this answers: `C:\Users\<user>\.rook` is a unix habit
    /// wearing a Windows path. Asked of the code rather than spelled, because
    /// the answer differs by platform and this machine is only one of them.
    #[test]
    fn a_fresh_install_on_windows_goes_under_local_app_data() {
        let user = Path::new("C:\\Users\\vart");
        let local = PathBuf::from("C:\\Users\\vart\\AppData\\Local");
        assert_eq!(default_home(user, Some(&local), |_| false), local.join("rook"));
    }

    /// And keeps working where it already is. Somebody upgrading has their
    /// sessions, their memory and their downloaded language servers in the old
    /// place; pointing them at an empty new one reads as the agent having
    /// forgotten everything, and moving gigabytes to tidy a path is not a
    /// trade worth making.
    #[test]
    fn an_install_that_already_has_a_home_keeps_it() {
        let user = Path::new("C:\\Users\\vart");
        let local = PathBuf::from("C:\\Users\\vart\\AppData\\Local");
        let legacy = user.join(".rook");
        assert_eq!(default_home(user, Some(&local), |p| p == legacy), legacy);
    }

    /// Unix has no such directory and no such complaint: `~/.rook` is where
    /// every dotfile of its kind lives, and where this has always been.
    #[test]
    fn unix_is_left_where_it_was() {
        let user = Path::new("/home/vart");
        assert_eq!(default_home(user, None, |_| false), user.join(".rook"));
    }

    /// One file, and only that one. Somebody keeping their configuration in a
    /// dotfiles repository wants it there — not the store, not the logs, and
    /// above all not the credentials.
    #[test]
    fn the_config_directory_moves_the_config_and_leaves_the_secrets() {
        let _alone = alone();
        let elsewhere = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ROOK_HOME", home.path());
            std::env::set_var("ROOK_CONFIG_DIR", elsewhere.path());
        }
        assert_eq!(config_file(), elsewhere.path().join("config.toml"));
        assert_eq!(secrets_file(), home.path().join("secrets.toml"));
        assert_eq!(store_dir(), home.path().join("store"));
        unsafe { std::env::remove_var("ROOK_CONFIG_DIR") };
        assert_eq!(config_file(), home.path().join("config.toml"), "and it is not sticky");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A directory made before `private_dir` existed, or by a shell, keeps its
    /// mode and hands every transcript to every account on the machine. It was
    /// created shut and left alone ever after, which is right and silent — so
    /// this is the half that is not silent.
    #[test]
    fn a_state_directory_others_can_read_is_named_with_the_mode_that_lets_them() {
        let _alone = alone();
        // Under the temporary directory rather than at it: `private_dir` leaves
        // an existing directory alone, and a temporary one arrives with
        // whatever mode the platform gives it.
        let parent = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ROOK_HOME", parent.path().join("state")) };
        ensure_dirs().unwrap();

        assert!(readable_by_others().is_empty(), "made shut: {:?}", readable_by_others());

        std::fs::set_permissions(store_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let loose = readable_by_others();
        assert_eq!(loose.len(), 1, "{loose:?}");
        assert_eq!(loose[0], (store_dir(), 0o755));

        // Group-only counts too: "others" is everyone who is not the owner.
        std::fs::set_permissions(store_dir(), std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(readable_by_others().len(), 1, "a group is other accounts as well");

        std::fs::set_permissions(store_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(readable_by_others().is_empty());
    }
}
