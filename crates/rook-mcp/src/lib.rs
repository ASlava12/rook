//! A Model Context Protocol client, stdio transport only.
//!
//! MCP over stdio is JSON-RPC 2.0 in newline-delimited JSON. Rook uses four
//! methods of it — `initialize`, `notifications/initialized`, `tools/list` and
//! `tools/call` — so this is written directly rather than pulling in the full
//! SDK, which would add 21 crates for a fraction of its surface. See
//! `docs/adr/0008-hand-written-mcp-client.md`.

pub mod protocol;

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};

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
}

pub type Result<T> = std::result::Result<T, McpError>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
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
            startup_timeout_secs: 20,
            call_timeout_secs: 120,
            enabled: true,
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<protocol::Incoming>>>>;

pub struct Server {
    name: String,
    info: ServerInfo,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    call_timeout: Duration,
    child: Mutex<Child>,
}

impl Server {
    /// Spawn the server and complete the MCP handshake.
    pub async fn connect(config: &ServerConfig) -> Result<Self> {
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }

        let mut child =
            command.spawn().map_err(|e| McpError::Spawn { command: config.command.clone(), source: e })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // An undrained stderr pipe fills and blocks the server mid-write, which
        // presents as a hang with no output anywhere.
        let name = config.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(server = %name, "{line}");
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let name = config.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<protocol::Incoming>(&line) else {
                    tracing::debug!(server = %name, "unparsable line: {line}");
                    continue;
                };
                match message.id {
                    Some(id) => {
                        if let Some(tx) = reader_pending.lock().await.remove(&id) {
                            let _ = tx.send(message);
                        }
                    }
                    // Notifications are not requested and nothing waits on them.
                    None => tracing::debug!(server = %name, method = ?message.method, "notification"),
                }
            }
            // The pipe closed: waiters would otherwise hang until their timeout.
            reader_pending.lock().await.clear();
        });

        let server = Self {
            name: config.name.clone(),
            info: ServerInfo::default(),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            call_timeout: Duration::from_secs(config.call_timeout_secs),
            child: Mutex::new(child),
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

        server.notify("notifications/initialized", None).await?;
        Ok(Self { info, ..server })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
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
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let line = serde_json::to_string(&protocol::Request { jsonrpc: "2.0", id, method, params })
            .map_err(|e| self.decode_error(method, e.to_string()))?;
        if let Err(e) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        let message = match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(McpError::Timeout { server: self.name.clone(), method: method.into(), timeout });
            }
            Ok(Err(_)) => return Err(McpError::Closed { server: self.name.clone() }),
            Ok(Ok(message)) => message,
        };

        if let Some(error) = message.error {
            return Err(McpError::Rpc { server: self.name.clone(), method: method.into(), error });
        }
        serde_json::from_value(message.result.unwrap_or(serde_json::Value::Null))
            .map_err(|e| self.decode_error(method, e.to_string()))
    }

    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let line = serde_json::to_string(&protocol::Notification { jsonrpc: "2.0", method, params })
            .map_err(|e| self.decode_error(method, e.to_string()))?;
        self.write_line(&line).await
    }

    async fn write_line(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| self.transport_error(e.to_string()))?;
        stdin.flush().await.map_err(|e| self.transport_error(e.to_string()))
    }

    fn transport_error(&self, message: String) -> McpError {
        McpError::Transport { server: self.name.clone(), message }
    }

    fn decode_error(&self, method: &str, message: String) -> McpError {
        McpError::Decode { server: self.name.clone(), method: method.into(), message }
    }
}
