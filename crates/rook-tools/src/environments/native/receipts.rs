//! Local receipts bind a remote checkpoint to its actual Rook session before
//! any apply request is sent. No file contents or secret values are recorded.
use super::*;
use rook_contain::files::RecoveryFiles;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
static CHANGING: Mutex<()> = Mutex::new(());
const MAX_BYTES: usize = 65536;
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub call: String,
    pub path: String,
    pub kind: String,
    pub status: String,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub arguments_sha256: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Ledger {
    version: u32,
    identity: String,
    owner: String,
    entries: BTreeMap<String, Entry>,
}
pub(super) struct Receipts {
    root: PathBuf,
    pub identity: String,
    pub owner: String,
    max: usize,
}
impl Receipts {
    pub fn new(ctx: &ToolContext, settings: &Settings) -> Result<Self> {
        let session = ctx.session.ok_or_else(|| invalid("native changes require an engine-owned session"))?;
        let Some(Profile::Ssh { host, program, remote_workspace }) =
            settings.profiles.get(&settings.selected)
        else {
            return Err(invalid("selected profile is not SSH"));
        };
        let root =
            ctx.execution.as_ref().ok_or_else(|| invalid("missing execution state"))?.state_root.clone();
        Ok(Self {
            root,
            identity: super::super::identity(&(host, program, remote_workspace, &ctx.workspace))?,
            owner: ulid::Ulid::from(session).to_string(),
            max: settings.max_calls,
        })
    }
    fn files(&self) -> Result<RecoveryFiles> {
        std::fs::create_dir_all(&self.root)
            .map_err(|_| invalid("cannot create private native receipt storage"))?;
        RecoveryFiles::open(&self.root, Path::new("native-workspaces"), true)
            .map_err(|_| invalid("cannot open private native receipts"))
    }
    fn path(&self) -> PathBuf {
        format!("{}.json", self.owner).into()
    }
    fn load(&self, files: &RecoveryFiles) -> Result<(Option<Vec<u8>>, Ledger)> {
        let bytes = match files.read(&self.path(), MAX_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(invalid("native receipt storage is unavailable or oversized")),
        };
        let ledger = if let Some(bytes) = &bytes {
            serde_json::from_slice(bytes)
                .map_err(|_| invalid("invalid native receipts; preserve state for inspection"))?
        } else {
            Ledger {
                version: 1,
                identity: self.identity.clone(),
                owner: self.owner.clone(),
                entries: BTreeMap::new(),
            }
        };
        if ledger.version != 1
            || ledger.identity != self.identity
            || ledger.owner != self.owner
            || ledger.entries.len() > 32
            || ledger.entries.iter().any(|(id, e)| {
                *id != e.call
                    || ulid::Ulid::from_string(id).is_err()
                    || e.path.len() > 1024
                    || !["write", "edit", "run"].contains(&e.kind.as_str())
                    || !["preparing", "prepared", "applied", "undone", "failed", "unknown"]
                        .contains(&e.status.as_str())
                    || [&e.before_sha256, &e.after_sha256, &e.arguments_sha256].iter().any(|h| {
                        h.as_ref().is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
                    })
            })
        {
            return Err(invalid(
                "native session/workspace binding or receipt changed; do not reuse it for another SSH root",
            ));
        }
        Ok((bytes, ledger))
    }
    fn change<T>(&self, update: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
        let _lock = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
        let files = self.files()?;
        let (old, mut ledger) = self.load(&files)?;
        let result = update(&mut ledger)?;
        let bytes = serde_json::to_vec(&ledger).map_err(|_| invalid("cannot encode native receipt"))?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("native receipt byte budget reached before mutation"));
        }
        if !files
            .replace_checked(&self.path(), old.as_deref(), old.as_ref().map(|_| 0o600), Some(&bytes), 0o600)
            .map_err(|_| invalid("cannot persist native receipt"))?
        {
            return Err(invalid("native receipt changed concurrently; no new mutation was admitted"));
        }
        Ok(result)
    }
    pub fn begin(&self, path: &str, kind: &str) -> Result<String> {
        let call = ulid::Ulid::generate().to_string();
        self.change(|ledger| {
            if ledger.entries.values().any(|e|["preparing","prepared","unknown"].contains(&e.status.as_str())) {return Err(invalid("a native outcome is unresolved; inspect and undo its exact receipt before a new mutation"));}
            if ledger.entries.len() >= self.max {
                return Err(invalid(
                    "native max_calls reached; retain and review checkpoints before another mutation",
                ));
            }
            ledger.entries.insert(
                call.clone(),
                Entry {
                    call: call.clone(),
                    path: path.into(),
                    kind: kind.into(),
                    status: "preparing".into(),
                    before_sha256: None,
                    after_sha256: None,
                    arguments_sha256: None,
                },
            );
            Ok(())
        })?;
        Ok(call)
    }
    pub fn get(&self, call: &str) -> Result<Entry> {
        let files = self.files()?;
        let (_, ledger) = self.load(&files)?;
        ledger
            .entries
            .get(call)
            .cloned()
            .ok_or_else(|| invalid("receipt does not belong to this session and SSH workspace"))
    }
    pub fn list(&self) -> Result<Value> {
        let files = self.files()?;
        let (_, ledger) = self.load(&files)?;
        serde_json::to_value(ledger.entries.values().collect::<Vec<_>>())
            .map_err(|_| invalid("cannot encode receipt metadata"))
    }
    pub fn observe(&self, call: &str, value: &Value) -> Result<Entry> {
        self.change(|ledger| {
            let entry = ledger.entries.get_mut(call).ok_or_else(|| invalid("native receipt disappeared"))?;
            if value["call"] != call
                || value["owner"] != self.owner
                || value["identity"] != self.identity
                || value["path"] != entry.path
                || value["kind"] != entry.kind
            {
                return Err(invalid("native callback does not match its owner/receipt"));
            }
            let hash = |name: &str| -> Result<Option<String>> {
                match value.get(name) {
                    Some(Value::Null) => Ok(None),
                    Some(Value::String(h)) if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) => {
                        Ok(Some(h.clone()))
                    }
                    _ => Err(invalid("invalid native checkpoint digest")),
                }
            };
            let before = hash("before_sha256")?;
            let after = hash("after_sha256")?;
            let arguments = hash("arguments_sha256")?;
            if (after.is_none() && entry.kind != "run")
                || arguments.is_none()
                || (entry.arguments_sha256.is_some()
                    && (entry.before_sha256 != before
                        || (entry.after_sha256.is_some() && entry.after_sha256 != after)
                        || entry.arguments_sha256 != arguments))
            {
                return Err(invalid("native checkpoint digests changed after admission"));
            }
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("native checkpoint has no phase"))?;
            entry.before_sha256 = before;
            entry.after_sha256 = after;
            entry.arguments_sha256 = arguments;
            if entry.status == "undone" && status != "undone" {
                return Err(invalid("late native observation cannot replace a terminal undo"));
            }
            if entry.status == "applied" && status == "prepared" {
                return Err(invalid("stale native preparation cannot replace an applied receipt"));
            }
            let was_unknown = entry.status == "unknown";
            entry.status = match status {
                "prepared" => {
                    if was_unknown {
                        "unknown"
                    } else {
                        "prepared"
                    }
                }
                "applied" => "applied",
                "undone" => "undone",
                "applying" | "undoing" | "running" => "unknown",
                _ => return Err(invalid("invalid native checkpoint phase")),
            }
            .into();
            Ok(entry.clone())
        })
    }
    pub fn uncertain(&self, call: &str) {
        let _ = self.change(|ledger| {
            if let Some(entry) = ledger.entries.get_mut(call)
                && entry.status != "undone"
            {
                entry.status = "unknown".into();
            }
            Ok(())
        });
    }
    pub fn failed(&self, call: &str) -> Result<()> {
        self.change(|ledger| {
            let entry = ledger.entries.get_mut(call).ok_or_else(|| invalid("missing native receipt"))?;
            entry.status = "failed".into();
            Ok(())
        })
    }
    pub fn control(&self, call: &str) -> Value {
        json!({"owner":self.owner,"call":call,"identity":self.identity})
    }
}
pub(super) struct Pending {
    pub receipts: Arc<Receipts>,
    pub call: String,
    pub finished: bool,
    pub admitted: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.finished {
            if self.admitted {
                self.receipts.uncertain(&self.call);
            } else {
                let _ = self.receipts.failed(&self.call);
            }
        }
    }
}

pub(super) fn has_unrestored(root: &Path, owner: &str) -> Result<bool> {
    let files = match RecoveryFiles::open(root, Path::new("native-workspaces"), false) {
        Ok(files) => files,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(invalid("native receipt storage unavailable; cannot certify a file rewind")),
    };
    let bytes = match files.read(Path::new(&format!("{owner}.json")), MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(invalid("native receipt storage unavailable or oversized")),
    };
    let ledger: Ledger = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("native receipts invalid; cannot certify a file rewind"))?;
    if ledger.version != 1
        || ledger.owner != owner
        || ledger.identity.len() != 64
        || ledger.entries.len() > 32
        || ledger.entries.values().any(|e| {
            !["preparing", "prepared", "applied", "undone", "failed", "unknown"].contains(&e.status.as_str())
        })
    {
        return Err(invalid("native receipt binding changed; cannot certify a file rewind"));
    }
    Ok(ledger.entries.values().any(|e| ["prepared", "applied", "unknown"].contains(&e.status.as_str())))
}
