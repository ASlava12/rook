//! A single bounded worker leaves input and turn streaming live during discovery.
use super::*;
use futures_util::FutureExt;
use rook_core::mcp_connections::Report;

#[cfg(test)]
mod tests;

pub(super) struct Connections {
    commands: mpsc::Sender<Option<String>>,
    events: mpsc::Receiver<Result<Report, String>>,
    worker: tokio::task::JoinHandle<()>,
    startup: Option<tokio::task::JoinHandle<()>>,
    local: Option<Arc<rook_core::McpSession>>,
    report: Option<Report>,
    pending: bool,
    note: String,
    selection: ListState,
    auth: super::mcp_auth::Auth,
}
impl Drop for Connections {
    fn drop(&mut self) {
        self.close();
    }
}
impl Connections {
    pub(super) fn close(&mut self) {
        self.worker.abort();
        if let Some(startup) = self.startup.take() {
            startup.abort();
        }
        self.auth.close();
    }

    pub(super) fn new(
        source: &crate::source::Source,
        runtime: &tokio::runtime::Runtime,
        session: Arc<rook_core::McpSession>,
        startup: Option<rook_core::mcp_connections::Startup>,
    ) -> Self {
        let local = source.here().map(|_| session.clone());
        let startup = startup
            .filter(|_| local.is_some() && session.pending_connections() != 0)
            .map(|startup| runtime.spawn(startup.run()));
        let report = local.as_ref().map(|session| session.report());
        let note = if startup.is_some() { "Connecting MCP servers…".into() } else { String::new() };
        let auth = super::mcp_auth::Auth::new(source, runtime, session.clone());
        let (commands, mut requests) = mpsc::channel::<Option<String>>(1);
        let (send, events) = mpsc::channel(1);
        let base = source.daemon_base().map(str::to_owned);
        let workspace = source.workspace().to_path_buf();
        let worker = runtime.spawn(async move {
            rook_llm::init_tls();
            let client = reqwest::Client::builder().timeout(Duration::from_secs(600)).build();
            while let Some(name) = requests.recv().await {
                let result = match &base {
                    Some(base) => match &client {
                        Ok(client) => remote(client, base, &workspace, name.as_deref()).await,
                        Err(_) => Err("could not create the MCP status client".into()),
                    },
                    None => match name {
                        Some(name) => session.reconnect_in(&workspace, &name).await,
                        None => Ok(session.report()),
                    },
                };
                if send.send(result).await.is_err() {
                    break;
                }
            }
        });
        Self {
            commands,
            events,
            worker,
            startup,
            local,
            report,
            pending: false,
            note,
            selection: ListState::default(),
            auth,
        }
    }

    pub(super) fn request(&mut self, name: Option<String>) {
        if self.pending {
            return;
        }
        if name.is_some() && self.starting() {
            self.note = "Initial MCP connections are still running; wait before reconnecting.".into();
            return;
        }
        self.auth.clear_notice();
        self.note = if name.is_some() {
            "Reconnecting; active turns keep their current tools…"
        } else {
            "Reading MCP connections…"
        }
        .into();
        match self.commands.try_send(name) {
            Ok(()) => self.pending = true,
            Err(_) => self.note = "MCP worker unavailable; reopen the window".into(),
        }
    }

    pub(super) fn poll(&mut self) -> Option<String> {
        let notice = self.poll_startup();
        if let Some(report) = self.auth.poll() {
            self.selection.select(if report.servers.is_empty() {
                None
            } else {
                Some(self.selection.selected().unwrap_or(0).min(report.servers.len() - 1))
            });
            self.report = Some(report);
        }
        if let Ok(result) = self.events.try_recv() {
            self.pending = false;
            match result {
                Ok(report) => {
                    self.note = report.issue.clone().unwrap_or_else(|| {
                        "r reconnects; u refreshes; l signs in; x signs out; o opens the sign-in URL; c cancels".into()
                    });
                    self.selection.select(if report.servers.is_empty() {
                        None
                    } else {
                        Some(self.selection.selected().unwrap_or(0).min(report.servers.len() - 1))
                    });
                    self.report = Some(report);
                }
                Err(error) => self.note = error,
            }
        }
        notice
    }

    fn starting(&self) -> bool {
        self.local.as_ref().is_some_and(|session| session.pending_connections() != 0)
    }

    fn poll_startup(&mut self) -> Option<String> {
        let startup = self.startup.as_ref()?;
        let finished = startup.is_finished();
        let report = self.local.as_ref()?.report();
        self.selection.select(if report.servers.is_empty() {
            None
        } else {
            Some(self.selection.selected().unwrap_or(0).min(report.servers.len() - 1))
        });
        let ready = report.servers.iter().filter(|s| s.state == "connected").count();
        let failed = report.servers.iter().filter(|s| s.state == "failed").count();
        self.report = Some(report);
        if !finished {
            return None;
        }
        let succeeded = matches!(self.startup.take()?.now_or_never(), Some(Ok(())));
        let text = if succeeded {
            format!("MCP startup finished: {ready} connected, {failed} failed. /mcp shows connection status.")
        } else {
            "MCP startup stopped. /mcp shows connection status and reconnect controls.".into()
        };
        self.note = text.clone();
        Some(text)
    }

    pub(super) fn authenticate(&mut self, name: String, logout: bool) {
        if self.starting() {
            self.note =
                "Initial MCP connections are still running; wait before changing authentication.".into();
            return;
        }
        self.auth.start(name, logout);
    }

    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        let count = self.report.as_ref().map_or(0, |r| r.servers.len());
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Char('u') => self.request(None),
            KeyCode::Char('o') => self.auth.open(),
            KeyCode::Char('c') => self.auth.cancel(),
            KeyCode::Char('l') | KeyCode::Char('x') => {
                if let Some(name) = self
                    .selection
                    .selected()
                    .and_then(|i| self.report.as_ref()?.servers.get(i))
                    .map(|s| s.name.clone())
                {
                    self.authenticate(name, key.code == KeyCode::Char('x'));
                }
            }
            KeyCode::Char('r') | KeyCode::Enter => {
                if let Some(name) = self
                    .selection
                    .selected()
                    .and_then(|i| self.report.as_ref()?.servers.get(i))
                    .map(|s| s.name.clone())
                {
                    self.request(Some(name));
                }
            }
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                self.selection.select(Some((self.selection.selected().unwrap_or(0) + 1).min(count - 1)))
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                self.selection.select(Some(self.selection.selected().unwrap_or(0).saturating_sub(1)))
            }
            _ => {}
        }
        false
    }

    pub(super) fn draw(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" MCP connections ");
        let inside = block.inner(area);
        f.render_widget(block, area);
        let height = if self.auth.visible() { inside.height.saturating_sub(4).min(12) } else { 4 };
        let [rows, note] = Layout::vertical([Constraint::Min(2), Constraint::Length(height)]).areas(inside);
        let items: Vec<_> = self
            .report
            .as_ref()
            .map(|r| {
                r.servers
                    .iter()
                    .map(|s| {
                        let status = format!(
                            "{} — {}{} · {} tools · {} active requests",
                            s.name,
                            s.state,
                            if s.reconnecting { " (reconnecting)" } else { "" },
                            s.tools,
                            s.active_requests
                        );
                        ListItem::new(vec![
                            Line::from(status),
                            Line::from(s.catalog.to_string()),
                            Line::from(
                                s.error.clone().or_else(|| s.last_request_error.clone()).unwrap_or_default(),
                            ),
                        ])
                    })
                    .collect()
            })
            .unwrap_or_default();
        if items.is_empty() {
            f.render_widget(
                Paragraph::new(
                    "No MCP servers installed. Add one with rook config edit, then /mcp reconnect <name>.",
                ),
                rows,
            );
        } else {
            f.render_stateful_widget(
                List::new(items)
                    .highlight_symbol("› ")
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                rows,
                &mut self.selection,
            );
        }
        let text = if self.auth.visible() { self.auth.text() } else { self.note.clone() };
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), note);
    }
}

async fn remote(
    client: &reqwest::Client,
    base: &str,
    workspace: &std::path::Path,
    name: Option<&str>,
) -> Result<Report, String> {
    let query = crate::remote::escaped(&workspace.to_string_lossy());
    let request = match name {
        Some(name) => client
            .post(format!("{base}/api/mcp/{}/reconnect?workspace={query}", crate::remote::escaped(name)))
            .json(&serde_json::json!({})),
        None => client.get(format!("{base}/api/mcp?workspace={query}")),
    };
    let mut response = request
        .send()
        .await
        .map_err(|_| "could not reach rookd; refresh status before retrying a reconnect")?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "could not read MCP status")? {
        if chunk.len() > (256 * 1024usize).saturating_sub(bytes.len()) {
            return Err("MCP status exceeds 256 KiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let error: rook_proto::ApiError =
            serde_json::from_slice(&bytes).map_err(|_| "rookd refused the MCP request")?;
        return Err(error.error);
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid MCP status response".into())
}
