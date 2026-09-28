//! Scheduled tasks are edited locally; one bounded HTTP worker polls state.
use super::*;
use rook_proto::schedule::{Action, Create, Spec, Task};

pub(super) struct Tasks {
    tasks: Vec<Task>,
    selected: usize,
    history: usize,
    draft: Typing,
    editing: Option<usize>,
    fields: Vec<String>,
    create_id: String,
    workspace: String,
    pending: bool,
    confirming_cancel: bool,
    note: String,
    open: Option<String>,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<Update>,
    worker: Option<tokio::task::JoinHandle<()>>,
}
enum Command {
    Create(Create),
    Control(String, Action),
    Delete(String),
}
enum Update {
    Snapshot(Vec<Task>),
    Saved(String),
    Created(Box<Task>),
    Error(String),
}
const LABELS: [&str; 8] = [
    "Goal",
    "Schedule",
    "Timezone",
    "Workspace",
    "Permissions",
    "Seconds per run",
    "Tokens per run",
    "Iterations per run",
];

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
        _: &rook_core::config::WorkConfig,
    ) -> Self {
        let (commands, receive) = mpsc::channel(8);
        let (send, events) = mpsc::channel(8);
        let worker = source.daemon_base().map(|base| runtime.spawn(observe(base.into(), receive, send)));
        let note = if worker.is_some() {
            "Loading schedules…"
        } else {
            "Shared daemon required; reopen without --alone."
        }
        .into();
        Self {
            tasks: vec![],
            selected: 0,
            history: 0,
            draft: Typing::default(),
            editing: None,
            fields: vec![],
            create_id: String::new(),
            workspace: source.workspace().display().to_string(),
            pending: false,
            confirming_cancel: false,
            note,
            open: None,
            commands,
            events,
            worker,
        }
    }
    pub(super) fn poll(&mut self) {
        while let Ok(update) = self.events.try_recv() {
            match update {
                Update::Snapshot(tasks) => {
                    let id = self.tasks.get(self.selected).map(|t| t.id.clone());
                    self.selected = id.and_then(|id| tasks.iter().position(|t| t.id == id)).unwrap_or(0);
                    self.tasks = tasks;
                    if self.note.starts_with("Loading") || self.note.starts_with("Connection") {
                        self.note = "Schedules run while rookd is running".into();
                    }
                }
                Update::Created(task) => {
                    self.tasks.retain(|t| t.id != task.id);
                    self.tasks.push(*task);
                    self.selected = self.tasks.len() - 1;
                    self.history = 0;
                    self.pending = false;
                    self.editing = None;
                    self.note = "Schedule saved".into();
                }
                Update::Saved(note) => {
                    self.pending = false;
                    self.editing = None;
                    self.note = note;
                }
                Update::Error(error) => {
                    self.pending = false;
                    self.note = error;
                }
            }
        }
    }
    pub(super) fn take_session(&mut self) -> Option<String> {
        self.open.take()
    }
    fn send(&mut self, command: Command) {
        if self.worker.is_none() {
            return;
        }
        if self.commands.try_send(command).is_ok() {
            self.pending = true;
            self.note = "Saving…".into();
        } else {
            self.note = "Request queue busy; retry shortly".into();
        }
    }
    pub(super) fn paste(&mut self, text: &str) {
        if self.editing.is_some()
            && !self.pending
            && self.draft.as_str().len().saturating_add(text.len()) <= 32768
        {
            self.draft.paste(text);
            self.create_id = rook_store::format_session_id(rook_store::new_session_id());
        }
    }
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.confirming_cancel {
            self.confirming_cancel = false;
            if key.code == KeyCode::Char('y')
                && let Some(task) = self.tasks.get(self.selected)
            {
                self.send(Command::Control(task.id.clone(), Action::CancelRun));
            }
            return false;
        }
        if let Some(field) = self.editing {
            if self.pending {
                return false;
            }
            match key.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Enter => {
                    self.fields[field] = self.draft.as_str().trim().into();
                    if field < LABELS.len() - 1 {
                        self.editing = Some(field + 1);
                        self.draft.set(&self.fields[field + 1]);
                    } else {
                        match self.request() {
                            Ok(request) => self.send(Command::Create(request)),
                            Err(error) => self.note = error.to_string(),
                        }
                    }
                }
                KeyCode::BackTab => {
                    self.fields[field] = self.draft.as_str().trim().into();
                    let previous = field.saturating_sub(1);
                    self.editing = Some(previous);
                    self.draft.set(&self.fields[previous]);
                }
                KeyCode::Backspace => self.draft.backspace(),
                KeyCode::Delete => self.draft.delete(),
                KeyCode::Left => self.draft.left(),
                KeyCode::Right => self.draft.right(),
                KeyCode::Home => self.draft.home(),
                KeyCode::End => self.draft.end(),
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => self.draft.kill_to_start(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.paste(&c.to_string())
                }
                _ => {}
            }
            return false;
        }
        if self.pending {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Char('n') => {
                self.fields = vec![
                    String::new(),
                    "weekdays 09:00".into(),
                    std::env::var("TZ").unwrap_or_else(|_| "UTC".into()),
                    self.workspace.clone(),
                    "assist".into(),
                    "3600".into(),
                    "100000".into(),
                    "100".into(),
                ];
                self.create_id = rook_store::format_session_id(rook_store::new_session_id());
                self.editing = Some(0);
                self.draft.clear();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.tasks.len().saturating_sub(1));
                self.history = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.history = 0;
            }
            KeyCode::Char('[') => self.history = self.history.saturating_add(1).min(15),
            KeyCode::Char(']') => self.history = self.history.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(run) =
                    self.tasks.get(self.selected).and_then(|t| t.history.iter().rev().nth(self.history))
                {
                    if self.tasks[self.selected].pending.as_deref() == Some(run.session.as_str()) {
                        self.note = "Waiting for a worker; the session is not created yet".into();
                        return false;
                    } else {
                        self.open = Some(run.session.clone());
                        return true;
                    }
                }
                self.note = "No session for this history entry yet".into();
            }
            KeyCode::Char('x') => {
                if self.tasks.get(self.selected).is_some_and(|t| !t.history.is_empty()) {
                    self.confirming_cancel = true;
                    self.note = "Cancel the latest session? y / n. Completed operations are kept.".into();
                }
            }
            KeyCode::Char('r' | 'e' | 'p' | 'd') => {
                if let Some(task) = self.tasks.get(self.selected) {
                    let id = task.id.clone();
                    let command = match key.code {
                        KeyCode::Char('r') => Command::Control(id, Action::RunNow),
                        KeyCode::Char('e') => Command::Control(id, Action::Enable),
                        KeyCode::Char('p') => Command::Control(id, Action::Disable),
                        _ => Command::Delete(id),
                    };
                    self.send(command);
                }
            }
            _ => {}
        }
        false
    }
    fn request(&self) -> Result<Create> {
        let f = &self.fields;
        let spec = Spec {
            goal: f[0].clone(),
            timing: f[1].clone(),
            timezone: f[2].clone(),
            workspace: f[3].clone(),
            stance: f[4].clone(),
            max_seconds: f[5].parse()?,
            max_tokens: f[6].parse()?,
            max_iterations: f[7].parse()?,
        };
        rook_core::schedules::next(&spec.timing, &spec.timezone, rook_core::work::managed::now())?;
        Ok(Create { id: self.create_id.clone(), spec })
    }
    pub(super) fn draw(&mut self, f: &mut Frame, area: Rect) {
        let [body, editor, footer] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(if self.editing.is_some() { 5 } else { 0 }),
            Constraint::Length(4),
        ])
        .areas(area);
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(body);
        let items: Vec<_> = self
            .tasks
            .iter()
            .map(|t| {
                ListItem::new(format!(
                    "{} {}\n{}",
                    if t.enabled { "ON" } else { "OFF" },
                    t.spec.timing,
                    t.spec.goal.chars().take(80).collect::<String>()
                ))
            })
            .collect();
        if items.is_empty() {
            f.render_widget(
                Paragraph::new("No tasks yet.\nn — new scheduled task").block(bordered(" tasks ")),
                left,
            );
        } else {
            let mut selection = ListState::default().with_selected(Some(self.selected));
            f.render_stateful_widget(
                List::new(items)
                    .block(bordered(" schedules "))
                    .highlight_symbol("▌")
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                left,
                &mut selection,
            );
        }
        let mut lines = vec![];
        if let Some(t) = self.tasks.get(self.selected) {
            lines.extend([
                t.spec.goal.clone(),
                format!("{} · {}", t.spec.timing, t.spec.timezone),
                format!(
                    "Next: {}",
                    t.next_at
                        .filter(|_| t.enabled)
                        .map(|n| rook_core::schedules::display_time(n, &t.spec.timezone))
                        .unwrap_or_else(|| "—".into())
                ),
                t.note.clone(),
                format!("Workspace: {}", t.spec.workspace),
                format!("Permissions: {}", t.spec.stance),
                format!(
                    "Per run: {}s · {} tokens · {} iterations",
                    t.spec.max_seconds, t.spec.max_tokens, t.spec.max_iterations
                ),
                "".into(),
                "History · [ older / ] newer · Enter opens session".into(),
            ]);
            for (i, run) in t.history.iter().rev().enumerate() {
                lines.push(format!(
                    "{} {} · {}",
                    if i == self.history { "▸" } else { " " },
                    rook_core::schedules::display_time(run.at, &t.spec.timezone),
                    run.status
                ));
                if i == self.history {
                    lines.push(run.reason.clone());
                }
            }
        }
        f.render_widget(
            Paragraph::new(lines.join("\n"))
                .wrap(Wrap { trim: false })
                .block(bordered(" schedule and sessions ")),
            right,
        );
        if let Some(field) = self.editing {
            let title = format!(
                " new schedule · {} ({}/8) · Enter next/save · Shift+Tab back ",
                LABELS[field],
                field + 1
            );
            let (row, col) = self.draft.caret();
            let top = row.saturating_sub(editor.height.saturating_sub(3));
            let left = col.saturating_sub(editor.width.saturating_sub(4));
            f.render_widget(
                Paragraph::new(self.draft.as_str()).scroll((top, left)).block(bordered(&title)),
                editor,
            );
            f.set_cursor_position((editor.x + 1 + col - left, editor.y + 1 + row - top));
        }
        let hint = match self.editing {
            Some(1) => "once 2026-12-01 03:00 | every 30m | daily 09:00 | weekdays 09:00 | weekly fri 09:00",
            Some(2) => "IANA timezone, e.g. Europe/Moscow or UTC; Ctrl+U clears the field",
            Some(4) => "readonly | assist (asks for approval) | autonomous (deny list still applies)",
            Some(_) => "Enter keeps the value; Shift+Tab goes back; Esc cancels; Ctrl+U clears",
            None => {
                "n new · r run now · e enable · p disable · x cancel run · d delete · Enter session · Esc back"
            }
        };
        f.render_widget(Paragraph::new(format!("{}\n{hint}", self.note)).wrap(Wrap { trim: false }), footer);
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
        Ok(c) => c,
        Err(e) => {
            let _ = updates.send(Update::Error(e.to_string())).await;
            return;
        }
    };
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! { value = commands.recv() => { let Some(c) = value else { return }; Some(c) }, _ = tick.tick() => None };
        if let Ok(address) = std::fs::read_to_string(rook_core::paths::daemon_address_file()) {
            base = address.trim().into();
        }
        if let Some(command) = command {
            let (path, method, body, note) = match command {
                Command::Create(request) => (
                    "/api/tasks".into(),
                    reqwest::Method::POST,
                    serde_json::to_value(request).ok(),
                    "Schedule saved",
                ),
                Command::Control(id, action) => (
                    format!("/api/tasks/{id}/control"),
                    reqwest::Method::POST,
                    serde_json::to_value(action).ok(),
                    "Schedule updated",
                ),
                Command::Delete(id) => {
                    (format!("/api/tasks/{id}"), reqwest::Method::DELETE, None, "Schedule deleted")
                }
            };
            let result = request::<serde_json::Value>(&client, &base, &path, method, body).await;
            let update = match result {
                Ok(value) => {
                    if note == "Schedule saved" {
                        match serde_json::from_value::<Task>(value) {
                            Ok(task) => Update::Created(Box::new(task)),
                            Err(error) => Update::Error(error.to_string()),
                        }
                    } else {
                        Update::Saved(note.into())
                    }
                }
                Err(e) => Update::Error(format!("{e}; draft kept")),
            };
            if updates.send(update).await.is_err() {
                return;
            }
        }
        let update =
            match request::<Vec<Task>>(&client, &base, "/api/tasks", reqwest::Method::GET, None).await {
                Ok(tasks) => Update::Snapshot(tasks),
                Err(e) => Update::Error(format!("Connection unavailable: {e}")),
            };
        if updates.send(update).await.is_err() {
            return;
        }
    }
}
