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
    #[error("{server}: {method} did not answer within {}s", timeout.as_secs())]
    Timeout { server: String, method: String, timeout: Duration },
    #[error("{server}: the server exited")]
    Closed { server: String },
    #[error("{server}: could not parse the response to {method}: {message}")]
    Decode { server: String, method: String, message: String },
    #[error("{server}: needs either a command or a url")]
    NotConfigured { server: String },
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
    transport: Box<dyn Transport>,
    call_timeout: Duration,
}

impl Server {
    /// Connect and complete the MCP handshake, over whichever transport the
    /// configuration describes.
    pub async fn connect(config: &ServerConfig) -> Result<Self> {
        let transport: Box<dyn Transport> = match config.url.as_deref() {
            Some(url) if !url.is_empty() => Box::new(http::Http::new(&config.name, url, &config.headers)?),
            _ if !config.command.trim().is_empty() => Box::new(stdio::Stdio::spawn(config)?),
            _ => return Err(McpError::NotConfigured { server: config.name.clone() }),
        };

        let server = Self {
            name: config.name.clone(),
            info: ServerInfo::default(),
            transport,
            call_timeout: Duration::from_secs(config.call_timeout_secs),
        };

        let info: ServerInfo = server
            .request_with(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": protocol::PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "clientInfo": { "name": "rook", "version": env!("CARGO_PKG_VERSION") },
                })),
                Duration::from_secs(config.startup_timeout_secs),
            )
            .await?;

        server.transport.notify("notifications/initialized", None).await?;
        Ok(Self { info, ..server })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.transport.child_pid()
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
        self.transport.shutdown().await;
    }

    async fn request<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<T> {
        self.request_with(method, params, self.call_timeout).await
    }

    async fn request_with<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<T> {
        let message = self.transport.request(method, params, timeout).await?;
        if let Some(error) = message.error {
            return Err(McpError::Rpc { server: self.name.clone(), method: method.into(), error });
        }
        serde_json::from_value(message.result.unwrap_or(serde_json::Value::Null)).map_err(|e| {
            McpError::Decode { server: self.name.clone(), method: method.into(), message: e.to_string() }
        })
    }
}
