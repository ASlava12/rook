//! Optional, retained Git worktrees for competing implementations.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

use crate::{CoreError, Result, Rook};

pub(crate) const TOOL: &str = "worktree";
const GIT_BYTES: usize = 256 * 1024;
static CREATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Shared workspace turns may coexist; removal must exclude even a child later
// resumed as an ordinary top-level session. Entries disappear with their leases.
static USES: std::sync::Mutex<std::collections::BTreeMap<PathBuf, Option<usize>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

pub(crate) struct Lease(PathBuf);
impl Lease {
    pub(crate) fn acquire(path: &Path, removing: bool) -> Result<Self> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut uses = USES.lock().unwrap_or_else(|e| e.into_inner());
        match uses.get_mut(&path) {
            Some(_) if removing => {
                return Err(CoreError::Other(
                    "worktree has an active turn; wait for it to finish before removal".into(),
                ));
            }
            Some(None) => return Err(CoreError::Other("worktree removal is in progress".into())),
            Some(Some(count)) => *count += 1,
            None => {
                uses.insert(path.clone(), (!removing).then_some(1));
            }
        }
        Ok(Self(path))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut uses = USES.lock().unwrap_or_else(|e| e.into_inner());
        match uses.get_mut(&self.0) {
            Some(Some(count)) if *count > 1 => *count -= 1,
            _ => {
                uses.remove(&self.0);
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Worktree {
    pub path: PathBuf,
    pub repository: PathBuf,
    pub base: String,
    pub finished: bool,
    pub removed: bool,
}

fn key(session: u128) -> String {
    format!("worktree/{session:032x}")
}

impl Worktree {
    pub(crate) fn save(&self, rook: &Rook, session: u128) -> Result<()> {
        rook.store.kv_set(&key(session), &serde_json::to_vec(self)?)?;
        Ok(())
    }

    pub(crate) fn report(&self, session: u128) -> String {
        format!(
            "Isolated worktree retained: {} (base {}). Changes are not applied to the parent. \
            Use worktree with session={} and action=status/diff/read to review; copy selected changes \
            with the ordinary editing tools. action=remove cleans up; discard=true explicitly discards edits.",
            self.path.display(),
            self.base,
            rook_store::format_session_id(session)
        )
    }
}

// No shell, no repository hooks or external diff programs. Bound both the wait
// and captured bytes; a large diff is an error with a narrower alternative.
async fn git(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = tokio::process::Command::new("git");
    #[cfg(windows)]
    command.creation_flags(rook_contain::NO_WINDOW);
    command
        .current_dir(root)
        .args(["-c", "core.hooksPath=/dev/null", "--no-pager"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| CoreError::Other(format!("git: {e}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    async fn drain(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> std::io::Result<(Vec<u8>, bool)> {
        let mut kept = Vec::new();
        let mut overflow = false;
        if let Some(mut pipe) = pipe {
            let mut buffer = [0u8; 8192];
            loop {
                let n = pipe.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                let take = n.min(GIT_BYTES.saturating_sub(kept.len()));
                kept.extend_from_slice(&buffer[..take]);
                overflow |= take < n;
            }
        }
        Ok((kept, overflow))
    }
    let done = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        tokio::try_join!(child.wait(), drain(stdout), drain(stderr))
    })
    .await
    .map_err(|_| {
        CoreError::Other(
            "git worktree operation exceeded 60 seconds; inspect git worktree list before retrying".into(),
        )
    })?
    .map_err(|e| CoreError::Other(format!("git: {e}")))?;
    let (status, (out, too_big), (err, _)) = done;
    if !status.success() {
        return Err(CoreError::Other(format!("git: {}", String::from_utf8_lossy(&err))));
    }
    if too_big {
        return Err(CoreError::Other(
            "git output exceeds 256 KiB; use action=read for individual files".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

pub(crate) async fn root(rook: &Rook) -> Result<PathBuf> {
    let root = PathBuf::from(git(&rook.workspace, &["rev-parse", "--show-toplevel"]).await?.trim());
    let workspace = rook.workspace.canonicalize().map_err(|e| CoreError::Other(e.to_string()))?;
    let root = root.canonicalize().map_err(|e| CoreError::Other(e.to_string()))?;
    if root != workspace {
        return Err(CoreError::Other("worktree delegation requires the repository root as workspace".into()));
    }
    Ok(root)
}

pub(crate) async fn allocation_paths(rook: &Rook) -> Result<Vec<String>> {
    let repository = root(rook).await?;
    let common = git(&repository, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await?;
    let common = PathBuf::from(common.trim());
    Ok(vec![
        common.join("rook-worktrees").display().to_string(),
        common.join("worktrees").display().to_string(),
    ])
}

/// Dropping a cancelled child's future releases its tools before making its
/// retained tree removable. A process crash leaves the conservative marker.
pub(crate) struct Finished<'a>(pub &'a Rook, pub u128);
impl Drop for Finished<'_> {
    fn drop(&mut self) {
        let result = (|| -> Result<()> {
            if let Some(bytes) = self.0.store.kv_get(&key(self.1))? {
                let mut tree: Worktree = serde_json::from_slice(&bytes)?;
                tree.finished = true;
                tree.save(self.0, self.1)?;
            }
            Ok(())
        })();
        if let Err(why) = result {
            tracing::warn!("worktree completion was not recorded: {why}");
        }
    }
}

pub(crate) async fn create(rook: &Rook, session: u128) -> Result<Worktree> {
    let _guard = CREATION.lock().await;
    let repository = root(rook).await?;
    if !git(&repository, &["status", "--porcelain", "--untracked-files=normal"]).await?.is_empty() {
        return Err(CoreError::Other("worktree delegation requires a clean workspace (including untracked files); commit or stash changes, or use isolation=shared".into()));
    }
    let common = git(&repository, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await?;
    let directory = PathBuf::from(common.trim()).join("rook-worktrees");
    let retained = match std::fs::read_dir(&directory) {
        Ok(entries) => entries.take(rook.config.agent.max_worktrees).count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(CoreError::Other(e.to_string())),
    };
    if retained >= rook.config.agent.max_worktrees {
        return Err(CoreError::Other(format!(
            "retained worktree limit ({}) reached; remove reviewed trees or raise [agent] max_worktrees",
            rook.config.agent.max_worktrees
        )));
    }
    std::fs::create_dir_all(&directory).map_err(|e| CoreError::Other(e.to_string()))?;
    let path = directory.join(rook_store::format_session_id(session));
    let base = git(&repository, &["rev-parse", "HEAD"]).await?.trim().to_string();
    let tree = Worktree { path, repository, base, finished: false, removed: false };
    // Persist the intended location before Git runs, so an interrupted checkout
    // can still be found and recovered by the owner.
    tree.save(rook, session)?;
    git(
        &tree.repository,
        &[
            "worktree",
            "add",
            "--detach",
            tree.path.to_str().ok_or_else(|| CoreError::Other("worktree path is not UTF-8".into()))?,
            &tree.base,
        ],
    )
    .await?;
    rook.move_session(session, &tree.path)?;
    Ok(tree)
}

pub(crate) fn retained_path(rook: &Rook, session: u128) -> Result<Option<PathBuf>> {
    Ok(rook
        .store
        .kv_get(&key(session))?
        .map(|bytes| serde_json::from_slice::<Worktree>(&bytes))
        .transpose()?
        .map(|tree| tree.path))
}

pub(crate) fn owned(rook: &Rook, parent: u128, args: &Value) -> Result<(u128, Worktree)> {
    let id =
        args.get("session").and_then(Value::as_str).and_then(rook_store::parse_session_id).ok_or_else(
            || CoreError::Other("worktree needs the child session id returned by delegate".into()),
        )?;
    let session = rook
        .store
        .get_session(id)?
        .filter(|s| s.parent == Some(parent))
        .ok_or_else(|| CoreError::Other("this is not a direct child of the current session".into()))?;
    let tree: Worktree = rook
        .store
        .kv_get(&key(session.id))?
        .map(|v| serde_json::from_slice(&v))
        .transpose()?
        .ok_or_else(|| CoreError::Other("this child has no isolated worktree".into()))?;
    if tree.removed {
        return Err(CoreError::Other("this worktree was removed".into()));
    }
    Ok((id, tree))
}

pub(crate) async fn inspect(rook: &Rook, parent: u128, args: &Value, vault: &crate::Vault) -> Result<String> {
    let (id, mut tree) = owned(rook, parent, args)?;
    match args.get("action").and_then(Value::as_str).unwrap_or("status") {
        "status" => {
            Ok(json!({"session":rook_store::format_session_id(id), "path":tree.path, "base":tree.base,
            "finished":tree.finished, "status":vault.redact(&git(&tree.path, &["status", "--short"]).await?)})
            .to_string())
        }
        "diff" => {
            let diff = git(&tree.path, &["diff", "--no-ext-diff", "--no-textconv", &tree.base, "--"]).await?;
            let untracked = git(&tree.path, &["ls-files", "--others", "--exclude-standard"]).await?;
            Ok(json!({"diff":vault.redact(&diff), "untracked":vault.redact(&untracked), "note":"Use action=read with path, offset and limit (lines) for complete files, including untracked files."}).to_string())
        }
        "read" => {
            use rook_tools::Tool;
            let mut ctx = crate::agent::tool_context(&rook.config, &tree.path, &rook.output_dir);
            ctx.allow_outside_workspace = false;
            rook_tools::files::ReadFile
                .call(&ctx, args)
                .await
                .map(|o| o.content)
                .map_err(|e| CoreError::Other(e.to_string()))
        }
        "remove" => {
            if !tree.finished {
                return Err(CoreError::Other("worktree is still running or was interrupted; wait for the child, or recover an interrupted checkout manually with git worktree".into()));
            }
            let _lease = Lease::acquire(&tree.path, true)?;
            let path =
                tree.path.to_str().ok_or_else(|| CoreError::Other("worktree path is not UTF-8".into()))?;
            let mut command = vec!["worktree", "remove"];
            if args.get("discard").and_then(Value::as_bool) == Some(true) {
                command.push("--force");
            }
            command.push(path);
            git(&tree.repository, &command).await?;
            tree.removed = true;
            tree.save(rook, id)?;
            Ok("worktree removed; the child transcript remains in the store".into())
        }
        _ => Err(CoreError::Other("action must be status, diff, read or remove".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::Lease;
    #[test]
    fn removal_excludes_running_and_resumed_turns_including_shared_children() {
        let root = tempfile::tempdir().unwrap();
        let parent = Lease::acquire(root.path(), false).unwrap();
        let child = Lease::acquire(root.path(), false).unwrap();
        assert!(Lease::acquire(root.path(), true).is_err());
        drop(parent);
        assert!(Lease::acquire(root.path(), true).is_err());
        drop(child);
        let removal = Lease::acquire(root.path(), true).unwrap();
        assert!(Lease::acquire(root.path(), false).is_err());
        drop(removal);
        assert!(Lease::acquire(root.path(), false).is_ok());
    }
}
