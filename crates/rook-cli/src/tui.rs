//! The terminal UI: a conversation, and a browser over everything stored.
//!
//! The browsing tabs make the agent's memory legible — which sessions exist,
//! what a turn actually cost, which skills apply here and why — without needing
//! a database client. The chat tab runs turns against the same engine, with the
//! same permission policy, so nothing is reachable here that is not reachable
//! from the CLI or the web.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};

use rook_core::agent::{AgentLoop, Progress};
use rook_core::{SessionSummary, TranscriptEntry};
use rook_llm::Delta;
use rook_proto::{ApprovalDecision, ChatEvent, ClientMessage};
use rook_skills::SkillCard;
use rook_store::StoreStats;
use rook_tools::ask::{Answer, AskRequest, ChannelAsker, Question};
use rook_tools::policy::{Approval, ApprovalRequest, ChannelApprover};
use tokio::sync::mpsc;

use crate::fmt;

const TABS: [&str; 6] = ["Chat", "Sessions", "Memory", "Skills", "Store", "Help"];

/// How often the loop wakes to drain turn events when no key is pressed.
const TICK: Duration = Duration::from_millis(60);

/// Said where a slash command would have run. Turns go to the daemon holding
/// the store and come back over its socket, but these read and write this
/// process's store directly — `/undo` rewinds files, `/new` starts a session —
/// and there is no endpoint behind most of them.
const NOT_HERE: &str = "`rookd` holds the store, so the slash commands are not available in this \
                        window. Turns are: they run there and stream back here.";

pub fn run(source: crate::source::Source, yes: bool, started: Option<String>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let mut terminal = ratatui::init();
    // The wheel is how a person scrolls back through what an agent said, and
    // without capture the terminal scrolls its own buffer instead — which under
    // the alternate screen is empty. Selecting text with the mouse needs Shift
    // held while this is on, which is the usual bargain and is in the help.
    let mouse = execute!(std::io::stdout(), event::EnableMouseCapture).is_ok();
    let daemon = source.daemon_base().map(str::to_string);
    let result = App::new(source, runtime, yes).run(&mut terminal);
    if mouse {
        let _ = execute!(std::io::stdout(), event::DisableMouseCapture);
    }
    ratatui::restore();
    // Said on the way out rather than on the way in, where it would scroll past
    // before the screen is drawn: a daemon this window started outlives it, on
    // purpose — the next window wants it — and a background process nobody was
    // told about is one nobody thinks to stop.
    if let (Some(from), Some(base)) = (started, daemon) {
        println!("`rookd` is still running at {base} — every window shares it.");
        println!("Started from {from}; stop it with `pkill -f rookd` when you are done.");
    }
    result
}

/// What a running turn reports back to the drawing loop.
enum TurnEvent {
    Started(u128),
    Text(String),
    Reasoning(String),
    Tool(String),
    ToolDone(String, bool),
    Spent {
        input: u32,
        output: u32,
        cached: u32,
    },
    Approval(ApprovalRequest),
    Ask(AskRequest),
    Done(String),
    Error(String),
    /// A turn the daemon is running, verbatim. Translated where the rest are
    /// handled rather than at the socket, so the two paths meet in one place.
    FromDaemon(Box<ChatEvent>),
}

struct Selected {
    goal: Option<String>,
    forked_at: Option<u64>,
    changes: rook_core::changes::Changes,
}

/// The message being typed, and where in it the cursor is.
///
/// It was a `String` with `push` and `pop`, so the cursor was always at the
/// end: a typo in the middle of a long prompt cost everything after it, and
/// there was no way to run the last one again. Every terminal has had this
/// since the seventies.
#[derive(Default)]
struct Typing {
    text: String,
    /// A byte offset, always on a character boundary — the moves below are what
    /// keeps it there, and the text is whatever somebody typed, which includes
    /// their own language.
    at: usize,
}

impl Typing {
    fn insert(&mut self, c: char) {
        self.text.insert(self.at, c);
        self.at += c.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some(c) = self.text[..self.at].chars().next_back() {
            self.at -= c.len_utf8();
            self.text.remove(self.at);
        }
    }

    fn delete(&mut self) {
        if self.at < self.text.len() {
            self.text.remove(self.at);
        }
    }

    fn left(&mut self) {
        if let Some(c) = self.text[..self.at].chars().next_back() {
            self.at -= c.len_utf8();
        }
    }

    fn right(&mut self) {
        if let Some(c) = self.text[self.at..].chars().next() {
            self.at += c.len_utf8();
        }
    }

    fn home(&mut self) {
        self.at = 0;
    }

    fn end(&mut self) {
        self.at = self.text.len();
    }

    /// Back over the run of spaces before the cursor and then over the word,
    /// which is what every shell does with ctrl-w.
    fn kill_word(&mut self) {
        let before = &self.text[..self.at];
        let trimmed = before.trim_end();
        let cut = trimmed.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        self.text.replace_range(cut..self.at, "");
        self.at = cut;
    }

    fn kill_to_start(&mut self) {
        self.text.replace_range(..self.at, "");
        self.at = 0;
    }

    fn kill_to_end(&mut self) {
        self.text.truncate(self.at);
    }

    fn set(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.at = self.text.len();
    }

    fn clear(&mut self) {
        self.set("");
    }

    fn take(&mut self) -> String {
        let taken = std::mem::take(&mut self.text);
        self.at = 0;
        taken
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    /// How far along the line the cursor is drawn: characters, not bytes.
    fn column(&self) -> u16 {
        self.text[..self.at].chars().count() as u16
    }
}

#[derive(Default)]
struct Chat {
    input: Typing,
    /// Prompts already sent, oldest first, and where in them the up-arrow has
    /// walked. Kept here rather than in the store: it is what this window has
    /// typed, and a session's transcript is the record.
    history: Vec<String>,
    recalled: Option<usize>,
    log: Vec<(&'static str, String)>,
    session: Option<u128>,
    busy: bool,
    pending: Option<ApprovalRequest>,
    asking: Option<Asking>,
    /// Lines back from the newest, so zero is pinned to the bottom.
    scroll: u16,
    /// Lines the pane held when it was last drawn, so a reader who has scrolled
    /// up keeps their place as the turn writes below them.
    drawn: u16,
    /// Input, output and cached tokens so far in this turn.
    spent: Option<(u32, u32, u32)>,
    /// When the turn in flight started. A long turn showed a fixed `working…`
    /// and a stream that only moved when the model spoke, which reads as a
    /// hang — and was reported as one.
    since: Option<std::time::Instant>,
    /// Where to send what the person answers while the daemon runs the turn:
    /// approvals, answers and a cancellation. `None` when the turn is this
    /// process's own.
    remote: Option<mpsc::UnboundedSender<ClientMessage>>,
}

/// Reasoning, which is read rather than glanced at.
///
/// It was the same `DarkGray` as the footer and the hints, and on a dark
/// terminal a page of it is genuinely hard to read — a turn that thinks out
/// loud fills the pane with it. Brighter than the chrome and still cooler than
/// an answer, because it is what the model is working through and not what it
/// is telling you.
const THINKING: Color = Color::Rgb(150, 156, 168);

/// A model's answer, styled the little that Markdown is actually used for.
///
/// The same subset the browser draws — fenced code, headings, bullets, inline
/// code and bold — because it is the same answer, and somebody moving between
/// the two should not meet two renderings of it. Everything else is left as
/// the text it is: a half-written table is more readable than a guess at one.
fn answer(body: &str, base: Style) -> Vec<Line<'static>> {
    let code = Style::default().fg(Color::Rgb(152, 195, 121));
    let mut lines = Vec::new();
    let mut fenced = false;
    for raw in body.split('\n') {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            // The fence itself is the marker, dimmed: dropping it as the
            // browser does leaves a block of code with no edge in a pane that
            // is all text.
            fenced = !fenced;
            lines.push(Line::from(Span::styled(raw.to_string(), Style::default().fg(Color::DarkGray))));
            continue;
        }
        if fenced {
            lines.push(Line::from(Span::styled(raw.to_string(), code)));
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            let text = heading.trim_start_matches('#').trim_start();
            if !text.is_empty() && heading.starts_with(['#', ' ']) {
                lines.push(Line::from(Span::styled(text.to_string(), base.add_modifier(Modifier::BOLD))));
                continue;
            }
        }
        lines.push(Line::from(inline(raw, base, code)));
    }
    lines
}

/// `code` and **bold** inside one line, in that order of precedence: a `*`
/// inside a span of code is code, which is how it is meant and how the
/// browser reads it.
fn inline(text: &str, base: Style, code: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(['`', '*']) {
        let (marker, closing) = match rest[at..].starts_with("**") {
            true => ("**", "**"),
            false if rest[at..].starts_with('`') => ("`", "`"),
            // A single `*` is a bullet or a multiplication sign, not an
            // emphasis this bothers with.
            false => {
                plain.push_str(&rest[..at + 1]);
                rest = &rest[at + 1..];
                continue;
            }
        };
        let after = at + marker.len();
        let Some(end) = rest[after..].find(closing).map(|e| after + e) else {
            break;
        };
        plain.push_str(&rest[..at]);
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut plain), base));
        }
        let inside = rest[after..end].to_string();
        spans.push(match marker {
            "`" => Span::styled(inside, code),
            _ => Span::styled(inside, base.add_modifier(Modifier::BOLD)),
        });
        rest = &rest[end + closing.len()..];
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// `12s`, `4m10s` — the two units a person waiting reads, and no more.
fn elapsed(since: std::time::Duration) -> String {
    let seconds = since.as_secs();
    match seconds < 60 {
        true => format!("{seconds}s"),
        false => format!("{}m{:02}s", seconds / 60, seconds % 60),
    }
}

/// Short enough for the footer, which already carries the mode, the effort and
/// whatever the last command said.
fn spent(totals: Option<(u32, u32, u32)>) -> String {
    let Some((input, output, cached)) = totals else { return String::new() };
    let cached = match cached {
        0 => String::new(),
        n => format!(" ({} cached)", thousands(n)),
    };
    format!("{} in / {} out{cached}", thousands(input), thousands(output))
}

/// The tail of a path, which is what tells two projects apart in a narrow list.
fn elsewhere(workspace: &str) -> String {
    std::path::Path::new(workspace)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.to_string())
}

fn thousands(n: u32) -> String {
    match n {
        0..=999 => n.to_string(),
        _ => format!("{:.1}k", n as f64 / 1000.0),
    }
}

/// One batch of questions, answered one at a time through the input line so the
/// TUI needs no second editor.
fn display(chosen: &[String]) -> String {
    if chosen.is_empty() { "(skipped)".into() } else { chosen.join(", ") }
}

struct Asking {
    id: String,
    questions: Vec<Question>,
    at: usize,
    chosen: Vec<Vec<String>>,
}

impl Asking {
    fn current(&self) -> &Question {
        &self.questions[self.at]
    }

    /// Takes one typed answer and moves on. Complete once it has taken one for
    /// every question, which is when the batch goes back to the agent.
    fn record(&mut self, typed: &str) -> Answer {
        let answer = self.current().interpret(typed);
        self.chosen.push(answer.chosen.clone());
        self.at += 1;
        answer
    }

    fn complete(&self) -> bool {
        self.at >= self.questions.len()
    }

    fn title(&self) -> String {
        format!(" question {} of {} ", self.at + 1, self.questions.len())
    }

    /// The panel's height comes from these lines, so it cannot disagree with
    /// what it is showing.
    fn panel(&self) -> Vec<Line<'static>> {
        let q = self.current();
        let mut lines = vec![Line::from(Span::styled(q.question.clone(), Style::default().fg(Color::Cyan)))];
        for (i, choice) in q.choices.iter().enumerate() {
            let recommended = if i == 0 && !q.multi { "  (recommended)" } else { "" };
            lines.push(Line::from(format!("  {}. {choice}{recommended}", i + 1)));
        }
        lines.push(Line::from(Span::styled(q.ask_line().to_string(), Style::default().fg(Color::DarkGray))));
        lines
    }
}

impl Chat {
    /// Append, merging consecutive pieces of a stream so a streamed reply is
    /// one paragraph rather than one line per token.
    ///
    /// Both kinds that stream, not just the answer: reasoning arrives token by
    /// token as well, and unmerged it rendered a word to a line with a blank
    /// line between — a page of `I` / `'ll` / `just` / `do`, which is what a
    /// model thinking out loud looked like in the terminal.
    fn push(&mut self, kind: &'static str, text: &str) {
        let streams = matches!(kind, "text" | "think");
        match self.log.last_mut() {
            Some((last, body)) if *last == kind && streams => body.push_str(text),
            _ => self.log.push((kind, text.to_string())),
        }
        self.trim();
    }

    /// Scrollback, not the record: the session holds every word of this and the
    /// Sessions tab reads it back, so an afternoon of turns need not sit in
    /// memory to stay recoverable.
    fn trim(&mut self) {
        let mut total: usize = self.log.iter().map(|(_, body)| body.len()).sum();
        while total > MAX_SCROLLBACK && self.log.len() > 1 {
            total -= self.log.remove(0).1.len();
        }
    }
}

/// About a screenful of history a thousand times over. Past this the older part
/// is in the store and nowhere else, which is where it was going anyway.
const MAX_SCROLLBACK: usize = 1 << 20;

struct App {
    /// Where the browsing tabs read from: this process's own store, or a
    /// running `rookd` holding it. A second window on a second project is the
    /// ordinary case, and it used to be an error message.
    source: crate::source::Source,
    runtime: tokio::runtime::Runtime,
    chat: Chat,
    events: mpsc::UnboundedReceiver<TurnEvent>,
    to_loop: mpsc::UnboundedSender<TurnEvent>,
    approver: Arc<ChannelApprover>,
    asker: Arc<ChannelAsker>,
    /// The same state the chat REPL keeps, so the slash commands are one
    /// implementation rather than two that drift.
    shared: crate::chat::Session,
    turn: Option<tokio::task::JoinHandle<()>>,
    tab: usize,
    sessions: Vec<SessionSummary>,
    session_state: ListState,
    transcript: Vec<TranscriptEntry>,
    /// What the selected session was for and what it did, which is usually why
    /// its transcript is being read at all.
    selected: Option<Selected>,
    transcript_scroll: u16,
    facts: Vec<rook_core::Fact>,
    skills: Vec<SkillCard>,
    skill_state: ListState,
    objects: Vec<(String, String, u64, u64)>,
    stats: Option<StoreStats>,
    status: String,
    quit: bool,
}

impl App {
    fn new(source: crate::source::Source, runtime: tokio::runtime::Runtime, yes: bool) -> Self {
        let (to_loop, events) = mpsc::unbounded_channel();
        let (requests, mut incoming) = mpsc::unbounded_channel::<ApprovalRequest>();

        let relay = to_loop.clone();
        runtime.spawn(async move {
            while let Some(request) = incoming.recv().await {
                if relay.send(TurnEvent::Approval(request)).is_err() {
                    break;
                }
            }
        });

        let (questions, mut asked) = mpsc::unbounded_channel::<AskRequest>();
        let relay = to_loop.clone();
        runtime.spawn(async move {
            while let Some(request) = asked.recv().await {
                if relay.send(TurnEvent::Ask(request)).is_err() {
                    break;
                }
            }
        });

        // The configuration is a file, so a window with no store of its own
        // still knows the stance, the effort and the servers it would use.
        let config = source.here().map(|rook| rook.config.clone()).unwrap_or_default();
        let workspace = source.workspace().to_path_buf();
        // Connected only where turns run here: a routed window's tools are the
        // daemon's, and spawning a second copy of every server to leave them
        // idle is a cost with nothing on the other side of it.
        let mcp = match source.here() {
            Some(rook) => Arc::new(runtime.block_on(rook.connect_mcp())),
            None => Arc::new(rook_core::McpSession::default()),
        };
        let patience = config.agent.answer_timeout();

        let mut app = Self {
            runtime,
            chat: Chat::default(),
            events,
            to_loop,
            approver: Arc::new(ChannelApprover::new(requests, patience)),
            asker: Arc::new(ChannelAsker::new(questions, patience)),
            shared: crate::chat::Session {
                policy: rook_core::agent::policy_for(&config),
                effort: std::cell::Cell::new(config.agent.effort()),
                servers: rook_core::agent::servers_for(&config, &workspace),
                jobs: rook_core::agent::jobs_for(&config),
                interjections: Default::default(),
                // Connected once: every turn would otherwise spawn each server,
                // wait out its handshake and kill it again.
                mcp,
                yes,
            },
            source,
            turn: None,
            tab: 0,
            sessions: Vec::new(),
            session_state: ListState::default(),
            transcript: Vec::new(),
            selected: None,
            transcript_scroll: 0,
            facts: Vec::new(),
            skills: Vec::new(),
            skill_state: ListState::default(),
            objects: Vec::new(),
            stats: None,
            status: String::new(),
            quit: false,
        };
        app.reload();
        app
    }

    fn reload(&mut self) {
        let workspace = self.source.workspace().to_path_buf();
        self.sessions = self.source.sessions().unwrap_or_default();
        self.skills = self.source.catalog(&workspace).unwrap_or_default();
        let here = workspace.display().to_string();
        self.facts = self
            .source
            .memory()
            .map(|facts| facts.into_iter().filter(|f| f.scope.applies_in(&here)).collect())
            .unwrap_or_default();
        self.stats = self.source.stats().ok();
        self.objects = self
            .source
            .objects(None, 300)
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.short, row.kind, row.size_raw, row.size_stored))
            .collect();
        if self.session_state.selected().is_none() && !self.sessions.is_empty() {
            self.session_state.select(Some(0));
        }
        if self.skill_state.selected().is_none() && !self.skills.is_empty() {
            self.skill_state.select(Some(0));
        }
        self.load_transcript();
        self.status = format!(
            "{} sessions · {} skills · {} on disk",
            self.sessions.len(),
            self.skills.len(),
            self.stats.as_ref().map(|s| fmt::bytes(s.disk_bytes())).unwrap_or_default()
        );
    }

    fn load_transcript(&mut self) {
        self.transcript.clear();
        self.selected = None;
        self.transcript_scroll = 0;
        let Some(i) = self.session_state.selected() else { return };
        let Some(session) = self.sessions.get(i) else { return };
        // Bounded on purpose: viewing a session with a huge tool result must not
        // itself become the memory problem.
        self.transcript = self.source.transcript(session.meta.id, 0, 500, 4_000).unwrap_or_default();
        self.selected = Some(Selected {
            goal: session.goal.clone(),
            forked_at: session.forked_at,
            // No diffs: the header is a summary, and a session that rewrote a
            // large file would take the pane over.
            changes: self.source.changes(session.meta.id, false).unwrap_or_default(),
        });
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            self.drain_turn_events();
            // Poll rather than block: a streaming turn has to keep redrawing
            // even while nobody is typing.
            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                    Event::Mouse(mouse) => self.on_scroll(mouse.kind),
                    _ => {}
                }
            }
        }
        if let Some(turn) = self.turn.take() {
            turn.abort();
        }
        Ok(())
    }

    /// Aborting drops the loop's future, which is how the chat REPL cancels
    /// too. Whatever it had already logged stays in the session, so a stopped
    /// turn is still readable — and the note says why it ends where it does.
    fn stop(&mut self, turn: tokio::task::JoinHandle<()>) {
        turn.abort();
        // Only where the store is here: a routed window does not run the turn
        // it is stopping, and the daemon writes its own note.
        if let (Some(session), Some(rook)) = (self.chat.session, self.source.here()) {
            rook.log(session, rook_store::EventKind::Note, "interrupted", "the user stopped this turn").ok();
        }
        self.chat.push("stat", "[stopped]");
        self.chat.busy = false;
        self.status = "turn stopped".into();
    }

    fn drain_turn_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                TurnEvent::Started(id) => self.chat.session = Some(id),
                TurnEvent::Text(text) => self.chat.push("text", &text),
                TurnEvent::Reasoning(text) => self.chat.push("think", &text),
                TurnEvent::Tool(name) => self.chat.push("tool", &format!("  · {name}")),
                TurnEvent::ToolDone(name, failed) => {
                    self.chat.push("tool", &format!("  · {name} {}", if failed { "✗" } else { "✓" }))
                }
                TurnEvent::Spent { input, output, cached } => self.chat.spent = Some((input, output, cached)),
                TurnEvent::Approval(request) => self.chat.pending = Some(request),
                TurnEvent::Ask(request) => {
                    self.chat.asking = Some(Asking {
                        id: request.id,
                        questions: request.questions,
                        at: 0,
                        chosen: Vec::new(),
                    })
                }
                TurnEvent::Done(note) => {
                    self.chat.push("stat", &note);
                    self.chat.busy = false;
                    self.reload();
                }
                TurnEvent::Error(message) => {
                    self.chat.push("err", &message);
                    self.chat.busy = false;
                }
                TurnEvent::FromDaemon(event) => self.heard_from_daemon(*event),
            }
        }
    }

    /// What the daemon streams, in the words this window already draws.
    ///
    /// One arm per event rather than a translation into `TurnEvent`: the two
    /// sets are close but not the same, and a mapping that pretended otherwise
    /// would have to invent a `Started` for a session id that arrives as text.
    fn heard_from_daemon(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Started { session } => {
                self.chat.session = rook_store::parse_session_id(&session);
            }
            ChatEvent::Text { text } => self.chat.push("text", &text),
            ChatEvent::Reasoning { text } => self.chat.push("think", &text),
            ChatEvent::Tool { name } => self.chat.push("tool", &format!("  · {name}")),
            ChatEvent::ToolDone { name, failed } => {
                self.chat.push("tool", &format!("  · {name} {}", if failed { "✗" } else { "✓" }))
            }
            ChatEvent::Remembered { text } => self.chat.push("stat", &format!("  remembered: {text}")),
            ChatEvent::Forgot { text } => self.chat.push("stat", &format!("  forgot: {text}")),
            ChatEvent::Spent { input_tokens, output_tokens, cached_tokens } => {
                self.chat.spent = Some((input_tokens, output_tokens, cached_tokens))
            }
            ChatEvent::Interjected { text } => self.chat.push("stat", &format!("  ↩ {text}")),
            ChatEvent::Approval { id, tool, action, preview, kind } => {
                self.chat.pending = Some(ApprovalRequest { id, tool, action, preview, kind })
            }
            ChatEvent::Ask { id, questions } => {
                self.chat.asking = Some(Asking {
                    id,
                    questions: questions
                        .into_iter()
                        .map(|q| rook_tools::ask::Question {
                            question: q.question,
                            choices: q.choices,
                            multi: q.multi,
                        })
                        .collect(),
                    at: 0,
                    chosen: Vec::new(),
                })
            }
            // Said only when it disagrees. A connection reports its settings
            // when it opens and again for each one this window sets, so the
            // handshake printed `stance: assist · effort: high` three times
            // before the turn had started — noise where a difference would
            // have been news.
            ChatEvent::Settings { mode, effort, .. } => {
                let ours = (self.shared.policy.stance().as_str(), self.shared.effort.get().as_str());
                if (mode.as_str(), effort.as_str()) != ours {
                    self.chat.push(
                        "stat",
                        &format!("  the daemon is running at {mode} · {effort}, not {} · {}", ours.0, ours.1),
                    );
                }
            }
            ChatEvent::Cancelled => {
                self.chat.push("stat", "[stopped]");
                self.finished();
            }
            ChatEvent::Done { steps, input_tokens, output_tokens, .. } => {
                self.chat
                    .push("stat", &format!("  {steps} step(s), {input_tokens} in / {output_tokens} out"));
                self.finished();
            }
            ChatEvent::Error { message } => {
                self.chat.push("err", &message);
                self.finished();
            }
        }
    }

    /// A remote turn is over: the window stops waiting and forgets where to
    /// send answers, so a key pressed afterwards is not sent into a turn that
    /// has ended.
    fn finished(&mut self) {
        self.chat.busy = false;
        self.chat.remote = None;
        self.reload();
    }

    fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            // A running turn is what there is to stop, as in the chat REPL and
            // the browser; quitting is what it means when there is nothing.
            // A turn the daemon runs is stopped by asking it: dropping the
            // socket here would leave the turn running with nobody reading it.
            if let Some(say) = self.chat.remote.take() {
                let _ = say.send(ClientMessage::Cancel);
                self.chat.push("stat", "[stopping]");
                return;
            }
            match self.turn.take_if(|turn| !turn.is_finished()) {
                Some(turn) => self.stop(turn),
                None => self.quit = true,
            }
            return;
        }
        // Before the per-tab dispatch: the chat tab is where you would want to
        // drop to read-only, and there a digit is a character in the message.
        match key.code {
            KeyCode::F(2) => return self.cycle_stance(),
            KeyCode::F(3) => return self.cycle_effort(),
            _ => {}
        }
        if self.tab == 0 {
            self.on_chat_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Right => self.tab = (self.tab + 1) % TABS.len(),
            KeyCode::BackTab | KeyCode::Left => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Char(c @ '1'..='6') => self.tab = c as usize - '1' as usize,
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.transcript_scroll = self.transcript_scroll.saturating_add(20)
            }
            KeyCode::PageUp => self.transcript_scroll = self.transcript_scroll.saturating_sub(20),
            _ => {}
        }
    }

    fn on_chat_key(&mut self, key: crossterm::event::KeyEvent) {
        // The line-editing keys every terminal has had for fifty years. Before
        // the approval, because they are about the box being typed in and an
        // approval is answered with a letter.
        if key.modifiers == KeyModifiers::CONTROL && self.chat.pending.is_none() {
            match key.code {
                // Some terminals send BS for the backspace key, which arrives
                // here as ctrl-h; unhandled it typed an `h` into the message.
                KeyCode::Char('h') => return self.chat.input.backspace(),
                KeyCode::Char('a') => return self.chat.input.home(),
                KeyCode::Char('e') => return self.chat.input.end(),
                KeyCode::Char('w') => return self.chat.input.kill_word(),
                KeyCode::Char('u') => return self.chat.input.kill_to_start(),
                KeyCode::Char('k') => return self.chat.input.kill_to_end(),
                _ => {}
            }
        }
        // An approval blocks the turn, so it takes the keyboard until answered.
        if let Some(request) = self.chat.pending.clone() {
            let approval = match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Approval::Once,
                KeyCode::Char('a') => Approval::ForRun,
                KeyCode::Char('k') => Approval::KindForRun,
                KeyCode::Char('n') | KeyCode::Esc => Approval::declined(),
                _ => return,
            };
            self.chat.push("stat", &format!("  {} → {}", request.action, approval.describe()));
            // To whoever asked: the loop in this process, or the daemon running
            // the turn for this window.
            match &self.chat.remote {
                Some(say) => {
                    let decision = match approval {
                        Approval::Once => ApprovalDecision::Once,
                        Approval::ForRun => ApprovalDecision::ForRun,
                        Approval::KindForRun => ApprovalDecision::KindForRun,
                        Approval::Deny(_) | Approval::Unanswered(_) => ApprovalDecision::Deny,
                    };
                    let _ = say.send(ClientMessage::Approval { id: request.id.clone(), decision });
                }
                None => self.approver.answer(&request.id, approval),
            }
            self.chat.pending = None;
            return;
        }

        match key.code {
            // A half-typed command is what Tab is for while one is being
            // typed; the tabs are still a Tab away from anything else.
            KeyCode::Tab if self.chat.input.as_str().starts_with('/') => self.complete(),
            KeyCode::Tab => self.tab = (self.tab + 1) % TABS.len(),
            KeyCode::BackTab => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            KeyCode::Esc if self.chat.input.is_empty() => self.quit = true,
            KeyCode::Esc => self.chat.input.clear(),
            KeyCode::Left => self.chat.input.left(),
            KeyCode::Right => self.chat.input.right(),
            KeyCode::Home => self.chat.input.home(),
            // With nothing typed there is no line to walk to the end of, and
            // `End` is where a hand goes to get back to the newest message.
            KeyCode::End if self.chat.input.is_empty() => self.chat.scroll = 0,
            KeyCode::End => self.chat.input.end(),
            KeyCode::Delete => self.chat.input.delete(),
            KeyCode::Up => self.recall(-1),
            KeyCode::Down => self.recall(1),
            KeyCode::Enter if self.chat.asking.is_some() => self.answer(),
            KeyCode::Enter => self.send(),
            KeyCode::Backspace => self.chat.input.backspace(),
            KeyCode::PageUp => self.chat.scroll = self.chat.scroll.saturating_sub(10),
            KeyCode::PageDown => self.chat.scroll = self.chat.scroll.saturating_add(10),
            KeyCode::Char(c) => self.chat.input.insert(c),
            _ => {}
        }
    }

    /// The previous prompt, and the one before it. Walking off the end brings
    /// back the empty line rather than sticking on the newest, which is what a
    /// shell does and what a hand expects.
    fn recall(&mut self, by: i32) {
        if self.chat.history.is_empty() {
            return;
        }
        let last = self.chat.history.len() - 1;
        self.chat.recalled = match (self.chat.recalled, by) {
            (None, -1) => Some(last),
            (None, _) => None,
            (Some(0), -1) => Some(0),
            (Some(at), -1) => Some(at - 1),
            (Some(at), _) if at >= last => None,
            (Some(at), _) => Some(at + 1),
        };
        match self.chat.recalled {
            Some(at) => self.chat.input.set(&self.chat.history[at].clone()),
            None => self.chat.input.clear(),
        }
    }

    /// A setting changed here, while the daemon is running the turn it applies
    /// to. Between turns there is nothing to tell: the next prompt carries them.
    fn tell_the_daemon(&self, name: &str, value: &str) {
        if let Some(say) = &self.chat.remote {
            let _ = say.send(ClientMessage::Setting { name: name.into(), value: value.into() });
        }
    }

    /// Hand the prompt to the daemon holding the store, and stream what it
    /// does back into the same events a local turn sends.
    ///
    /// The socket is the browser's, which is the point: one engine, and the
    /// second window is another client of it rather than a second copy that
    /// cannot exist.
    fn send_to_daemon(&mut self, prompt: String) {
        let Some(base) = self.source.daemon_base().map(|b| b.to_string()) else {
            self.chat.busy = false;
            return self.chat.push("err", "no daemon to run this");
        };
        let (say, mut outgoing) = mpsc::unbounded_channel::<ClientMessage>();
        let (heard, mut incoming) = mpsc::unbounded_channel::<ChatEvent>();
        let workspace = self.source.workspace().to_path_buf();

        // The settings this window is showing, said before the prompt: a
        // connection starts at the daemon's own and a stance cycled here would
        // otherwise be a lie the footer keeps telling.
        let _ = say.send(ClientMessage::Setting {
            name: "mode".into(),
            value: self.shared.policy.stance().as_str().to_string(),
        });
        let _ = say.send(ClientMessage::Setting {
            name: "effort".into(),
            value: self.shared.effort.get().as_str().to_string(),
        });
        let _ = say.send(ClientMessage::Prompt {
            session: self.chat.session.map(rook_store::format_session_id),
            text: prompt,
        });
        self.chat.remote = Some(say);

        let to_loop = self.to_loop.clone();
        let relay = to_loop.clone();
        self.runtime.spawn(async move {
            while let Some(event) = incoming.recv().await {
                if relay.send(TurnEvent::FromDaemon(Box::new(event))).is_err() {
                    break;
                }
            }
        });
        self.turn = Some(self.runtime.spawn(async move {
            if let Err(e) = crate::remote::hold(&base, &workspace, &mut outgoing, heard).await {
                fail(&to_loop, e.to_string());
            }
        }));
    }

    /// Finish the command being typed, as far as it is unambiguous.
    ///
    /// One match completes to the name and, where it takes arguments, a space
    /// to type them after. Several complete to what they share, which is how a
    /// person discovers `/se` is two commands rather than a typo.
    fn complete(&mut self) {
        let matches = crate::chat::commands_matching(self.chat.input.as_str());
        let Some((first, args, _)) = matches.first() else { return };
        let common = matches.iter().skip(1).fold(first.to_string(), |common, (name, ..)| {
            common.chars().zip(name.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
        });
        self.chat.input.set(&match matches.len() {
            1 if args.is_empty() => format!("/{first}"),
            1 => format!("/{first} "),
            _ => format!("/{common}"),
        });
    }

    /// The wheel, on whichever pane the tab is showing. Three lines a notch,
    /// which is what a terminal sends and what every other reader does with it.
    ///
    /// The two panes count their scroll from opposite ends: the chat is pinned
    /// to the newest line, so its offset is lines back from there, and a
    /// transcript is read from the top.
    fn on_scroll(&mut self, wheel: MouseEventKind) {
        const BY: u16 = 3;
        match (self.tab, wheel) {
            (0, MouseEventKind::ScrollUp) => self.chat.scroll = self.chat.scroll.saturating_add(BY),
            (0, MouseEventKind::ScrollDown) => self.chat.scroll = self.chat.scroll.saturating_sub(BY),
            (1, MouseEventKind::ScrollUp) => {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(BY)
            }
            (1, MouseEventKind::ScrollDown) => {
                self.transcript_scroll = self.transcript_scroll.saturating_add(BY)
            }
            _ => {}
        }
    }

    /// One question per Enter. The input line is the answer field, so typing
    /// past the choices works here exactly as it does in the plain CLI.
    fn command(&mut self, command: &str) {
        // The slash commands read and write this process's store directly, so a
        // window reading through a daemon says so rather than half-working.
        let Some(rook) = self.source.here().cloned() else {
            return self.chat.push("stat", &format!("  {NOT_HERE}"));
        };
        let mut session = self.chat.session.unwrap_or_else(|| rook.start_session("").unwrap_or_default());
        let said = self.runtime.block_on(crate::chat::dispatch(&rook, &mut session, &self.shared, command));
        self.chat.session = Some(session);
        match said {
            Ok(said) if said.quit => self.quit = true,
            Ok(said) => {
                for line in said.text.lines() {
                    self.chat.push("stat", line);
                }
                self.reload();
            }
            Err(e) => self.chat.push("err", &format!("{e}")),
        }
    }

    /// Through the stances in the order they are declared, so the key walks
    /// from least latitude to most and wraps — rather than an order of its own,
    /// which is a second list of the same thing.
    fn cycle_stance(&mut self) {
        use rook_tools::policy::Stance;
        let now = self.shared.policy.stance();
        let at = Stance::ALL.iter().position(|s| *s == now).unwrap_or(0);
        let next = Stance::ALL[(at + 1) % Stance::ALL.len()];
        self.shared.policy.set_stance(next);
        self.tell_the_daemon("mode", next.as_str());
        self.chat.push("stat", &format!("  stance: {}", next.as_str()));
    }

    fn cycle_effort(&mut self) {
        use rook_llm::Effort::*;
        let next = match self.shared.effort.get() {
            Low => Medium,
            Medium => High,
            High => XHigh,
            XHigh => Max,
            Max => Low,
        };
        self.shared.effort.set(next);
        self.tell_the_daemon("effort", next.as_str());
        self.chat.push("stat", &format!("  effort: {}", next.as_str()));
    }

    fn answer(&mut self) {
        let Some(mut asking) = self.chat.asking.take() else { return };
        let answer = asking.record(&self.chat.input.take());
        self.chat.push("stat", &format!("  {} → {}", answer.question, display(&answer.chosen)));
        match (asking.complete(), &self.chat.remote) {
            (false, _) => self.chat.asking = Some(asking),
            (true, Some(say)) => {
                let answers = asking.chosen.clone();
                let _ = say.send(ClientMessage::Answers { id: asking.id.clone(), answers });
            }
            (true, None) => self.asker.answer(&asking.id, asking.chosen),
        }
    }

    fn send(&mut self) {
        let prompt = self.chat.input.take().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        // Kept whatever happens to it next — a refused turn is the one most
        // worth having back — and never twice in a row, which is the only
        // duplicate a history walk trips over.
        if self.chat.history.last() != Some(&prompt) {
            self.chat.history.push(prompt.clone());
        }
        self.chat.recalled = None;
        // Typed while a turn runs, it goes to the turn. It used to be dropped
        // where it was taken, so watching one go the wrong way left nothing to
        // do but stop it and start again.
        if self.chat.busy {
            self.shared.interjections.say(&prompt);
            self.chat.push("you", &format!("› {prompt}"));
            self.chat.push("stat", "  (the turn will see this at its next step)");
            self.chat.scroll = 0;
            return;
        }
        // `/btw` is a turn without tools, so it goes down the normal path; every
        // other slash command is answered here, by the same code the plain CLI
        // runs.
        if let Some(command) = prompt.strip_prefix('/').filter(|c| !c.starts_with("btw ")) {
            self.chat.push("you", &format!("› {prompt}"));
            return self.command(command);
        }
        let aside = prompt.strip_prefix("/btw ").map(|q| q.trim().to_string());
        self.chat.push("you", &format!("› {prompt}"));
        self.chat.busy = true;
        self.chat.since = Some(std::time::Instant::now());
        self.chat.scroll = 0;

        let Some(rook) = self.source.here().cloned() else {
            return self.send_to_daemon(prompt);
        };
        let to_loop = self.to_loop.clone();
        let approver = self.approver.clone();
        let asker = self.asker.clone();
        let policy = self.shared.policy.clone();
        let effort = self.shared.effort.get();
        let servers = self.shared.servers.clone();
        let mcp = self.shared.mcp.clone();
        let jobs = self.shared.jobs.clone();
        let interjections = self.shared.interjections.clone();
        let session = self.chat.session;
        let yes = self.shared.yes;

        self.turn = Some(self.runtime.spawn(async move {
            let session = match session {
                Some(id) => id,
                None => match rook.start_session("") {
                    Ok(id) => {
                        let _ = to_loop.send(TurnEvent::Started(id));
                        id
                    }
                    Err(e) => return fail(&to_loop, e.to_string()),
                },
            };

            let provider = match rook_llm::from_spec_with(
                &rook.config.agent.model,
                rook.config.agent.stream_idle(),
                rook.config.agent.context_window,
            ) {
                Ok(provider) => provider,
                Err(e) => return fail(&to_loop, e.to_string()),
            };

            let mut agent = AgentLoop::new(&rook, provider.into(), session);
            if let Some(question) = aside {
                let emit = to_loop.clone();
                let result = agent
                    .aside(&question, |delta| {
                        if let Delta::Text(text) = delta {
                            let _ = emit.send(TurnEvent::Reasoning(text.clone()));
                        }
                    })
                    .await;
                let _ = match result {
                    Ok(_) => to_loop.send(TurnEvent::Done("[aside]".into())),
                    Err(e) => to_loop.send(TurnEvent::Error(e.to_string())),
                };
                return;
            }
            agent.effort = effort;
            if yes {
                agent.allow_everything_not_denied();
            } else {
                agent.policy = policy;
                agent.approver = approver;
                agent.ask_via(asker);
            }
            agent.interjections = interjections;
            rook_core::agent::equip(&mut agent, servers, &mcp, jobs);

            let emit = to_loop.clone();
            let result = agent
                .run_with(&prompt, |progress| {
                    let event = match progress {
                        Progress::Delta(Delta::Text(text)) => TurnEvent::Text(text.clone()),
                        Progress::Delta(Delta::Reasoning(text)) => TurnEvent::Reasoning(text.clone()),
                        Progress::Delta(Delta::ToolCall(call)) => TurnEvent::Tool(call.name.clone()),
                        Progress::Delegated { task, done, total } => {
                            TurnEvent::Reasoning(format!("\n  [{done}/{total}] {task}"))
                        }
                        Progress::Delegating { task, tool } => {
                            TurnEvent::Reasoning(format!("\n    {task}: {tool}"))
                        }
                        Progress::ToolDone { name, failed } => TurnEvent::ToolDone(name.to_string(), failed),
                        Progress::Spent { input, output, cached } => {
                            TurnEvent::Spent { input, output, cached }
                        }
                        Progress::Delta(Delta::Done { .. } | Delta::ReasoningDone(_)) => return,
                    };
                    let _ = emit.send(event);
                })
                .await;

            let _ = match result {
                Ok(outcome) => to_loop.send(TurnEvent::Done(format!(
                    "{}[{} steps · {} in / {} out{}]",
                    outcome.memory_note().map(|n| format!("{n}\n")).unwrap_or_default(),
                    outcome.steps,
                    outcome.input_tokens,
                    outcome.output_tokens,
                    if outcome.delegated.is_empty() {
                        String::new()
                    } else {
                        format!(" · {} sub-agent(s)", outcome.delegated.len())
                    }
                ))),
                Err(e) => to_loop.send(TurnEvent::Error(e.to_string())),
            };
        }));
    }

    fn move_selection(&mut self, delta: isize) {
        let (state, len) = match self.tab {
            1 => (&mut self.session_state, self.sessions.len()),
            3 => (&mut self.skill_state, self.skills.len()),
            _ => {
                self.transcript_scroll = self.transcript_scroll.saturating_add_signed(delta as i16 * 3);
                return;
            }
        };
        if len == 0 {
            return;
        }
        let current = state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        state.select(Some(next));
        if self.tab == 1 {
            self.load_transcript();
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)])
                .areas(f.area());

        let tabs = Tabs::new(TABS.iter().map(|t| Span::raw(*t)).collect::<Vec<_>>())
            .select(self.tab)
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(format!(
                " rook {} — {}{} ",
                rook_core::AGENT_VERSION,
                self.source.workspace().display(),
                // Whether this window holds the store or shares one: it decides
                // what the slash commands can do and whose engine runs the
                // turns, and it was invisible.
                match self.source.daemon_base() {
                    Some(base) => format!(" — via {base}"),
                    None => " — this window holds the store".into(),
                }
            )));
        f.render_widget(tabs, header);

        match self.tab {
            0 => self.draw_chat(f, body),
            1 => self.draw_sessions(f, body),
            2 => self.draw_memory(f, body),
            3 => self.draw_skills(f, body),
            4 => self.draw_store(f, body),
            _ => self.draw_help(f, body),
        }

        // Per tab, because every key here is a character in the message box on
        // the chat tab: the footer promised `j/k move`, somebody trying to
        // scroll back typed `jjkkk` into their next prompt, and a hint that
        // does nothing where it is shown reads as an application that has
        // stopped responding.
        let keys: Vec<(&str, &str)> = match self.tab {
            0 => vec![
                ("↹ ", "tab  "),
                ("↑↓ ", "history  "),
                ("PgUp/PgDn ", "scroll  "),
                ("/ ", "commands  "),
                ("^C ", "stop  "),
            ],
            _ => vec![("↹/1-5 ", "tab  "), ("j/k ", "move  "), ("r ", "reload  "), ("q ", "quit  ")],
        };
        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        for (key, what) in keys {
            spans.push(Span::styled(key, Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(what));
        }
        spans.push(Span::styled("F2/F3 ", Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(format!(
            "{}/{}  {}  {}",
            self.shared.policy.stance().as_str(),
            self.shared.effort.get().as_str(),
            spent(self.chat.spent),
            self.status
        )));

        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray)),
            footer,
        );
    }

    fn draw_chat(&mut self, f: &mut Frame, area: Rect) {
        // Only one of the two can be up: an approval blocks the turn that would
        // have to be running for a question to arrive.
        // What the commands are is a thing nobody could see: they are in
        // `/help` and in the Help tab, which is where you look after giving up.
        // Shown while one is typed, they are a menu.
        let completing = match self.chat.input.as_str().starts_with('/') {
            true => crate::chat::commands_matching(self.chat.input.as_str()),
            false => Vec::new(),
        };
        let blocking = match (&self.chat.pending, &self.chat.asking) {
            (Some(request), _) => approval_height(request, area.width, area.height),
            (_, Some(asking)) => asking.panel().len() as u16 + 2,
            _ if !completing.is_empty() => ((completing.len() + 2) as u16).min((area.height / 2).max(3)),
            _ => 0,
        };
        let [log, ask, input] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(blocking), Constraint::Length(3)])
                .areas(area);

        let mut lines: Vec<Line> = Vec::new();
        for (kind, body) in &self.chat.log {
            let style = match *kind {
                "you" => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                "tool" => Style::default().fg(Color::Magenta),
                "think" => Style::default().fg(THINKING).add_modifier(Modifier::ITALIC),
                "stat" => Style::default().fg(Color::DarkGray),
                "err" => Style::default().fg(Color::Red),
                _ => Style::default(),
            };
            // The model's own words are the only ones with Markdown in them:
            // a tool line or a note is written here and says what it says.
            match *kind {
                "text" => lines.extend(answer(body, style)),
                _ => lines
                    .extend(body.split('\n').map(|line| Line::from(Span::styled(line.to_string(), style)))),
            }
            lines.push(Line::from(""));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Ask it something. Tab switches to the browsing tabs.",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Pinned to the bottom while streaming, because a reply that scrolls
        // off the top as it arrives is unreadable — but only while the reader
        // has not scrolled up. `scroll` counts lines back from the newest, so
        // holding a position as the turn writes means growing it by what
        // arrived; otherwise reading back during a long turn drifts to the
        // bottom every few hundred milliseconds.
        let total = lines.len() as u16;
        if self.chat.scroll > 0 {
            self.chat.scroll = self.chat.scroll.saturating_add(total.saturating_sub(self.chat.drawn));
        }
        self.chat.drawn = total;
        let visible = log.height.saturating_sub(2);
        let overflow = total.saturating_sub(visible);
        // Not past the first line: scrolling into blank space above the
        // conversation reads as the pane having lost it.
        self.chat.scroll = self.chat.scroll.min(overflow);
        let scroll = overflow.saturating_sub(self.chat.scroll);

        // Scrolled up, the pane looks exactly like a pane that has stopped
        // receiving: same border, same title, nothing moving. It says where it
        // is and how to get back.
        let title = match (self.chat.session, self.chat.scroll) {
            (Some(id), 0) => format!(" {} ", rook_store::format_session_id(id)),
            (Some(id), back) => {
                format!(" {} — {back} lines back, End returns ", rook_store::format_session_id(id))
            }
            (None, 0) => " new session ".into(),
            (None, back) => format!(" new session — {back} lines back, End returns "),
        };
        f.render_widget(
            Paragraph::new(lines).block(bordered(&title)).wrap(Wrap { trim: false }).scroll((scroll, 0)),
            log,
        );

        if let Some(request) = &self.chat.pending {
            let mut lines = vec![Line::from(Span::styled(
                format!("{} wants to {}", request.tool, request.action),
                Style::default().fg(Color::Yellow),
            ))];
            // Coloured the way a diff is read, because that is what it usually
            // is; the panel is small, so what does not fit scrolls off the top
            // rather than pushing the question off the bottom.
            let room = ask.height.saturating_sub(3) as usize;
            if let Some(preview) = &request.preview {
                let shown: Vec<&str> = preview.lines().collect();
                let from = shown.len().saturating_sub(room);
                lines.extend(shown[from..].iter().map(|line| {
                    let colour = match line.as_bytes().first() {
                        Some(b'+') => Color::Green,
                        Some(b'-') => Color::Red,
                        _ => Color::DarkGray,
                    };
                    Line::from(Span::styled((*line).to_string(), Style::default().fg(colour)))
                }));
            }
            // Named rather than called "this kind": what `k` would allow is
            // the difference between answering once and answering all
            // afternoon, and a person will not press a key for a category they
            // cannot see the edges of.
            let kind = match request.kind.is_empty() {
                true => String::new(),
                false => format!(" · [k] every {}", crate::approve::listed(&request.kind)),
            };
            lines.push(Line::from(Span::styled(
                format!("[y]es once · [a]lways this run{kind} · [n]o"),
                Style::default().fg(Color::DarkGray),
            )));
            f.render_widget(
                Paragraph::new(lines).block(bordered(" approval ")).wrap(Wrap { trim: false }),
                ask,
            );
        } else if let Some(asking) = &self.chat.asking {
            f.render_widget(Paragraph::new(asking.panel()).block(bordered(&asking.title())), ask);
        } else if !completing.is_empty() {
            let lines: Vec<Line> = completing
                .iter()
                .map(|(name, args, what)| {
                    Line::from(vec![
                        Span::styled(format!("/{name} "), Style::default().fg(Color::Cyan)),
                        Span::styled(format!("{args:<10} "), Style::default().fg(Color::DarkGray)),
                        Span::styled((*what).to_string(), Style::default().fg(Color::DarkGray)),
                    ])
                })
                .collect();
            f.render_widget(Paragraph::new(lines).block(bordered(" commands · tab completes ")), ask);
        }

        let prompt = match (self.chat.busy && self.chat.asking.is_none(), self.chat.since) {
            (true, Some(since)) => format!("  working… {}  ", elapsed(since.elapsed())),
            (true, None) => "  working… ".to_string(),
            _ => "› ".to_string(),
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prompt.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw(self.chat.input.as_str().to_string()),
            ]))
            .block(bordered("")),
            input,
        );
        // Wherever the box takes typing, which is everywhere but an approval:
        // a running turn takes what is typed as an interjection, and hiding the
        // caret there left somebody typing into a box with no sign of it. An
        // approval is answered with a letter and has the keyboard.
        if self.chat.pending.is_none() {
            f.set_cursor_position((
                input.x + 1 + prompt.chars().count() as u16 + self.chat.input.column(),
                input.y + 1,
            ));
        }
    }

    fn draw_sessions(&mut self, f: &mut Frame, area: Rect) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(area);
        let here = self.source.workspace().display().to_string();

        let items: Vec<ListItem> = self
            .sessions
            .iter()
            .map(|s| {
                ListItem::new(vec![
                    Line::from(Span::styled(
                        match s.meta.title.trim().is_empty() {
                            true => "(untitled)".to_string(),
                            false => s.meta.title.chars().take(40).collect::<String>(),
                        },
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        format!(
                            "  {} · {} events · {}",
                            fmt::ago(s.meta.updated_at),
                            s.meta.event_count,
                            // The pane is a third of the width and only one of
                            // these fits. Which project a session came from
                            // tells two rows apart; which model ran it does not,
                            // and it is the same one for all of them anyway.
                            match s.meta.workspace == here {
                                true => s.meta.model.clone(),
                                false => elsewhere(&s.meta.workspace),
                            },
                        ),
                        Style::default().fg(Color::DarkGray),
                    )),
                ])
            })
            .collect();

        f.render_stateful_widget(
            List::new(items)
                .block(bordered(" Sessions "))
                .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                .highlight_symbol("▌"),
            left,
            &mut self.session_state,
        );

        let mut lines: Vec<Line> = Vec::new();
        if let Some(selected) = &self.selected {
            if let Some(goal) = &selected.goal {
                lines.push(Line::from(Span::styled(
                    format!("goal: {goal}"),
                    Style::default().fg(Color::Cyan),
                )));
            }
            if let Some(at) = selected.forked_at {
                lines.push(Line::from(Span::styled(
                    format!("forked from its parent at event {at}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if selected.changes.touched() > 0 {
                lines.push(Line::from(Span::styled(
                    format!("changed {}", selected.changes.summary()),
                    Style::default().fg(Color::Yellow),
                )));
                for file in selected.changes.files.iter().filter(|f| f.lines_added + f.lines_removed > 0) {
                    lines.push(Line::from(Span::styled(
                        format!("  {} +{} -{}", file.path, file.lines_added, file.lines_removed),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
        }
        for e in &self.transcript {
            lines.push(Line::from(vec![
                Span::styled(format!("#{:<4} ", e.seq), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<13}", e.kind), kind_style(&e.kind)),
                Span::styled(
                    format!("{}  {} → {}", e.label, fmt::bytes(e.bytes), fmt::bytes(e.stored_bytes)),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            for line in e.body.lines().take(40) {
                lines.push(Line::from(Span::raw(format!("  {line}"))));
            }
            if e.truncated {
                lines.push(Line::from(Span::styled(
                    "  … elided; `rook store cat` for the full object",
                    Style::default().fg(Color::Yellow),
                )));
            }
            lines.push(Line::from(""));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "no events in this session",
                Style::default().fg(Color::DarkGray),
            )));
        }

        f.render_widget(
            Paragraph::new(lines)
                .block(bordered(" Transcript "))
                .wrap(Wrap { trim: false })
                .scroll((self.transcript_scroll, 0)),
            right,
        );
    }

    fn draw_memory(&mut self, f: &mut Frame, area: Rect) {
        let rows: Vec<Vec<String>> = self
            .facts
            .iter()
            .map(|fact| {
                vec![
                    fact.id.clone(),
                    if fact.pinned { "pin".into() } else { String::new() },
                    fact.scope.label().rsplit('/').next().unwrap_or("global").to_string(),
                    fact.text.chars().take(70).collect(),
                    fact.tags.join(" "),
                ]
            })
            .collect();

        let mut lines: Vec<Line> = Vec::new();
        if rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "nothing remembered yet — the agent writes here as it learns, and \
                 `rook memory rm <id>` corrects it",
                Style::default().fg(Color::DarkGray),
            )));
        }
        for row in fmt::table(&["id", "", "scope", "fact", "tags"], &rows).lines() {
            lines.push(Line::from(Span::raw(row.to_string())));
        }
        f.render_widget(
            Paragraph::new(lines).block(bordered(&format!(" Memory ({}) ", self.facts.len()))),
            area,
        );
    }

    fn draw_skills(&mut self, f: &mut Frame, area: Rect) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        // An empty pane reads as a broken one. The Memory tab already says
        // what an empty one means and how it fills; this said nothing at all,
        // which on a fresh install is what everybody sees first.
        if self.skills.is_empty() {
            let empty = Paragraph::new(vec![
                Line::from("no skills here yet"),
                Line::from(""),
                Line::from("`rook skills search <words>` looks in the configured sources,"),
                Line::from("`rook skills install <name>` puts one in, and the agent can"),
                Line::from("write its own — they land in the same directory either way."),
            ])
            .style(Style::default().fg(Color::DarkGray))
            .block(bordered(" Skills "));
            f.render_widget(empty, area);
            return;
        }

        let items: Vec<ListItem> = self
            .skills
            .iter()
            .map(|c| {
                let mark = if c.applicable { "✓" } else { "·" };
                let color = if c.applicable { Color::Green } else { Color::DarkGray };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::raw(format!("{:<24}", c.name)),
                    Span::styled(format!("{:<9}", c.version), Style::default().fg(Color::DarkGray)),
                    Span::styled(c.source.clone(), Style::default().fg(Color::Blue)),
                ]))
            })
            .collect();

        f.render_stateful_widget(
            List::new(items)
                .block(bordered(" Skills "))
                .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                .highlight_symbol("▌"),
            left,
            &mut self.skill_state,
        );

        let mut lines = Vec::new();
        if let Some(card) = self.skill_state.selected().and_then(|i| self.skills.get(i)) {
            lines.push(Line::from(Span::styled(
                format!("{} {}", card.name, card.version),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("source {} · body ~{} tokens", card.source, card.body_tokens),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(card.description.clone()));
            lines.push(Line::from(""));
            if card.applicable {
                lines.push(Line::from(Span::styled(
                    "applies in this environment",
                    Style::default().fg(Color::Green),
                )));
            } else {
                lines.push(Line::from(Span::styled("blocked here:", Style::default().fg(Color::Yellow))));
                for m in &card.mismatches {
                    lines.push(Line::from(format!("  · {m}")));
                }
            }
            if !card.keywords.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("keywords: {}", card.keywords.join(", ")),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        let skill_errors = self.source.here().map(|rook| rook.skill_errors.clone()).unwrap_or_default();
        if !skill_errors.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("failed to load:", Style::default().fg(Color::Red))));
            for e in &skill_errors {
                lines.push(Line::from(format!("  {e}")));
            }
        }
        f.render_widget(Paragraph::new(lines).block(bordered(" Detail ")).wrap(Wrap { trim: false }), right);
    }

    fn draw_store(&mut self, f: &mut Frame, area: Rect) {
        let [top, bottom] = Layout::vertical([Constraint::Length(12), Constraint::Min(3)]).areas(area);

        let mut lines = Vec::new();
        if let Some(s) = &self.stats {
            lines.push(Line::from(vec![
                Span::raw(format!("{:<16}", "logical")),
                Span::styled(fmt::bytes(s.bytes_raw), Style::default().add_modifier(Modifier::BOLD)),
            ]));
            lines.push(Line::from(vec![
                Span::raw(format!("{:<16}", "stored")),
                Span::styled(fmt::bytes(s.bytes_stored), Style::default().fg(Color::Green)),
                Span::styled(
                    format!("   {:.1}x compression", s.compression_ratio()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            lines.push(Line::from(format!("{:<16}{}", "saved by dedup", fmt::bytes(s.dedup_saved_hint))));
            lines.push(Line::from(format!(
                "{:<16}{}   (index {}, objects {})",
                "on disk",
                fmt::bytes(s.disk_bytes()),
                fmt::bytes(s.index_bytes),
                fmt::bytes(s.external_bytes)
            )));
            lines.push(Line::from(format!(
                "{:<16}{} objects · {} events · {} refs",
                "counts", s.objects, s.events, s.refs
            )));
            lines.push(Line::from(""));
            let max = s.per_kind.iter().map(|k| k.bytes_stored).max().unwrap_or(0);
            for k in &s.per_kind {
                lines.push(Line::from(vec![
                    Span::raw(format!("{:<14}", k.kind)),
                    Span::styled(fmt::bar(k.bytes_stored, max, 24), Style::default().fg(Color::Cyan)),
                    Span::raw(format!(
                        "  {:>9}  {:>9}  {:.1}x",
                        fmt::bytes(k.bytes_raw),
                        fmt::bytes(k.bytes_stored),
                        k.ratio()
                    )),
                ]));
            }
        }
        f.render_widget(Paragraph::new(lines).block(bordered(" Store ")).wrap(Wrap { trim: false }), top);

        let items: Vec<ListItem> = self
            .objects
            .iter()
            .map(|(id, kind, raw, stored)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{id}  "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{kind:<13}"), kind_style(kind)),
                    Span::raw(format!("{:>10} → {:>10}", fmt::bytes(*raw), fmt::bytes(*stored))),
                ]))
            })
            .collect();
        f.render_widget(List::new(items).block(bordered(" Objects (newest 300) ")), bottom);
    }

    fn draw_help(&mut self, f: &mut Frame, area: Rect) {
        let text = vec![
            Line::from(Span::styled("rook", Style::default().add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from("The Chat tab runs turns; the rest browse what is stored."),
            Line::from("Everything here is also on the command line, as tables or --json:"),
            Line::from(""),
            Line::from("  rook store stat                what memory costs, per kind"),
            Line::from("  rook store ls / cat <id>       list and print raw objects"),
            Line::from("  rook store gc --dry-run        what would be collected"),
            Line::from("  rook session ls / show <id>    transcripts by sequence number"),
            Line::from("  rook skills ls / why <name>    what applies here, and why not"),
            Line::from("  rook skills history <name>     every captured version"),
            Line::from("  rook skills rollback <n> <id>  restore one, undoably"),
            Line::from("  rook checkpoint create <name>  snapshot part of the workspace"),
            Line::from("  rook doctor                    detected toolchains and platform"),
            Line::from(""),
            Line::from(Span::styled("keys", Style::default().add_modifier(Modifier::BOLD))),
            Line::from("  Tab         switch tab          j k ↑ ↓   move"),
            Line::from("  1-5         switch tab, outside Chat where digits are text"),
            Line::from("  Space/PgDn  scroll transcript    r         reload"),
            Line::from("  wheel       scrolls either pane; hold Shift to select text"),
            Line::from("  q / Esc     quit (Ctrl-C anywhere)"),
            Line::from(""),
            Line::from("  In Chat:    Enter sends · Esc clears, then quits"),
            Line::from("              /btw <question> asks without joining the conversation"),
            Line::from("              y / a / n answer an approval"),
            Line::from("              enter     answer a question, one at a time"),
            Line::from("              /…        tab completes; the list shows as you type"),
            Line::from("              PgUp/PgDn scroll back through the conversation"),
            Line::from("              ↑ / ↓     the prompts already sent, newest first"),
            Line::from("              ← → home end · ctrl-a/e/w/u/k edit the line being typed"),
            Line::from("              F2 / F3   cycle approvals / reasoning effort"),
            Line::from("              ctrl-c    stops a running turn, or quits when none is"),
        ];
        f.render_widget(Paragraph::new(text).block(bordered(" Help ")), area);
    }
}

fn fail(to_loop: &mpsc::UnboundedSender<TurnEvent>, message: String) {
    let _ = to_loop.send(TurnEvent::Error(message));
}

/// How tall the approval panel has to be for what it is approving to be read.
///
/// It was four rows whatever it held, and the first of those is the command
/// itself — so `run_command wants to run cargo test --workspace …` was cut at
/// the panel's width with nothing saying so, and the approval was of a
/// sentence nobody had seen the end of. cline hit the same thing in a toast
/// and fixed it the same way: give the box a real edge, and let the text wrap
/// at it.
///
/// Capped at half the chat area, because the panel must not push the
/// conversation off the screen — past the cap the preview scrolls off the top,
/// which is what the drawing already does with it.
fn approval_height(request: &ApprovalRequest, width: u16, room: u16) -> u16 {
    let inside = width.saturating_sub(2).max(1) as usize;
    let header = format!("{} wants to {}", request.tool, request.action);
    let wrapped = header.lines().map(|line| line.chars().count().div_ceil(inside).max(1)).sum::<usize>();
    let preview = request.preview.as_deref().map_or(0, |p| p.lines().count());
    // The header, as much preview as there is, the key line, and two borders.
    let wanted = wrapped + preview + 3;
    (wanted as u16).clamp(4, (room / 2).max(4))
}

fn bordered(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title.bold())
}

fn kind_style(kind: &str) -> Style {
    let color = match kind {
        "user" => Color::Cyan,
        "assistant" => Color::White,
        "tool-call" => Color::Magenta,
        "tool-result" | "tool_result" => Color::Blue,
        "skill" => Color::Green,
        "error" => Color::Red,
        "compaction" => Color::Yellow,
        "file" => Color::Blue,
        "snapshot" => Color::Green,
        _ => Color::Gray,
    };
    Style::default().fg(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asking_to_run(command: &str) -> ApprovalRequest {
        ApprovalRequest {
            id: "1".into(),
            tool: "run_command".into(),
            action: format!("run {command}"),
            preview: None,
            kind: vec!["echo".into()],
        }
    }

    /// The panel was four rows whatever it held, and the command is its first
    /// line: at eighty columns everything past the sixtieth character of a
    /// command was cut, silently, in the one place where reading the whole
    /// thing is the point.
    #[test]
    fn the_approval_panel_is_tall_enough_for_the_command_being_approved() {
        let long = format!("cargo test --workspace -- {}", "an-exact-test-name ".repeat(12));
        let width = 80;
        let inside = (width - 2) as usize;
        let needed = format!("run_command wants to run {long}").chars().count().div_ceil(inside);
        assert!(needed > 1, "the test has to need more than one line to be about wrapping: {needed}");

        let tall = approval_height(&asking_to_run(&long), width, 40);

        assert!(
            tall as usize >= needed + 3,
            "{tall} rows for {needed} lines of command, a key line and two borders"
        );
    }

    /// And never so tall that the conversation it is about is off the screen.
    #[test]
    fn the_approval_panel_leaves_the_chat_on_the_screen() {
        let sprawling = "x ".repeat(4_000);
        assert!(approval_height(&asking_to_run(&sprawling), 80, 40) <= 20, "half of forty rows");
        assert!(approval_height(&asking_to_run("ls"), 80, 6) >= 4, "and never less than it had");
    }

    /// The pane kept every word of every turn for the life of the process. What
    /// makes a bound safe here is that the session holds all of it: the
    /// Sessions tab reads back what the pane has let go of.
    #[test]
    fn the_chat_pane_keeps_a_bounded_tail_rather_than_everything() {
        let mut chat = Chat::default();
        let filler = "x".repeat(1024);
        for i in 0..4_000 {
            chat.push("tool", &format!("  · a tool call number {i}: {filler}"));
        }

        let pushed = 4_000 * (filler.len() + 32);
        assert!(pushed > MAX_SCROLLBACK, "the test has to exceed the bound to be about it: {pushed}");
        let held: usize = chat.log.iter().map(|(_, body)| body.len()).sum();
        assert!(held <= MAX_SCROLLBACK, "{held} bytes held against a bound of {MAX_SCROLLBACK}");
        assert!(
            chat.log.last().unwrap().1.contains("number 3999"),
            "and it is the tail that is kept, not the head"
        );
    }

    #[test]
    fn streamed_text_still_joins_into_one_block() {
        let mut chat = Chat::default();
        chat.push("text", "the sky ");
        chat.push("text", "is blue");

        assert_eq!(chat.log.len(), 1, "a reply arrives in fragments and reads as one: {:?}", chat.log);
        assert_eq!(chat.log[0].1, "the sky is blue");
    }

    /// Reasoning streams the same way an answer does, and unmerged it rendered
    /// one word per line with a blank line between: a model thinking out loud
    /// filled the pane with `I` / `'ll` / `just` / `do` and nothing readable.
    #[test]
    fn streamed_reasoning_joins_the_same_way_an_answer_does() {
        let mut chat = Chat::default();
        for word in ["I", "'ll", " just", " do", " the", " table", " dump"] {
            chat.push("think", word);
        }

        assert_eq!(chat.log.len(), 1, "one block, not seven: {:?}", chat.log);
        assert_eq!(chat.log[0].1, "I'll just do the table dump");
    }

    /// Merging is per stream and not across them: an answer that follows a
    /// thought is a different block, and so is every tool line between.
    #[test]
    fn a_thought_and_the_answer_after_it_stay_apart() {
        let mut chat = Chat::default();
        chat.push("think", "reading it");
        chat.push("tool", "  · read_file");
        chat.push("text", "8443");

        let kinds: Vec<&str> = chat.log.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, ["think", "tool", "text"]);
    }

    fn drawn(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    /// An answer arrived as its Markdown source: `## Heading`, fences, and
    /// backticks around every identifier. The browser has drawn the little of
    /// it a model uses since the beginning; the terminal showed the source.
    #[test]
    fn an_answer_is_drawn_rather_than_shown_as_its_source() {
        let body = "## Where it goes\n\nThe dir comes from `tempfile`, **not** the config.\n\n                    ```rust\nlet f = NamedTemporaryFile::new();\n```\n";
        let base = Style::default();

        let lines = answer(body, base);
        let text = drawn(&lines);

        assert_eq!(text[0], "Where it goes", "the marker is the styling, not the text");
        assert_eq!(text[2], "The dir comes from tempfile, not the config.", "and so are the ticks");
        assert!(text.iter().any(|l| l.contains("let f = NamedTemporaryFile::new();")), "{text:?}");
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD), "a heading is bold");
    }

    /// A fence keeps its edge: dropping the ``` as the browser does leaves a
    /// block of code with nothing marking it in a pane that is all text.
    #[test]
    fn a_fence_keeps_the_line_that_opened_it() {
        let lines = drawn(&answer("before\n```sh\nls\n```\nafter", Style::default()));
        assert_eq!(lines, ["before", "```sh", "ls", "```", "after"]);
    }

    /// A `*` on its own is a bullet or a multiplication sign, and an unclosed
    /// tick is a model still typing. Neither is emphasis, and neither may eat
    /// the rest of the line.
    #[test]
    fn half_written_markup_is_left_as_the_text_it_is() {
        for text in ["2 * 3 * 4", "a `half written", "**unclosed bold", "- a bullet"] {
            assert_eq!(drawn(&answer(text, Style::default()))[0], text, "{text:?}");
        }
    }

    /// The input was a `String` with `push` and `pop`: the cursor never left
    /// the end, so fixing a typo in the middle of a long prompt cost every
    /// character after it.
    #[test]
    fn the_message_being_typed_has_a_cursor_in_it() {
        let mut line = Typing::default();
        for c in "cargo tets".chars() {
            line.insert(c);
        }
        line.backspace();
        line.backspace();
        line.insert('s');
        line.insert('t');
        assert_eq!(line.as_str(), "cargo test");
        assert_eq!(line.column(), 10);

        line.home();
        line.delete();
        assert_eq!(line.as_str(), "argo test", "delete takes the character ahead of the cursor");

        line.end();
        line.kill_word();
        assert_eq!(line.as_str(), "argo ", "ctrl-w takes the word behind it");
        assert_eq!(line.column(), 5);
        line.kill_word();
        assert_eq!(line.as_str(), "", "and the spaces before that word with it");
    }

    /// Characters, not bytes: the cursor is drawn at a column, and a prompt in
    /// any language somebody types put it in the wrong place.
    #[test]
    fn the_cursor_is_counted_in_characters() {
        let mut line = Typing::default();
        for c in "привет".chars() {
            line.insert(c);
        }
        assert_eq!(line.column(), 6);
        line.left();
        line.backspace();
        assert_eq!(line.as_str(), "привт", "two-byte characters step one at a time");
    }

    /// Reasoning was the same grey as the footer and the hints. A turn that
    /// thinks out loud fills the pane with it, and a page of chrome-coloured
    /// text is hard to read on a dark terminal — which is how it was reported.
    #[test]
    fn reasoning_is_brighter_than_the_chrome_around_it() {
        let (Color::Rgb(r, g, b), Color::Rgb(dr, dg, db)) = (THINKING, Color::Rgb(80, 80, 80)) else {
            unreachable!("both are literals")
        };
        let brightness = |r: u8, g: u8, b: u8| r as u32 + g as u32 + b as u32;
        assert!(
            brightness(r, g, b) > brightness(dr, dg, db),
            "reasoning is read, not glanced at: {r},{g},{b}"
        );
        // And not so bright it competes with the answer, which is what the
        // turn is actually telling you.
        assert!(brightness(r, g, b) < brightness(255, 255, 255), "still quieter than white");
    }

    /// A turn that thinks for two minutes showed a fixed `working…` and a
    /// stream that moved only when the model spoke. It was reported as a hang,
    /// which it was not.
    #[test]
    fn how_long_it_has_been_working_is_said_in_the_units_someone_waiting_reads() {
        assert_eq!(elapsed(std::time::Duration::from_secs(9)), "9s");
        assert_eq!(elapsed(std::time::Duration::from_secs(59)), "59s");
        assert_eq!(elapsed(std::time::Duration::from_secs(60)), "1m00s");
        assert_eq!(elapsed(std::time::Duration::from_secs(250)), "4m10s");
    }

    /// The commands were discoverable only from `/help`, which is where you
    /// look after giving up. Typing `/se` narrows to what could follow it.
    #[test]
    fn typing_a_slash_narrows_to_the_commands_that_could_follow() {
        let all = crate::chat::commands_matching("/");
        assert!(all.len() > 10, "everything, for a bare slash: {}", all.len());

        let se: Vec<&str> = crate::chat::commands_matching("/se").iter().map(|(n, ..)| *n).collect();
        assert_eq!(se, ["session", "search"], "two share the prefix, so both are offered");

        let past = crate::chat::commands_matching("/search rook");
        assert_eq!(past.len(), 1, "past the name it is an argument, not a prefix");
        assert_eq!(past[0].0, "search");

        assert!(crate::chat::commands_matching("/nosuch").is_empty());
    }

    fn asking(questions: Vec<Question>) -> Asking {
        Asking { id: "1".into(), questions, at: 0, chosen: Vec::new() }
    }

    fn question(text: &str, choices: &[&str], multi: bool) -> Question {
        Question { question: text.into(), choices: choices.iter().map(|c| c.to_string()).collect(), multi }
    }

    /// A ratatui buffer holds characters cell by cell, so what the screen shows
    /// has to be read back a row at a time rather than searched as text.
    fn screen(asking: &Asking, width: u16) -> Vec<String> {
        let height = asking.panel().len() as u16 + 2;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new(asking.panel()).block(bordered(&asking.title())), f.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_string())
            .collect()
    }

    #[test]
    fn a_question_shows_its_choices_numbered_and_the_first_as_recommended() {
        let lines = screen(&asking(vec![question("Which target?", &["staging", "prod"], false)]), 50);

        assert!(lines[0].contains("question 1 of 1"), "{lines:?}");
        assert!(lines[1].contains("Which target?"), "{lines:?}");
        assert!(lines[2].contains("1. staging  (recommended)"), "{lines:?}");
        assert!(lines[3].contains("2. prod") && !lines[3].contains("recommended"), "{lines:?}");
        assert!(lines[4].contains("a number, or your own answer"), "{lines:?}");
    }

    #[test]
    fn the_panel_is_exactly_as_tall_as_what_it_draws() {
        let asking = asking(vec![question("Which?", &["a", "b", "c"], true)]);
        let lines = screen(&asking, 40);

        assert_eq!(lines.len(), asking.panel().len() + 2, "one border row above and below");
        assert!(lines.last().unwrap().starts_with('╰'), "the last row is the border: {lines:?}");
        assert!(!lines[2].contains("recommended"), "a multi-select recommends nothing: {lines:?}");
        assert!(lines[5].contains("numbers, comma-separated"), "{lines:?}");
    }

    #[test]
    fn a_free_text_question_draws_no_choice_rows() {
        let lines = screen(&asking(vec![question("Why?", &[], false)]), 40);

        assert_eq!(lines.len(), 4, "border, question, prompt, border: {lines:?}");
        assert!(lines[2].contains("your answer:"), "{lines:?}");
    }

    #[test]
    fn a_batch_is_answered_one_question_at_a_time_and_kept_in_order() {
        let mut a = asking(vec![
            question("Which target?", &["staging", "prod"], false),
            question("Why?", &[], false),
        ]);

        assert_eq!(a.record("2").chosen, ["prod"]);
        assert!(!a.complete(), "a batch is not done until every question is");
        assert_eq!(a.record("the canary is unhealthy").chosen, ["the canary is unhealthy"]);

        assert!(a.complete());
        assert_eq!(a.chosen, vec![vec!["prod".to_string()], vec!["the canary is unhealthy".into()]]);
    }

    #[test]
    fn an_empty_line_answers_the_question_it_was_typed_at_not_the_next_one() {
        let mut a = asking(vec![question("first", &["a", "b"], false), question("second", &[], false)]);

        let answer = a.record("");
        assert_eq!(answer.question, "first");
        assert_eq!(answer.chosen, ["a"], "enter takes the recommendation");
        assert_eq!(a.record("").chosen, Vec::<String>::new(), "free text has none to take");
    }

    #[test]
    fn the_title_counts_through_the_batch() {
        let mut a = asking(vec![question("first", &[], false), question("second", &[], false)]);
        assert!(screen(&a, 40)[0].contains("question 1 of 2"));
        a.at = 1;
        let lines = screen(&a, 40);
        assert!(lines[0].contains("question 2 of 2"), "{lines:?}");
        assert!(lines[1].contains("second"), "{lines:?}");
    }
}
