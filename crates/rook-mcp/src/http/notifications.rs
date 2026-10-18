//! Optional GET notifications belong to the transport, never to a model turn.
use super::*;

struct Listener {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    version: Option<reqwest::header::HeaderValue>,
    session: Option<String>,
    token: Option<std::sync::Arc<dyn crate::oauth::TokenSource>>,
    clock: std::sync::Arc<crate::catalog::Clock>,
    owner: u64,
    patience: Duration,
    retry_ms: Mutex<Option<u64>>,
}
enum End {
    Unsupported,
    Disconnected,
    Stop,
}

impl Http {
    pub(super) fn watch_catalog_stream(&self, patience: Duration) {
        let mut task = self.catalog_listener.lock().unwrap_or_else(|e| e.into_inner());
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let listener = Listener {
            client: self.client.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            version: self.version.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            session: self.session.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            token: self.token.clone(),
            clock: self.catalog_clock.clone(),
            owner: self.connection,
            patience,
            retry_ms: Mutex::new(None),
        };
        listener.clock.start_listener(listener.owner);
        *task = Some(tokio::spawn(async move { listener.run().await }));
    }

    pub(super) fn stop_catalog_stream(&self) {
        if let Some(task) = self.catalog_listener.lock().unwrap_or_else(|e| e.into_inner()).take() {
            task.abort();
            self.catalog_clock.listener_state(self.connection, 5);
        }
    }
}

impl Listener {
    fn request(&self, method: reqwest::Method, token: Option<&str>) -> reqwest::RequestBuilder {
        let accept = if method == reqwest::Method::GET {
            "text/event-stream"
        } else {
            "application/json, text/event-stream"
        };
        let mut request = self.client.request(method, &self.url);
        for (name, value) in &self.headers {
            if !name.eq_ignore_ascii_case("mcp-protocol-version") {
                request = request.header(name, value);
            }
        }
        if let Some(version) = &self.version {
            request = request.header("mcp-protocol-version", version);
        }
        if let Some(session) = &self.session {
            request = request.header(SESSION_HEADER, session);
        }
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.header("accept", accept)
    }

    async fn run(self) {
        for attempt in 0..3 {
            self.clock.listener_state(self.owner, 2);
            match self.listen().await {
                End::Unsupported => {
                    self.clock.listener_state(self.owner, 4);
                    return;
                }
                End::Stop => {
                    self.clock.listener_state(self.owner, 5);
                    return;
                }
                End::Disconnected => self.clock.listener_state(self.owner, 5),
            }
            if attempt < 2 {
                let wait = self.retry_ms.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or(1000 << attempt);
                // Never reconnect earlier than the server requests. An
                // excessive delay stops automatic recovery rather than hiding
                // an effectively permanent background timer.
                if wait > 300_000 {
                    return;
                }
                self.clock.listener_state(self.owner, 2);
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
        }
    }

    async fn listen(&self) -> End {
        let ready = tokio::time::timeout(self.patience, async {
            let token = match &self.token {
                Some(source) => Some(source.token().await.ok()?),
                None => None,
            };
            let response = self.request(reqwest::Method::GET, token.as_deref()).send().await.ok()?;
            Some((response, token))
        })
        .await
        .ok()
        .flatten();
        let Some((response, token)) = ready else { return End::Disconnected };
        if matches!(response.status().as_u16(), 404 | 405) {
            return End::Unsupported;
        }
        if !response.status().is_success() {
            return if response.status().is_server_error() { End::Disconnected } else { End::Stop };
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return End::Stop;
        }
        self.clock.listener_state(self.owner, 3);
        // A resumed GET need not replay missed events. Force a fresh complete
        // catalog instead of claiming its old prefix covers that gap.
        self.clock.changed();
        let mut frames = EventReader::new(MAX_FRAME_BYTES);
        let mut bytes = response.bytes_stream();
        let mut refused_requests = 0usize;
        while let Some(chunk) = bytes.next().await {
            let Ok(chunk) = chunk else { return End::Disconnected };
            let mut from = 0;
            while from < chunk.len() {
                let Ok((used, message)) = frames.next_frame(&chunk[from..]) else { return End::Stop };
                if let Some(retry) = frames.retry_ms {
                    *self.retry_ms.lock().unwrap_or_else(|e| e.into_inner()) = Some(retry);
                }
                from += used;
                let Some(message) = message else { continue };
                if token.as_deref().is_some_and(|token| reflects(&message, token)) {
                    return End::Stop;
                }
                self.clock.observe(&message);
                if message.method.is_some() && message.id.is_some() {
                    // This stream has no invocation owner. It cannot ask a
                    // person for input or inherit a concurrent call's consent.
                    refused_requests += 1;
                    if refused_requests > 32 {
                        return End::Stop;
                    }
                    let id = message
                        .id
                        .filter(|id| id.as_u64().is_some() || id.as_str().is_some_and(|s| s.len() <= 128));
                    let reply = serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"No invocation owns this server request"}});
                    let acknowledged = tokio::time::timeout(
                        Duration::from_secs(5),
                        self.request(reqwest::Method::POST, token.as_deref()).json(&reply).send(),
                    )
                    .await;
                    if !matches!(acknowledged, Ok(Ok(response)) if response.status() == reqwest::StatusCode::ACCEPTED)
                    {
                        return End::Disconnected;
                    }
                }
            }
        }
        End::Disconnected
    }
}
