//! Bounded shell preimages and selected, conflict-checked restoration.
use crate::{CoreError, Result, Rook};
use rook_contain::files::{RecoveryFiles, file_mode};
use rook_store::{EventKind, Kind, NewEvent, ObjectId, Store};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

static CAPTURES: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
// A restore cannot overlap another owned shell capture, including a background
// job. This is deliberately process-wide rather than an unbounded root registry.
static SHELL_WINDOWS: std::sync::LazyLock<Arc<tokio::sync::RwLock<()>>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::RwLock::new(())));

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub max_receipts: usize,
    pub max_files: usize,
    pub max_entries: usize,
    pub file_bytes: usize,
    pub total_bytes: usize,
    pub state_bytes: usize,
    pub exclude: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_receipts: 8,
            max_files: 1024,
            max_entries: 4096,
            file_bytes: 8 * 1024 * 1024,
            total_bytes: 32 * 1024 * 1024,
            state_bytes: 4 * 1024 * 1024,
            exclude: vec![".git".into(), "target".into(), "node_modules".into(), ".venv".into()],
        }
    }
}
impl Settings {
    pub(crate) fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, value, low, high) in [
            ("max_receipts", self.max_receipts, 1, 32),
            ("max_files", self.max_files, 1, 8192),
            ("max_entries", self.max_entries, 1, 32768),
            ("file_bytes", self.file_bytes, 1024, 16 * 1024 * 1024),
            ("total_bytes", self.total_bytes, 1024, 256 * 1024 * 1024),
            ("state_bytes", self.state_bytes, 65536, 8 * 1024 * 1024),
        ] {
            if !(low..=high).contains(&value) {
                errors.push(format!("shell_undo.{name}: expected {low}..={high}"));
            }
        }
        if self.max_entries < self.max_files {
            errors.push("shell_undo.max_entries must cover max_files".into());
        }
        if self.exclude.len() > 32
            || self.exclude.iter().any(|name| {
                name.is_empty()
                    || name.len() > 256
                    || name.contains(['/', '\\'])
                    || name.chars().any(char::is_control)
            })
        {
            errors.push("shell_undo.exclude: at most 32 exact entry names, each 1..256 bytes".into());
        }
        errors
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct File {
    pub(crate) object: String,
    pub(crate) bytes: usize,
    pub(crate) mode: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) source: String,
    pub(crate) files: BTreeMap<String, File>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    id: String,
    workspace: String,
    operation: Option<String>,
    before: Snapshot,
    after: Option<Snapshot>,
    state: String,
    job: Option<String>,
    error: Option<String>,
    exclude: Vec<String>,
    restored: Vec<String>,
    restores: usize,
    #[serde(default)]
    attempts: Vec<RestoreAttempt>,
    #[serde(default)]
    recovery_note: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RestoreAttempt {
    source: Snapshot,
    paths: Vec<String>,
    completed: Vec<String>,
    pending: Option<String>,
    state: String,
    error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    version: u32,
    items: Vec<Receipt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    Inspect { receipt: String },
    Recover { receipt: String, note: String },
    Prepare { receipt: String, paths: Vec<String> },
    Restore { receipt: String, paths: Vec<String>, token: String },
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 65536 {
        return Err(bad("shell undo request exceeds 64 KiB"));
    }
    if text.trim().is_empty() {
        return Ok(Request::List);
    }
    Ok(serde_json::from_str(text)?)
}
fn bad(text: impl Into<String>) -> CoreError {
    CoreError::Other(text.into())
}
fn key(owner: u128) -> String {
    format!("shell-undo/{owner:032x}")
}
fn valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && Path::new(path).components().all(|part| matches!(part, std::path::Component::Normal(_)))
}
pub(crate) fn hash(value: &impl Serialize, maximum: usize) -> Result<String> {
    Ok(ObjectId::of(&crate::persistence::encode_with_limit(value, maximum)?).to_hex())
}
fn decode(bytes: Option<&[u8]>, limits: &Settings) -> Result<Saved> {
    let state: Saved = match bytes {
        Some(bytes) if bytes.len() <= limits.state_bytes => serde_json::from_slice(bytes)?,
        Some(_) => return Err(bad("shell undo exceeds shell_undo.state_bytes")),
        None => Saved { version: 1, items: Vec::new() },
    };
    if state.version != 1 || state.items.len() > limits.max_receipts {
        return Err(bad("unsupported shell undo state or receipt limit"));
    }
    for receipt in &state.items {
        if receipt.restored.len() > 2 * limits.max_files
            || receipt.restores > 8
            || receipt.attempts.len() > 8
            || receipt.exclude.len() > 32
            || rook_store::parse_session_id(&receipt.id).is_none()
            || receipt.workspace.len() > 4096
            || receipt.error.as_ref().is_some_and(|s| s.len() > 2048)
            || receipt.recovery_note.as_ref().is_some_and(|s| s.len() > 4096)
            || !matches!(
                receipt.state.as_str(),
                "started" | "background" | "finished" | "uncertain" | "interrupted" | "recovered"
            )
        {
            return Err(bad("shell undo receipt exceeds its bounds"));
        }
        for attempt in &receipt.attempts {
            if attempt.paths.is_empty()
                || attempt.paths.len() > 128
                || attempt.paths.iter().any(|p| !valid(p))
                || attempt.completed.len() > attempt.paths.len()
                || attempt.completed.iter().any(|p| !attempt.paths.contains(p))
                || attempt.pending.as_ref().is_some_and(|p| !attempt.paths.contains(p))
                || !matches!(attempt.state.as_str(), "started" | "finished" | "partial" | "reviewed")
                || attempt.error.as_ref().is_some_and(|s| s.len() > 2048)
            {
                return Err(bad("invalid shell restore attempt"));
            }
        }
        for snapshot in std::iter::once(&receipt.before)
            .chain(receipt.after.iter())
            .chain(receipt.attempts.iter().map(|a| &a.source))
        {
            if snapshot.files.len() > limits.max_files
                || hash(&snapshot.files, limits.state_bytes)? != snapshot.source
            {
                return Err(bad("shell undo source checksum or count mismatch"));
            }
            let mut bytes = 0usize;
            for (path, file) in &snapshot.files {
                bytes = bytes.saturating_add(file.bytes);
                if !valid(path)
                    || ObjectId::from_hex(&file.object).is_none()
                    || file.bytes > limits.file_bytes
                    || file.mode > 0o777
                    || bytes > limits.total_bytes
                {
                    return Err(bad("unsupported shell undo file or byte limit"));
                }
            }
        }
    }
    Ok(state)
}
fn saved(store: &Store, owner: u128, limits: &Settings) -> Result<Saved> {
    if !limits.errors().is_empty() {
        return Err(bad("invalid shell undo settings; run rook config check"));
    }
    if store.get_session_limited(owner, 256 * 1024)?.is_none() {
        return Err(bad("shell undo owner is missing"));
    }
    let bytes = store.kv_get_limited(&key(owner), limits.state_bytes)?;
    decode(bytes.as_deref(), limits)
}
fn update(
    store: &Store,
    owner: u128,
    limits: &Settings,
    change: impl FnOnce(&mut Saved) -> Result<()>,
) -> Result<Saved> {
    let bytes = store.kv_update_session(owner, &key(owner), limits.state_bytes, None, |old| {
        let mut state = decode(old, limits).map_err(|e| rook_store::StoreError::Encoding(e.to_string()))?;
        change(&mut state).map_err(|e| rook_store::StoreError::Encoding(e.to_string()))?;
        let bytes = crate::persistence::encode_with_limit(&state, limits.state_bytes)
            .map_err(|e| rook_store::StoreError::Encoding(e.to_string()))?;
        decode(Some(&bytes), limits).map_err(|e| rook_store::StoreError::Encoding(e.to_string()))?;
        Ok(bytes)
    })?;
    decode(Some(&bytes), limits)
}
fn item<'a>(state: &'a Saved, id: &str) -> Result<&'a Receipt> {
    state
        .items
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(|| bad("shell receipt not found in this session"))
}
fn item_mut<'a>(state: &'a mut Saved, id: &str) -> Result<&'a mut Receipt> {
    state
        .items
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| bad("shell receipt not found in this session"))
}
pub(crate) fn capture(store: &Store, workspace: &Path, limits: &Settings, keep: bool) -> Result<Snapshot> {
    let reader = RecoveryFiles::open(workspace, Path::new("."), false).map_err(|e| bad(e.to_string()))?;
    let mut pending = vec![PathBuf::from(".")];
    let mut entries = 0usize;
    let mut total = 0usize;
    let mut files = BTreeMap::new();
    while let Some(dir) = pending.pop() {
        for (name, regular, directory) in reader
            .entries(&dir, limits.max_entries.saturating_sub(entries))
            .map_err(|e| bad(format!("shell_undo.max_entries: {e}")))?
        {
            entries += 1;
            if limits.exclude.contains(&name) {
                continue;
            }
            if name.contains(['/', '\\']) {
                return Err(bad("shell undo does not normalize separator characters in file names"));
            }
            let path = if dir == Path::new(".") { PathBuf::from(&name) } else { dir.join(&name) };
            let relative =
                path.to_str().ok_or_else(|| bad("shell undo requires UTF-8 paths"))?.replace('\\', "/");
            if !valid(&relative) {
                return Err(bad("shell undo path exceeds its name bound"));
            }
            if directory {
                pending.push(path);
                continue;
            }
            if !regular {
                return Err(bad(format!(
                    "cannot snapshot {relative}: shell undo only supports regular files; configure an explicit excluded entry if appropriate"
                )));
            }
            if files.len() >= limits.max_files {
                return Err(bad("shell_undo.max_files reached before command start"));
            }
            let data = reader
                .read(&path, limits.file_bytes.min(limits.total_bytes.saturating_sub(total)))
                .map_err(|e| bad(format!("shell_undo.file_bytes/total_bytes for {relative}: {e}")))?;
            total = total.saturating_add(data.len());
            let mode = file_mode(&reader.metadata(&path).map_err(|e| bad(e.to_string()))?);
            let object = if keep { store.put(Kind::FileBlob, &data)? } else { ObjectId::of(&data) };
            files.insert(relative, File { object: object.to_hex(), bytes: data.len(), mode });
        }
    }
    Ok(Snapshot { source: hash(&files, limits.state_bytes)?, files })
}
pub(crate) fn pin(
    store: &Store,
    owner: u128,
    workspace: &Path,
    snapshot: &Snapshot,
    label: &str,
) -> Result<()> {
    let set = crate::fileset::FileSet {
        kind: "checkpoint".into(),
        name: label.into(),
        version: String::new(),
        captured_at: rook_store::now_unix(),
        root: workspace.display().to_string(),
        files: snapshot.files.iter().map(|(path, file)| (path.clone(), file.object.clone())).collect(),
        absent: Vec::new(),
        total_bytes: snapshot.files.values().map(|file| file.bytes as u64).sum(),
        note: Some(
            if label.starts_with("experiment-") {
                "Experiment retained source; inspect through its fixed experiment receipt."
            } else {
                "Shell undo preimages; restore only through reviewed shell receipts."
            }
            .into(),
        ),
    };
    let encoded = crate::persistence::encode_with_limit(&set, 8 * 1024 * 1024)?;
    store.append_event(owner, NewEvent::new(EventKind::Note, Kind::Snapshot, &encoded).label(label))?;
    Ok(())
}
fn changes(receipt: &Receipt) -> Vec<String> {
    let Some(after) = &receipt.after else { return Vec::new() };
    let mut paths = std::collections::BTreeSet::new();
    for path in receipt.before.files.keys().chain(after.files.keys()) {
        if receipt.before.files.get(path) != after.files.get(path) {
            paths.insert(path.clone());
        }
    }
    paths.into_iter().collect()
}
fn summary(receipt: &Receipt, details: bool) -> serde_json::Value {
    serde_json::json!({"id":receipt.id,"state":receipt.state,"workspace":receipt.workspace,"operation":receipt.operation,"job":receipt.job,"error":receipt.error,"before_source":receipt.before.source,"after_source":receipt.after.as_ref().map(|snapshot|&snapshot.source),"captured_files":receipt.before.files.len(),"excluded_entry_names":receipt.exclude,"changed_paths":if details {Some(changes(receipt))} else {None},"restored_paths":receipt.restored,"restore_count":receipt.restores,"restore_attempts":if details {Some(&receipt.attempts)} else {None},"recovery_note":receipt.recovery_note})
}

pub(crate) struct Operation {
    store: Arc<Store>,
    owner: u128,
    workspace: PathBuf,
    limits: Settings,
    id: String,
    open: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
    _window: tokio::sync::OwnedRwLockReadGuard<()>,
}
impl Operation {
    fn disarm(&mut self) {
        self.open = false;
    }
    fn complete(&mut self) -> Result<()> {
        let after = capture(&self.store, &self.workspace, &self.limits, false);
        update(&self.store, self.owner, &self.limits, |state| {
            let receipt = item_mut(state, &self.id)?;
            match after {
                Ok(snapshot) => {
                    receipt.after = Some(snapshot);
                    receipt.state = "finished".into();
                }
                Err(error) => {
                    receipt.state = "uncertain".into();
                    receipt.error = Some(rook_tools::elide_middle(&error.to_string(), 2048));
                }
            }
            Ok(())
        })?;
        self.open = false;
        Ok(())
    }
    pub(crate) async fn finish(
        mut self,
        job: Option<(&Arc<rook_tools::jobs::Jobs>, &str)>,
        processes_stopped: bool,
    ) -> Result<String> {
        let id = self.id.clone();
        if let Some((jobs, job)) = job {
            update(&self.store, self.owner, &self.limits, |state| {
                let receipt = item_mut(state, &self.id)?;
                receipt.job = Some(job.into());
                receipt.state = "background".into();
                Ok(())
            })?;
            let jobs = Arc::downgrade(jobs);
            let job = job.to_owned();
            tokio::spawn(async move {
                loop {
                    let Some(registry) = jobs.upgrade() else { return };
                    if registry.is_running(Some(&job)) {
                        // Do not make an observer the job's lifetime owner. A
                        // closed standalone frontend must still drop its Jobs
                        // registry and stop the originally owned processes.
                        drop(registry);
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                    if registry.get(&job).is_none_or(|observed| observed.exit_code.is_none()) {
                        return;
                    }
                    if let Err(error) = registry.quiesce(&job).await {
                        let _ = update(&self.store, self.owner, &self.limits, |state| {
                            let receipt = item_mut(state, &self.id)?;
                            receipt.state = "uncertain".into();
                            receipt.error = Some(rook_tools::elide_middle(&error.to_string(), 2048));
                            Ok(())
                        });
                        self.disarm();
                        return;
                    }
                    break;
                }
                // Snapshot I/O must not stall the runtime's pipe readers.
                let _ = tokio::task::spawn_blocking(move || self.complete()).await;
            });
        } else {
            if !processes_stopped {
                update(&self.store, self.owner, &self.limits, |state| {
                    let receipt = item_mut(state, &self.id)?;
                    receipt.state = "uncertain".into();
                    receipt.error = Some("Owned command cleanup was not certified; inspect execution recovery and remaining processes before recording recovery.".into());
                    Ok(())
                })?;
                self.open = false;
                return Ok(id);
            }
            tokio::task::spawn_blocking(move || self.complete()).await.map_err(|e| bad(e.to_string()))??;
        }
        Ok(id)
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        if self.open {
            let _ = update(&self.store, self.owner, &self.limits, |state| {
                let receipt = item_mut(state, &self.id)?;
                receipt.state = "interrupted".into();
                receipt.error = Some(
                    "No final source snapshot was certified. Inspect execution recovery before restoring."
                        .into(),
                );
                Ok(())
            });
        }
    }
}
impl Rook {
    pub(crate) async fn shell_undo_begin(&self, owner: u128) -> Result<Option<Operation>> {
        if !self.config.shell_undo.enabled {
            return Ok(None);
        }
        let limits = self.config.shell_undo.clone();
        if !limits.errors().is_empty() {
            return Err(bad("invalid shell undo configuration"));
        }
        let workspace = self.workspace.canonicalize().map_err(|e| bad(e.to_string()))?;
        if self.store.root().canonicalize().map_err(|e| bad(e.to_string()))?.starts_with(&workspace) {
            return Err(bad(
                "shell undo cannot snapshot its own store; move ROOK_HOME outside the workspace",
            ));
        }
        let metadata = self
            .store
            .get_session_limited(owner, 256 * 1024)?
            .ok_or_else(|| bad("shell undo owner missing"))?;
        if Path::new(&metadata.workspace).canonicalize().ok().as_ref() != Some(&workspace) {
            return Err(bad("shell undo owner belongs to another workspace"));
        }
        let permit = CAPTURES
            .clone()
            .try_acquire_owned()
            .map_err(|_| bad("shell snapshot capacity is busy; retry after a command completes"))?;
        let window = SHELL_WINDOWS
            .clone()
            .try_read_owned()
            .map_err(|_| bad("a shell restore is in progress; retry after it finishes"))?;
        let store = self.store.clone();
        let operation =
            crate::execution::current(self, owner)?.and_then(|execution| execution.pending.map(|op| op.id));
        tokio::task::spawn_blocking(move || {
            if saved(&store, owner, &limits)?.items.len() >= limits.max_receipts {
                return Err(bad(
                    "shell_undo.max_receipts reached; use another session or raise its bounded limit",
                ));
            }
            let before = capture(&store, &workspace, &limits, true)?;
            pin(&store, owner, &workspace, &before, "shell-undo-source")?;
            let id = rook_store::format_session_id(rook_store::new_session_id());
            update(&store, owner, &limits, |state| {
                if state.items.len() >= limits.max_receipts {
                    return Err(bad("shell_undo.max_receipts reached"));
                }
                state.items.push(Receipt {
                    id: id.clone(),
                    workspace: workspace.display().to_string(),
                    operation,
                    before,
                    after: None,
                    state: "started".into(),
                    job: None,
                    error: None,
                    exclude: limits.exclude.clone(),
                    restored: Vec::new(),
                    restores: 0,
                    attempts: Vec::new(),
                    recovery_note: None,
                });
                Ok(())
            })?;
            Ok(Some(Operation {
                store,
                owner,
                workspace,
                limits,
                id,
                open: true,
                _permit: permit,
                _window: window,
            }))
        })
        .await
        .map_err(|e| bad(e.to_string()))?
    }
    /// Shared source and selected restore contract for shell receipts.
    pub fn shell_undo_request(&self, owner: u128, request: Request) -> Result<serde_json::Value> {
        let _writing = if matches!(&request, Request::List | Request::Inspect { .. }) {
            None
        } else {
            Some(SHELL_WINDOWS.clone().try_write_owned().map_err(|_| bad("a shell command or restore is still active; stop it and wait for cleanup before restoring"))?)
        };
        let limits = &self.config.shell_undo;
        let state = saved(&self.store, owner, limits)?;
        let root = self.workspace.canonicalize().map_err(|e| bad(e.to_string()))?;
        let metadata = self
            .store
            .get_session_limited(owner, 256 * 1024)?
            .ok_or_else(|| bad("shell undo owner missing"))?;
        if Path::new(&metadata.workspace).canonicalize().ok().as_ref() != Some(&root) {
            return Err(bad("shell receipt belongs to another workspace"));
        }
        if state.items.iter().any(|r| r.workspace != root.display().to_string()) {
            return Err(bad("shell source workspace mismatch"));
        }
        if let Request::Recover { receipt, note } = &request {
            if note.trim().is_empty() || note.len() > 4096 {
                return Err(bad(
                    "record an inspection note of 1..4096 bytes, including how remaining processes were stopped",
                ));
            }
            if crate::execution::is_active(self, owner) {
                return Err(bad("stop the active session before recovery"));
            }
            if let Some(reason) = self.recovery_block(owner)? {
                return Err(bad(reason));
            }
            let previous = item(&state, receipt)?;
            if previous.after.is_some()
                && previous.attempts.iter().all(|a| a.state == "finished" || a.state == "reviewed")
            {
                return Err(bad("this shell receipt has no interrupted capture or restore to review"));
            }
            // An explicit inspected recovery records what exists now. It never
            // certifies that the interrupted command finished successfully.
            let observed = if previous.after.is_none() {
                Some(capture(&self.store, &root, limits, false)?)
            } else {
                None
            };
            let recovered = update(&self.store, owner, limits, |state| {
                let target = item_mut(state, receipt)?;
                if let Some(observed) = observed {
                    target.after = Some(observed);
                    target.state = "recovered".into();
                }
                target.recovery_note = Some(note.clone());
                for attempt in &mut target.attempts {
                    if attempt.state != "finished" {
                        attempt.state = "reviewed".into();
                    }
                }
                Ok(())
            })?;
            return Ok(summary(item(&recovered, receipt)?, true));
        }
        let (id, paths, expected) = match request {
            Request::List => {
                return Ok(
                    serde_json::json!({"items":state.items.iter().map(|receipt|summary(receipt,false)).collect::<Vec<_>>() }),
                );
            }
            Request::Inspect { receipt } => return Ok(summary(item(&state, &receipt)?, true)),
            Request::Prepare { receipt, paths } => (receipt, paths, None),
            Request::Restore { receipt, paths, token } => (receipt, paths, Some(token)),
            Request::Recover { .. } => unreachable!("recovery handled above"),
        };
        let receipt = item(&state, &id)?;
        if receipt.workspace != root.display().to_string() {
            return Err(bad("shell source workspace mismatch"));
        }
        if matches!(receipt.state.as_str(), "started" | "background") {
            return Err(bad(
                "shell command has not certified completion; inspect or stop its execution first",
            ));
        }
        if crate::execution::is_active(self, owner) {
            return Err(bad("stop the active session before preparing a shell restore"));
        }
        if let Some(reason) = self.recovery_block(owner)? {
            return Err(bad(reason));
        }
        if receipt.attempts.iter().any(|a| a.state == "started" || a.state == "partial") {
            return Err(bad(
                "an earlier restore is interrupted or partial; inspect its completed and pending paths, then record explicit recovery",
            ));
        }
        if paths.is_empty() || paths.len() > 128 || paths.iter().any(|path| !valid(path)) {
            return Err(bad("select 1..128 exact changed file paths"));
        }
        if receipt.after.is_none() {
            return Err(bad(
                "this interrupted shell has no final snapshot; inspect execution recovery, stop remaining processes and use the recover action with an inspection note",
            ));
        }
        let after = receipt.after.as_ref().ok_or_else(|| bad("missing final shell snapshot"))?;
        let reader = RecoveryFiles::open(&root, Path::new("."), false).map_err(|e| bad(e.to_string()))?;
        let changed = changes(receipt);
        let mut current = BTreeMap::new();
        let mut unique = std::collections::BTreeSet::new();
        let mut total = 0usize;
        for path in &paths {
            if !unique.insert(path) || !changed.contains(path) || receipt.restored.contains(path) {
                return Err(bad("restore paths must be distinct files changed in this shell window"));
            }
            let data = match reader
                .read(Path::new(path), limits.file_bytes.min(limits.total_bytes.saturating_sub(total)))
            {
                Ok(data) => {
                    total = total.saturating_add(data.len());
                    Some(data)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(bad(format!("cannot inspect {path}: {error}"))),
            };
            let file = match &data {
                Some(data) => Some(File {
                    object: ObjectId::of(data).to_hex(),
                    bytes: data.len(),
                    mode: file_mode(&reader.metadata(Path::new(path)).map_err(|e| bad(e.to_string()))?),
                }),
                None => None,
            };
            if file.as_ref() != after.files.get(path) {
                return Err(bad(format!(
                    "restore conflict at {path}: current source differs from the command's final snapshot"
                )));
            }
            current.insert(path.clone(), file);
        }
        let token = hash(
            &(owner, &id, &receipt.before.source, &after.source, receipt.restores, &current),
            limits.state_bytes,
        )?;
        if expected.is_none() {
            return Ok(
                serde_json::json!({"receipt":id,"token":token,"paths":paths,"source":after.source,"restores_before":receipt.before.source}),
            );
        }
        if expected.as_deref() != Some(token.as_str()) {
            return Err(bad("shell restore source changed; prepare and inspect a new token"));
        }
        if receipt.restores >= 8 {
            return Err(bad("shell restore history reached eight operations"));
        }
        // Validate all retained preimages before the first filesystem mutation.
        let mut bodies = BTreeMap::new();
        let mut inverse = BTreeMap::new();
        for path in &paths {
            if let Some(file) = receipt.before.files.get(path) {
                let object =
                    ObjectId::from_hex(&file.object).ok_or_else(|| bad("invalid shell preimage id"))?;
                let bytes = self.store.get_range(&object, 0, file.bytes.saturating_add(1))?;
                if bytes.len() != file.bytes || ObjectId::of(&bytes) != object {
                    return Err(bad(format!("shell preimage missing or corrupt for {path}")));
                }
                bodies.insert(path.clone(), bytes);
            }
            if let Some(file) = current.get(path).and_then(Option::as_ref) {
                let bytes = reader.read(Path::new(path), file.bytes).map_err(|e| bad(e.to_string()))?;
                if ObjectId::of(&bytes).to_hex() != file.object {
                    return Err(bad(format!("restore conflict at {path}")));
                }
                self.store.put(Kind::FileBlob, &bytes)?;
                inverse.insert(path.clone(), file.clone());
            }
        }
        let inverse = Snapshot { source: hash(&inverse, limits.state_bytes)?, files: inverse };
        pin(&self.store, owner, &root, &inverse, "shell-undo-restore-source")?;
        let index = receipt.attempts.len();
        update(&self.store, owner, limits, |state| {
            let receipt = item_mut(state, &id)?;
            receipt.restores += 1;
            receipt.attempts.push(RestoreAttempt {
                source: inverse.clone(),
                paths: paths.clone(),
                completed: Vec::new(),
                pending: None,
                state: "started".into(),
                error: None,
            });
            // Reserve space for every completion and a bounded failure before
            // touching files. Running out of receipt space mid-restore would
            // turn a recoverable file operation into unrecorded uncertainty.
            let mut largest = state.clone();
            let target = item_mut(&mut largest, &id)?;
            for path in &paths {
                if !target.restored.contains(path) {
                    target.restored.push(path.clone());
                }
            }
            target.attempts[index].completed = paths.clone();
            target.attempts[index].pending = paths.iter().max_by_key(|path| path.len()).cloned();
            target.attempts[index].error = Some("x".repeat(2048));
            crate::persistence::encode_with_limit(&largest, limits.state_bytes)?;
            Ok(())
        })?;
        let mut restored = Vec::new();
        let result = (|| -> Result<()> {
            for path in &paths {
                let expected = current.get(path).and_then(Option::as_ref);
                let bytes = expected
                    .map(|file| reader.read(Path::new(path), file.bytes))
                    .transpose()
                    .map_err(|e| bad(e.to_string()))?;
                if bytes.as_ref().map(|bytes| ObjectId::of(bytes).to_hex())
                    != expected.map(|file| file.object.clone())
                {
                    return Err(bad(format!("restore conflict at {path}; already restored: {restored:?}")));
                }
                let mode = receipt.before.files.get(path).map_or(0o644, |file| file.mode);
                update(&self.store, owner, limits, |state| {
                    item_mut(state, &id)?.attempts[index].pending = Some(path.clone());
                    Ok(())
                })?;
                if !reader
                    .replace_checked(
                        Path::new(path),
                        bytes.as_deref(),
                        expected.map(|file| file.mode),
                        bodies.get(path).map(Vec::as_slice),
                        mode,
                    )
                    .map_err(|e| {
                        bad(format!("restore failed at {path}; already restored {restored:?}: {e}"))
                    })?
                {
                    return Err(bad(format!("restore conflict at {path}; already restored {restored:?}")));
                }
                restored.push(path.clone());
                update(&self.store, owner, limits, |state| {
                    let receipt = item_mut(state, &id)?;
                    if !receipt.restored.contains(path) {
                        receipt.restored.push(path.clone());
                    }
                    receipt.attempts[index].completed.push(path.clone());
                    receipt.attempts[index].pending = None;
                    Ok(())
                })?;
            }
            Ok(())
        })();
        update(&self.store, owner, limits, |state| {
            let receipt = item_mut(state, &id)?;
            receipt.attempts[index].state = if result.is_ok() { "finished" } else { "partial" }.into();
            receipt.attempts[index].error =
                result.as_ref().err().map(|e| rook_tools::elide_middle(&e.to_string(), 2048));
            Ok(())
        })?;
        result?;
        Ok(serde_json::json!({"receipt":id,"restored":restored,"inverse_source":inverse.source}))
    }
}
