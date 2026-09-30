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

/// How much of a failing server's body is worth repeating back.
const MOST_QUOTED_BYTES: usize = 500;

/// The whole body, refused as it arrives rather than measured once it is here.
///
/// The url is configuration and the body is whatever answers it, so reading it
/// all and then looking at the length is a cap already paid.
async fn whole_text(mut response: reqwest::Response, server: &str) -> Result<String> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(None) => return Ok(String::from_utf8_lossy(&body).into_owned()),
            Ok(Some(chunk)) => {
                if chunk.len() > MAX_FRAME_BYTES.saturating_sub(body.len()) {
                    return Err(McpError::Transport {
                        server: server.into(),
                        message: format!(
                            "answered with more than the {MAX_FRAME_BYTES} bytes one message may \
                             be, and was still sending"
                        ),
                    });
                }
                body.extend_from_slice(&chunk);
            }
            Err(e) => {
                return Err(McpError::Transport { server: server.into(), message: e.to_string() });
            }
        }
    }
}

/// As much of a failure's body as the message will carry, and no more.
async fn quoted_text(mut response: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() < MOST_QUOTED_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                body.extend_from_slice(&chunk[..chunk.len().min(MOST_QUOTED_BYTES - body.len())])
            }
            _ => break,
        }
    }
    rook_llm::truncate(&String::from_utf8_lossy(&body), MOST_QUOTED_BYTES)
}

pub(crate) struct Http {
    name: String,
    url: String,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
    session: Mutex<Option<String>>,
    version: Mutex<Option<reqwest::header::HeaderValue>>,
    next_id: AtomicU64,
    token: Option<std::sync::Arc<dyn crate::oauth::TokenSource>>,
}

impl Http {
    pub(crate) fn new(
        name: &str,
        url: &str,
        headers: &std::collections::HashMap<String, String>,
        proxy: &rook_llm::Proxy,
        token: Option<std::sync::Arc<dyn crate::oauth::TokenSource>>,
    ) -> Result<Self> {
        rook_llm::init_tls();
        let client = reqwest::Client::builder()
            .user_agent(concat!("rook/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        // One server, one address: a server inside the building takes no proxy
        // whatever is configured, the same as a model endpoint on this network.
        let client = proxy
            .on(client, Some(url))
            .map_err(|message| McpError::Transport { server: name.into(), message })?;
        let client = client
            .build()
            .map_err(|e| McpError::Transport { server: name.into(), message: e.to_string() })?;
        Ok(Self {
            name: name.to_string(),
            url: url.to_string(),
            client,
            headers: headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            session: Mutex::new(None),
            version: Mutex::new(None),
            next_id: AtomicU64::new(1),
            token,
        })
    }

    async fn post(&self, body: &impl serde::Serialize) -> Result<reqwest::Response> {
        let mut request =
            self.client.post(&self.url).header("accept", "application/json, text/event-stream").json(body);
        for (name, value) in &self.headers {
            if !name.eq_ignore_ascii_case("mcp-protocol-version") {
                request = request.header(name, value);
            }
        }
        if let Some(version) = self.version.lock().ok().and_then(|v| v.clone()) {
            request = request.header("mcp-protocol-version", version);
        }
        if let Some(source) = &self.token {
            let token = source
                .token()
                .await
                .map_err(|_| McpError::Unauthorized { server: self.name.clone(), offered: Vec::new() })?;
            request = request.bearer_auth(token);
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
    fn negotiated(&self, version: &str) -> Result<()> {
        let invalid = || McpError::Decode {
            server: self.name.clone(),
            method: "initialize".into(),
            message: "invalid negotiated protocol version".into(),
        };
        // The protocol's versions are dates; bound the header before retaining
        // a server-authored value and reject whitespace/control characters.
        if version.is_empty() || version.len() > 64 || !version.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid());
        }
        let value = reqwest::header::HeaderValue::from_str(version).map_err(|_| invalid())?;
        *self.version.lock().map_err(|_| invalid())? = Some(value);
        Ok(())
    }

    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<Incoming> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = Request { jsonrpc: "2.0", id, method, params };

        tokio::time::timeout(timeout, async {
            let response = self.post(&body).await?;
            let status = response.status();
            let event_stream = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.starts_with("text/event-stream"));

            // Said as what it is rather than as a status: a 401 is the one
            // failure here a person can act on, and the header is where the
            // authorisation server is named.
            if status == reqwest::StatusCode::UNAUTHORIZED {
                let offered = response
                    .headers()
                    .get_all(reqwest::header::WWW_AUTHENTICATE)
                    .iter()
                    .filter_map(|value| value.to_str().ok())
                    .map(str::to_string)
                    .collect();
                return Err(McpError::Unauthorized { server: self.name.clone(), offered });
            }
            if !status.is_success() {
                return Err(McpError::Transport {
                    server: self.name.clone(),
                    message: format!("{status}: {}", quoted_text(response).await),
                });
            }

            if event_stream {
                read_event_stream(&self.name, method, id, response, timeout).await
            } else {
                let text = whole_text(response, &self.name).await?;
                serde_json::from_str(&text).map_err(|e| McpError::Decode {
                    server: self.name.clone(),
                    method: method.into(),
                    message: format!("{e}: {}", rook_llm::truncate(&text, 300)),
                })
            }
        })
        .await
        .map_err(|_| McpError::Timeout {
            server: self.name.clone(),
            method: method.into(),
            timeout,
            said: String::new(),
        })?
    }

    async fn notify(&self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let body = Notification { jsonrpc: "2.0", method, params };
        // A notification has no answer; 202 is the usual reply and any 2xx is fine.
        let response = self.post(&body).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(McpError::Unauthorized {
                server: self.name.clone(),
                offered: response
                    .headers()
                    .get_all(reqwest::header::WWW_AUTHENTICATE)
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .map(str::to_owned)
                    .collect(),
            });
        }
        if !response.status().is_success() {
            return Err(McpError::Transport {
                server: self.name.clone(),
                message: format!("{}: {}", response.status(), quoted_text(response).await),
            });
        }
        Ok(())
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
    let mut frames = EventReader::new(MAX_FRAME_BYTES);

    loop {
        let chunk = match tokio::time::timeout(timeout, bytes.next()).await {
            Err(_) => {
                return Err(McpError::Timeout {
                    server: server.into(),
                    method: method.into(),
                    timeout,
                    said: String::new(),
                });
            }
            Ok(None) => return Err(McpError::Closed { server: server.into(), said: String::new() }),
            Ok(Some(chunk)) => {
                chunk.map_err(|e| McpError::Transport { server: server.into(), message: e.to_string() })?
            }
        };
        if let Some(message) = frames.feed(&chunk, id).map_err(|()| McpError::Transport {
            server: server.into(),
            message: format!("an event exceeded {MAX_FRAME_BYTES} bytes"),
        })? {
            return Ok(message);
        }
    }
}

/// Incremental SSE framing caps bytes before appending. Parsing a transport
/// chunk as one allocation would let either one huge event or many small ones
/// bypass the per-event memory bound. UTF-8 stays in bytes until JSON parsing.
struct EventReader {
    bytes: Vec<u8>,
    after_cr: bool,
    limit: usize,
}
impl EventReader {
    fn new(limit: usize) -> Self {
        Self { bytes: Vec::new(), after_cr: false, limit }
    }
    fn feed(&mut self, chunk: &[u8], id: u64) -> std::result::Result<Option<Incoming>, ()> {
        for &byte in chunk {
            if self.after_cr && byte == b'\n' {
                self.after_cr = false;
                continue;
            }
            self.after_cr = byte == b'\r';
            let byte = if self.after_cr { b'\n' } else { byte };
            let complete = byte == b'\n' && self.bytes.last() == Some(&b'\n');
            if self.bytes.len() == self.limit {
                return Err(());
            }
            self.bytes.push(byte);
            if complete {
                let mut data = Vec::new();
                for line in self.bytes.split(|&b| b == b'\n') {
                    if let Some(value) = line.strip_prefix(b"data:") {
                        if !data.is_empty() {
                            data.push(b'\n');
                        }
                        data.extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
                    }
                }
                self.bytes.clear();
                if let Ok(message) = serde_json::from_slice::<Incoming>(&data)
                    && message.id == Some(id)
                {
                    return Ok(Some(message));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    #[test]
    fn sse_limits_each_event_before_retaining_it_even_in_one_large_chunk() {
        let mut reader = EventReader::new(128);
        let many = b": ignored\r\n\r\n".repeat(1000);
        assert!(many.len() > 128);
        assert!(reader.feed(&many, 1).unwrap().is_none());
        assert!(reader.bytes.is_empty());
        let oversized = vec![b'x'; 1024];
        assert!(reader.feed(&oversized, 1).is_err());
        assert_eq!(reader.bytes.len(), 128, "the cap is applied before the offending byte is copied");
    }
    #[test]
    fn multiline_sse_and_split_utf8_and_crlf_deliver_one_json_response() {
        let wire = "data: {\r\ndata: \"id\":7,\r\ndata: \"result\":{\"text\":\"Привет 🙂\"}}\r\n\r\n";
        let mut reader = EventReader::new(1024);
        let mut found = None;
        for byte in wire.as_bytes() {
            if let Some(message) = reader.feed(&[*byte], 7).unwrap() {
                assert!(found.is_none());
                found = Some(message);
            }
        }
        assert_eq!(found.unwrap().result.unwrap()["text"], "Привет 🙂");
    }
}
