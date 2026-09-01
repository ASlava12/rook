//! A Model Context Protocol client.
//!
//! MCP is JSON-RPC 2.0, carried either over a subprocess's pipes or over HTTP.
//! Rook uses four methods of it — `initialize`, `notifications/initialized`,
//! `tools/list` and `tools/call` — so this is written directly rather than
//! pulling in the full SDK, which would add 21 crates for a fraction of its
//! surface. See `docs/adr/0008-hand-written-mcp-client.md`.

pub mod http;
pub mod protocol;
pub mod stdio;
pub mod transport;

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use transport::Transport;

pub use protocol::{RpcError, ServerInfo, ToolDescriptor, ToolResult};

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("could not start {command:?}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{server}: transport error: {message}")]
    Transport { server: String, message: String },
    #[error("{server}: {method} returned an error: {} (code {})", .error.message, .error.code)]
    Rpc { server: String, method: String, error: RpcError },
    #[error("{server}: {method} did not answer within {}s{}", timeout.as_secs(), last_words(said))]
    Timeout { server: String, method: String, timeout: Duration, said: String },
    #[error("{server}: the server exited{}", last_words(said))]
    Closed { server: String, said: String },
    #[error("{server}: could not parse the response to {method}: {message}")]
    Decode { server: String, method: String, message: String },
    #[error("{server}: needs either a command or a url")]
    NotConfigured { server: String },
}

/// A failing subprocess explains itself on stderr and nowhere else — the
/// protocol reports only that the pipe closed — so what it said is carried into
/// the error rather than left in a debug log nobody has enabled. Empty for a
/// server that said nothing, and for HTTP, which has no such channel.
fn last_words(said: &str) -> String {
    match said.is_empty() {
        true => String::new(),
        false => format!(", last saying: {said}"),
    }
}

impl McpError {
    /// The pipe, not the server: something a restart could fix. A server that
    /// answered — with an rpc error, or an unparseable response — is working,
    /// and restarting it would only throw the answer away.
    pub fn is_transport(&self) -> bool {
        matches!(self, McpError::Transport { .. } | McpError::Closed { .. })
    }
}

pub type Result<T> = std::result::Result<T, McpError>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub name: String,
    /// A subprocess to speak to over its pipes.
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
    /// An HTTP endpoint, used instead of `command` when set.
    pub url: Option<String>,
    /// Extra headers for the HTTP transport, which is where an API key goes.
    pub headers: HashMap<String, String>,
    /// A server that never completes the handshake must fail fast rather than
    /// stalling the agent's startup.
    pub startup_timeout_secs: u64,
    pub call_timeout_secs: u64,
    pub enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: None,
            headers: HashMap::new(),
            startup_timeout_secs: 20,
            call_timeout_secs: 120,
            enabled: true,
        }
    }
}

pub struct Server {
    name: String,
    info: ServerInfo,
    /// Replaceable, because a subprocess that dies would otherwise take its
    /// tools out for the rest of the run: every later call returned the same
    /// transport error and nothing tried again.
    transport: tokio::sync::RwLock<Box<dyn Transport>>,
    call_timeout: Duration,
    config: ServerConfig,
    restarts: std::sync::atomic::AtomicU32,
}

/// Enough to survive a crash and a restart loop's first turns, and few enough
/// that a server dying on every call is not respawned on every call.
const MOST_RESTARTS: u32 = 3;

impl Server {
    /// Connect and complete the MCP handshake, over whichever transport the
    /// configuration describes.
    pub async fn connect(config: &ServerConfig) -> Result<Self> {
        let transport = Self::transport_for(config)?;
        let info = Self::handshake(transport.as_ref(), config).await?;
        Ok(Self {
            name: config.name.clone(),
            info,
            transport: tokio::sync::RwLock::new(transport),
            call_timeout: Duration::from_secs(config.call_timeout_secs),
            config: config.clone(),
            restarts: std::sync::atomic::AtomicU32::new(0),
        })
    }

    fn transport_for(config: &ServerConfig) -> Result<Box<dyn Transport>> {
        match config.url.as_deref() {
            Some(url) if !url.is_empty() => {
                Ok(Box::new(http::Http::new(&config.name, url, &config.headers)?))
            }
            _ if !config.command.trim().is_empty() => Ok(Box::new(stdio::Stdio::spawn(config)?)),
            _ => Err(McpError::NotConfigured { server: config.name.clone() }),
        }
    }

    /// Straight at the transport, never through the retrying path: a server
    /// that cannot complete its handshake is not one a restart brings back, and
    /// routing this through `request_with` would have restart call connect call
    /// restart.
    async fn handshake(transport: &dyn Transport, config: &ServerConfig) -> Result<ServerInfo> {
        let message = transport
            .request(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": protocol::PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "clientInfo": { "name": "rook", "version": env!("CARGO_PKG_VERSION") },
                })),
                Duration::from_secs(config.startup_timeout_secs),
            )
            .await?;
        if let Some(error) = message.error {
            return Err(McpError::Rpc { server: config.name.clone(), method: "initialize".into(), error });
        }
        let info =
            serde_json::from_value(message.result.unwrap_or(serde_json::Value::Null)).map_err(|e| {
                McpError::Decode {
                    server: config.name.clone(),
                    method: "initialize".into(),
                    message: e.to_string(),
                }
            })?;
        transport.notify("notifications/initialized", None).await?;
        Ok(info)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    pub async fn child_pid(&self) -> Option<u32> {
        self.transport.read().await.child_pid()
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>> {
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default)]
            tools: Vec<ToolDescriptor>,
        }
        let listing: Listing = self.request("tools/list", Some(serde_json::json!({}))).await?;
        Ok(listing.tools)
    }

    pub async fn call_tool(&self, tool: &str, arguments: &serde_json::Value) -> Result<ToolResult> {
        self.request("tools/call", Some(serde_json::json!({ "name": tool, "arguments": arguments }))).await
    }

    pub async fn shutdown(&self) {
        self.transport.read().await.shutdown().await;
    }

    async fn request<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<T> {
        self.request_with(method, params, self.call_timeout).await
    }

    /// Replace a dead transport, up to a few times per run. `false` when the
    /// cap is reached or the server will not come back, and then the caller
    /// reports what actually went wrong.
    async fn restart(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.restarts.fetch_add(1, Ordering::Relaxed) >= MOST_RESTARTS {
            return false;
        }
        let Ok(fresh) = Self::transport_for(&self.config) else { return false };
        if Self::handshake(fresh.as_ref(), &self.config).await.is_err() {
            return false;
        }
        tracing::warn!(server = %self.name, "restarted after a transport failure");
        *self.transport.write().await = fresh;
        true
    }

    async fn request_with<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<T> {
        // Bound, so the read guard is dropped before the arms below run: a
        // guard in the scrutinee lives to the end of the match, and `restart`
        // wants the write side of the same lock.
        let first = self.transport.read().await.request(method, params.clone(), timeout).await;
        let message = match first {
            Ok(message) => message,
            // A dead subprocess is worth one restart and one retry. Anything the
            // server itself answered — an rpc error, a decode failure — is the
            // server working, and restarting it would only lose the answer.
            Err(e) if e.is_transport() && self.restart().await => {
                self.transport.read().await.request(method, params, timeout).await?
            }
            Err(e) => return Err(e),
        };
        if let Some(error) = message.error {
            return Err(McpError::Rpc { server: self.name.clone(), method: method.into(), error });
        }
        serde_json::from_value(message.result.unwrap_or(serde_json::Value::Null)).map_err(|e| {
            McpError::Decode { server: self.name.clone(), method: method.into(), message: e.to_string() }
        })
    }
}
