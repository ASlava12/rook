//! The streamable-HTTP transport.
//!
//! Each request is a POST. The server may answer with a single JSON object or
//! with an event stream, and both are correct, so the client has to handle
//! either. A session id handed back on `initialize` must accompany every later
//! request, or the server treats each one as a new connection.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;

use crate::protocol::{Incoming, Notification, Request};
use crate::transport::Transport;
use crate::{McpError, Result};

const SESSION_HEADER: &str = "mcp-session-id";

/// A single frame this large is a broken or hostile endpoint: the protocol sends
/// one small JSON object per event.
const MAX_FRAME_BYTES: usize = 8 << 20;

pub(crate) struct Http {
    name: String,
    url: String,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
    session: Mutex<Option<String>>,
    next_id: AtomicU64,
}

impl Http {
    pub(crate) fn new(
        name: &str,
        url: &str,
        headers: &std::collections::HashMap<String, String>,
    ) -> Result<Self> {
        rook_llm::init_tls();
        let client = reqwest::Client::builder()
            .user_agent(concat!("rook/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| McpError::Transport { server: name.into(), message: e.to_string() })?;
        Ok(Self {
            name: name.to_string(),
            url: url.trim_end_matches('/').to_string(),
            client,
            headers: headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            session: Mutex::new(None),
            next_id: AtomicU64::new(1),
        })
    }

    async fn post(&self, body: &impl serde::Serialize) -> Result<reqwest::Response> {
        let mut request =
            self.client.post(&self.url).header("accept", "application/json, text/event-stream").json(body);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Ok(session) = self.session.lock()
            && let Some(id) = session.as_deref()
        {
            request = request.header(SESSION_HEADER, id);
        }

        let response = request
            .send()
            .await
            .map_err(|e| McpError::Transport { server: self.name.clone(), message: e.to_string() })?;

        if let Some(id) = response.headers().get(SESSION_HEADER).and_then(|v| v.to_str().ok())
            && let Ok(mut session) = self.session.lock()
        {
            *session = Some(id.to_string());
        }
        Ok(response)
    }
}

#[async_trait]
impl Transport for Http {
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<Incoming> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = Request { jsonrpc: "2.0", id, method, params };

        let response = tokio::time::timeout(timeout, self.post(&body))
            .await
            .map_err(|_| McpError::Timeout { server: self.name.clone(), method: method.into(), timeout })??;

        let status = response.status();
        let event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport {
                server: self.name.clone(),
                message: format!("{status}: {}", truncate(&body, 500)),
            });
        }

        if event_stream {
            read_event_stream(&self.name, method, id, response, timeout).await
        } else {
            let text = response
                .text()
                .await
                .map_err(|e| McpError::Transport { server: self.name.clone(), message: e.to_string() })?;
            serde_json::from_str(&text).map_err(|e| McpError::Decode {
                server: self.name.clone(),
                method: method.into(),
                message: format!("{e}: {}", truncate(&text, 300)),
            })
        }
    }

    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let body = Notification { jsonrpc: "2.0", method, params };
        // A notification has no answer; 202 is the usual reply and any 2xx is fine.
        self.post(&body).await.map(|_| ())
    }

    async fn shutdown(&self) {}
}

/// Read frames until the answer to `id` arrives.
///
/// The stream may carry notifications and server-initiated requests alongside
/// the response; anything that is not the reply we are waiting for is skipped
/// rather than treated as one.
async fn read_event_stream(
    server: &str,
    method: &str,
    id: u64,
    response: reqwest::Response,
    timeout: Duration,
) -> Result<Incoming> {
    let mut bytes = response.bytes_stream();
    let mut buffer = String::new();
    let mut scanned = 0usize;

    loop {
        let chunk = match tokio::time::timeout(timeout, bytes.next()).await {
            Err(_) => {
                return Err(McpError::Timeout { server: server.into(), method: method.into(), timeout });
            }
            Ok(None) => return Err(McpError::Closed { server: server.into() }),
            Ok(Some(chunk)) => {
                chunk.map_err(|e| McpError::Transport { server: server.into(), message: e.to_string() })?
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if buffer.len() > MAX_FRAME_BYTES {
            return Err(McpError::Transport {
                server: server.into(),
                message: format!("an event passed {MAX_FRAME_BYTES} bytes with no separator"),
            });
        }

        while let Some(offset) = buffer[scanned..].find("\n\n") {
            let end = scanned + offset;
            scanned = 0;
            let frame: String = buffer.drain(..end + 2).collect();
            for line in frame.lines() {
                let Some(data) = line.strip_prefix("data:") else { continue };
                let Ok(message) = serde_json::from_str::<Incoming>(data.trim()) else { continue };
                if message.id == Some(id) {
                    return Ok(message);
                }
            }
        }
        scanned = buffer.len().saturating_sub(1);
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let cut = (0..=max).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    format!("{}…", &text[..cut])
}
