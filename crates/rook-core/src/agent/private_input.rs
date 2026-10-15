//! Request receipts contain metadata/status only. Values go directly to Vault.
use super::AgentLoop;
use crate::{CoreError, Result, Vault};
use rook_proto::{SecretPrompt, SecretStatus};
use rook_tools::private_input::SecretSink;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
};

const MAX_BYTES: usize = 16384;
static WRITING: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize)]
struct Entry {
    token: String,
    status: String,
}
#[derive(Serialize, Deserialize)]
struct Ledger {
    scope: String,
    entries: BTreeMap<String, Entry>,
}

fn load(store: &rook_store::Store, key: &str) -> Result<Option<Ledger>> {
    let Some(bytes) = store.kv_get_limited(key, MAX_BYTES)? else { return Ok(None) };
    let ledger: Ledger = serde_json::from_slice(&bytes)?;
    if ledger.scope.len() != 64
        || !ledger.scope.bytes().all(|b| b.is_ascii_hexdigit())
        || ledger.entries.len() > 32
        || ledger.entries.iter().any(|(name, entry)| {
            crate::secrets::usable_name(name).is_err()
                || rook_store::parse_session_id(&entry.token).is_none()
                || entry.status.len() > 32
        })
    {
        return Err(CoreError::Other(
            "private request receipts are invalid; preserve the store for inspection".into(),
        ));
    }
    Ok(Some(ledger))
}
fn save(store: &rook_store::Store, key: &str, ledger: &Ledger) -> Result<()> {
    crate::persistence::save_json(store, key, ledger)?;
    store.flush()?;
    Ok(())
}

struct Sink {
    store: Arc<rook_store::Store>,
    key: String,
    scope: String,
    name: String,
    token: String,
    path: PathBuf,
    // 0 waiting, 1 admitted save, 2 terminal, 3 cancelled before admission.
    state: AtomicU8,
}
impl Sink {
    fn update(&self, status: &str, expected: Option<&str>) -> Result<bool> {
        let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut ledger) = load(&self.store, &self.key)? else { return Ok(false) };
        if ledger.scope != self.scope {
            return Ok(false);
        }
        let Some(entry) = ledger.entries.get_mut(&self.name) else { return Ok(false) };
        if entry.token != self.token || expected.is_some_and(|expected| entry.status != expected) {
            return Ok(false);
        }
        entry.status = status.into();
        save(&self.store, &self.key, &ledger)?;
        Ok(true)
    }
    fn finish(&self, status: SecretStatus) -> Result<SecretStatus> {
        if status == SecretStatus::Saved {
            let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
            let saved = load(&self.store, &self.key)?.is_some_and(|ledger| {
                ledger.scope == self.scope
                    && ledger
                        .entries
                        .get(&self.name)
                        .is_some_and(|e| e.token == self.token && e.status == "saved")
            });
            return Ok(if saved { status } else { SecretStatus::Failed });
        }
        if self.state.load(Ordering::Acquire) == 1 {
            return Ok(SecretStatus::Cancelled);
        }
        self.state.store(2, Ordering::Release);
        let label = serde_json::to_value(status)?.as_str().unwrap_or("failed").to_owned();
        self.update(&label, None)?;
        Ok(status)
    }
}
impl SecretSink for Sink {
    fn save(&self, value: &str) -> SecretStatus {
        if value.is_empty()
            || value.len() > 8192
            || self.state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err()
        {
            return SecretStatus::Cancelled;
        }
        if !matches!(self.update("saving", Some("waiting")), Ok(true)) {
            self.state.store(2, Ordering::Release);
            return SecretStatus::Cancelled;
        }
        let saved = Vault::load_from(self.path.clone())
            .and_then(|mut vault| {
                if vault.contains_name(&self.name) {
                    return Err(CoreError::Other(
                        "secret was configured elsewhere while this form waited".into(),
                    ));
                }
                vault.keep_missing(&self.name, value).and_then(|saved| {
                    if saved {
                        Ok(())
                    } else {
                        Err(CoreError::Other("secret was configured elsewhere".into()))
                    }
                })
            })
            .is_ok();
        self.state.store(2, Ordering::Release);
        if self.update(if saved { "saved" } else { "failed" }, Some("saving")).is_err() {
            return SecretStatus::Failed;
        }
        if saved { SecretStatus::Saved } else { SecretStatus::Failed }
    }
    fn close(&self) {
        if self.state.compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            let _ = self.update("cancelled", Some("waiting"));
        }
    }
}
struct Close(Arc<Sink>);
impl Drop for Close {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl AgentLoop<'_> {
    pub(super) async fn request_private_secret(&self, args: &Value) -> Result<Value> {
        if self.checking || self.review_only {
            return Err(CoreError::Other("a reviewer cannot request credentials".into()));
        }
        let fields = args
            .as_object()
            .filter(|fields| fields.keys().all(|key| matches!(key.as_str(), "name" | "reason")))
            .ok_or_else(|| {
                CoreError::Other(
                    "request_secret accepts only name and reason; values must use private input".into(),
                )
            })?;
        let name =
            crate::secrets::usable_name(fields.get("name").and_then(Value::as_str).unwrap_or_default())?;
        let reason = fields
            .get("reason")
            .and_then(Value::as_str)
            .filter(|reason| {
                !reason.trim().is_empty()
                    && reason.len() <= 1024
                    && !reason.chars().any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
            })
            .ok_or_else(|| CoreError::Other("request_secret needs a reason of 1..1024 bytes".into()))?;
        let result = |status: Value| json!({"name":name,"status":status,"guidance":"Only the named secret and status are returned. Never request its value in chat or repeat a declined, skipped, expired or unanswered request. Continue without it if possible; otherwise report the missing prerequisite."});
        if self.vault.contains_name(&name) || self.vault.import_saved(&name)? {
            return Ok(result(json!("configured")));
        }
        let Some(asker) = &self.asker else { return Ok(result(json!("unavailable"))) };
        if !asker.supports_private_input() {
            return Ok(result(json!("unavailable")));
        }
        let boundary = if let Some(run) = &self.managed_work {
            json!([run.id, run.generation])
        } else {
            json!(self.began_at_seq)
        };
        let scope = rook_store::ObjectId::of(
            json!([self.rook.workspace.to_string_lossy(), boundary]).to_string().as_bytes(),
        )
        .to_hex();
        let key = format!("private-input/{}", rook_store::format_session_id(self.session));
        let token = rook_store::format_session_id(rook_store::new_session_id());
        {
            let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
            let mut ledger = load(&self.rook.store, &key)?
                .filter(|l| l.scope == scope)
                .unwrap_or_else(|| Ledger { scope: scope.clone(), entries: BTreeMap::new() });
            if let Some(previous) = ledger.entries.get(&name) {
                return Ok(result(json!(previous.status)));
            }
            if ledger.entries.len() >= 32 {
                return Ok(result(json!("busy")));
            }
            ledger.entries.insert(name.clone(), Entry { token: token.clone(), status: "waiting".into() });
            save(&self.rook.store, &key, &ledger)?;
        }
        let sink = Arc::new(Sink {
            store: self.rook.store.clone(),
            key,
            scope,
            name: name.clone(),
            token,
            path: self.vault.file_path().into(),
            state: AtomicU8::new(0),
        });
        let _close = Close(sink.clone());
        let status = asker
            .request_secret(
                SecretPrompt {
                    id: String::new(),
                    session: rook_store::format_session_id(self.session),
                    name: name.clone(),
                    reason: self.vault.redact(reason),
                },
                sink.clone(),
            )
            .await;
        let status = sink.finish(status)?;
        if status == SecretStatus::Saved && !self.vault.import_saved(&name)? {
            return Ok(result(json!("failed")));
        }
        Ok(result(serde_json::to_value(status)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(dir: &std::path::Path) -> Sink {
        let store = Arc::new(rook_store::Store::open(dir.join("store")).unwrap());
        let sink = Sink {
            store,
            key: "private-input/test".into(),
            scope: "0".repeat(64),
            name: "deploy".into(),
            token: rook_store::format_session_id(rook_store::new_session_id()),
            path: dir.join("secrets.toml"),
            state: AtomicU8::new(0),
        };
        save(
            &sink.store,
            &sink.key,
            &Ledger {
                scope: sink.scope.clone(),
                entries: BTreeMap::from([(
                    sink.name.clone(),
                    Entry { token: sink.token.clone(), status: "waiting".into() },
                )]),
            },
        )
        .unwrap();
        sink
    }

    #[test]
    fn closed_and_replaced_receipts_never_admit_a_private_write() {
        let dir = tempfile::tempdir().unwrap();
        let sink = waiting(dir.path());
        sink.close();
        assert_eq!(sink.save("never-write-this"), SecretStatus::Cancelled);
        assert!(!sink.path.exists());
        assert_eq!(load(&sink.store, &sink.key).unwrap().unwrap().entries["deploy"].status, "cancelled");
        drop(sink);
        let sink = waiting(dir.path());
        let mut ledger = load(&sink.store, &sink.key).unwrap().unwrap();
        ledger.entries.get_mut("deploy").unwrap().token =
            rook_store::format_session_id(rook_store::new_session_id());
        save(&sink.store, &sink.key, &ledger).unwrap();
        assert_eq!(sink.save("never-write-this"), SecretStatus::Cancelled);
        assert!(!sink.path.exists());
    }

    #[test]
    fn private_save_preserves_credentials_configured_elsewhere_and_durable_receipts_have_no_value() {
        let dir = tempfile::tempdir().unwrap();
        let sink = waiting(dir.path());
        Vault::load_from(sink.path.clone()).unwrap().keep("deploy", "configured-elsewhere").unwrap();
        assert_eq!(sink.save("must-not-replace"), SecretStatus::Failed);
        assert_eq!(
            rook_tools::Secrets::value(&Vault::load_from(sink.path.clone()).unwrap(), "deploy").unwrap(),
            "configured-elsewhere"
        );
        drop(sink);
        std::fs::remove_file(dir.path().join("secrets.toml")).unwrap();
        let sink = waiting(dir.path());
        assert_eq!(sink.save("private-value"), SecretStatus::Saved);
        assert_eq!(sink.save("private-replay"), SecretStatus::Cancelled);
        drop(sink);
        let reopened = rook_store::Store::open(dir.path().join("store")).unwrap();
        let bytes = reopened.kv_get("private-input/test").unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private-value"));
        assert_eq!(load(&reopened, "private-input/test").unwrap().unwrap().entries["deploy"].status, "saved");
    }
}
