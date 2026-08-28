//! The stdio transport: a subprocess speaking newline-delimited JSON-RPC.

use std::collections::HashMap;
use std::process::Stdio as ProcessStdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};

use crate::protocol::{Incoming, Notification, Request};
use crate::transport::Transport;
use crate::{McpError, Result, ServerConfig};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Incoming>>>>;

/// The same cap the HTTP transport puts on one event, for the same reason:
/// `lines()` grows a single line until the machine runs out of memory, and how
/// long a line a server sends is decided by the server. Not a trust boundary —
/// a stdio server is a subprocess with the user's own privileges and needs no
/// trick to do harm — but a broken one should cost its own connection rather
/// than the agent's memory.
const MAX_LINE_BYTES: u64 = 8 << 20;

pub(crate) struct Stdio {
    name: String,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Child>,
}

#[derive(PartialEq, Eq)]
enum Line {
    Read,
    Ended,
}

/// One line into `line`, or [`Line::Ended`] when the stream closed or a line
/// passed [`MAX_LINE_BYTES`].
///
/// A line over the cap ends the connection rather than being resynchronised
/// past: a server that sends one is broken, the waiters are released by the
/// same path that handles a closed pipe, and [`crate::Server::restart`] is what
/// brings a broken server back.
async fn read_bounded<R>(reader: &mut R, line: &mut Vec<u8>) -> Line
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    line.clear();
    match reader.take(MAX_LINE_BYTES).read_until(b'\n', line).await {
        Ok(0) => Line::Ended,
        Ok(_) if line.last() != Some(&b'\n') => Line::Ended,
        Ok(_) => Line::Read,
        Err(_) => Line::Ended,
    }
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
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            while read_bounded(&mut reader, &mut line).await == Line::Read {
                tracing::debug!(server = %name, "{}", String::from_utf8_lossy(&line).trim_end());
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let name = config.name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            while read_bounded(&mut reader, &mut line).await == Line::Read {
                let Ok(message) = serde_json::from_slice::<Incoming>(&line) else {
                    tracing::debug!(server = %name, "unparsable line: {}", String::from_utf8_lossy(&line));
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

    fn child_pid(&self) -> Option<u32> {
        self.child.try_lock().ok()?.id()
    }

    async fn shutdown(&self) {
        let _ = self.child.lock().await.kill().await;
    }
}
