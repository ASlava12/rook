//! Explicit, source-attributed repair of a missing registered checkout.
use crate::{CoreError, Result, Rook, worktrees};
use rook_contain::files::RecoveryFiles;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const SESSION_BYTES: usize = 256 * 1024;
const PATH_BYTES: usize = 32768;

fn error(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
fn io_error(e: std::io::Error) -> CoreError {
    error(format!("worktree recovery: {e}"))
}

#[derive(Clone, Serialize)]
struct Limits {
    files: usize,
    bytes: usize,
    file_bytes: usize,
    metadata_bytes: usize,
}

/// Capture this request under the engine's short read lock, then release it
/// before calling the asynchronous methods. It retains no engine guard.
pub struct Request {
    store: Arc<rook_store::Store>,
    parent: u128,
    child: u128,
    root: PathBuf,
    limits: Limits,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub parent: String,
    pub child: String,
    pub path: PathBuf,
    pub repository: PathBuf,
    pub admin: PathBuf,
    pub head: String,
    pub index_sha256: String,
    pub state: String,
    pub files: usize,
    pub bytes: usize,
    pub skipped_sparse: usize,
    pub skipped_submodules: usize,
    pub review_token: Option<String>,
    pub last_restore: Option<Receipt>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub source: String,
    pub state: String,
    pub restored: usize,
    pub preserved: usize,
    pub note: String,
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    oid: String,
    mode: String,
    size: usize,
}
struct Snapshot {
    report: Report,
    entries: Vec<Entry>,
    common: PathBuf,
    tree: worktrees::Worktree,
    index: Vec<u8>,
    symlinks: bool,
}

fn receipt_key(child: u128) -> String {
    format!("worktree-recovery/{child:032x}")
}

pub fn prepare(rook: &Rook, parent: u128, child: u128) -> Result<Request> {
    let a = &rook.config.agent;
    let limits = Limits {
        files: a.worktree_recovery_max_files,
        bytes: a.worktree_recovery_max_bytes,
        file_bytes: a.worktree_recovery_max_file_bytes,
        metadata_bytes: a.worktree_recovery_max_metadata_bytes,
    };
    if !(1..=65536).contains(&limits.files)
        || !(1..=256 * 1024 * 1024).contains(&limits.bytes)
        || !(1..=64 * 1024 * 1024).contains(&limits.file_bytes)
        || !(1024..=16 * 1024 * 1024).contains(&limits.metadata_bytes)
    {
        return Err(error("invalid agent.worktree_recovery limits; repair configuration first"));
    }
    Ok(Request { store: rook.store.clone(), parent, child, root: rook.workspace.clone(), limits })
}

impl Request {
    pub async fn inspect(self) -> Result<Report> {
        tokio::time::timeout(std::time::Duration::from_secs(60), Box::pin(self.snapshot()))
            .await
            .map_err(|_| error("worktree diagnosis exceeded 60 seconds; no checkout was changed"))?
            .and_then(|s| {
                crate::persistence::encode_with_limit(&s.report, SESSION_BYTES)?;
                Ok(s.report)
            })
    }

    pub async fn restore(self, review_token: &str) -> Result<Report> {
        if review_token.len() != 64 || !review_token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error("restore needs the review_token from a fresh worktree diagnosis"));
        }
        tokio::time::timeout(std::time::Duration::from_secs(60), Box::pin(self.restore_inner(review_token)))
            .await
            .map_err(|_| {
                error("worktree restore exceeded 60 seconds; inspect its recovery receipt before retrying")
            })?
    }

    fn ownership(&self) -> Result<worktrees::Worktree> {
        let parent = self
            .store
            .get_session_limited(self.parent, SESSION_BYTES)?
            .ok_or_else(|| error("parent session no longer exists"))?;
        let child = self
            .store
            .get_session_limited(self.child, SESSION_BYTES)?
            .filter(|s| s.parent == Some(self.parent))
            .ok_or_else(|| error("worktree recovery requires a direct child of the selected parent"))?;
        let tree: worktrees::Worktree = self
            .store
            .kv_get_limited(&worktrees::key(self.child), SESSION_BYTES)?
            .map(|b| serde_json::from_slice(&b))
            .transpose()?
            .ok_or_else(|| error("this child has no saved isolated worktree"))?;
        if tree.removed {
            return Err(error("this worktree was explicitly removed; restoration is unavailable"));
        }
        for path in [
            &tree.path,
            &tree.repository,
            &self.root,
            Path::new(&parent.workspace),
            Path::new(&child.workspace),
        ] {
            if path.to_str().is_none_or(|p| p.len() > PATH_BYTES) {
                return Err(error("recovery requires UTF-8 paths no longer than 32768 bytes"));
            }
        }
        let root = self.root.canonicalize().map_err(io_error)?;
        if Path::new(&parent.workspace).canonicalize().map_err(io_error)? != root
            || tree.repository.canonicalize().map_err(io_error)? != root
        {
            return Err(error("saved parent/repository identity differs from this engine's workspace"));
        }
        let child_root = worktrees::lease_path(Path::new(&child.workspace))?;
        if child_root != worktrees::lease_path(&tree.path)? && child_root != root {
            return Err(error("child workspace was moved away from its registered checkout"));
        }
        Ok(tree)
    }

    async fn snapshot(&self) -> Result<Snapshot> {
        let tree = self.ownership()?;
        let root = self.root.canonicalize().map_err(io_error)?;
        let top = text(
            git(&root, None, None, &["rev-parse", "--show-toplevel"], None, self.limits.metadata_bytes)
                .await?,
        )?;
        if Path::new(top.trim()).canonicalize().map_err(io_error)? != root {
            return Err(error("parent workspace must be the registered repository root"));
        }
        let common = text(
            git(
                &root,
                None,
                None,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                None,
                self.limits.metadata_bytes,
            )
            .await?,
        )?;
        let common = PathBuf::from(common.trim()).canonicalize().map_err(io_error)?;
        let relative = PathBuf::from("rook-worktrees").join(rook_store::format_session_id(self.child));
        let expected = common.join(&relative);
        if worktrees::lease_path(&tree.path)? != worktrees::lease_path(&expected)? {
            return Err(error("saved worktree path is outside its owned repository location"));
        }
        // A retained handle rejects linked prefixes even when they resolve back
        // inside the repository. Never create directories during diagnosis.
        let checkout = match RecoveryFiles::open(&common, &relative, false) {
            Ok(dir) => Some(dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error(e)),
        };
        let admins = RecoveryFiles::open(&common, Path::new("worktrees"), false).map_err(io_error)?;
        let mut admin = None;
        let registrations = std::fs::read_dir(common.join("worktrees")).map_err(io_error)?;
        for (count, item) in registrations.enumerate() {
            if count >= self.limits.files {
                return Err(error("Git registration count exceeds agent.worktree_recovery_max_files"));
            }
            let item = item.map_err(io_error)?;
            let name = item.file_name();
            let inside = Path::new(&name).join("gitdir");
            let target = text(admins.read(&inside, self.limits.metadata_bytes).map_err(io_error)?)?;
            if worktrees::lease_path(Path::new(target.trim()))?
                == worktrees::lease_path(&expected.join(".git"))?
            {
                if admin.is_some() {
                    return Err(error("multiple Git registrations claim this checkout"));
                }
                admin = Some(common.join("worktrees").join(name));
            }
        }
        let admin = admin.ok_or_else(|| {
            error("registered Git checkout metadata is missing; automatic restoration is unavailable")
        })?;
        let admin_rel = admin.strip_prefix(&common).map_err(|_| error("invalid Git admin path"))?;
        let files = RecoveryFiles::open(&common, admin_rel, false).map_err(io_error)?;
        let link = text(files.read(Path::new("commondir"), self.limits.metadata_bytes).map_err(io_error)?)?;
        if admin.join(link.trim()).canonicalize().map_err(io_error)? != common {
            return Err(error("registered checkout has a foreign common repository"));
        }
        let head = text(
            git(
                &root,
                Some(&admin),
                None,
                &["rev-parse", "--verify", "HEAD"],
                None,
                self.limits.metadata_bytes,
            )
            .await?,
        )?;
        let head = head.trim().to_owned();
        if !oid(&head) {
            return Err(error("registered checkout HEAD is not an object identity"));
        }
        if files.metadata(Path::new("index")).map_err(io_error)?.len() > self.limits.metadata_bytes as u64 {
            return Err(error("Git index exceeds agent.worktree_recovery_max_metadata_bytes"));
        }
        let index = files.read(Path::new("index"), self.limits.metadata_bytes).map_err(io_error)?;
        admit_index(&index, head.len() / 2, &self.limits)?;
        let index_hash = hex::encode(Sha256::digest(&index));
        let healthy = if let Some(dir) = &checkout {
            if dir.exists(Path::new(".git")).map_err(io_error)? {
                let marker =
                    text(dir.read(Path::new(".git"), self.limits.metadata_bytes).map_err(io_error)?)?;
                let target = marker
                    .trim()
                    .strip_prefix("gitdir: ")
                    .ok_or_else(|| error("existing .git is not the registered checkout marker"))?;
                let target = Path::new(target);
                let target = if target.is_absolute() { target.to_path_buf() } else { expected.join(target) };
                if target.canonicalize().map_err(io_error)? != admin.canonicalize().map_err(io_error)? {
                    return Err(error("existing .git points to another checkout; it will not be replaced"));
                }
                true
            } else {
                false
            }
        } else {
            false
        };
        // Git reads an admitted, private copy of the index. A growing/replaced
        // original cannot make the external process read an unbounded file.
        let mut frozen = tempfile::NamedTempFile::new().map_err(io_error)?;
        frozen.write_all(&index).map_err(io_error)?;
        frozen.flush().map_err(io_error)?;
        let listed = git(
            &root,
            Some(&admin),
            Some(frozen.path()),
            &["ls-files", "--stage", "-v", "-z"],
            None,
            self.limits.metadata_bytes,
        )
        .await?;
        let (mut entries, sparse, submodules) = parse_index(&listed, &self.limits)?;
        if let Some(checkout) = &checkout {
            for entry in &entries {
                checkout.exists(&entry.path).map_err(io_error)?;
            }
        }
        let requests = entries.iter().map(|e| format!("{}\n", e.oid)).collect::<String>();
        let sizes = git(
            &root,
            Some(&admin),
            None,
            &["cat-file", "--batch-check"],
            Some(requests.as_bytes()),
            self.limits.metadata_bytes,
        )
        .await?;
        let sizes = text(sizes)?;
        let mut lines = sizes.lines();
        let mut bytes = 0usize;
        for entry in &mut entries {
            let line = lines.next().ok_or_else(|| error("Git omitted a requested blob size"))?;
            let mut parts = line.split_whitespace();
            let (object, kind, size) = (parts.next(), parts.next(), parts.next());
            if parts.next().is_some() || object != Some(entry.oid.as_str()) || kind != Some("blob") {
                return Err(error("registered index refers to a missing/non-blob object"));
            }
            entry.size = size
                .ok_or_else(|| error("Git omitted a blob size"))?
                .parse()
                .map_err(|_| error("Git returned an invalid blob size"))?;
            bytes = bytes.checked_add(entry.size).ok_or_else(|| error("recovery byte count overflow"))?;
            if entry.size > self.limits.file_bytes
                || bytes > self.limits.bytes
                || (entry.mode == "120000" && entry.size > PATH_BYTES)
            {
                return Err(error(
                    "Git blobs exceed agent.worktree_recovery_max_file_bytes/max_bytes; no files were copied",
                ));
            }
        }
        if lines.next().is_some() {
            return Err(error("Git returned unexpected blob size records"));
        }
        let symlinks = text(
            git(
                &root,
                Some(&admin),
                None,
                &["config", "--type=bool", "--default=true", "--get", "core.symlinks"],
                None,
                self.limits.metadata_bytes,
            )
            .await?,
        )?
        .trim()
            == "true";
        let mut hash = Sha256::new();
        hash.update(self.parent.to_be_bytes());
        hash.update(self.child.to_be_bytes());
        hash.update(index_hash.as_bytes());
        hash.update(head.as_bytes());
        hash.update(crate::persistence::encode_with_limit(
            &(&root, &expected, &admin, &self.limits, symlinks),
            SESSION_BYTES,
        )?);
        let review_token = (!healthy).then(|| hex::encode(hash.finalize()));
        let last_restore = self
            .store
            .kv_get_limited(&receipt_key(self.child), SESSION_BYTES)?
            .map(|b| serde_json::from_slice(&b))
            .transpose()?;
        let report = Report {
            parent: rook_store::format_session_id(self.parent), child: rook_store::format_session_id(self.child),
            path: expected, repository: root, admin, head, index_sha256: index_hash,
            state: if healthy { "present" } else if checkout.is_some() { "recreated_directory" } else { "missing_checkout" }.into(),
            files: entries.len(), bytes, skipped_sparse: sparse, skipped_submodules: submodules,
            review_token, last_restore,
            note: "Source is the saved registered Git index, including staged edits. Restore adds only missing files and the registered .git marker; existing entries are preserved. Raw blob bytes are used without filters or line-ending conversion. Unstaged/untracked files deleted with the checkout cannot be recovered. Sparse files and submodule checkouts are not materialized. This does not complete the child's task or certify files/tests in the parent.".into(),
        };
        Ok(Snapshot { report, entries, common, tree, index, symlinks })
    }

    async fn restore_inner(&self, token: &str) -> Result<Report> {
        let mut snapshot = self.snapshot().await?;
        if snapshot.report.review_token.as_deref() != Some(token) {
            return Err(error(
                "checkout/source/configuration changed or is already present; inspect again before restoration",
            ));
        }
        let _lease = worktrees::Lease::acquire(&snapshot.report.path, true)?;
        if crate::execution::is_active_in(&self.store, self.child) {
            return Err(error("child execution is active; stop it before restoration"));
        }
        let staging = tempfile::tempdir().map_err(io_error)?;
        stage(&snapshot, staging.path()).await?;
        let staged = RecoveryFiles::open(staging.path(), Path::new("."), false).map_err(io_error)?;
        for (i, _) in
            snapshot.entries.iter().enumerate().filter(|(_, e)| e.mode == "120000" && snapshot.symlinks)
        {
            let target = staged.read(Path::new(&i.to_string()), PATH_BYTES).map_err(io_error)?;
            if target.is_empty() || target.contains(&0) {
                return Err(error("registered index has an invalid symlink target; destination unchanged"));
            }
            #[cfg(windows)]
            if Path::new(&text(target)?).has_root() {
                return Err(error(
                    "absolute native symlink restoration is unavailable on Windows; use manual Git repair or review core.symlinks=false before retrying",
                ));
            }
        }
        self.check_source(&snapshot).await?;
        let mut guard = ReceiptGuard {
            store: self.store.clone(), child: self.child,
            receipt: Receipt { source: token.to_owned(), state: "running".into(), restored: 0, preserved: 0,
                note: "Explicit restore admitted; source is the registered index, not current parent files/tests.".into() },
            terminal: false,
        };
        guard.save()?;
        let relative =
            snapshot.report.path.strip_prefix(&snapshot.common).map_err(|_| error("invalid destination"))?;
        let destination = RecoveryFiles::open(&snapshot.common, relative, true).map_err(io_error)?;
        if destination.exists(Path::new(".git")).map_err(io_error)? {
            return Err(error("a .git entry appeared during review; inspect again"));
        }
        for (i, entry) in snapshot.entries.iter().enumerate() {
            if destination.exists(&entry.path).map_err(io_error)? {
                guard.receipt.preserved += 1;
                continue;
            }
            let mut input = std::fs::File::open(staging.path().join(i.to_string())).map_err(io_error)?;
            let created = if entry.mode == "120000" && snapshot.symlinks {
                use std::io::Read;
                let mut bytes = Vec::new();
                input.take(PATH_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(io_error)?;
                if bytes.len() > PATH_BYTES {
                    return Err(error("symlink target exceeds recovery path limit"));
                }
                #[cfg(unix)]
                let target = {
                    use std::os::unix::ffi::OsStringExt;
                    PathBuf::from(std::ffi::OsString::from_vec(bytes))
                };
                #[cfg(not(unix))]
                let target = PathBuf::from(text(bytes)?);
                destination.symlink_new(&entry.path, &target).map_err(io_error)?
            } else {
                destination
                    .copy_new(&entry.path, &mut input, entry.size as u64, entry.mode == "100755")
                    .map_err(io_error)?
            };
            if created {
                guard.receipt.restored += 1;
            } else {
                guard.receipt.preserved += 1;
            }
        }
        self.check_source(&snapshot).await?;
        let admin = rook_contain::files::git_path(&snapshot.report.admin).map_err(io_error)?;
        if admin.contains(['\r', '\n']) {
            return Err(error("Git admin path cannot be encoded in a checkout marker"));
        }
        if !destination
            .write_new(Path::new(".git"), format!("gitdir: {admin}\n").as_bytes())
            .map_err(io_error)?
        {
            return Err(error(
                "a .git entry appeared while restoring; existing files were preserved, inspect before retrying",
            ));
        }
        // Repair a create interrupted before move_session without claiming the
        // delegated task finished. The old Worktree JSON layout is unchanged.
        self.store
            .update_session(self.child, |s| s.workspace = snapshot.report.path.display().to_string())?;
        snapshot.tree.finished = true;
        let tree_bytes = crate::persistence::encode_with_limit(&snapshot.tree, SESSION_BYTES)?;
        guard.receipt.state = "completed".into();
        guard.receipt.note = "Checkout restored from the reviewed registered index. Existing entries preserved; child task completion and current tests remain unknown.".into();
        let receipt_bytes = crate::persistence::encode_with_limit(&guard.receipt, SESSION_BYTES)?;
        self.store.kv_update_session_values(
            self.child,
            &[(&worktrees::key(self.child), &tree_bytes), (&receipt_key(self.child), &receipt_bytes)],
        )?;
        self.store.flush()?;
        guard.terminal = true;
        snapshot.report.state = "restored".into();
        snapshot.report.review_token = None;
        snapshot.report.last_restore = Some(guard.receipt.clone());
        Ok(snapshot.report)
    }

    async fn check_source(&self, snapshot: &Snapshot) -> Result<()> {
        let tree = self.ownership()?;
        if tree.path != snapshot.tree.path
            || tree.repository != snapshot.tree.repository
            || tree.base != snapshot.tree.base
        {
            return Err(error("saved worktree identity changed during restoration"));
        }
        let files = RecoveryFiles::open(
            &snapshot.common,
            snapshot
                .report
                .admin
                .strip_prefix(&snapshot.common)
                .map_err(|_| error("invalid Git admin path"))?,
            false,
        )
        .map_err(io_error)?;
        let target = text(files.read(Path::new("gitdir"), self.limits.metadata_bytes).map_err(io_error)?)?;
        let common = text(files.read(Path::new("commondir"), self.limits.metadata_bytes).map_err(io_error)?)?;
        if worktrees::lease_path(Path::new(target.trim()))?
            != worktrees::lease_path(&snapshot.report.path.join(".git"))?
            || snapshot.report.admin.join(common.trim()).canonicalize().map_err(io_error)? != snapshot.common
        {
            return Err(error("Git registration changed during restoration; inspect the partial receipt"));
        }
        if files.read(Path::new("index"), self.limits.metadata_bytes).map_err(io_error)? != snapshot.index {
            return Err(error("registered index changed during restoration; inspect the partial receipt"));
        }
        let head = text(
            git(
                &snapshot.report.repository,
                Some(&snapshot.report.admin),
                None,
                &["rev-parse", "--verify", "HEAD"],
                None,
                self.limits.metadata_bytes,
            )
            .await?,
        )?;
        if head.trim() != snapshot.report.head {
            return Err(error("registered HEAD changed during restoration; inspect the partial receipt"));
        }
        Ok(())
    }
}

struct ReceiptGuard {
    store: Arc<rook_store::Store>,
    child: u128,
    receipt: Receipt,
    terminal: bool,
}
impl ReceiptGuard {
    fn save(&self) -> Result<()> {
        let bytes = crate::persistence::encode_with_limit(&self.receipt, SESSION_BYTES)?;
        self.store.kv_update_session_values(self.child, &[(&receipt_key(self.child), &bytes)])?;
        self.store.flush()?;
        Ok(())
    }
}
impl Drop for ReceiptGuard {
    fn drop(&mut self) {
        if !self.terminal {
            self.receipt.state = "partial_or_cancelled".into();
            self.receipt.note = "Restoration did not reach a committed completion. Published files were preserved; inspect the checkout and re-diagnose before retrying.".into();
            if let Err(e) = self.save() {
                tracing::warn!("worktree restore receipt not saved: {e}");
            }
        }
    }
}

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

// Validate the envelope before Git sees it. Split/sparse directory indices can
// make Git load another index/tree whose size has not been admitted here.
// https://git-scm.com/docs/gitformat-index (versions 2, 3, 4).
fn admit_index(index: &[u8], hash_bytes: usize, limits: &Limits) -> Result<()> {
    let malformed = || error("malformed registered Git index; repair it manually");
    if index.len() < 12 + hash_bytes || &index[..4] != b"DIRC" {
        return Err(malformed());
    }
    let number = |at: usize| -> Result<usize> {
        let bytes: [u8; 4] =
            index.get(at..at + 4).ok_or_else(malformed)?.try_into().map_err(|_| malformed())?;
        Ok(u32::from_be_bytes(bytes) as usize)
    };
    let version = number(4)?;
    let count = number(8)?;
    if !matches!(version, 2..=4) {
        return Err(error("unsupported Git index version; use manual checkout repair"));
    }
    if count > limits.files {
        return Err(error("index entries exceed agent.worktree_recovery_max_files"));
    }
    let end = index.len() - hash_bytes;
    let mut at = 12usize;
    for _ in 0..count {
        let start = at;
        let flags_at = at.checked_add(40 + hash_bytes).ok_or_else(malformed)?;
        let flags = index.get(flags_at..flags_at + 2).ok_or_else(malformed)?;
        at = flags_at + 2;
        if flags[0] & 0x40 != 0 {
            if version == 2 {
                return Err(malformed());
            }
            at += 2;
        }
        if version == 4 {
            let mut groups = 0;
            loop {
                let byte = *index.get(at).ok_or_else(malformed)?;
                at += 1;
                groups += 1;
                if byte & 0x80 == 0 {
                    break;
                }
                if groups >= 10 {
                    return Err(malformed());
                }
            }
        }
        let tail = index.get(at..end).ok_or_else(malformed)?;
        at += tail.iter().position(|b| *b == 0).ok_or_else(malformed)? + 1;
        if version != 4 {
            at = start + (at - start).div_ceil(8) * 8;
        }
        if at > end {
            return Err(malformed());
        }
    }
    while at < end {
        let signature = index.get(at..at + 4).ok_or_else(malformed)?;
        let size = number(at + 4)?;
        if signature.first().is_none_or(|b| !b.is_ascii_uppercase()) {
            return Err(error(
                "split/sparse-directory or unknown required Git index extension; automatic restoration is unavailable; use manual Git repair",
            ));
        }
        at = at
            .checked_add(8)
            .and_then(|n| n.checked_add(size))
            .filter(|n| *n <= end)
            .ok_or_else(malformed)?;
    }
    Ok(())
}
fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| error("Git recovery metadata is not UTF-8; no paths were guessed"))
}

fn parse_index(bytes: &[u8], limits: &Limits) -> Result<(Vec<Entry>, usize, usize)> {
    let mut entries = Vec::new();
    let mut sparse = 0;
    let mut submodules = 0;
    let mut seen = BTreeSet::new();
    for (count, record) in bytes.split(|b| *b == 0).filter(|b| !b.is_empty()).enumerate() {
        if count >= limits.files {
            return Err(error("index entries exceed agent.worktree_recovery_max_files"));
        }
        let line = std::str::from_utf8(record).map_err(|_| error("index paths are not UTF-8"))?;
        let (fields, name) = line.split_once('\t').ok_or_else(|| error("invalid Git index record"))?;
        let mut parts = fields.split_whitespace();
        let fields = [
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
            parts.next().unwrap_or(""),
        ];
        if parts.next().is_some() || fields[3] != "0" || !oid(fields[2]) {
            return Err(error("registered index has unresolved/conflicting entries; resolve it manually"));
        }
        validate_path(name)?;
        #[cfg(windows)]
        let identity = name.to_lowercase();
        #[cfg(not(windows))]
        let identity = name.to_owned();
        if !seen.insert(identity) {
            return Err(error("index paths collide on this platform"));
        }
        if fields[0].eq_ignore_ascii_case("S") {
            sparse += 1;
            continue;
        }
        if fields[1] == "160000" {
            submodules += 1;
            continue;
        }
        if !matches!(fields[1], "100644" | "100755" | "120000") {
            return Err(error("unsupported index mode"));
        }
        entries.push(Entry {
            path: PathBuf::from(name),
            oid: fields[2].into(),
            mode: fields[1].into(),
            size: 0,
        });
    }
    // A tracked link/file must never become the parent of another output.
    for entry in &entries {
        for parent in entry.path.ancestors().skip(1).filter(|p| !p.as_os_str().is_empty()) {
            let name = parent.to_str().ok_or_else(|| error("invalid index path"))?;
            #[cfg(windows)]
            let name = name.replace('\\', "/").to_lowercase();
            if seen.contains::<str>(name.as_ref()) {
                return Err(error("index file/submodule is an ancestor of another file"));
            }
        }
    }
    Ok((entries, sparse, submodules))
}

fn validate_path(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > PATH_BYTES || name.starts_with('/') || name.contains('\0') {
        return Err(error("invalid/oversized index path"));
    }
    for part in name.split('/') {
        if part.is_empty() || matches!(part, "." | "..") || part.eq_ignore_ascii_case(".git") {
            return Err(error("index path crosses a recovery boundary"));
        }
        #[cfg(windows)]
        {
            let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            if part.contains(['\\', ':'])
                || part.ends_with(['.', ' '])
                || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
                || (device.len() == 4
                    && (device.starts_with("COM") || device.starts_with("LPT"))
                    && device.as_bytes()[3].is_ascii_digit())
            {
                return Err(error("index path is unsafe or ambiguous on Windows"));
            }
        }
    }
    Ok(())
}

fn command(
    root: &Path,
    admin: Option<&Path>,
    index: Option<&Path>,
    args: &[&str],
) -> Result<tokio::process::Command> {
    let mut c = tokio::process::Command::new("git");
    #[cfg(windows)]
    c.creation_flags(rook_contain::NO_WINDOW);
    c.current_dir(root).args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "--no-pager"]);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        c.env_remove(key);
    }
    if let Some(admin) = admin {
        c.env("GIT_DIR", rook_contain::files::git_path(admin).map_err(io_error)?);
    }
    if let Some(index) = index {
        c.env("GIT_INDEX_FILE", rook_contain::files::git_path(index).map_err(io_error)?);
    }
    c.args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    Ok(c)
}

async fn drain(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
    maximum: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut overflow = false;
    let mut chunk = [0; 8192];
    loop {
        let n = pipe.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        let keep = n.min(maximum.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..keep]);
        overflow |= keep < n;
    }
    Ok((bytes, overflow))
}

async fn git(
    root: &Path,
    admin: Option<&Path>,
    index: Option<&Path>,
    args: &[&str],
    input: Option<&[u8]>,
    maximum: usize,
) -> Result<Vec<u8>> {
    let mut child = command(root, admin, index, args)?.spawn().map_err(io_error)?;
    let stdout = child.stdout.take().ok_or_else(|| error("missing Git stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| error("missing Git stderr"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| error("missing Git stdin"))?;
    let write = async {
        if let Some(input) = input {
            stdin.write_all(input).await?;
        }
        drop(stdin);
        Ok::<_, std::io::Error>(())
    };
    let (status, out, err, _) =
        tokio::try_join!(child.wait(), drain(stdout, maximum), drain(stderr, maximum), write)
            .map_err(io_error)?;
    if out.1 || err.1 {
        return Err(error("Git output exceeds agent.worktree_recovery_max_metadata_bytes"));
    }
    if !status.success() {
        return Err(error(format!("Git recovery inspection failed: {}", text(err.0)?.trim())));
    }
    Ok(out.0)
}

async fn stage(snapshot: &Snapshot, directory: &Path) -> Result<()> {
    let mut child =
        command(&snapshot.report.repository, Some(&snapshot.report.admin), None, &["cat-file", "--batch"])?
            .spawn()
            .map_err(io_error)?;
    let mut stdin = child.stdin.take().ok_or_else(|| error("missing blob input"))?;
    let stdout = child.stdout.take().ok_or_else(|| error("missing blob output"))?;
    let stderr = child.stderr.take().ok_or_else(|| error("missing blob errors"))?;
    let write = async {
        for e in &snapshot.entries {
            stdin.write_all(e.oid.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }
        drop(stdin);
        Ok::<_, std::io::Error>(())
    };
    let read = async {
        let mut stream = BufReader::new(stdout);
        for (i, e) in snapshot.entries.iter().enumerate() {
            let mut header = Vec::new();
            (&mut stream).take(256).read_until(b'\n', &mut header).await.map_err(io_error)?;
            if header.last() != Some(&b'\n') {
                return Err(error("oversized or missing Git blob header"));
            }
            let header = text(header)?;
            if header.trim() != format!("{} blob {}", e.oid, e.size) {
                return Err(error("Git blob changed after size admission"));
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(i.to_string()))
                .map_err(io_error)?;
            let mut remaining = e.size;
            let mut chunk = vec![0; 65536];
            while remaining > 0 {
                let take = remaining.min(chunk.len());
                stream.read_exact(&mut chunk[..take]).await.map_err(io_error)?;
                file.write_all(&chunk[..take]).map_err(io_error)?;
                remaining -= take;
            }
            let mut end = [0];
            stream.read_exact(&mut end).await.map_err(io_error)?;
            if end[0] != b'\n' {
                return Err(error("invalid Git blob delimiter"));
            }
        }
        let mut end = [0];
        if stream.read(&mut end).await.map_err(io_error)? != 0 {
            return Err(error("unexpected trailing Git blob data"));
        }
        Ok(())
    };
    let (status, err, _, _) = tokio::try_join!(
        async { child.wait().await.map_err(io_error) },
        async { drain(stderr, SESSION_BYTES).await.map_err(io_error) },
        async { write.await.map_err(io_error) },
        read
    )?;
    if !status.success() || err.1 {
        return Err(error("Git could not stage bounded restore blobs; destination unchanged"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        store: tempfile::TempDir,
        workspace: tempfile::TempDir,
        rook: Rook,
        parent: u128,
        child: u128,
        tree: worktrees::Worktree,
    }
    fn git_test(root: &Path, args: &[&str]) -> String {
        let result = std::process::Command::new("git")
            .current_dir(root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.autocrlf=false",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&result.stderr));
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    }
    async fn fixture() -> Fixture {
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("notes.txt"), b"baseline\n").unwrap();
        std::fs::create_dir(workspace.path().join("nested")).unwrap();
        std::fs::write(workspace.path().join("nested/other.txt"), b"other\n").unwrap();
        git_test(workspace.path(), &["init", "-q"]);
        // Worktree creation uses its own Git invocation. Persist this fixture's
        // byte policy so a Windows runner's global autocrlf cannot alter blobs.
        git_test(workspace.path(), &["config", "core.autocrlf", "false"]);
        git_test(workspace.path(), &["add", "."]);
        git_test(workspace.path(), &["commit", "-qm", "baseline"]);
        let rook = Rook::from_parts(
            rook_store::Store::open(store.path()).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("test", "test", "0.10.0"),
            rook_skills::SkillIndex::default(),
            workspace.path().to_path_buf(),
        );
        let parent = rook.start_session("parent").unwrap();
        let child = rook.fork_for_subtask(parent, "child").unwrap();
        let tree = worktrees::create(&rook, child).await.unwrap();
        Fixture { store, workspace, rook, parent, child, tree }
    }
    fn delete_checkout(f: &Fixture) {
        assert!(
            f.tree
                .path
                .canonicalize()
                .unwrap()
                .starts_with(f.workspace.path().canonicalize().unwrap().join(".git/rook-worktrees"))
        );
        std::fs::remove_dir_all(&f.tree.path).unwrap();
        assert!(!f.tree.path.exists());
    }

    #[tokio::test]
    async fn missing_checkout_restores_the_registered_index_preserves_recreated_files_and_reopens_its_receipt()
     {
        let f = fixture().await;
        std::fs::write(f.tree.path.join("notes.txt"), b"staged edit\n").unwrap();
        std::fs::write(f.tree.path.join("added.bin"), [0, 255, 1, 0]).unwrap();
        git_test(&f.tree.path, &["add", "."]);
        std::fs::write(f.tree.path.join("untracked"), b"lost untracked").unwrap();
        // A checkout must never execute the repository's smudge program.
        git_test(&f.tree.path, &["config", "filter.guard.smudge", "this-program-must-never-be-executed"]);
        std::fs::write(f.tree.path.join(".gitattributes"), b"*.txt filter=guard\n").unwrap();
        git_test(&f.tree.path, &["add", ".gitattributes"]);
        delete_checkout(&f);
        let before = prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap();
        assert_eq!(before.state, "missing_checkout");
        assert_eq!(before.files, 4);
        assert!(!f.tree.path.exists(), "diagnosis may not create a checkout");
        std::fs::create_dir(&f.tree.path).unwrap();
        std::fs::write(f.tree.path.join("notes.txt"), b"manual recreated edit").unwrap();
        std::fs::write(f.tree.path.join("retained-untracked"), b"keep").unwrap();
        let restored = prepare(&f.rook, f.parent, f.child)
            .unwrap()
            .restore(before.review_token.as_deref().unwrap())
            .await
            .unwrap();
        assert_eq!(restored.state, "restored");
        assert!(restored.review_token.is_none());
        let receipt = restored.last_restore.unwrap();
        assert_eq!(receipt.state, "completed");
        assert_eq!((receipt.restored, receipt.preserved), (3, 1));
        assert_eq!(std::fs::read(f.tree.path.join("notes.txt")).unwrap(), b"manual recreated edit");
        assert_eq!(std::fs::read(f.tree.path.join("added.bin")).unwrap(), [0, 255, 1, 0]);
        assert_eq!(std::fs::read(f.tree.path.join("nested/other.txt")).unwrap(), b"other\n");
        assert_eq!(std::fs::read(f.tree.path.join("retained-untracked")).unwrap(), b"keep");
        assert!(!f.tree.path.join("untracked").exists());
        let after = prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap();
        assert_eq!(before.index_sha256, after.index_sha256);
        assert_eq!(before.head, after.head);
        assert_eq!(after.state, "present");
        assert!(after.review_token.is_none());
        let parent_bytes = std::fs::read(f.workspace.path().join("notes.txt")).unwrap();
        assert_eq!(parent_bytes, b"baseline\n");
        std::fs::remove_file(f.tree.path.join("nested/other.txt")).unwrap();
        assert!(
            prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap().review_token.is_none(),
            "ordinary tracked deletion in a healthy checkout is user intent"
        );
        let store_path = f.store.path().to_path_buf();
        let child = f.child;
        drop(f.rook);
        let reopened = rook_store::Store::open(&store_path).unwrap();
        let saved: Receipt = serde_json::from_slice(
            &reopened.kv_get_limited(&receipt_key(child), SESSION_BYTES).unwrap().unwrap(),
        )
        .unwrap();
        assert_eq!(saved.state, "completed");
        assert_eq!(saved.source, receipt.source);
    }

    #[tokio::test]
    async fn stale_sources_reached_admission_limits_and_live_missing_leases_cannot_start_restoration() {
        let mut f = fixture().await;
        let old = prepare(&f.rook, f.parent, f.child).unwrap().snapshot().await.unwrap();
        std::fs::write(f.tree.path.join("notes.txt"), b"changed after review\n").unwrap();
        git_test(&f.tree.path, &["add", "notes.txt"]);
        assert!(prepare(&f.rook, f.parent, f.child).unwrap().check_source(&old).await.is_err());
        let live = worktrees::Lease::acquire(&f.tree.path, false).unwrap();
        delete_checkout(&f);
        let fresh = prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap();
        let token = fresh.review_token.unwrap();
        let why = prepare(&f.rook, f.parent, f.child).unwrap().restore(&token).await.unwrap_err();
        assert!(why.to_string().contains("active turn"), "{why}");
        assert!(!f.tree.path.exists());
        drop(live);
        assert!(fresh.files > 1 && fresh.bytes > 1);
        for limit in ["files", "bytes", "file", "metadata"] {
            f.rook.config = crate::Config::default();
            match limit {
                "files" => f.rook.config.agent.worktree_recovery_max_files = 1,
                "bytes" => f.rook.config.agent.worktree_recovery_max_bytes = 1,
                "file" => f.rook.config.agent.worktree_recovery_max_file_bytes = 1,
                _ => {
                    f.rook.config.agent.worktree_recovery_max_metadata_bytes = 1024;
                    // The original index is now genuinely larger than admission.
                    let admin = fresh.admin.clone();
                    let file = std::fs::OpenOptions::new().write(true).open(admin.join("index")).unwrap();
                    file.set_len(1025).unwrap();
                    assert!(
                        file.metadata().unwrap().len()
                            > f.rook.config.agent.worktree_recovery_max_metadata_bytes as u64
                    );
                }
            }
            let why = prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap_err();
            assert!(why.to_string().contains("exceed"), "{limit}: {why}");
            assert!(!f.tree.path.exists());
        }
    }

    #[tokio::test]
    async fn foreign_checkout_markers_unmerged_indices_and_saved_path_tampering_are_refused() {
        let f = fixture().await;
        std::fs::write(f.tree.path.join(".git"), b"gitdir: foreign\n").unwrap();
        assert!(prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.is_err());
        assert_eq!(std::fs::read(f.tree.path.join(".git")).unwrap(), b"gitdir: foreign\n");
        let mut tree = f.tree;
        tree.path = f.workspace.path().join("outside-owned-location");
        tree.save(&f.rook, f.child).unwrap();
        assert!(prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.is_err());
        let limits = Limits { files: 4, bytes: 4096, file_bytes: 4096, metadata_bytes: 4096 };
        let conflict = format!("M 100644 {} 2\tnote\0", "a".repeat(40));
        assert!(parse_index(conflict.as_bytes(), &limits).unwrap_err().to_string().contains("unresolved"));
    }

    #[tokio::test]
    async fn registered_index_versions_preserve_sparse_gitlink_and_symlink_text_semantics() {
        for version in ["3", "4"] {
            let f = fixture().await;
            let blob = git_test(&f.tree.path, &["rev-parse", "HEAD:notes.txt"]);
            let head = git_test(&f.tree.path, &["rev-parse", "HEAD"]);
            git_test(&f.tree.path, &["config", "core.symlinks", "false"]);
            git_test(
                &f.tree.path,
                &["update-index", "--add", "--cacheinfo", &format!("120000,{blob},link.txt")],
            );
            git_test(
                &f.tree.path,
                &["update-index", "--add", "--cacheinfo", &format!("160000,{head},module")],
            );
            git_test(&f.tree.path, &["update-index", "--skip-worktree", "nested/other.txt"]);
            git_test(&f.tree.path, &["update-index", "--chmod=+x", "notes.txt"]);
            git_test(&f.tree.path, &["update-index", &format!("--index-version={version}")]);
            let initial = prepare(&f.rook, f.parent, f.child).unwrap().snapshot().await.unwrap();
            assert_eq!(u32::from_be_bytes(initial.index[4..8].try_into().unwrap()).to_string(), version);
            delete_checkout(&f);
            let review = prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap();
            assert_eq!((review.files, review.skipped_sparse, review.skipped_submodules), (2, 1, 1));
            prepare(&f.rook, f.parent, f.child)
                .unwrap()
                .restore(review.review_token.as_deref().unwrap())
                .await
                .unwrap();
            assert_eq!(std::fs::read(f.tree.path.join("link.txt")).unwrap(), b"baseline\n");
            assert!(!f.tree.path.join("nested/other.txt").exists());
            assert!(!f.tree.path.join("module").exists());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(f.tree.path.join("notes.txt")).unwrap().permissions().mode() & 0o111,
                    0o111
                );
            }
        }
    }

    #[tokio::test]
    async fn a_real_split_index_is_refused_before_git_can_read_unadmitted_shared_metadata() {
        let f = fixture().await;
        git_test(&f.tree.path, &["update-index", "--split-index"]);
        let marker = std::fs::read_to_string(f.tree.path.join(".git")).unwrap();
        let admin = PathBuf::from(marker.trim().strip_prefix("gitdir: ").unwrap());
        let bytes = std::fs::read(admin.join("index")).unwrap();
        assert!(
            bytes.windows(4).any(|w| w == b"link"),
            "setup genuinely created the required split-index extension"
        );
        delete_checkout(&f);
        let why = prepare(&f.rook, f.parent, f.child).unwrap().inspect().await.unwrap_err();
        assert!(why.to_string().contains("split/sparse-directory"), "{why}");
        assert!(!f.tree.path.exists());
    }
}
