//! The stdio transport: a subprocess speaking newline-delimited JSON-RPC.

use std::collections::HashMap;
use std::process::Stdio as ProcessStdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};

use crate::protocol::{Incoming, Notification, Request};
use crate::transport::Transport;
use crate::{McpError, Result, ServerConfig};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Incoming>>>>;

pub(crate) struct Stdio {
    name: String,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Child>,
}

impl Stdio {
    pub(crate) fn spawn(config: &ServerConfig) -> Result<Self> {
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(ProcessStdio::piped())
            .stdout(ProcessStdio::piped())
            .stderr(ProcessStdio::piped())
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
                let Ok(message) = serde_json::from_str::<Incoming>(&line) else {
                    tracing::debug!(server = %name, "unparsable line: {line}");
                    continue;
                };
                match message.id {
                    Some(id) => {
                        if let Some(tx) = reader_pending.lock().await.remove(&id) {
                            let _ = tx.send(message);
                        }
                    }
                    None => tracing::debug!(server = %name, method = ?message.method, "notification"),
                }
            }
            // The pipe closed: waiters would otherwise hang until their timeout.
            reader_pending.lock().await.clear();
        });

        Ok(Self {
            name: config.name.clone(),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
        })
    }

    async fn write_line(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| McpError::Transport { server: self.name.clone(), message: e.to_string() })?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::Transport { server: self.name.clone(), message: e.to_string() })
    }
}

#[async_trait]
impl Transport for Stdio {
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<Incoming> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let line = serde_json::to_string(&Request { jsonrpc: "2.0", id, method, params }).map_err(|e| {
            McpError::Decode { server: self.name.clone(), method: method.into(), message: e.to_string() }
        })?;
        if let Err(e) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::Timeout { server: self.name.clone(), method: method.into(), timeout })
            }
            Ok(Err(_)) => Err(McpError::Closed { server: self.name.clone() }),
            Ok(Ok(message)) => Ok(message),
        }
    }

    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let line = serde_json::to_string(&Notification { jsonrpc: "2.0", method, params }).map_err(|e| {
            McpError::Decode { server: self.name.clone(), method: method.into(), message: e.to_string() }
        })?;
        self.write_line(&line).await
    }

    async fn shutdown(&self) {
        let _ = self.child.lock().await.kill().await;
    }
}
