//! Session-owned connections. A replacement is published only after discovery;
//! an agent already using the old catalog keeps its own connection alive.
use std::sync::{Arc, RwLock};

use futures_util::{StreamExt, stream};
use rook_mcp::{McpError, Server, ServerConfig, ToolDescriptor};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_servers: usize,
    pub parallel_connects: usize,
    pub oauth_max_entries: usize,
    pub oauth_max_pending: usize,
    pub oauth_max_bytes: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_servers: 32,
            parallel_connects: 4,
            oauth_max_entries: 64,
            oauth_max_pending: 16,
            oauth_max_bytes: 1048576,
        }
    }
}

impl Settings {
    pub fn error(self) -> Option<&'static str> {
        if !(1..=256).contains(&self.max_servers) {
            Some("mcp_connections.max_servers: expected 1..=256")
        } else if !(1..=32).contains(&self.parallel_connects) {
            Some("mcp_connections.parallel_connects: expected 1..=32")
        } else if !(1..=64).contains(&self.oauth_max_pending) {
            Some("mcp_connections.oauth_max_pending: expected 1..=64")
        } else if !(1..=256).contains(&self.oauth_max_entries) {
            Some("mcp_connections.oauth_max_entries: expected 1..=256")
        } else if !(65536..=4194304).contains(&self.oauth_max_bytes) {
            Some("mcp_connections.oauth_max_bytes: expected 65536..=4194304")
        } else {
            None
        }
    }
}

type Connection = (Arc<Server>, Vec<ToolDescriptor>);

struct Entry {
    name: String,
    enabled: bool,
    transport: &'static str,
    installed: Option<Connection>,
    generation: u64,
    error: Option<&'static str>,
}

impl Entry {
    fn new(config: &ServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            enabled: config.enabled,
            transport: if config.url.as_deref().is_some_and(|url| !url.is_empty()) {
                "http"
            } else {
                "stdio"
            },
            installed: None,
            generation: 0,
            error: None,
        }
    }
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    issue: Option<&'static str>,
    reconnecting: Option<String>,
    closed: bool,
}

/// This report deliberately contains no command, URL, headers, environment or
/// server-authored error text. A server can echo credentials in any response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub servers: Vec<Status>,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub name: String,
    pub transport: String,
    /// Installed catalog state, not a claim that an idle remote is still alive.
    pub state: String,
    pub reconnecting: bool,
    pub generation: u64,
    pub tools: usize,
    pub active_requests: usize,
    pub last_request_error: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct McpSession {
    state: RwLock<State>,
    operation: tokio::sync::Mutex<()>,
    limits: Settings,
}

impl McpSession {
    /// No store lock is needed: probes and the long-lived frontends share the
    /// same admission and discovery rules.
    pub async fn connect<'a>(
        configs: impl IntoIterator<Item = &'a ServerConfig>,
        proxy: &rook_llm::Proxy,
        limits: Settings,
    ) -> Self {
        let mut state = State::default();
        match admitted(configs, limits) {
            Err(issue) => state.issue = Some(issue),
            Ok(configs) => {
                let configs: Vec<ServerConfig> = configs.into_iter().cloned().collect();
                state.entries = stream::iter(configs.into_iter().map(|config| async move {
                    let mut entry = Entry::new(&config);
                    if config.enabled {
                        match candidate(&config, proxy, limits).await {
                            Ok(connection) => {
                                entry.installed = Some(connection);
                                entry.generation = 1;
                            }
                            Err(error) => entry.error = Some(error),
                        }
                    }
                    entry
                }))
                .buffered(limits.parallel_connects)
                .collect()
                .await;
            }
        }
        Self { state: RwLock::new(state), operation: Default::default(), limits }
    }

    /// A fixed snapshot for one turn: swapping the manager cannot move an
    /// already-approved invocation onto a different server halfway through.
    pub fn servers(&self) -> Vec<Connection> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .filter_map(|entry| entry.installed.clone())
            .collect()
    }

    pub fn failures(&self) -> Vec<(String, String)> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        let mut failures: Vec<_> = state
            .entries
            .iter()
            .filter_map(|entry| entry.error.map(|error| (entry.name.clone(), error.to_owned())))
            .collect();
        if let Some(issue) = state.issue {
            failures.push(("MCP".into(), issue.into()));
        }
        failures
    }

    pub fn tool_count(&self) -> usize {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .filter_map(|entry| entry.installed.as_ref())
            .map(|(_, tools)| tools.len())
            .sum()
    }

    pub fn report(&self) -> Report {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        Report {
            issue: state.issue.map(str::to_owned),
            servers: state
                .entries
                .iter()
                .map(|entry| Status {
                    name: entry.name.clone(),
                    transport: entry.transport.into(),
                    state: if state.closed {
                        "closed"
                    } else if !entry.enabled {
                        "disabled"
                    } else if entry.installed.is_some() {
                        "connected"
                    } else {
                        "failed"
                    }
                    .into(),
                    reconnecting: state.reconnecting.as_deref() == Some(&entry.name),
                    generation: entry.generation,
                    tools: entry.installed.as_ref().map_or(0, |(_, tools)| tools.len()),
                    active_requests: entry
                        .installed
                        .as_ref()
                        .map_or(0, |(server, _)| server.active_requests()),
                    last_request_error: entry
                        .installed
                        .as_ref()
                        .and_then(|(server, _)| server.last_request_error())
                        .map(str::to_owned),
                    error: entry.error.map(str::to_owned),
                })
                .collect(),
        }
    }

    /// Reload only trusted declarations. Config edits apply to the selected
    /// connection without rebuilding language servers or the conversation.
    pub async fn reconnect_in(&self, workspace: &std::path::Path, name: &str) -> Result<Report, String> {
        let config = crate::Config::load_for(workspace)
            .map_err(|_| "could not load configuration; run rook config check".to_owned())?;
        let (plugins, errors) = crate::plugins::discover(workspace);
        if !errors.is_empty() {
            return Err("could not load trusted plugin declarations; check installed plugin manifests".into());
        }
        self.reconnect(
            name,
            config.mcp.iter().chain(plugins.iter().flat_map(|p| &p.mcp)),
            &config.proxy.for_mcp(),
        )
        .await
        .map_err(str::to_owned)
    }

    pub async fn reconnect<'a>(
        &self,
        name: &str,
        configs: impl IntoIterator<Item = &'a ServerConfig>,
        proxy: &rook_llm::Proxy,
    ) -> Result<Report, &'static str> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "an MCP connection change is already running; try again when it finishes")?;
        let configs = admitted(configs, self.limits)?;
        let config = configs
            .into_iter()
            .find(|config| config.name == name)
            .ok_or("server is not configured; add it with rook config edit first")?;
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return Err("MCP session is closed; open a new session");
            }
            if !state.entries.iter().any(|entry| entry.name == name) {
                if state.entries.len() >= self.limits.max_servers {
                    return Err(
                        "MCP session is full; restart after removing unused declarations or increase mcp_connections.max_servers",
                    );
                }
                state.entries.push(Entry::new(config));
            }
            state.reconnecting = Some(name.to_owned());
        }
        // Cancellation, timeout and early return must all clear the busy mark.
        let _reset = ResetBusy(&self.state);
        let result =
            if config.enabled { candidate(config, proxy, self.limits).await.map(Some) } else { Ok(None) };
        {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            let entry = state
                .entries
                .iter_mut()
                .find(|entry| entry.name == name)
                .ok_or("MCP declaration disappeared; retry the connection change")?;
            match result {
                Ok(connection) => {
                    // Dropping our Arc does not shut down tools still held by an
                    // active turn. Never replay a tool call during replacement.
                    *entry = Entry {
                        installed: connection,
                        generation: entry.generation.saturating_add(1),
                        ..Entry::new(config)
                    };
                    state.issue = None;
                }
                Err(error) => {
                    entry.error = Some(error);
                    return Err(error);
                }
            }
            state.reconnecting = None;
        }
        Ok(self.report())
    }

    pub async fn shutdown(&self) {
        let _operation = self.operation.lock().await;
        let servers: Vec<_> = {
            let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
            state.closed = true;
            state.entries.iter_mut().filter_map(|entry| entry.installed.take()).collect()
        };
        for (server, _) in servers {
            server.shutdown().await;
        }
    }
}

struct ResetBusy<'a>(&'a RwLock<State>);
impl Drop for ResetBusy<'_> {
    fn drop(&mut self) {
        self.0.write().unwrap_or_else(|e| e.into_inner()).reconnecting = None;
    }
}

pub(crate) fn admitted<'a>(
    configs: impl IntoIterator<Item = &'a ServerConfig>,
    limits: Settings,
) -> Result<Vec<&'a ServerConfig>, &'static str> {
    if let Some(error) = limits.error() {
        return Err(error);
    }
    let mut kept: Vec<&ServerConfig> = Vec::new();
    for config in configs {
        if kept.len() == limits.max_servers {
            return Err(
                "too many MCP declarations; increase mcp_connections.max_servers or remove unused servers",
            );
        }
        if config.name.trim().is_empty()
            || config.name.len() > 256
            || config.name.chars().any(char::is_control)
        {
            return Err("MCP names must be nonempty, at most 256 bytes and contain no control characters");
        }
        if kept.iter().any(|previous| previous.name == config.name) {
            return Err("duplicate MCP server name; give each configured or plugin server a unique name");
        }
        kept.push(config);
    }
    Ok(kept)
}

async fn candidate(
    config: &ServerConfig,
    proxy: &rook_llm::Proxy,
    limits: Settings,
) -> Result<Connection, &'static str> {
    let token = crate::mcp_auth::credentials(config, proxy, limits)?;
    let server = Arc::new(
        match token {
            Some(token) => Server::connect_with_token(config, proxy, Some(token)).await,
            None => Server::connect(config, proxy).await,
        }
        .map_err(safe_error)?,
    );
    let tools = server.list_tools().await.map_err(safe_error)?;
    Ok((server, tools))
}

fn safe_error(error: McpError) -> &'static str {
    match error {
        McpError::Unauthorized { .. } => "authentication required; configure this server's credentials",
        McpError::Spawn { .. } => {
            "could not start server; check command, working directory and executable permissions"
        }
        McpError::Transport { .. } | McpError::Closed { .. } => {
            "connection failed; check server availability and transport configuration"
        }
        McpError::Timeout { .. } => {
            "server did not finish startup or discovery within its configured timeout"
        }
        McpError::Decode { .. } => {
            "invalid or oversized catalog/protocol response; check server configuration and catalog limits"
        }
        McpError::Rpc { .. } => "server refused initialization or discovery; check its configuration",
        McpError::NotConfigured { .. } => "set command or url with rook config edit, or disable this server",
    }
}
