//! Where Rook keeps things, on every supported platform.
//!
//! One root directory, overridable with `ROOK_HOME`. Deliberately not the
//! platform-idiomatic split across config/data/cache directories: an agent's
//! state is one thing users back up, sync and inspect together, and scattering
//! it makes "where did my agent's memory go" a support question.

use std::path::{Path, PathBuf};

/// `$ROOK_HOME`, else `~/.rook` (`%USERPROFILE%\.rook` on Windows).
pub fn home() -> PathBuf {
    if let Ok(explicit) = std::env::var("ROOK_HOME")
        && !explicit.is_empty()
    {
        return PathBuf::from(explicit);
    }
    user_home().join(".rook")
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

pub fn config_file() -> PathBuf {
    home().join("config.toml")
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

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in [home(), store_dir(), user_skills_dir(), logs_dir()] {
        private_dir(&d)?;
    }
    Ok(())
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
