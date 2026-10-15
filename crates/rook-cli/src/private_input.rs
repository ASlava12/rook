//! Private input never uses the chat composer, its undo stack or prompt history.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rook_proto::{SecretPrompt, SecretStatus};
use std::{
    io::{IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) struct Draft {
    pub prompt: SecretPrompt,
    value: String,
}

impl Draft {
    pub fn new(prompt: SecretPrompt) -> Self {
        Self { prompt, value: String::new() }
    }
    pub fn push(&mut self, text: &str) {
        if self.value.len().saturating_add(text.len()) <= 8192 && !text.chars().any(char::is_control) {
            self.value.push_str(text);
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Option<SecretStatus> {
        match key.code {
            KeyCode::Esc => Some(SecretStatus::Declined),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SecretStatus::Cancelled)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(SecretStatus::Skipped)
            }
            KeyCode::Enter => {
                Some(if self.value.is_empty() { SecretStatus::Skipped } else { SecretStatus::Saved })
            }
            KeyCode::Backspace => {
                self.value.pop();
                None
            }
            KeyCode::Char(ch) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                let mut bytes = [0; 4];
                self.push(ch.encode_utf8(&mut bytes));
                None
            }
            _ => None,
        }
    }
    pub fn value(&self, status: SecretStatus) -> Option<&str> {
        (status == SecretStatus::Saved).then_some(self.value.as_str())
    }
}

pub(crate) fn available() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Dropping the waiter stops terminal polling too; it never leaves a detached
/// thread consuming later prompts or saving a half-entered credential.
pub(crate) async fn read(prompt: SecretPrompt) -> (Draft, SecretStatus) {
    if !available() {
        return (Draft::new(prompt), SecretStatus::Unavailable);
    }
    static TERMINAL: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
        std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(1)));
    let Ok(permit) = TERMINAL.clone().acquire_owned().await else {
        return (Draft::new(prompt), SecretStatus::Unavailable);
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    let fallback = prompt.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut draft = Draft::new(prompt);
        let mut out = std::io::stdout();
        let _ = writeln!(
            out,
            "\nPrivate credential: {}\n{}\nValue is hidden. Enter saves; Esc declines; Ctrl-D skips.",
            draft.prompt.name, draft.prompt.reason
        );
        let _ = out.flush();
        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if crossterm::terminal::enable_raw_mode().is_err() {
            return (draft, SecretStatus::Unavailable);
        }
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                if !self.0 {
                    let _ = crossterm::terminal::disable_raw_mode();
                }
            }
        }
        let _restore = Restore(was_raw);
        let began = Instant::now();
        let status = loop {
            if cancelled.load(Ordering::Acquire) {
                break SecretStatus::Cancelled;
            }
            if began.elapsed() >= Duration::from_secs(300) {
                break SecretStatus::Expired;
            }
            match crossterm::event::poll(Duration::from_millis(100)) {
                Ok(false) => continue,
                Err(_) => break SecretStatus::Unavailable,
                Ok(true) => {}
            }
            match crossterm::event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if let Some(status) = draft.key(key) {
                        break status;
                    }
                }
                Ok(Event::Paste(text)) => draft.push(&text),
                Ok(_) => {}
                Err(_) => break SecretStatus::Unavailable,
            }
        };
        let _ = writeln!(out);
        (draft, status)
    })
    .await
    .unwrap_or_else(|_| (Draft::new(fallback), SecretStatus::Failed))
}

pub(crate) async fn remote(base: &str, prompt: SecretPrompt, interactive: bool) {
    let (draft, status) =
        if interactive { read(prompt).await } else { (Draft::new(prompt), SecretStatus::Skipped) };
    let status = match status {
        SecretStatus::Expired | SecretStatus::Unavailable | SecretStatus::Failed => SecretStatus::Cancelled,
        s => s,
    };
    let sent = submit_remote(base, &draft.prompt, status, draft.value(status)).await;
    if interactive {
        eprintln!(
            "private response: {}",
            if sent.is_some() { "delivered" } else { "not confirmed; inspect the request status" }
        );
    }
}

pub(crate) async fn submit_remote(
    base: &str,
    prompt: &SecretPrompt,
    status: SecretStatus,
    value: Option<&str>,
) -> Option<SecretStatus> {
    rook_llm::init_tls();
    let body = if let Some(value) = value {
        serde_json::json!({"status":status,"value":value})
    } else {
        serde_json::json!({"status":status})
    };
    let url = format!("{base}/api/sessions/{}/private-input/{}", prompt.session, prompt.id);
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?
        .post(url)
        .timeout(Duration::from_secs(15))
        .json(&body)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    // The daemon emits a tiny status-only response; bound it as it arrives.
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 1024 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    serde_json::from_value(body.get("status")?.clone()).ok()
}

/// Keep reading daemon lifecycle events while a person considers a request.
/// An expired prompt must release the terminal, including without any keypress.
pub(crate) struct RemoteRequests {
    base: String,
    interactive: bool,
    active: Option<(String, tokio::task::JoinHandle<()>)>,
    refusals: Vec<tokio::task::JoinHandle<()>>,
}
impl RemoteRequests {
    pub fn new(base: &str, interactive: bool) -> Self {
        Self { base: base.into(), interactive, active: None, refusals: Vec::new() }
    }
    pub fn observe(&mut self, event: &rook_proto::ChatEvent) -> bool {
        use rook_proto::ChatEvent;
        self.refusals.retain(|task| !task.is_finished());
        if self.active.as_ref().is_some_and(|(_, task)| task.is_finished()) {
            self.active = None;
        }
        match event {
            ChatEvent::SecretRequest { prompt } => {
                if self.active.as_ref().is_some_and(|(id, _)| id == &prompt.id) {
                    return true;
                }
                let base = self.base.clone();
                let prompt = prompt.clone();
                if self.active.is_none() {
                    let id = prompt.id.clone();
                    let interactive = self.interactive;
                    self.active = Some((
                        id,
                        tokio::spawn(async move {
                            remote(&base, prompt, interactive).await;
                        }),
                    ));
                } else if self.refusals.len() < 32 {
                    self.refusals.push(tokio::spawn(async move {
                        remote(&base, prompt, false).await;
                    }));
                }
                true
            }
            ChatEvent::Inputs { questions, .. } => {
                if self.active.as_ref().is_some_and(|(id, _)| !questions.contains(id)) {
                    self.close();
                }
                false
            }
            ChatEvent::Snapshot { .. }
            | ChatEvent::Done { .. }
            | ChatEvent::Failed { .. }
            | ChatEvent::Cancelled => {
                self.close();
                false
            }
            _ => false,
        }
    }
    fn close(&mut self) {
        if let Some((_, task)) = self.active.take() {
            task.abort();
        }
    }
}
impl Drop for RemoteRequests {
    fn drop(&mut self) {
        self.close();
        for task in &self.refusals {
            task.abort();
        }
    }
}
