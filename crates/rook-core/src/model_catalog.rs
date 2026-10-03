//! Endpoint metadata cache. Offline reads never resolve credentials or build a
//! provider. Cached facts are observations with an age, never live reachability.
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rook_llm::{CatalogLimits, LlmError, ModelInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Config, Vault};

const FILE: &str = "models-v1.json";
const LOCK: &str = "models-v1.lock";

mod runtime;
pub(crate) use runtime::Snapshot;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(flatten)]
    pub limits: CatalogLimits,
    pub cache_enabled: bool,
    pub cache_ttl_secs: u64,
    pub cache_max_entries: usize,
    pub learned_window_entries: usize,
    pub cache_max_bytes: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            limits: CatalogLimits::default(),
            cache_enabled: true,
            cache_ttl_secs: 300,
            cache_max_entries: 16,
            learned_window_entries: 16,
            cache_max_bytes: 4 * 1024 * 1024,
        }
    }
}
impl Settings {
    fn bounded(self) -> Self {
        Self {
            limits: self.limits.bounded(),
            cache_enabled: self.cache_enabled,
            cache_ttl_secs: self.cache_ttl_secs.min(86400),
            cache_max_entries: self.cache_max_entries.clamp(1, 128),
            learned_window_entries: self.learned_window_entries.clamp(1, 128),
            cache_max_bytes: self.cache_max_bytes.clamp(4096, 16 * 1024 * 1024),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    PreferCache,
    Refresh,
    Offline,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Endpoint,
    Cache,
    StaleCache,
    Configuration,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub models: Vec<ModelInfo>,
    pub origin: Origin,
    pub observed_at: Option<u64>,
    pub age_secs: Option<u64>,
    pub credentials_resolved: bool,
    pub notices: Vec<String>,
}
impl Listing {
    pub fn description(&self) -> String {
        match self.origin {
            Origin::Endpoint => "metadata: endpoint response".into(),
            Origin::Cache => format!("metadata: cache, observed {}s ago", self.age_secs.unwrap_or(0)),
            Origin::StaleCache => {
                format!("metadata: stale cache, observed {}s ago", self.age_secs.unwrap_or(0))
            }
            Origin::Configuration => "metadata: configuration only; endpoint capabilities are unknown".into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    scope: String,
    credential: String,
    observed_at: u64,
    /// Old entries predate pagination and may contain only the first page.
    #[serde(default)]
    complete: bool,
    /// JSON held as text so the outer record count and the chosen model count
    /// can both be enforced during decoding, without materializing other lists.
    models: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Reads one selected endpoint. A failover's catalog must never be cached under
/// the preferred endpoint's identity. --recheck uses the separate live probe.
pub async fn discover(
    config: &Config,
    vault: &Vault,
    name: &str,
    mode: Mode,
    directory: &Path,
) -> rook_llm::Result<Listing> {
    let settings = config.model_catalog.bounded();
    let scope = crate::models::catalog_scope(config, vault, name)?;
    let mut cached = if settings.cache_enabled {
        read(directory, settings)
            .ok()
            .and_then(|entries| entries.into_iter().find(|entry| entry.scope == scope && entry.complete))
    } else {
        None
    };
    let current = now();
    // Future timestamps cannot turn a clock rollback into an immortal cache.
    if cached.as_ref().is_some_and(|entry| entry.observed_at > current) {
        cached = None;
    }
    if mode == Mode::Offline {
        if let Some(entry) = cached
            && let Ok(listing) = from_cache(entry, current, settings, false)
        {
            return Ok(listing);
        }
        return Ok(Listing {
            models: vec![ModelInfo {
                id: crate::models::model_named(config, name),
                owned_by: None,
                context_window: config
                    .models
                    .get(name.trim())
                    .and_then(|m| m.context_window)
                    .or(config.agent.context_window),
                max_context_window: None,
                loaded: None,
                quantization: None,
                capabilities: Default::default(),
            }],
            origin: Origin::Configuration,
            observed_at: None,
            age_secs: None,
            credentials_resolved: false,
            notices: vec![
                "offline: no usable cached catalog; this is the configured model, not an endpoint listing"
                    .into(),
            ],
        });
    }
    let endpoint = crate::models::endpoint_for(config, vault, name)?
        .map(Ok)
        .unwrap_or_else(|| rook_llm::endpoint_from_spec(name, config.agent.context_window))?;
    let credential = credential(&scope, &endpoint);
    // Constructing the provider checks endpoint/key transport policy even when
    // a fresh cache exists. Resolving a different account invalidates the entry.
    let provider = rook_llm::from_endpoints_with(
        vec![endpoint],
        config.agent.stream_idle(),
        rook_llm::Prefer::AsConfigured,
    )?;
    cached = cached.filter(|entry| entry.credential == credential);
    if mode == Mode::PreferCache
        && cached
            .as_ref()
            .is_some_and(|entry| current.saturating_sub(entry.observed_at) < settings.cache_ttl_secs)
        && let Some(entry) = cached.as_ref()
        && let Ok(listing) = decode_cache(entry, current, settings, true)
    {
        return Ok(listing);
    }
    match provider.models_with(settings.limits).await {
        Ok(models) => {
            let observed_at = now();
            let mut notices = Vec::new();
            if settings.cache_enabled
                && let Err(error) = remember(directory, settings, &scope, &credential, observed_at, &models)
            {
                notices.push(format!("metadata was fetched but could not be cached ({})", error.kind()));
            }
            Ok(Listing {
                models,
                origin: Origin::Endpoint,
                observed_at: Some(observed_at),
                age_secs: Some(0),
                credentials_resolved: true,
                notices,
            })
        }
        Err(error) => {
            let unavailable = matches!(
                &error,
                LlmError::Unreachable { .. }
                    | LlmError::Stalled { .. }
                    | LlmError::Status { status: 429 | 500..=599, .. }
            );
            if mode == Mode::PreferCache
                && unavailable
                && let Some(entry) = cached
                && let Ok(mut listing) = from_cache(entry, now(), settings, true)
            {
                listing.origin = Origin::StaleCache;
                listing.notices.push("endpoint unavailable; showing cached observations. Use --refresh to see the connection error".into());
                return Ok(listing);
            }
            Err(error)
        }
    }
}

fn credential(scope: &str, endpoint: &rook_llm::Endpoint) -> String {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update(endpoint.key.as_deref().unwrap_or_default().as_bytes());
    hex::encode(digest.finalize())
}

fn from_cache(
    entry: Entry,
    current: u64,
    settings: Settings,
    credentials_resolved: bool,
) -> rook_llm::Result<Listing> {
    decode_cache(&entry, current, settings, credentials_resolved)
}
fn decode_cache(
    entry: &Entry,
    current: u64,
    settings: Settings,
    credentials_resolved: bool,
) -> rook_llm::Result<Listing> {
    let models = rook_llm::catalog::entries(&entry.models, "data", settings.limits.max_models)?;
    let age = current.saturating_sub(entry.observed_at);
    Ok(Listing {
        models,
        origin: if age < settings.cache_ttl_secs { Origin::Cache } else { Origin::StaleCache },
        observed_at: Some(entry.observed_at),
        age_secs: Some(age),
        credentials_resolved,
        notices: if credentials_resolved {
            vec![]
        } else {
            vec!["offline: endpoint and current credentials were not checked".into()]
        },
    })
}

fn read(directory: &Path, settings: Settings) -> io::Result<Vec<Entry>> {
    let text = rook_contain::files::read_text(directory, Path::new(FILE), settings.cache_max_bytes)?;
    rook_llm::catalog::entries(&text, "entries", settings.cache_max_entries).map_err(io::Error::other)
}

/// A bounded serializer does not first allocate an oversize JSON string and
/// check it afterward. The same cap covers escaped model names and metadata.
pub(crate) fn encode(value: &impl Serialize, maximum: usize) -> io::Result<Vec<u8>> {
    struct Buffer {
        bytes: Vec<u8>,
        maximum: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("model cache exceeds byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer { bytes: Vec::new(), maximum };
    serde_json::to_writer(&mut buffer, value).map_err(io::Error::other)?;
    Ok(buffer.bytes)
}

// A concurrently spawned child may hold a duplicate until exec. Closing our
// descriptor alone leaves that lock alive, making the next local write look
// busy; release the lock explicitly on every return path.
struct CacheLock(std::fs::File);
impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn remember(
    directory: &Path,
    settings: Settings,
    scope: &str,
    credential: &str,
    observed_at: u64,
    models: &[ModelInfo],
) -> io::Result<()> {
    if models.len() > settings.limits.max_models {
        return Err(io::Error::other("model cache exceeds model limit"));
    }
    #[derive(Serialize)]
    struct Models<'a> {
        data: &'a [ModelInfo],
    }
    let models = String::from_utf8(encode(&Models { data: models }, settings.cache_max_bytes)?)
        .map_err(io::Error::other)?;
    crate::paths::private_dir(directory)?;
    let lock = rook_contain::files::lock_file(directory, Path::new(LOCK))?;
    lock.try_lock().map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "model cache is being updated"))?;
    let _guard = CacheLock(lock);
    // No networking under the lock. Atomic replacement allows readers to keep
    // using the previous complete file, and a killed writer leaves it intact.
    let mut entries = read(directory, settings).unwrap_or_default();
    entries.retain(|entry| entry.scope != scope && entry.observed_at <= observed_at);
    entries.sort_by_key(|entry| entry.observed_at);
    while entries.len() >= settings.cache_max_entries {
        entries.remove(0);
    }
    entries.push(Entry {
        scope: scope.into(),
        credential: credential.into(),
        observed_at,
        complete: true,
        models,
    });
    #[derive(Serialize)]
    struct Entries<'a> {
        entries: &'a [Entry],
    }
    loop {
        match encode(&Entries { entries: &entries }, settings.cache_max_bytes) {
            Ok(bytes) => return rook_contain::files::write_private(directory, Path::new(FILE), &bytes),
            Err(error) if entries.len() <= 1 => return Err(error),
            Err(_) => {
                entries.remove(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            owned_by: None,
            context_window: None,
            max_context_window: None,
            loaded: None,
            quantization: None,
            capabilities: Default::default(),
        }
    }
    #[test]
    fn entry_and_byte_budgets_evict_old_observations_and_reject_one_oversize_entry() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings { cache_max_entries: 2, cache_max_bytes: 4096, ..Default::default() };
        for index in 0..4 {
            remember(dir.path(), settings, &index.to_string(), "credential-hash", index, &[model("one")])
                .unwrap();
        }
        let entries = read(dir.path(), settings).unwrap();
        assert_eq!(entries.iter().map(|entry| entry.scope.as_str()).collect::<Vec<_>>(), ["2", "3"]);
        let before = std::fs::read(dir.path().join(FILE)).unwrap();
        assert!(remember(dir.path(), settings, "huge", "hash", 5, &[model(&"x".repeat(5000))]).is_err());
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), before);
        for index in 5..12 {
            remember(
                dir.path(),
                Settings { cache_max_entries: 128, ..settings },
                &index.to_string(),
                "hash",
                index,
                &[model(&"x".repeat(1800))],
            )
            .unwrap();
        }
        assert!(std::fs::metadata(dir.path().join(FILE)).unwrap().len() <= 4096);
        assert!(read(dir.path(), settings).unwrap().len() <= 2);
    }
    #[test]
    fn cache_read_refuses_oversize_files_and_too_many_entries_before_retaining_them() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings { cache_max_bytes: 4096, cache_max_entries: 1, ..Default::default() };
        std::fs::write(dir.path().join(FILE), " ".repeat(4097)).unwrap();
        assert!(read(dir.path(), settings).is_err());
        std::fs::write(dir.path().join(FILE), r#"{"entries":[{"scope":"1","credential":"h","observed_at":0,"models":"{}"},{"scope":"2","credential":"h","observed_at":0,"models":"{}"}]}"#).unwrap();
        assert!(read(dir.path(), settings).unwrap_err().to_string().contains("exceeds 1 models"));
    }
    #[test]
    fn a_busy_writer_does_not_block_or_corrupt_the_previous_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        remember(dir.path(), settings, "one", "hash", 1, &[model("one")]).unwrap();
        let lock = rook_contain::files::lock_file(dir.path(), Path::new(LOCK)).unwrap();
        lock.try_lock().unwrap();
        assert!(remember(dir.path(), settings, "two", "hash", 2, &[model("two")]).is_err());
        assert_eq!(read(dir.path(), settings).unwrap()[0].scope, "one");
        lock.unlock().unwrap();
        drop(lock);
        remember(dir.path(), settings, "two", "hash", 2, &[model("two")]).unwrap();
        assert_eq!(read(dir.path(), settings).unwrap().len(), 2);
    }
    #[test]
    fn a_duplicate_handle_does_not_keep_a_finished_cache_update_locked() {
        let dir = tempfile::tempdir().unwrap();
        let file = rook_contain::files::lock_file(dir.path(), Path::new(LOCK)).unwrap();
        file.try_lock().unwrap();
        let duplicate = file.try_clone().unwrap();
        let guard = CacheLock(file);
        let next = rook_contain::files::lock_file(dir.path(), Path::new(LOCK)).unwrap();
        assert!(next.try_lock().is_err(), "fixture must hold the lock before release");
        drop(guard);
        next.try_lock().expect("the duplicate must not extend the writer's lifetime");
        next.unlock().unwrap();
        drop(duplicate);
    }
    #[cfg(unix)]
    #[test]
    fn cache_files_are_private_and_symlinks_cannot_read_or_overwrite_an_outside_file() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("keep");
        std::fs::write(&path, "keep").unwrap();
        symlink(&path, dir.path().join(FILE)).unwrap();
        assert!(read(dir.path(), Settings::default()).is_err());
        assert!(remember(dir.path(), Settings::default(), "one", "hash", 1, &[model("one")]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        std::fs::write(dir.path().join(FILE), "old").unwrap();
        std::fs::set_permissions(dir.path().join(FILE), std::fs::Permissions::from_mode(0o644)).unwrap();
        remember(dir.path(), Settings::default(), "one", "hash", 1, &[model("one")]).unwrap();
        assert_eq!(std::fs::metadata(dir.path().join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
