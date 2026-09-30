//! One consent worker per window; input and running turns never wait on a browser.
use super::*;
use rook_core::mcp_connections::Report;

pub(super) struct Auth {
    base: Option<String>,
    workspace: std::path::PathBuf,
    runtime: tokio::runtime::Handle,
    session: Arc<rook_core::McpSession>,
    worker: Option<tokio::task::JoinHandle<()>>,
    browser: Option<tokio::task::JoinHandle<()>>,
    send: mpsc::Sender<Event>,
    events: mpsc::Receiver<Event>,
    url: Option<String>,
    remote_id: Option<String>,
    pub note: String,
}
enum Event {
    Url { url: String, id: Option<String> },
    Finished(Result<Option<Report>, String>),
    Note(String),
    Cancelled,
}
impl Drop for Auth {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
        if let Some(browser) = &self.browser {
            browser.abort();
        }
    }
}
impl Auth {
    pub(super) fn new(
        source: &crate::source::Source,
        runtime: &tokio::runtime::Runtime,
        session: Arc<rook_core::McpSession>,
    ) -> Self {
        let (send, events) = mpsc::channel(4);
        Self {
            base: source.daemon_base().map(str::to_owned),
            workspace: source.workspace().to_path_buf(),
            runtime: runtime.handle().clone(),
            session,
            worker: None,
            browser: None,
            send,
            events,
            url: None,
            remote_id: None,
            note: String::new(),
        }
    }
    pub(super) fn start(&mut self, name: String, logout: bool) {
        if self.worker.is_some() {
            self.note = "Sign-in is running; c cancels it first".into();
            return;
        }
        if let Some(browser) = self.browser.take() {
            browser.abort();
        }
        let (send, events) = mpsc::channel(4);
        self.send = send;
        self.events = events;
        self.url = None;
        self.remote_id = None;
        self.note = if logout { "Signing out…" } else { "Preparing sign-in…" }.into();
        let (base, workspace, session, send) =
            (self.base.clone(), self.workspace.clone(), self.session.clone(), self.send.clone());
        self.worker = Some(self.runtime.spawn(async move {
            let result = async {
                if let Some(base) = base {
                    remote(&base, &workspace, &name, logout, &send).await
                } else {
                    let (config, proxy, limits) =
                        rook_core::mcp_auth::configured(&workspace, &name).map_err(str::to_owned)?;
                    if logout {
                        rook_core::mcp_auth::logout(&config, limits).await.map_err(str::to_owned)?;
                        Ok(None)
                    } else {
                        rook_core::mcp_auth::login_native(&config, &proxy, limits, |url| {
                            let _ = send.try_send(Event::Url { url: url.into(), id: None });
                        })
                        .await
                        .map_err(str::to_owned)?;
                        session.reconnect_in(&workspace, &name).await.map(Some)
                    }
                }
            }
            .await;
            let _ = send.send(Event::Finished(result)).await;
        }));
    }
    pub(super) fn poll(&mut self) -> Option<Report> {
        let mut report = None;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Url { url, id } => {
                    self.url = Some(url);
                    self.remote_id = id;
                    self.note = "o opens the sign-in URL in your browser; c cancels. Esc returns to chat while sign-in continues.".into();
                }
                Event::Finished(result) => {
                    self.worker.take();
                    self.url = None;
                    self.remote_id = None;
                    match result {
                        Ok(value) => {
                            self.note = if value.is_some() {
                                "Signed in and reconnected."
                            } else {
                                "Saved credentials removed. Already dispatched requests may finish."
                            }
                            .into();
                            report = value;
                        }
                        Err(error) => self.note = error,
                    }
                }
                Event::Note(note) => self.note = note,
                Event::Cancelled => {
                    if let Some(worker) = self.worker.take() {
                        worker.abort();
                    }
                    self.url = None;
                    self.remote_id = None;
                    self.note = "Sign-in cancelled.".into();
                    let (send, events) = mpsc::channel(4);
                    self.send = send;
                    self.events = events;
                }
            }
        }
        report
    }
    pub(super) fn cancel(&mut self) {
        let Some(worker) = self.worker.take() else { return };
        // A remote callback belongs to the daemon and may be exchanging a code.
        // Ask it first; it refuses cancellation once that exchange has started.
        if let (Some(base), Some(id)) = (self.base.clone(), self.remote_id.clone()) {
            self.worker = Some(worker);
            if self.browser.as_ref().is_some_and(|t| !t.is_finished()) {
                return;
            }
            let send = self.send.clone();
            self.browser = Some(self.runtime.spawn(async move {
                let result = async {
                    let client = client()?;
                    read::<serde_json::Value>(
                        client.delete(format!("{base}/api/mcp/oauth/{}", crate::remote::escaped(&id))),
                    )
                    .await?;
                    Ok::<_, String>(())
                }
                .await;
                let event = match result {
                    Ok(()) => Event::Cancelled,
                    Err(error) => Event::Note(error),
                };
                let _ = send.send(event).await;
            }));
        } else {
            worker.abort();
            if let Some(browser) = self.browser.take() {
                browser.abort();
            }
            let (send, events) = mpsc::channel(4);
            self.send = send;
            self.events = events;
            self.url = None;
            self.note = if self.base.is_some() {
                "Stopped waiting. Use l or the web panel to inspect any pending daemon sign-in."
            } else {
                "Sign-in cancelled."
            }
            .into();
        }
    }
    pub(super) fn open(&mut self) {
        let Some(url) = self.url.clone() else { return };
        if self.browser.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let send = self.send.clone();
        self.browser = Some(self.runtime.spawn(async move {
            #[cfg(target_os = "macos")]
            let mut command = tokio::process::Command::new("open");
            #[cfg(target_os = "windows")]
            let mut command = {
                let mut c = tokio::process::Command::new("rundll32.exe");
                c.arg("url.dll,FileProtocolHandler");
                c
            };
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            let mut command = tokio::process::Command::new("xdg-open");
            #[cfg(windows)]
            command.creation_flags(rook_contain::NO_WINDOW);
            let status = tokio::time::timeout(
                Duration::from_secs(10),
                command
                    .arg(url)
                    .kill_on_drop(true)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            )
            .await;
            if !matches!(status, Ok(Ok(s)) if s.success()) {
                let _ = send
                    .send(Event::Note("Could not open a browser. Open the displayed URL manually.".into()))
                    .await;
            }
        }));
    }
    pub(super) fn text(&self) -> String {
        match &self.url {
            Some(url) => format!("{}\n{url}", self.note),
            None => self.note.clone(),
        }
    }
    pub(super) fn clear_notice(&mut self) {
        if self.worker.is_none() {
            self.note.clear();
        }
    }
    pub(super) fn visible(&self) -> bool {
        !self.note.is_empty()
    }
}

#[derive(serde::Deserialize)]
struct Attempt {
    id: String,
    server: String,
    status: String,
    authorization_url: Option<String>,
    error: Option<String>,
}
fn client() -> Result<reqwest::Client, String> {
    rook_llm::init_tls();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "could not create OAuth control client".into())
}
async fn read<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T, String> {
    let mut response = request.send().await.map_err(|_| "could not reach rookd for MCP sign-in")?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "MCP sign-in response was interrupted")? {
        if chunk.len() > (4 * 1024 * 1024usize).saturating_sub(bytes.len()) {
            return Err("MCP sign-in response exceeds 4 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(serde_json::from_slice::<rook_proto::ApiError>(&bytes)
            .map(|e| e.error)
            .unwrap_or_else(|_| "MCP sign-in request was refused".into()));
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid MCP sign-in response".into())
}
async fn remote(
    base: &str,
    workspace: &std::path::Path,
    name: &str,
    logout: bool,
    send: &mpsc::Sender<Event>,
) -> Result<Option<Report>, String> {
    let client = client()?;
    let query = format!("?workspace={}", crate::remote::escaped(&workspace.to_string_lossy()));
    let server = crate::remote::escaped(name);
    if logout {
        read::<serde_json::Value>(
            client.post(format!("{base}/api/mcp/{server}/logout{query}")).json(&serde_json::json!({})),
        )
        .await?;
        return Ok(None);
    }
    let attempts: Vec<Attempt> = read(client.get(format!("{base}/api/mcp/oauth{query}"))).await?;
    let attempt = match attempts.into_iter().find(|a| a.server == name && a.status == "waiting") {
        Some(attempt) => attempt,
        None => {
            read(
                client
                    .post(format!("{base}/api/mcp/{server}/login{query}"))
                    .json(&serde_json::json!({"redirect_uri":format!("{base}/mcp-oauth-callback.html")})),
            )
            .await?
        }
    };
    let url = attempt.authorization_url.ok_or("Rook did not return a sign-in URL; refresh MCP status")?;
    send.send(Event::Url { url, id: Some(attempt.id.clone()) }).await.map_err(|_| "MCP window closed")?;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let progress: Attempt =
            read(client.get(format!("{base}/api/mcp/oauth/{}", crate::remote::escaped(&attempt.id)))).await?;
        match progress.status.as_str() {
            "connected" => return read(client.get(format!("{base}/api/mcp{query}"))).await.map(Some),
            "failed" => {
                return Err(progress.error.unwrap_or_else(|| "MCP sign-in failed; start again".into()));
            }
            "starting" | "waiting" | "completing" => {}
            _ => return Err("unknown MCP sign-in state".into()),
        }
    }
}
