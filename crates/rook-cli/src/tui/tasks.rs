//! Background tasks are a pane, with a responsive editor and live receipts.
//! One bounded worker polls HTTP; the terminal thread never waits for a model.
use super::*;
use rook_proto::work::{Action, Run, Start, Status, Steer, Steering};

pub(super) struct Tasks {
    runs: Vec<Run>,
    selection: ListState,
    selected: Option<String>,
    detail: Option<Run>,
    compose: Option<Compose>,
    draft: Typing,
    autonomous: bool,
    pending: bool,
    waiting_for: Option<(String, String)>,
    confirming_cancel: bool,
    note: String,
    scroll: u16,
    workspace: String,
    goal_bytes: usize,
    message_bytes: usize,
    defaults: String,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Update>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

enum Compose {
    New,
    Correction { task: String, id: String },
}
enum Command {
    Select(String),
    Start(Box<Start>),
    Steer(String, Steer),
    Control(String, Action),
    Forget(String),
}
#[derive(Debug)]
enum Update {
    Snapshot(Vec<Run>, Option<Box<Run>>),
    Accepted(String, Option<Steering>),
    Controlled(String),
    Error(String),
    Disconnected(String),
}

impl Drop for Tasks {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

impl Tasks {
    pub(super) fn new(
        source: &crate::source::Source,
        runtime: &tokio::runtime::Runtime,
        config: &rook_core::config::WorkConfig,
    ) -> Self {
        let (commands, receive) = mpsc::channel(8);
        let (send, events) = mpsc::channel(8);
        let worker = source.daemon_base().map(|base| runtime.spawn(observe(base.into(), receive, send)));
        let note = if worker.is_some() {
            "Loading background tasks…"
        } else {
            "Shared daemon required. Reopen `rook tui` without --alone."
        }
        .into();
        Self {
            runs: vec![],
            selection: ListState::default(),
            selected: None,
            detail: None,
            compose: None,
            draft: Typing::default(),
            autonomous: false,
            pending: false,
            waiting_for: None,
            confirming_cancel: false,
            note,
            scroll: 0,
            workspace: source.workspace().display().to_string(),
            goal_bytes: config.max_goal_bytes,
            message_bytes: config.max_message_bytes,
            defaults: format!(
                "Budget: {} days · {} iterations · {} tokens (0 = unlimited)",
                config.max_seconds as f64 / 86400.0,
                config.max_iterations,
                config.max_tokens
            ),
            commands,
            events,
            worker,
        }
    }

    pub(super) fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Update::Snapshot(runs, detail) => {
                    if self.selected.as_ref().is_none_or(|id| !runs.iter().any(|r| &r.id == id)) {
                        self.selected = detail
                            .as_ref()
                            .map(|r| r.id.clone())
                            .or_else(|| runs.first().map(|r| r.id.clone()));
                    }
                    self.selection.select(runs.iter().position(|r| Some(&r.id) == self.selected.as_ref()));
                    self.runs = runs;
                    if detail.as_ref().is_some_and(|r| Some(&r.id) == self.selected.as_ref()) {
                        if let (Some((task, message)), Some(after)) = (&self.waiting_for, &detail)
                            && after.id == *task
                            && after.instructions.iter().any(|m| m.id == *message && m.applied_at.is_some())
                        {
                            self.note = "✓ Correction taken into the agent's context. Execution is not yet confirmed.".into();
                            self.waiting_for = None;
                        }
                        self.detail = detail.map(|r| *r);
                    } else if self.selected.is_none() {
                        self.detail = None;
                    }
                    if self.note.starts_with("Loading") || self.note.starts_with("Connection unavailable") {
                        self.note = "Live status · closing this window leaves tasks running".into();
                    }
                }
                Update::Accepted(task, receipt) => {
                    self.pending = false;
                    self.waiting_for = receipt
                        .as_ref()
                        .filter(|r| r.applied_at.is_none())
                        .map(|r| (task.clone(), r.id.clone()));
                    self.selected = Some(task);
                    self.compose = None;
                    self.draft.clear();
                    self.scroll = 0;
                    self.note = match receipt {
                        Some(receipt) if receipt.applied_at.is_some() => {
                            "✓ Correction taken into the agent's context. Execution is not yet confirmed."
                                .into()
                        }
                        Some(_) => "Correction saved; awaiting the agent's next safe boundary.".into(),
                        None => "Task started in background. Enter sends a correction to the selected task."
                            .into(),
                    };
                }
                Update::Controlled(note) => {
                    self.pending = false;
                    self.note = note;
                }
                Update::Error(error) => {
                    self.pending = false;
                    self.note = if self.compose.is_some() {
                        format!("{error} · draft kept; Enter retries")
                    } else {
                        error
                    };
                }
                Update::Disconnected(error) => {
                    self.note = format!("Connection unavailable: {error}. Reconnecting…");
                }
            }
        }
    }

    fn send(&mut self, command: Command, mutation: bool) {
        if self.worker.is_none() {
            return;
        }
        match self.commands.try_send(command) {
            Ok(()) => {
                if mutation {
                    self.pending = true;
                    self.note = "Saving…".into();
                }
            }
            Err(_) => self.note = "Request queue busy; try again in a moment.".into(),
        }
    }

    pub(super) fn paste(&mut self, text: &str) {
        if self.compose.is_none() || self.pending {
            return;
        }
        let cap =
            if matches!(self.compose, Some(Compose::New)) { self.goal_bytes } else { self.message_bytes };
        if self.draft.as_str().len().saturating_add(text.len()) > cap {
            self.note = format!("Text exceeds the {cap}-byte limit; draft kept.");
            return;
        }
        self.draft.paste(text);
        self.new_receipt_id();
    }

    fn new_receipt_id(&mut self) {
        if let Some(Compose::Correction { id, .. }) = &mut self.compose {
            *id = rook_store::format_session_id(rook_store::new_session_id());
        }
    }

    /// True closes the pane; an editor's Escape first dismisses just the editor.
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.confirming_cancel {
            self.confirming_cancel = false;
            if key.code == KeyCode::Char('y')
                && let Some(id) = self.selected.clone()
            {
                self.send(Command::Control(id, Action::Cancel), true);
            }
            return false;
        }
        if self.compose.is_some() {
            if key.code == KeyCode::Esc {
                if !self.pending {
                    self.compose = None;
                }
                return false;
            }
            if self.pending {
                return false;
            }
            if key.code == KeyCode::Enter && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
            {
                self.paste("\n");
                return false;
            }
            if key.code == KeyCode::Enter {
                let text = self.draft.as_str().trim().to_string();
                if text.is_empty() {
                    self.note = "Write a goal or correction first.".into();
                    return false;
                }
                let command = match &self.compose {
                    Some(Compose::New) => Command::Start(Box::new(Start {
                        conversation: None,
                        goal: text,
                        workspace: Some(self.workspace.clone()),
                        autonomous: self.autonomous,
                        max_iterations: None,
                        max_tokens: None,
                        max_seconds: None,
                    })),
                    Some(Compose::Correction { task, id }) => {
                        Command::Steer(task.clone(), Steer { id: id.clone(), text })
                    }
                    None => return false,
                };
                self.send(command, true);
                return false;
            }
            match key.code {
                KeyCode::Tab if matches!(self.compose, Some(Compose::New)) => {
                    self.autonomous = !self.autonomous
                }
                KeyCode::Backspace => {
                    self.draft.backspace();
                    self.new_receipt_id();
                }
                KeyCode::Delete => {
                    self.draft.delete();
                    self.new_receipt_id();
                }
                KeyCode::Left => self.draft.left(),
                KeyCode::Right => self.draft.right(),
                KeyCode::Up => {
                    self.draft.up();
                }
                KeyCode::Down => {
                    self.draft.down();
                }
                KeyCode::Home => self.draft.home(),
                KeyCode::End => self.draft.end(),
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                    self.draft.kill_to_start();
                    self.new_receipt_id();
                }
                KeyCode::Char('w') if key.modifiers == KeyModifiers::CONTROL => {
                    self.draft.kill_word();
                    self.new_receipt_id();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.paste(&c.to_string())
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Char('n') if !self.pending => {
                self.compose = Some(Compose::New);
                self.draft.clear();
            }
            KeyCode::Enter | KeyCode::Char('i') if !self.pending => {
                if let Some(id) = self.selected.clone() {
                    if self.runs.iter().any(|r| r.id == id && r.status.terminal()) {
                        self.note = "This task has ended; n starts a new goal.".into();
                    } else {
                        self.compose = Some(Compose::Correction {
                            task: id,
                            id: rook_store::format_session_id(rook_store::new_session_id()),
                        });
                        self.draft.clear();
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.select(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select(1),
            KeyCode::Char('p') | KeyCode::Char('r') if !self.pending => {
                if let Some(id) = self.selected.clone() {
                    self.send(
                        Command::Control(
                            id,
                            if key.code == KeyCode::Char('p') { Action::Pause } else { Action::Resume },
                        ),
                        true,
                    );
                }
            }
            KeyCode::Char('x') if !self.pending && self.selected.is_some() => self.confirming_cancel = true,
            KeyCode::Char('d') if !self.pending => {
                if let Some(id) = self.selected.clone() {
                    self.send(Command::Forget(id), true);
                }
            }
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(8),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
            _ => {}
        }
        false
    }

    fn select(&mut self, delta: isize) {
        if self.runs.is_empty() {
            return;
        }
        let at = self.selection.selected().unwrap_or(0).saturating_add_signed(delta).min(self.runs.len() - 1);
        self.selected = Some(self.runs[at].id.clone());
        self.selection.select(Some(at));
        self.scroll = 0;
        self.detail = None;
        self.send(Command::Select(self.runs[at].id.clone()), false);
    }

    pub(super) fn draw(&mut self, f: &mut Frame, area: Rect) {
        let editor_height = if self.compose.is_some() { 7 } else { 0 };
        let [body, editor, note] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(editor_height),
            Constraint::Length(if self.compose.is_some() { 5 } else { 3 }),
        ])
        .areas(area);
        let [list, detail] = if body.width >= 80 {
            Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)]).areas(body)
        } else {
            Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(body)
        };
        let items: Vec<ListItem> = self
            .runs
            .iter()
            .map(|run| {
                ListItem::new(vec![
                    Line::from(Span::styled(
                        format!("{:?} · {} iterations", run.status, run.iterations),
                        Style::default().fg(match run.status {
                            Status::Completed => Color::Green,
                            Status::Blocked | Status::Limited => Color::Yellow,
                            _ => Color::Cyan,
                        }),
                    )),
                    Line::from(run.goal.chars().take(120).collect::<String>()),
                ])
            })
            .collect();
        if items.is_empty() {
            f.render_widget(
                Paragraph::new("No tasks yet.\n\nn — new background task").block(bordered(" tasks ")),
                list,
            );
        } else {
            f.render_stateful_widget(
                List::new(items)
                    .block(bordered(" tasks · ↑↓ select "))
                    .highlight_symbol("▌")
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                list,
                &mut self.selection,
            );
        }
        let mut lines = Vec::<Line>::new();
        if let Some(run) = &self.detail {
            lines.push(Line::from(Span::styled(run.goal.clone(), Style::default().bold())));
            lines.push(Line::from(format!("{:?}: {}", run.status, run.reason)));
            lines.push(Line::from(format!("{} iterations · {} tokens", run.iterations, run.tokens)));
            lines.push(Line::from(format!("Workspace: {}", run.workspace)));
            lines.push(Line::from(if run.autonomous {
                "Policy: autonomous (deny list still applies)"
            } else {
                "Policy: configured; unattended requests are refused"
            }));
            if let Some(at) = run.next_attempt_at {
                lines.push(Line::from(format!(
                    "Retry in {}s",
                    at.saturating_sub(rook_core::work::managed::now())
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Corrections", Style::default().bold())));
            if run.instructions.is_empty() {
                lines.push(Line::from("Enter — send a correction to this task"));
            }
            for receipt in &run.instructions {
                lines.push(Line::from(Span::styled(
                    if receipt.applied_at.is_some() {
                        "✓ In context"
                    } else {
                        "Pending — saved, not yet in context"
                    },
                    Style::default().fg(if receipt.applied_at.is_some() {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                )));
                lines.extend(receipt.text.lines().map(|text| Line::from(text.to_string())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Latest answer", Style::default().bold())));
            lines.extend(run.reply.lines().map(|text| Line::from(text.to_string())));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Verification", Style::default().bold())));
            lines.extend(run.verification.lines().map(|text| Line::from(text.to_string())));
        } else {
            lines.push(Line::from(
                "Select a task, or press n to start one.\nTasks continue when you close the TUI.",
            ));
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(bordered(" live task · PgUp/PgDn scroll ")),
            detail,
        );
        if let Some(compose) = &self.compose {
            let title = match compose {
                Compose::New => format!(
                    " new goal · Tab: autonomous [{}] · Enter start · Esc back ",
                    if self.autonomous { "x" } else { " " }
                ),
                Compose::Correction { .. } => {
                    " correction · Enter send · Alt/Shift+Enter newline · Esc back ".into()
                }
            };
            let (row, column) = self.draft.caret();
            let top = row.saturating_sub(editor.height.saturating_sub(3));
            let left = column.saturating_sub(editor.width.saturating_sub(4));
            f.render_widget(
                Paragraph::new(self.draft.as_str()).scroll((top, left)).block(bordered(&title)),
                editor,
            );
            f.set_cursor_position((editor.x + 1 + column - left, editor.y + 1 + row - top));
        }
        let text = if self.confirming_cancel {
            "Cancel this task? y / n — completed operations are kept.".into()
        } else if matches!(self.compose, Some(Compose::New)) {
            format!(
                "{}\nAutonomous approves operations allowed by the deny list. {}",
                self.defaults, self.note
            )
        } else {
            self.note.clone()
        };
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(bordered(" status ")), note);
    }
}

async fn request<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    method: reqwest::Method,
    body: Option<serde_json::Value>,
) -> Result<T> {
    let mut request = client.request(method, format!("{base}{path}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status();
    let value: serde_json::Value = response.json().await?;
    if !status.is_success() {
        anyhow::bail!("{}", value["error"].as_str().unwrap_or("daemon request failed"));
    }
    Ok(serde_json::from_value(value)?)
}

async fn observe(mut base: String, mut commands: mpsc::Receiver<Command>, updates: mpsc::Sender<Update>) {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(client) => client,
        Err(error) => {
            let _ = updates.send(Update::Error(error.to_string())).await;
            return;
        }
    };
    let mut selected: Option<String> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            value = commands.recv() => { let Some(command) = value else { return }; Some(command) }
            _ = tick.tick() => None,
        };
        // The daemon can restart on another ephemeral port. Its published
        // address, not a stale window URL, is the reconnection point.
        if let Ok(address) = std::fs::read_to_string(rook_core::paths::daemon_address_file()) {
            base = address.trim().into();
        }
        if let Some(command) = command {
            let result: Result<Option<Update>> = async {
                match command {
                    Command::Select(id) => {
                        selected = Some(id);
                        Ok(None)
                    }
                    Command::Start(start) => {
                        let run: Run = request(
                            &client,
                            &base,
                            "/api/work",
                            reqwest::Method::POST,
                            Some(serde_json::to_value(start)?),
                        )
                        .await?;
                        selected = Some(run.id.clone());
                        Ok(Some(Update::Accepted(run.id, None)))
                    }
                    Command::Steer(id, message) => {
                        let receipt: Steering = request(
                            &client,
                            &base,
                            &format!("/api/work/{id}/steer"),
                            reqwest::Method::POST,
                            Some(serde_json::to_value(message)?),
                        )
                        .await?;
                        selected = Some(id.clone());
                        Ok(Some(Update::Accepted(id, Some(receipt))))
                    }
                    Command::Control(id, action) => {
                        let run: Run = request(
                            &client,
                            &base,
                            &format!("/api/work/{id}/control"),
                            reqwest::Method::POST,
                            Some(serde_json::to_value(action)?),
                        )
                        .await?;
                        selected = Some(id);
                        Ok(Some(Update::Controlled(run.reason)))
                    }
                    Command::Forget(id) => {
                        let _: serde_json::Value = request(
                            &client,
                            &base,
                            &format!("/api/work/{id}"),
                            reqwest::Method::DELETE,
                            None,
                        )
                        .await?;
                        selected = None;
                        Ok(Some(Update::Controlled("Task record forgotten; sessions are kept.".into())))
                    }
                }
            }
            .await;
            let update = match result {
                Ok(update) => update,
                Err(error) => Some(Update::Error(error.to_string())),
            };
            if let Some(update) = update
                && updates.send(update).await.is_err()
            {
                return;
            }
        }
        let snapshot: Result<Update> = async {
            let runs: Vec<Run> = request(&client, &base, "/api/work", reqwest::Method::GET, None).await?;
            if selected.as_ref().is_none_or(|id| !runs.iter().any(|r| &r.id == id)) {
                selected = runs.first().map(|r| r.id.clone());
            }
            let detail = match &selected {
                Some(id) => Some(Box::new(
                    request(&client, &base, &format!("/api/work/{id}"), reqwest::Method::GET, None).await?,
                )),
                None => None,
            };
            Ok(Update::Snapshot(runs, detail))
        }
        .await;
        if updates.send(snapshot.unwrap_or_else(|e| Update::Disconnected(e.to_string()))).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_acknowledgements_preserve_the_draft_and_selected_task() {
        let (commands, _) = mpsc::channel(8);
        let (send, events) = mpsc::channel(8);
        let mut pane = Tasks {
            runs: vec![],
            selection: ListState::default(),
            selected: None,
            detail: None,
            compose: Some(Compose::New),
            draft: Typing::default(),
            autonomous: false,
            pending: false,
            waiting_for: Some(("selected".into(), "one".into())),
            confirming_cancel: false,
            note: String::new(),
            scroll: 0,
            workspace: ".".into(),
            goal_bytes: 12,
            message_bytes: 8,
            defaults: String::new(),
            commands,
            events,
            worker: None,
        };
        pane.paste("unsent draft");
        pane.paste("too large");
        assert_eq!(pane.draft.as_str(), "unsent draft", "the byte cap is actually reached");
        let mut run = Run {
            conversation: None,
            id: "selected".into(),
            workspace: ".".into(),
            goal: "inspect".into(),
            status: Status::Running,
            reason: "working".into(),
            created_at: 1,
            updated_at: 1,
            next_attempt_at: None,
            autonomous: false,
            max_iterations: 10,
            max_tokens: 0,
            max_seconds: 604800,
            iterations: 0,
            tokens: 0,
            consecutive_failures: 0,
            session: None,
            instructions: vec![Steering {
                id: "one".into(),
                text: "correction".into(),
                submitted_at: 1,
                applied_at: None,
                session: None,
            }],
            recent: vec![],
            reply: String::new(),
            verification: String::new(),
        };
        send.try_send(Update::Snapshot(vec![run.clone()], Some(Box::new(run.clone())))).unwrap();
        pane.poll();
        assert_eq!(pane.selected.as_deref(), Some("selected"));
        run.instructions[0].applied_at = Some(2);
        let mut other = run.clone();
        other.id = "other".into();
        send.try_send(Update::Snapshot(vec![other, run.clone()], Some(Box::new(run)))).unwrap();
        pane.poll();
        assert!(pane.note.contains("taken into the agent's context"), "{}", pane.note);
        assert_eq!(pane.selected.as_deref(), Some("selected"));
        assert_eq!(pane.selection.selected(), Some(1));
        assert_eq!(pane.draft.as_str(), "unsent draft", "polling cannot overwrite editing");
    }
}
