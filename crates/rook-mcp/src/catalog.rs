//! A successful catalog means every advertised page was read. A partial prefix
//! cannot make the omitted tools discoverable through the agent's lazy catalog.
use crate::{McpError, Result, Server, ServerConfig, ToolDescriptor};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Clock {
    revision: AtomicU64,
    notifications: Mutex<(u64, u8)>,
    negotiated: AtomicU8,
}
impl Clock {
    pub(crate) fn changed(&self) {
        // Keep the saturating update compatible with both MSRV 1.90 and
        // current Rust, which renamed fetch_update to try_update.
        let mut current = self.revision.load(Ordering::Acquire);
        loop {
            match self.revision.compare_exchange_weak(
                current,
                current.saturating_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
    pub(crate) fn observe(&self, message: &crate::protocol::Incoming) {
        if self.negotiated.load(Ordering::Acquire) != 1
            && message.id.is_none()
            && message.error.is_none()
            && message.result.is_none()
            && message.method.as_deref() == Some("notifications/tools/list_changed")
        {
            self.changed();
        }
    }
    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    pub(crate) fn negotiate(&self, supported: bool) {
        self.negotiated.store(if supported { 2 } else { 1 }, Ordering::Release);
    }
    fn negotiated(&self) -> bool {
        self.negotiated.load(Ordering::Acquire) == 2
    }
    pub(crate) fn stdio(&self) {
        *self.notifications.lock().unwrap_or_else(|e| e.into_inner()) = (0, 1);
    }
    pub(crate) fn start_listener(&self, owner: u64) {
        *self.notifications.lock().unwrap_or_else(|e| e.into_inner()) = (owner, 2);
    }
    pub(crate) fn listener_state(&self, owner: u64, mode: u8) {
        let mut state = self.notifications.lock().unwrap_or_else(|e| e.into_inner());
        if state.0 != owner {
            return;
        }
        if state.1 == 3 && mode != 3 {
            self.changed();
        }
        state.1 = mode;
    }
    fn notifications(&self) -> &'static str {
        match self.notifications.lock().unwrap_or_else(|e| e.into_inner()).1 {
            1 => "stdio",
            2 => "HTTP notifications connecting",
            3 => "HTTP notifications connected",
            4 => "HTTP GET unsupported; response streams only",
            5 => "HTTP notifications disconnected; reconnect server",
            _ => "HTTP response streams",
        }
    }
}

struct Snapshot {
    revision: u64,
    generation: u64,
    tools: Arc<Vec<ToolDescriptor>>,
}
#[derive(Default)]
struct State {
    snapshot: Option<Snapshot>,
    failed_at: Option<Instant>,
}
#[derive(Default)]
pub(crate) struct Cache {
    state: Mutex<State>,
    refresh: tokio::sync::Mutex<()>,
}

/// Payload-free status; reading it never probes an idle server.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CatalogStatus {
    pub configured: bool,
    pub negotiated: bool,
    pub enabled: bool,
    pub generation: u64,
    pub tools: usize,
    pub notifications: String,
    pub stale: bool,
    pub retry_after_secs: Option<u64>,
    pub error: Option<String>,
}
const COOLDOWN: Duration = Duration::from_secs(5);

struct Refresh<'a> {
    server: &'a Server,
    complete: bool,
    _lease: tokio::sync::MutexGuard<'a, ()>,
}
impl Drop for Refresh<'_> {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        let mut state = self.server.catalog_cache.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.snapshot.as_ref().is_none_or(|s| s.revision != self.server.catalog_clock.revision()) {
            state.failed_at = Some(Instant::now());
        }
        // Publish cooldown before releasing the coalescing lease, including
        // cancellation. A queued reader cannot race into another failed list.
    }
}

impl std::fmt::Display for CatalogStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.configured {
            return write!(f, "tool-list refresh off");
        }
        write!(
            f,
            "catalog generation {} · {}",
            self.generation,
            if self.stale { "stale" } else { "current" }
        )?;
        write!(f, " · {}", self.notifications)?;
        if !self.negotiated {
            write!(f, " · listChanged not negotiated")?;
        }
        if let Some(error) = &self.error {
            write!(f, " · {error}")?;
        }
        if let Some(seconds) = self.retry_after_secs {
            write!(f, " · retry after {seconds}s")?;
        }
        Ok(())
    }
}

impl ServerConfig {
    /// Shared with offline configuration checks; never opens a connection.
    pub fn catalog_error(&self) -> Option<&'static str> {
        if !(1024..=32 * 1024 * 1024).contains(&self.catalog_max_bytes) {
            Some("catalog_max_bytes must be 1024..=33554432")
        } else if !(1..=16384).contains(&self.catalog_max_tools) {
            Some("catalog_max_tools must be 1..=16384")
        } else if !(1..=256).contains(&self.catalog_max_pages) {
            Some("catalog_max_pages must be 1..=256")
        } else if !(1..=300).contains(&self.catalog_timeout_secs) {
            Some("catalog_timeout_secs must be 1..=300")
        } else {
            None
        }
    }
}

struct Budget(usize);
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::other("catalog byte limit exceeded"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Server {
    fn refresh_enabled(&self) -> bool {
        self.config.refresh_tools && self.catalog_clock.negotiated()
    }

    fn can_refresh_catalog(&self) -> bool {
        self.catalog_clock.negotiated()
            || !(self.has_resources() && self.info.capabilities.get("tools").is_none())
    }

    pub fn catalog_status(&self) -> CatalogStatus {
        let state = self.catalog_cache.state.lock().unwrap_or_else(|e| e.into_inner());
        let enabled = self.refresh_enabled();
        let revision = self.catalog_clock.revision();
        CatalogStatus {
            configured: self.config.refresh_tools,
            negotiated: self.catalog_clock.negotiated(),
            enabled,
            generation: state.snapshot.as_ref().map_or(0, |s| s.generation),
            tools: state.snapshot.as_ref().map_or(0, |s| s.tools.len()),
            notifications: self.catalog_clock.notifications().into(),
            stale: self.config.refresh_tools
                && self.can_refresh_catalog()
                && state.snapshot.as_ref().is_none_or(|s| s.revision != revision || revision == u64::MAX),
            retry_after_secs: state
                .failed_at
                .and_then(|at| COOLDOWN.checked_sub(at.elapsed()))
                .map(|d| d.as_millis().div_ceil(1000) as u64),
            error: state
                .failed_at
                .map(|_| "tool catalog refresh incomplete; previous snapshot retained".into()),
        }
    }

    /// The last complete catalog, never a partly refreshed prefix.
    pub fn cached_tools(&self) -> Option<(u64, Vec<ToolDescriptor>)> {
        let state = self.catalog_cache.state.lock().unwrap_or_else(|e| e.into_inner());
        state.snapshot.as_ref().map(|s| (s.generation, s.tools.as_ref().clone()))
    }

    /// Coalesce notification refreshes while existing tool calls keep running.
    /// Failures retain the previous snapshot and expose only a fixed diagnostic.
    pub async fn refresh_tool_catalog(&self) {
        if !self.catalog_status().stale {
            return;
        }
        let timeout = Duration::from_secs(self.config.catalog_timeout_secs);
        let _ = tokio::time::timeout(timeout, async {
            let lease = self.catalog_cache.refresh.lock().await;
            let status = self.catalog_status();
            if !status.stale || status.retry_after_secs.is_some() {
                return Ok(());
            }
            let mut refresh = Refresh { server: self, complete: false, _lease: lease };
            let (revision, tools) = self.consistent_catalog().await?;
            self.install_catalog(revision, tools);
            refresh.complete = true;
            Ok::<_, McpError>(())
        })
        .await;
    }

    fn install_catalog(&self, revision: u64, tools: Vec<ToolDescriptor>) {
        let mut state = self.catalog_cache.state.lock().unwrap_or_else(|e| e.into_inner());
        let generation = state.snapshot.as_ref().map_or(1, |s| s.generation.saturating_add(1));
        state.snapshot = Some(Snapshot { revision, generation, tools: Arc::new(tools) });
        state.failed_at = None;
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>> {
        let timeout = Duration::from_secs(self.config.catalog_timeout_secs);
        tokio::time::timeout(timeout, async {
            let _refresh = self.catalog_cache.refresh.lock().await;
            let (revision, tools) = self.consistent_catalog().await?;
            self.install_catalog(revision, tools.clone());
            Ok(tools)
        })
        .await
        .map_err(|_| McpError::Timeout {
            server: self.name.clone(),
            method: "tools/list catalog".into(),
            timeout,
            said: String::new(),
        })?
    }

    async fn consistent_catalog(&self) -> Result<(u64, Vec<ToolDescriptor>)> {
        for _ in 0..3 {
            let revision = self.catalog_clock.revision();
            let tools = self.catalog().await?;
            if !self.config.refresh_tools
                || (revision == self.catalog_clock.revision() && revision != u64::MAX)
            {
                return Ok((revision, tools));
            }
        }
        Err(McpError::Decode {
            server: self.name.clone(),
            method: "tools/list".into(),
            message: "tool catalog kept changing during discovery; no partial catalog installed".into(),
        })
    }

    async fn catalog(&self) -> Result<Vec<ToolDescriptor>> {
        let bad = |message: &str| McpError::Decode {
            server: self.name.clone(),
            method: "tools/list".into(),
            message: message.into(),
        };
        let mut budget = Budget(self.config.catalog_max_bytes);
        let mut tools = Vec::new();
        let mut names = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..self.config.catalog_max_pages {
            let params = cursor.as_ref().map(|c| json!({"cursor":c})).unwrap_or_else(|| json!({}));
            let mut result: Value = self.request("tools/list", Some(params)).await?;
            // Count the entire page (including continuation and ignored fields)
            // without allocating a second serialized copy.
            serde_json::to_writer(&mut budget, &result)
                .map_err(|_| bad("catalog_max_bytes exceeded; no partial catalog installed"))?;
            let Some(Value::Array(page)) = result.get_mut("tools").map(Value::take) else {
                return Err(bad("tools/list must return a tools array"));
            };
            if page.len() > self.config.catalog_max_tools.saturating_sub(tools.len()) {
                return Err(bad("catalog_max_tools exceeded; no partial catalog installed"));
            }
            for item in page {
                let tool: ToolDescriptor =
                    serde_json::from_value(item).map_err(|_| bad("invalid tool descriptor"))?;
                if tool.name.is_empty() || !names.insert(tool.name.clone()) {
                    return Err(bad("empty or duplicate tool name; no partial catalog installed"));
                }
                tools.push(tool);
            }
            cursor = match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(tools),
                Some(Value::String(next)) if !next.is_empty() && next.len() <= 4096 => Some(next.clone()),
                _ => return Err(bad("invalid nextCursor; expected a nonempty string of at most 4096 bytes")),
            };
            if !cursors.insert(cursor.clone()) {
                return Err(bad("tools/list cursor cycle; no partial catalog installed"));
            }
        }
        Err(bad("catalog_max_pages exceeded; no partial catalog installed"))
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn concurrent_invalidations_do_not_lose_revisions_or_wrap_at_saturation() {
        let clock = Arc::new(Clock::default());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let clock = clock.clone();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        clock.changed();
                    }
                });
            }
        });
        assert_eq!(clock.revision(), 4000);
        clock.revision.store(u64::MAX - 1, Ordering::Release);
        clock.changed();
        clock.changed();
        assert_eq!(clock.revision(), u64::MAX, "saturation must never make an old snapshot current again");
    }
}
