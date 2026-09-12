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
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
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

/// What is over the conversation, when anything is.
///
/// Tabs were the shape before this, and their cost was constant: eight names
/// across the top of every screen, a Tab key that meant "leave the
/// conversation" where every other terminal means "complete this", and a
/// footer that had to say something different on each one. The conversation is
/// the window now, and everything else is summoned, used and dismissed — which
/// is how opencode reads, and how a tool that is mostly one thing should.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay {
    /// Everything reachable, filtered as it is typed. `^P`, the key every
    /// editor has made mean this.
    Palette,
    /// Every call this conversation made, with what it was given and what came
    /// back. `^o`, from the conversation, for the call you are looking at.
    Calls,
    Sessions,
    Memory,
    Skills,
    Store,
    Checkpoints,
    Docs,
    Help,
}

impl Overlay {
    /// The panes the palette offers, in the order somebody reaches for them.
    const PANES: [Overlay; 8] = [
        Overlay::Calls,
        Overlay::Sessions,
        Overlay::Docs,
        Overlay::Memory,
        Overlay::Skills,
        Overlay::Checkpoints,
        Overlay::Store,
        Overlay::Help,
    ];

    fn name(self) -> &'static str {
        match self {
            Overlay::Palette => "commands",
            Overlay::Calls => "calls",
            Overlay::Sessions => "sessions",
            Overlay::Memory => "memory",
            Overlay::Skills => "skills",
            Overlay::Store => "store",
            Overlay::Checkpoints => "checkpoints",
            Overlay::Docs => "docs",
            Overlay::Help => "help",
        }
    }

    /// What it is for, said where somebody is choosing between them.
    fn what(self) -> &'static str {
        match self {
            Overlay::Palette => "everything reachable from here",
            Overlay::Calls => "what each call was given and what came back",
            Overlay::Sessions => "past conversations — enter continues one here",
            Overlay::Memory => "what the agent believes, and how to correct it",
            Overlay::Skills => "what applies in this workspace, and why",
            Overlay::Store => "what memory costs, per kind of object",
            Overlay::Checkpoints => "snapshots of the workspace, and putting one back",
            Overlay::Docs => "documentation gathered here, with its sources",
            Overlay::Help => "the keys and the commands",
        }
    }

    /// The keys the footer promises while this one is up. Per overlay, because
    /// a hint that does nothing where it is shown reads as an application that
    /// has stopped responding.
    fn keys(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Overlay::Palette => &[("↑↓ ", "choose  "), ("⏎ ", "open  "), ("esc ", "close  ")],
            Overlay::Calls => &[("j/k ", "move  "), ("r ", "reload  "), ("esc ", "close  ")],
            Overlay::Sessions => &[("j/k ", "move  "), ("⏎ ", "continue  "), ("r ", "reload  ")],
            Overlay::Memory => &[
                ("j/k ", "move  "),
                ("a ", "add  "),
                ("A ", "global  "),
                ("d ", "forget  "),
                ("u ", "undo  "),
            ],
            Overlay::Skills => &[("j/k ", "move  "), ("c ", "capture  "), ("u ", "roll back  ")],
            Overlay::Store => &[("r ", "reload  ")],
            Overlay::Checkpoints => &[("j/k ", "move  "), ("c ", "take one  "), ("R ", "restore  ")],
            Overlay::Docs => &[("j/k ", "move  "), ("d ", "drop  "), ("r ", "reload  ")],
            Overlay::Help => &[("esc ", "close  ")],
        }
    }
}

/// One call, as the pane that answers "what did that actually do" reads it.
///
/// The mark is not stored anywhere — `ToolDone`'s `failed` is a stream event and
/// the log keeps the result, not a verdict about it — so the pane shows what
/// came back and lets it speak, rather than inventing a tick from the text.
struct Call {
    seq: u64,
    doing: String,
    name: String,
    given: String,
    came_back: Option<String>,
    elided: bool,
}

/// A session's calls, newest first, each with the result that answers it.
///
/// Paired by order within a tool name, which is the rule every front end uses:
/// a result carries only the name, and a turn that reads two files at once logs
/// two `read_file` results told apart by nothing else. Pairing them the other
/// way would show a call returning another call's bytes, which is worse than
/// showing nothing.
fn paired(entries: Vec<TranscriptEntry>) -> Vec<Call> {
    let mut calls: Vec<Call> = Vec::new();
    let mut waiting: Vec<usize> = Vec::new();
    for entry in entries {
        match entry.kind.as_str() {
            "tool-call" => {
                waiting.push(calls.len());
                calls.push(Call {
                    seq: entry.seq,
                    doing: match entry.doing.is_empty() {
                        true => entry.label.clone(),
                        false => entry.doing,
                    },
                    name: entry.label,
                    given: entry.body,
                    came_back: None,
                    elided: entry.truncated,
                });
            }
            "tool-result" => {
                let waited = waiting.iter().position(|at| calls[*at].name == entry.label);
                if let Some(at) = waited.map(|at| waiting.remove(at)) {
                    calls[at].came_back = Some(entry.body);
                    calls[at].elided |= entry.truncated;
                }
            }
            _ => {}
        }
    }
    // Newest first: the question is almost always about what just happened.
    calls.reverse();
    calls
}

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
    // the alternate screen is empty. The other half of that bargain is that the
    // terminal never sees the drag that selects a line, so `^s` hands the mouse
    // back; the comment that used to be here said the modifier for it was in
    // the help, and it was in neither.
    let mouse = execute!(std::io::stdout(), event::EnableMouseCapture).is_ok();
    // Without this a pasted newline arrives as the Enter key, so a paragraph
    // pasted into the box was sent one line at a time — the first as a prompt
    // and the rest chasing it. With it the terminal brackets the paste and the
    // whole of it arrives as one event, newlines included.
    let pasting = execute!(std::io::stdout(), event::EnableBracketedPaste).is_ok();
    let daemon = source.daemon_base().map(str::to_string);
    let mut app = App::new(source, runtime, yes);
    app.mouse = mouse;
    let result = app.run(&mut terminal);
    if mouse {
        let _ = execute!(std::io::stdout(), event::DisableMouseCapture);
    }
    if pasting {
        let _ = execute!(std::io::stdout(), event::DisableBracketedPaste);
    }
    ratatui::restore();
    // Written down, not only returned — and after the screen is restored, so it
    // does not land on top of the UI. A window that ends leaves nothing behind:
    // its stderr is a terminal that may already be closed, and the question
    // afterwards is always the same one — did it finish, or did it die? Today
    // that question was asked of a window that was simply gone, with no crash
    // report, no log line and no trace of any kind, while the turn it had been
    // drawing had finished perfectly well in the daemon. At `warn`, because a
    // window ending is the thing someone comes here to read about.
    match &result {
        Ok(()) => tracing::warn!("the window closed"),
        Err(e) => tracing::warn!("the window ended: {e}"),
    }
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
    /// A call the model has just made: the tool's own name, which is what
    /// pairs it with the finish, and what to show, which is the work.
    Tool {
        name: String,
        said: String,
    },
    ToolDone(String, bool),
    /// A sub-agent's progress. Its own kind rather than more reasoning: work
    /// happening in another agent is not the model thinking out loud, and it
    /// was drawn in the same grey as the thoughts it was buried in.
    Agent(String),
    /// Which step of the budget the turn is on.
    Step(u32, u32),
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

    /// The start of the row the cursor is in, and the end of it.
    fn row_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |nl| nl + 1)
    }

    fn row_end(&self, at: usize) -> usize {
        self.text[at..].find('\n').map_or(self.text.len(), |nl| at + nl)
    }

    /// Up a row, keeping the column where the row above is long enough.
    ///
    /// False when there is no row above, which is when the key means the
    /// previous prompt instead — the box holds several rows now, and Up walking
    /// straight into the history took a half-written message with it.
    fn up(&mut self) -> bool {
        let start = self.row_start(self.at);
        if start == 0 {
            return false;
        }
        let column = self.text[start..self.at].chars().count();
        // `start - 1` is the newline that ends the row above.
        self.at = self.along(self.row_start(start - 1), start - 1, column);
        true
    }

    /// Down a row. False when there is none, where the key means the next
    /// prompt.
    fn down(&mut self) -> bool {
        let end = self.row_end(self.at);
        if end == self.text.len() {
            return false;
        }
        let column = self.text[self.row_start(self.at)..self.at].chars().count();
        self.at = self.along(end + 1, self.row_end(end + 1), column);
        true
    }

    /// `column` characters along the row from `from`, or its end — a short row
    /// takes the cursor to where it ends rather than past it.
    fn along(&self, from: usize, to: usize, column: usize) -> usize {
        self.text[from..to].char_indices().nth(column).map_or(to, |(at, _)| from + at)
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

    /// How far along its one line the cursor is: characters, not bytes. For the
    /// boxes that hold one line — the palette, a fact being added, a topic.
    fn column(&self) -> u16 {
        self.text[..self.at].chars().count() as u16
    }

    /// Where the cursor is drawn in the message box, which holds newlines: the
    /// row it is on and how far along that row.
    fn caret(&self) -> (u16, u16) {
        let before = &self.text[..self.at];
        let row = before.matches('\n').count() as u16;
        let column = before.rsplit('\n').next().unwrap_or_default().chars().count() as u16;
        (row, column)
    }

    fn rows(&self) -> u16 {
        (self.text.matches('\n').count() + 1) as u16
    }

    /// Text arriving from the terminal in one piece, newlines and all.
    ///
    /// A terminal delivers a pasted newline as the Enter key, so a paragraph
    /// pasted in was sent a line at a time: the first line went as a prompt and
    /// the rest chased it as prompts of their own. Bracketed paste is what tells
    /// a paste from typing, and this is the half that keeps the newlines.
    fn paste(&mut self, text: &str) {
        // `\r\n` and a bare `\r` both mean a new line here. A `\r` left in
        // would move the cursor back over what was already drawn.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.at, &text);
        self.at += text.len();
    }

    /// The file being named at the cursor: what follows the last `@` of the
    /// word the cursor is in, or nothing.
    ///
    /// The word rather than the line, because a prompt is a sentence and `@`
    /// belongs to one file in it — and behind the cursor rather than the whole
    /// text, because going back to fix an earlier mention should offer that
    /// mention, not the last one typed. An email address is the reason a `@`
    /// with a character before it is not one of these.
    fn mentioning(&self) -> Option<&str> {
        let before = &self.text[..self.at];
        let word = before.rsplit([' ', '\t', '\n']).next()?;
        let fragment = word.strip_prefix('@')?;
        match fragment.contains('@') {
            true => None,
            false => Some(fragment),
        }
    }

    /// Grow the mention being typed to `common` without finishing it: several
    /// files share this much, so the cursor stays where more can be typed.
    fn narrow(&mut self, common: &str) {
        let Some(fragment) = self.mentioning() else { return };
        if common.len() <= fragment.len() {
            return;
        }
        let start = self.at - fragment.len();
        self.text.replace_range(start..self.at, common);
        self.at = start + common.len();
    }

    /// Put `path` where the mention being typed is, and a space after it: the
    /// name is finished and the sentence goes on.
    fn mention(&mut self, path: &str) {
        let Some(fragment) = self.mentioning() else { return };
        let start = self.at - fragment.len() - '@'.len_utf8();
        // A space after it unless there is one already, which is what
        // completing a mention in the middle of a line runs into.
        let gap = match self.text[self.at..].starts_with(' ') {
            true => "",
            false => " ",
        };
        self.text.replace_range(start..self.at, &format!("@{path}{gap}"));
        self.at = start + path.len() + '@'.len_utf8() + gap.len();
    }
}

/// A fact being typed on the Memory tab, and how far it will reach. Scope is
/// chosen by the key that starts the line — `a` files it against this
/// workspace, `A` everywhere — because it is the part a person gets wrong once
/// and then lives with.
#[derive(Default)]
struct Adding {
    text: Typing,
    global: bool,
}

#[derive(Default)]
struct Chat {
    input: Typing,
    /// Prompts already sent, oldest first, and where in them the up-arrow has
    /// walked. Kept here rather than in the store: it is what this window has
    /// typed, and a session's transcript is the record.
    history: Vec<String>,
    recalled: Option<usize>,
    /// What was being typed when the walk through history started, put back
    /// when the walk comes past the newest entry again.
    ///
    /// It was cleared instead, so pressing Up on a half-written message and
    /// Down to come back left an empty box: the walk destroyed the thing it was
    /// offering an alternative to.
    draft: String,
    log: Vec<(&'static str, String)>,
    /// Calls announced and not yet finished. A message announces several
    /// before any of them runs, so this is what pairs a finish with its line —
    /// core's, because the chat REPL and `rook run` ask the same question and
    /// used to answer it by marking whatever line the cursor was on.
    running_calls: rook_core::calls::Running,
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
    /// What the newest request carried, which is the context as the provider
    /// counted it: the cumulative figure above less what it was before this
    /// reply. A running total was being read as the size of the context, and it
    /// is thirty-nine steps' worth of them.
    carried: u32,
    /// Which step of its budget the turn in flight is on. A window showing
    /// only that something is happening cannot say how much room is left, and
    /// a turn at step 190 of 200 is about to stop mid-task.
    step: Option<(u32, u32)>,
    /// When the turn in flight started. A long turn showed a fixed `working…`
    /// and a stream that only moved when the model spoke, which reads as a
    /// hang — and was reported as one.
    since: Option<std::time::Instant>,
    /// When this window last heard anything at all from the turn. Silence is
    /// the one thing a person cannot read: a build running, a model thinking
    /// and a wedged turn all draw the same screen, and the last of the three
    /// was reported as a hang twice.
    heard: Option<std::time::Instant>,
    /// Where to send what the person answers while the daemon runs the turn:
    /// approvals, answers and a cancellation. `None` when the turn is this
    /// process's own.
    remote: Option<mpsc::UnboundedSender<ClientMessage>>,
    /// Whether this window has already asked the daemon whether the turn it is
    /// drawing is still running. Asked once per silence, and forgotten the
    /// moment anything is heard.
    asked_if_alive: bool,
}

/// The configuration a window shows, whether or not it holds a store of its own.
///
/// The comment here used to say that a window with no store still knows its
/// settings because the configuration is a file — and the code beside it fell
/// back to the built-in defaults instead of reading that file. So a window
/// routed through a daemon drew a footer saying the turn would give up on a
/// silent model after ninety seconds while the daemon running it was waiting
/// twenty minutes, as the file said. A number a person reads to decide whether
/// to keep waiting is worse wrong than absent.
///
/// A daemon on another machine reads its own file and this one cannot see it.
/// The local file is still the closer answer of the two.
fn config_seen_by(here: Option<&rook_core::Rook>, file: std::path::PathBuf) -> rook_core::Config {
    match here {
        Some(rook) => rook.config.clone(),
        None => rook_core::Config::load_from(file).unwrap_or_default(),
    }
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

/// What a long pause is waiting on, in the words of the log being read: the
/// tool that is still running, or the model that has sent nothing.
fn waiting_on(quiet: std::time::Duration, running: Option<&str>, patience: std::time::Duration) -> String {
    match running {
        // A tool has its own timeout and its own line; this one is about the
        // model, and naming a deadline that is not this wait's would be worse
        // than naming none.
        Some(tool) => format!(" · {tool} running {}", crate::fmt::elapsed(quiet)),
        None => format!(
            " · nothing from the model for {} of {}",
            crate::fmt::elapsed(quiet),
            crate::fmt::elapsed(patience)
        ),
    }
}

/// Short enough for the footer, which already carries the mode, the effort and
/// whatever the last command said.
///
/// The window's share comes first because it is the question the rest of the
/// line was being read as the answer to: `1595.2k in` beside `2.4 MiB on disk`
/// was taken for how full the context was, when it is what thirty-nine steps
/// have spent between them. Somebody asked whether their context had passed a
/// million; it was at 29%.
///
/// The share is the newest request's own input, which the provider counted,
/// rather than an estimate of what the next one will carry — measured beats
/// guessed, and it is already here.
fn spent(totals: Option<(u32, u32, u32)>, carried: u32, usable: usize) -> String {
    let Some((input, output, cached)) = totals else { return String::new() };
    // A percentage rather than a second large number: what is worth knowing
    // about the cache is how much of the bill it took, not its size.
    let cached = match (cached, input) {
        (0, _) | (_, 0) => String::new(),
        // Rounded, not truncated: 86.998% shown as 86 is a percentage point
        // given away for nothing.
        (n, all) => format!(" ({}% cached)", (n as u64 * 100 + all as u64 / 2) / all as u64),
    };
    let window = match (carried, usable) {
        (0, _) | (_, 0) => String::new(),
        (carried, usable) => {
            format!("ctx {}/{} · ", thousands(carried), thousands(usable.min(u32::MAX as usize) as u32))
        }
    };
    format!("{window}{} in / {} out{cached}", thousands(input), thousands(output))
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

    /// A call has started: write its line, and remember which line it was.
    ///
    /// The name is kept beside what was written because the two differ — the
    /// line says `read src/main.rs` and the finish only names `read_file` —
    /// and because a message can announce several calls before any of them
    /// runs. They finish in the order they were announced, so a queue per name
    /// pairs each finish with the line it belongs to.
    fn tool_started(&mut self, name: &str, said: &str) {
        self.push("tool", &format!("  · {said}"));
        self.running_calls.started(name, said);
    }

    /// Mark the line that call was written for.
    ///
    /// The line gains its mark rather than a second line being written under
    /// it, which is what the code did while the comment below said otherwise:
    /// every call read as two events, and a turn of a dozen calls filled the
    /// pane twice over.
    fn tool_done(&mut self, name: &str, failed: bool) {
        let (said, took) = self.running_calls.finished(name);
        // A long call says how long it was: `working…` counts the turn, not
        // which call it is waiting on, so a turn that sat on one command for a
        // minute left no trace of it once the command came back.
        let mark = match (failed, took) {
            (true, None) => " ✗".to_string(),
            (false, None) => " ✓".to_string(),
            (true, Some(took)) => format!(" ✗ {}", crate::fmt::elapsed(took)),
            (false, Some(took)) => format!(" ✓ {}", crate::fmt::elapsed(took)),
        };
        let mark = mark.as_str();
        let unmarked = self
            .log
            .iter_mut()
            .find(|(kind, body)| *kind == "tool" && body.trim_start().trim_start_matches("· ") == said);
        match unmarked {
            Some((_, body)) => body.push_str(mark),
            // A call whose start was never seen — a window that attached to a
            // daemon mid-turn — still says that it finished.
            None => self.push("tool", &format!("  · {said}{mark}")),
        }
    }

    /// A turn is under way, whether this window started it or joined it.
    ///
    /// The clocks start now and not when the turn did: what they answer is
    /// "has it stopped", and a window that has been watching for four seconds
    /// cannot say more than that.
    fn began(&mut self) {
        self.busy = true;
        self.since = Some(std::time::Instant::now());
        self.step = None;
        self.heard = self.since;
        self.asked_if_alive = false;
    }

    /// A turn is over, however it ended.
    ///
    /// The window stops waiting and forgets where to send answers, so a key
    /// pressed afterwards is not sent into a turn that has ended. And whatever
    /// the turn was waiting for goes with it, which is the part `^c` left
    /// behind: an approval on screen takes the whole keyboard and answers
    /// nobody, so the window reads as frozen, and a question on screen captures
    /// Enter, so nothing can be sent. Said rather than done silently — a
    /// question that vanishes unexplained is its own confusion.
    fn ended(&mut self) {
        self.busy = false;
        self.remote = None;
        let waiting = self.pending.take().is_some() || self.asking.take().is_some();
        if waiting {
            self.push("stat", "  the turn ended, so what it was waiting for is gone");
        }
    }

    /// Whether this window has been drawing `working…` long enough to be worth
    /// asking the daemon whether there is still a turn behind it.
    ///
    /// Only a turn the daemon is running can be asked about — a turn this
    /// process runs itself cannot get lost on the way here — and only once per
    /// silence, so a slow model is asked about once rather than sixteen times a
    /// second.
    fn worth_asking_if_alive(&self, patience: std::time::Duration) -> bool {
        self.busy
            && !self.asked_if_alive
            && self.remote.is_some()
            && self.session.is_some()
            && self.heard.is_some_and(|at| at.elapsed() > patience)
    }

    /// The tool the turn is in the middle of, read from the log the person is
    /// looking at: a call is written when it starts and gains its mark when it
    /// ends, so the last line answers it without a second place to keep it.
    fn running(&self) -> Option<&str> {
        let (kind, line) = self.log.last()?;
        let done = line.ends_with('✓') || line.ends_with('✗');
        (*kind == "tool" && !done).then(|| line.trim().trim_start_matches("· "))
    }

    /// Silence, named — after half a minute of it, and not before, because a
    /// pause between tokens is ordinary and a caption that cries wolf is one
    /// more thing to ignore.
    fn silence(&self, patience: std::time::Duration) -> String {
        match self.heard.map(|at| at.elapsed()).filter(|d| *d >= QUIET) {
            Some(quiet) => waiting_on(quiet, self.running(), patience),
            None => String::new(),
        }
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

/// How many typed prompts are kept between windows. The plain chat keeps its
/// own with rustyline in the same file; this is the ceiling either way, and
/// what accumulates without one is a file that grows a line per prompt for as
/// long as the agent is used.
const PROMPTS_KEPT: usize = 200;

/// What was typed in earlier windows, oldest first.
///
/// The same file the plain chat's history goes in: it is one person on one
/// machine, and an up-arrow that knows what they typed in `rook chat` and not
/// what they typed in `rook tui` is two histories of one conversation.
fn remembered_prompts() -> Vec<String> {
    let path = rook_core::paths::home().join("history");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let all: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let kept: Vec<String> = all.iter().rev().take(PROMPTS_KEPT).rev().map(|l| l.to_string()).collect();
    // Trimmed where it is read rather than where it is written: appending is
    // one line and rewriting the file is not something to do per prompt.
    if all.len() > PROMPTS_KEPT * 2 {
        let _ = std::fs::write(&path, kept.join("\n") + "\n");
    }
    kept
}

/// One more line in that file. Appended as it is typed, because a window that
/// is killed rather than closed still typed it.
fn remember_prompt(prompt: &str) {
    use std::io::Write;
    let path = rook_core::paths::home().join("history");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{prompt}");
    }
}

/// Long enough that an ordinary pause between tokens says nothing, short
/// enough to answer "has it stopped?" before anyone reaches for ctrl-c.
const QUIET: std::time::Duration = std::time::Duration::from_secs(30);

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
    /// Whether this window is taking the mouse, and so whether the terminal's
    /// own selection works.
    ///
    /// Both are wanted and only one can be had: capture is how the wheel
    /// scrolls back through what the agent said, and while it is on the
    /// terminal never sees the drag that selects a line to copy. Every terminal
    /// offers a modifier to get past it and no two agree on which — so this is
    /// a key instead, and the footer says which of the two you currently have.
    mouse: bool,
    /// Every file in the workspace, walked when a mention starts and kept until
    /// it ends. Walking per keystroke would be twenty thousand files sixty
    /// times a second to narrow a list already in hand; walking once per
    /// mention also means a file written during the turn is picked up by the
    /// next `@` rather than never.
    files_here: Option<Vec<String>>,
    events: mpsc::UnboundedReceiver<TurnEvent>,
    to_loop: mpsc::UnboundedSender<TurnEvent>,
    approver: Arc<ChannelApprover>,
    asker: Arc<ChannelAsker>,
    /// The same state the chat REPL keeps, so the slash commands are one
    /// implementation rather than two that drift.
    shared: crate::chat::Session,
    turn: Option<tokio::task::JoinHandle<()>>,
    /// `None` is the ordinary state: the conversation, whole.
    overlay: Option<Overlay>,
    /// What is typed into the palette, and where the cursor sits in its list.
    palette: Typing,
    palette_at: usize,
    /// The model this window talks to, for the footer. Read once: a file read
    /// per sixty-millisecond tick is a file read per frame.
    model: String,
    /// How many files a walk of the workspace may look at, from the same
    /// setting that caps the search tool's looking. Read once, for the reason
    /// above.
    most_files: usize,
    /// What a request may carry, so the footer can say how much of it the
    /// newest one used. Read once with the model, for the same reason.
    usable: usize,
    /// How long a stream may say nothing before the turn gives up on it.
    ///
    /// Shown beside the silence, because a wait with no end named reads as no
    /// end: on a local model a large context is minutes of prompt processing
    /// before the first token, and `nothing from the model for 12m` alone
    /// cannot be told from a turn that will sit there forever. With the
    /// deadline beside it, the same line says the wait is bounded and when.
    patience: std::time::Duration,
    sessions: Vec<SessionSummary>,
    session_state: ListState,
    /// Every call this conversation made, newest first, with what it was given
    /// and what came back. Read from the session's own log rather than kept as
    /// the turn runs: a window that attached to a daemon mid-turn saw none of
    /// the earlier ones, and the log has them all.
    calls: Vec<Call>,
    call_state: ListState,
    transcript: Vec<TranscriptEntry>,
    /// What the selected session was for and what it did, which is usually why
    /// its transcript is being read at all.
    selected: Option<Selected>,
    transcript_scroll: u16,
    facts: Vec<rook_core::Fact>,
    fact_state: ListState,
    adding: Option<Adding>,
    /// The last fact forgotten here, so `u` puts it back: a `d` on the wrong
    /// row is otherwise a version of memory nobody meant to write.
    forgotten: Option<rook_core::Fact>,
    /// What the last write did, said on the pane it changed. The footer is
    /// twenty-odd columns by the time the keys and the settings have had
    /// theirs, which is not enough to name a fact.
    fact_note: String,
    skills: Vec<SkillCard>,
    skill_state: ListState,
    /// Every captured version of the selected skill, newest first. Loaded with
    /// the selection, as a session's transcript is: it is what makes a
    /// rollback something you can see before you ask for it.
    skill_versions: Vec<rook_core::SkillVersionRecord>,
    /// What the last skill action did, said on the pane it changed.
    skill_note: String,
    /// Named snapshots of the workspace: what `checkpoint create` takes and
    /// what `checkpoint restore` puts back, which were a terminal away.
    checkpoints: Vec<(String, String)>,
    checkpoint_state: ListState,
    /// A checkpoint being named, when one is.
    naming: Option<Typing>,
    /// A restore waiting for a yes. It writes over the workspace, which is the
    /// one thing here worth asking about twice.
    restoring: Option<(String, String)>,
    checkpoint_note: String,
    /// The documentation gathered here, and the set under the cursor read out.
    /// A copy nobody can see is one nobody trusts — and the question a person
    /// actually has about it is which pages it was made from.
    docs: Vec<rook_core::docs::Kept>,
    docs_state: ListState,
    doc_set: Option<rook_core::DocSet>,
    docs_note: String,
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

        let config = config_seen_by(source.here().map(|r| &**r), rook_core::paths::config_file());
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
            chat: Chat { history: remembered_prompts(), ..Chat::default() },
            files_here: None,
            most_files: config.sandbox.max_files_searched,
            // Only when the window is configured. Otherwise it is the
            // provider's to report, and a share of a guess is worse than no
            // share at all.
            usable: config
                .agent
                .context_window
                .map(|window| {
                    rook_core::context::ContextBudget::new(window, config.agent.compact_at).usable()
                })
                .unwrap_or(0),
            patience: config.agent.stream_idle(),
            events,
            to_loop,
            approver: Arc::new(ChannelApprover::new(requests, patience)),
            asker: Arc::new(ChannelAsker::new(questions, config.agent.decide_alone_after())),
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
            // Enabled by `run` before this window draws; a window that could
            // not take the mouse says so by leaving this false, and the footer
            // then offers nothing to toggle.
            mouse: false,
            turn: None,
            overlay: None,
            palette: Typing::default(),
            palette_at: 0,
            model: rook_core::Config::load().map(|c| c.agent.model).unwrap_or_default(),
            sessions: Vec::new(),
            session_state: ListState::default(),
            calls: Vec::new(),
            call_state: ListState::default(),
            transcript: Vec::new(),
            selected: None,
            transcript_scroll: 0,
            facts: Vec::new(),
            fact_state: ListState::default(),
            adding: None,
            forgotten: None,
            fact_note: String::new(),
            skills: Vec::new(),
            skill_state: ListState::default(),
            skill_versions: Vec::new(),
            skill_note: String::new(),
            checkpoints: Vec::new(),
            docs: Vec::new(),
            docs_state: ListState::default(),
            doc_set: None,
            docs_note: String::new(),
            checkpoint_state: ListState::default(),
            naming: None,
            restoring: None,
            checkpoint_note: String::new(),
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
        self.checkpoints = self.source.checkpoints().unwrap_or_default();
        let last = self.checkpoints.len().saturating_sub(1);
        self.checkpoint_state.select(
            (!self.checkpoints.is_empty()).then(|| self.checkpoint_state.selected().unwrap_or(0).min(last)),
        );
        self.load_calls();
        self.docs = self.source.docs_kept().unwrap_or_default();
        let last = self.docs.len().saturating_sub(1);
        self.docs_state
            .select((!self.docs.is_empty()).then(|| self.docs_state.selected().unwrap_or(0).min(last)));
        self.load_doc_set();
        self.objects = self
            .source
            .objects(None, 300)
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.short, row.kind, row.size_raw, row.size_stored))
            .collect();
        // Clamped rather than kept: forgetting the last fact leaves the
        // selection past the end, and the next `d` would find nothing there.
        let last = self.facts.len().saturating_sub(1);
        self.fact_state
            .select((!self.facts.is_empty()).then(|| self.fact_state.selected().unwrap_or(0).min(last)));
        if self.session_state.selected().is_none() && !self.sessions.is_empty() {
            self.session_state.select(Some(0));
        }
        if self.skill_state.selected().is_none() && !self.skills.is_empty() {
            self.skill_state.select(Some(0));
        }
        self.load_transcript();
        self.load_versions();
        self.status = format!(
            "{} sessions · {} skills · {} on disk",
            self.sessions.len(),
            self.skills.len(),
            self.stats.as_ref().map(|s| fmt::bytes(s.disk_bytes())).unwrap_or_default()
        );
    }

    /// The captured versions of the skill under the cursor.
    fn load_versions(&mut self) {
        self.skill_versions.clear();
        let Some(name) = self.selected_skill() else { return };
        self.skill_versions = self.source.skill_history(&name).unwrap_or_default();
    }

    fn selected_skill(&self) -> Option<String> {
        self.skill_state.selected().and_then(|at| self.skills.get(at)).map(|card| card.name.clone())
    }

    /// The calls this conversation made, newest first, paired with their
    /// results.
    ///
    /// Paired by order within a tool name, which is the same rule every front
    /// end uses: a result carries only the name, and a turn that reads two files
    /// at once produces two `read_file` results that are told apart by nothing
    /// else. Read from the log rather than accumulated as the turn streams,
    /// because a window that attached to a daemon mid-turn never saw the
    /// earlier calls and the log has them all.
    fn load_calls(&mut self) {
        self.calls.clear();
        self.call_state.select(None);
        let Some(session) = self.chat.session else { return };
        // Enough of a result to answer the question, not the whole object: a
        // command's output can be megabytes, and `store cat` is what reads one
        // of those whole.
        let entries = self.source.transcript(session, 0, 2_000, 8_000).unwrap_or_default();
        self.calls = paired(entries);
        self.call_state.select((!self.calls.is_empty()).then_some(0));
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
            self.still_running();
            // Poll rather than block: a streaming turn has to keep redrawing
            // even while nobody is typing.
            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                    Event::Paste(text) => self.on_paste(&text),
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
        self.chat.ended();
        self.status = "turn stopped".into();
    }

    /// Asks the daemon whether the turn this window is drawing is still
    /// running — once, after a silence longer than the turn itself would wait
    /// for the model.
    ///
    /// A window is told when a turn ends and should not have to ask. But being
    /// told is one message, and one message can be lost: a turn's ending was
    /// put in a queue and the task that emptied the queue was aborted in the
    /// same breath, so a window that had streamed eleven minutes of work went
    /// on drawing `working…` over a turn the daemon had already finished. That
    /// particular race is fixed. Asking is for the next one, because the
    /// failure it produces — patience — is the one a person cannot tell from
    /// the thing working.
    fn still_running(&mut self) {
        if !self.chat.worth_asking_if_alive(self.patience) {
            return;
        }
        let (Some(remote), Some(session)) = (self.chat.remote.clone(), self.chat.session) else {
            return;
        };
        self.chat.asked_if_alive = true;
        // A send that fails is the answer: the connection carrying this turn is
        // gone, so nothing will ever arrive on it and no question can be put to
        // it either. Without this the window asked into a closed channel and
        // went on drawing `working…` — which is the failure this whole check
        // exists to end, reached by a different road.
        if remote.send(ClientMessage::Attach { session: rook_store::format_session_id(session) }).is_err() {
            self.chat.push(
                "err",
                "[lost the connection to the daemon — it may have been restarted, and this turn \
                 may still be running there. `^p` → sessions → this one rejoins it]",
            );
            self.chat.ended();
        }
    }

    fn drain_turn_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.chat.heard = Some(std::time::Instant::now());
            match event {
                TurnEvent::Started(id) => self.chat.session = Some(id),
                TurnEvent::Text(text) => self.chat.push("text", &text),
                TurnEvent::Reasoning(text) => self.chat.push("think", &text),
                TurnEvent::Tool { name, said } => self.chat.tool_started(&name, &said),
                TurnEvent::Agent(line) => self.chat.push("agent", &line),
                TurnEvent::Step(at, of) => self.chat.step = Some((at, of)),
                TurnEvent::ToolDone(name, failed) => self.chat.tool_done(&name, failed),
                TurnEvent::Spent { input, output, cached } => {
                    self.chat.carried = input.saturating_sub(self.chat.spent.map_or(0, |(was, ..)| was));
                    self.chat.spent = Some((input, output, cached));
                }
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
                    self.finished();
                }
                TurnEvent::Error(message) => {
                    self.chat.push("err", &message);
                    self.finished();
                }
                TurnEvent::FromDaemon(event) => self.heard_from_daemon(*event),
            }
            // After the event has been read, not before: the answer to "is it
            // still running?" is one of these, and clearing the question first
            // would leave the answer looking like somebody else's.
            self.chat.asked_if_alive = false;
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
            // Joined a turn this window did not start. Said out loud: a window
            // that opens onto a session and finds it already working looks,
            // for a second, like a window answering something you did not ask.
            ChatEvent::Attached { session, running } => {
                self.chat.session = rook_store::parse_session_id(&session);
                // A window that asked because it had been drawing `working…`
                // over a long silence is answering a different question, and
                // only one of the two answers is news.
                let worried = self.chat.asked_if_alive && self.chat.busy;
                match (running, worried) {
                    (true, true) => {}
                    (false, true) => {
                        self.chat.push(
                            "stat",
                            "[this turn is over — the daemon has nothing running in this session. \
                             Its ending never reached this window, so what is above may be short of \
                             where it stopped; `session show` has the whole of it]",
                        );
                        self.chat.ended();
                    }
                    (true, false) => {
                        self.chat.push("stat", "[joined a turn already running here]");
                        self.chat.began();
                    }
                    (false, false) => self.chat.push("stat", "[nothing is running in this session]"),
                }
            }
            ChatEvent::Text { text } => self.chat.push("text", &text),
            ChatEvent::Reasoning { text } => self.chat.push("think", &text),
            ChatEvent::Tool { name, doing } => {
                let said = from_daemon(&name, &doing);
                self.chat.tool_started(&name, &said)
            }
            // The daemon sends the tool's name and not its arguments, so a
            // window attached to one says less than a window running the turn
            // itself. What it must not do is say it twice.
            ChatEvent::ToolDone { name, failed } => self.chat.tool_done(&name, failed),
            ChatEvent::Step { at, of } => self.chat.step = Some((at, of)),
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
            ChatEvent::Done { steps, input_tokens, output_tokens, files_changed, stopped, .. } => {
                if let Some(why) = rook_core::agent::why_it_stopped(&stopped) {
                    self.chat.push("stat", &format!("  {why}"));
                }
                if let Some(note) = rook_core::agent::changed_note(&files_changed) {
                    self.chat.push("stat", &format!("  {note}"));
                }
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

    /// A turn is over — answered, stopped or failed.
    fn finished(&mut self) {
        self.chat.ended();
        self.reload();
    }

    /// Give the mouse to the terminal, or take it back.
    ///
    /// A failure leaves the flag where it was rather than claiming the swap:
    /// a footer that says the wheel is yours while the terminal is still
    /// selecting is worse than one that never changed.
    fn take_or_yield_the_mouse(&mut self) {
        let swapped = match self.mouse {
            true => execute!(std::io::stdout(), event::DisableMouseCapture).is_ok(),
            false => execute!(std::io::stdout(), event::EnableMouseCapture).is_ok(),
        };
        if swapped {
            self.mouse = !self.mouse;
        }
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
        // The two settings worth changing mid-turn, from wherever you are.
        match key.code {
            KeyCode::F(2) => return self.cycle_stance(),
            KeyCode::F(3) => return self.cycle_effort(),
            _ => {}
        }
        // `^p` from anywhere, including from inside another overlay: a palette
        // you have to close something else to reach is a palette nobody uses.
        // `^p` alone, and not `^k` beside it: `^k` kills to the end of the line
        // and has done in every shell for fifty years — taking it for a palette
        // broke the message box, which a test noticed and a person would have
        // noticed sooner.
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('p') {
            self.palette.clear();
            self.palette_at = 0;
            self.overlay = Some(Overlay::Palette);
            return;
        }
        // `^s` hands the mouse back to the terminal so a line can be selected
        // and copied, and takes it again for the wheel. Not a modifier held
        // while dragging: every terminal has one and no two agree on which —
        // Shift here, Option there, a setting somewhere else — so the answer
        // to "I cannot copy what it said" was a different answer per terminal
        // and none of them was in this window.
        //
        // `^s` is safe to take: raw mode clears `IXON`, so it arrives as a key
        // rather than stopping the terminal's output as it would in a shell.
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('s') {
            return self.take_or_yield_the_mouse();
        }
        match self.overlay {
            Some(overlay) => self.on_overlay_key(overlay, key),
            None => self.on_chat_key(key),
        }
    }

    /// Keys while a pane is up. Esc closes, and every pane's own keys are the
    /// ones it had as a tab — what changed is how you get to it.
    /// Text pasted into whichever box is taking typing.
    ///
    /// Whole, and never sent: a paste is material to work with, and deciding it
    /// was a prompt because it ended in a newline is what made pasting a
    /// paragraph impossible. An approval has the keyboard and takes no text, so
    /// a paste arriving there is dropped rather than typed into a box nobody can
    /// see.
    fn on_paste(&mut self, text: &str) {
        match self.overlay {
            Some(Overlay::Palette) => self.palette.paste(text),
            // The one-line boxes take a paste as one line: a fact or a topic
            // with a newline in it is not two of them.
            Some(Overlay::Memory) => {
                if let Some(adding) = &mut self.adding {
                    adding.text.paste(&text.replace('\n', " "));
                }
            }
            Some(_) => {}
            None if self.chat.pending.is_none() => self.chat.input.paste(text),
            None => {}
        }
    }

    fn on_overlay_key(&mut self, overlay: Overlay, key: crossterm::event::KeyEvent) {
        if key.code == KeyCode::Esc {
            self.overlay = None;
            return;
        }
        if overlay == Overlay::Palette {
            return self.on_palette_key(key);
        }
        if overlay == Overlay::Memory && self.on_memory_key(key) {
            return;
        }
        if overlay == Overlay::Skills && self.on_skill_key(key) {
            return;
        }
        if overlay == Overlay::Checkpoints && self.on_checkpoint_key(key) {
            return;
        }
        if overlay == Overlay::Docs
            && key.code == KeyCode::Char('d')
            && let Some(set) = self.docs_state.selected().and_then(|at| self.docs.get(at)).cloned()
        {
            self.docs_note = match self.source.forget_docs(&set.topic, Some(&set.version)) {
                Ok(0) => format!("{} {} was already gone", set.topic, set.version),
                Ok(_) => {
                    format!("dropped {} {} — the agent will read it again when asked", set.topic, set.version)
                }
                Err(e) => format!("could not drop it: {e}"),
            };
            self.reload();
            return;
        }
        match key.code {
            // The session under the cursor, taken up in the chat — and the
            // overlay closes, because continuing one is a thing you do in the
            // conversation, which is now underneath.
            KeyCode::Enter if overlay == Overlay::Sessions => {
                self.continue_selected();
                self.overlay = None;
            }
            KeyCode::Char('q') => self.overlay = None,
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

    /// Typing in the palette narrows the list; enter takes what is under the
    /// cursor.
    fn on_palette_key(&mut self, key: crossterm::event::KeyEvent) {
        let found = self.palette_entries();
        match key.code {
            KeyCode::Enter => {
                let Some((name, _)) = found.get(self.palette_at.min(found.len().saturating_sub(1))) else {
                    return;
                };
                let name = name.clone();
                self.overlay = None;
                match name.strip_prefix('/') {
                    // A command goes to the message box rather than running
                    // itself: several take an argument, and one that ran the
                    // moment it was chosen would be a command with no way to
                    // give it one.
                    Some(command) => {
                        let bare = command.split_whitespace().next().unwrap_or(command);
                        self.chat.input.set(&format!("/{bare} "));
                    }
                    None => {
                        self.overlay = Overlay::PANES.iter().copied().find(|pane| pane.name() == name);
                        self.reload();
                    }
                }
            }
            KeyCode::Down => self.palette_at = (self.palette_at + 1).min(found.len().saturating_sub(1)),
            KeyCode::Up => self.palette_at = self.palette_at.saturating_sub(1),
            KeyCode::Backspace => {
                self.palette.backspace();
                self.palette_at = 0;
            }
            KeyCode::Char(c) => {
                self.palette.insert(c);
                self.palette_at = 0;
            }
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
                // Not a line-editing key, but it belongs to this branch: the
                // conversation says a call happened and this is the one gesture
                // that says what it did. `o` for what came out of it.
                KeyCode::Char('o') => {
                    self.load_calls();
                    self.transcript_scroll = 0;
                    self.overlay = Some(Overlay::Calls);
                    return;
                }
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
            // Tab completes, and does nothing else. It used to leave the
            // conversation for the next tab, which is not what Tab means in
            // any other box somebody has ever typed into.
            KeyCode::Tab => self.complete(),
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
            // Within the message first: the box holds several rows, and a key
            // that leaves it takes what is written with it.
            KeyCode::Up => {
                if !self.chat.input.up() {
                    self.recall(-1);
                }
            }
            KeyCode::Down => {
                if !self.chat.input.down() {
                    self.recall(1);
                }
            }
            // A newline by hand, since Enter sends. Shift+Enter is what a hand
            // reaches for and almost no terminal tells it from Enter; Alt is
            // the modifier that actually arrives.
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => self.chat.input.insert('\n'),
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
        // Kept before the first step of the walk, which is the only step that
        // has it to lose.
        if self.chat.recalled.is_none() {
            self.chat.draft = self.chat.input.as_str().to_string();
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
            None => {
                let draft = std::mem::take(&mut self.chat.draft);
                self.chat.input.set(&draft);
            }
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
        let opening = ClientMessage::Prompt {
            session: self.chat.session.map(rook_store::format_session_id),
            text: prompt,
        };
        // On the socket this window already has, if it has one: attaching to a
        // session opens one before there is a prompt, and a second socket would
        // be a second view of the same daemon arguing with the first.
        if let Some(say) = &self.chat.remote {
            let _ = say.send(opening);
            return;
        }
        if !self.talk_to_daemon(opening) {
            self.chat.busy = false;
        }
    }

    /// Open a socket to the daemon and say one thing on it.
    ///
    /// `false` when there is no daemon to say it to. Everything after the
    /// opening message arrives as events, which is why this does not wait for
    /// an answer: the loop reads them like any other.
    fn talk_to_daemon(&mut self, opening: ClientMessage) -> bool {
        let Some(base) = self.source.daemon_base().map(|b| b.to_string()) else {
            self.chat.push("err", "no daemon to run this");
            return false;
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
        let _ = say.send(opening);
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
        true
    }

    /// The files the mention being typed names, best first — and nothing at
    /// all when no mention is being typed.
    ///
    /// The walk happens on the first call of a mention and is kept until the
    /// mention ends, because the fragment grows a character at a time and the
    /// answer narrows from the same set each time.
    fn mentioned(&mut self) -> Vec<String> {
        let Some(fragment) = self.chat.input.mentioning().map(str::to_string) else {
            self.files_here = None;
            return Vec::new();
        };
        if self.files_here.is_none() {
            let workspace = self.source.workspace().to_path_buf();
            self.files_here = Some(rook_core::mention::here(&workspace, self.most_files));
        }
        let here = self.files_here.as_deref().unwrap_or_default();
        rook_core::mention::matching(here, &fragment, 8)
    }

    /// Finish the command being typed, as far as it is unambiguous.
    ///
    /// One match completes to the name and, where it takes arguments, a space
    /// to type them after. Several complete to what they share, which is how a
    /// person discovers `/se` is two commands rather than a typo.
    fn complete(&mut self) {
        // A file being named takes the key first: `/` starts a line and `@` is
        // in the middle of one, so only one of the two can be being typed.
        let files = self.mentioned();
        if let Some((first, rest)) = files.split_first() {
            let common = rest.iter().fold(first.clone(), |common: String, path| {
                common.chars().zip(path.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
            });
            // One match is finished, several complete to what they share —
            // which is how a directory of near-identical names narrows without
            // the list being read.
            match files.len() {
                1 => self.chat.input.mention(first),
                _ => self.chat.input.narrow(&common),
            }
            return;
        }
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
        match (self.overlay, wheel) {
            (None, MouseEventKind::ScrollUp) => self.chat.scroll = self.chat.scroll.saturating_add(BY),
            (None, MouseEventKind::ScrollDown) => self.chat.scroll = self.chat.scroll.saturating_sub(BY),
            (Some(Overlay::Sessions), MouseEventKind::ScrollUp) => {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(BY)
            }
            (Some(Overlay::Sessions), MouseEventKind::ScrollDown) => {
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
        let was = self.chat.session;
        let mut session = was.unwrap_or_else(|| rook.start_session("").unwrap_or_default());
        let said = self.runtime.block_on(crate::chat::dispatch(&rook, &mut session, &self.shared, command));
        // A command that moved this window to another session — `/session
        // <id>`, `/new` — takes the pane with it. Continuing a conversation in
        // a window showing somebody else's is continuing it blind.
        if was.is_some_and(|before| before != session) {
            self.recall_conversation(session);
        }
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

    /// Continue the selected session in the chat tab.
    ///
    /// Works in a window that reads through a daemon as well, where the slash
    /// command cannot: switching is this window's own state and the transcript
    /// is a routed read, so neither needs the store to be here.
    fn continue_selected(&mut self) {
        if self.chat.busy {
            self.chat.push("stat", "  a turn is running here — stop it or let it finish first");
            self.overlay = None;
            return;
        }
        let Some(session) = self.session_state.selected().and_then(|at| self.sessions.get(at)) else {
            return;
        };
        let (id, title) = (session.meta.id, session.meta.title.clone());
        self.chat.session = Some(id);
        self.recall_conversation(id);
        self.chat.push(
            "stat",
            &format!(
                "  continuing {} — {}",
                rook_store::format_session_id(id),
                match title.trim() {
                    "" => "(untitled)",
                    named => named,
                }
            ),
        );
        // And join whatever the daemon is running there. A turn belongs to the
        // daemon, so a session switched to may already be working — and a
        // window that showed its transcript and none of its progress was the
        // half of this that made switching useless.
        let joining = ClientMessage::Attach { session: rook_store::format_session_id(id) };
        match &self.chat.remote {
            Some(say) => {
                let _ = say.send(joining);
            }
            None if self.source.daemon_base().is_some() => {
                self.talk_to_daemon(joining);
            }
            None => {}
        }
        self.overlay = None;
    }

    /// The tail of a session's transcript, in the chat pane, as the window
    /// would have shown it had it been here.
    ///
    /// The tail because that is where a conversation is picked up, and bounded
    /// for the reason the scrollback is: the store holds all of it and the
    /// Sessions tab reads it back.
    fn recall_conversation(&mut self, session: u128) {
        const RECALLED: usize = 60;
        self.chat.log.clear();
        self.chat.scroll = 0;
        let events = self
            .sessions
            .iter()
            .find(|s| s.meta.id == session)
            .map(|s| s.meta.event_count)
            .unwrap_or_default();
        let from = events.saturating_sub(RECALLED as u64);
        for entry in self.source.transcript(session, from, RECALLED, 4_000).unwrap_or_default() {
            match entry.kind.as_str() {
                "user" => self.chat.push("you", &entry.body),
                "assistant" => self.chat.push("text", &entry.body),
                // What it was doing, the same words as while it was running.
                "tool-call" => {
                    self.chat.push("tool", &format!("  · {}", rook_core::calls::within(&entry.doing, 72)))
                }
                _ => {}
            }
        }
        if self.chat.log.is_empty() {
            self.chat.push("stat", "  (nothing said in this session yet)");
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
            remember_prompt(&prompt);
        }
        self.chat.recalled = None;
        self.chat.draft.clear();
        // Typed while a turn runs, it goes to the turn. It used to be dropped
        // where it was taken, so watching one go the wrong way left nothing to
        // do but stop it and start again.
        if self.chat.busy {
            self.shared.interjections.say(&prompt);
            self.chat.push("you", &prompt);
            self.chat.push("stat", "  (the turn will see this at its next step)");
            self.chat.scroll = 0;
            return;
        }
        // `/btw` is a turn without tools, so it goes down the normal path; every
        // other slash command is answered here, by the same code the plain CLI
        // runs.
        // A command is one line. Pasted text that happens to start with a path
        // is not `/Users/...` the command, and reading it as one would answer a
        // paste with "no such command".
        // `/continue` is a prompt rather than a command: what it does is start
        // a turn, in the session already open, with a fresh allowance. A limit
        // is not a verdict on the task and the work is still in the session.
        let prompt = match rook_core::agent::carrying_on(&prompt) {
            true => rook_core::agent::CARRY_ON.to_string(),
            false => prompt,
        };
        if let Some(command) =
            prompt.strip_prefix('/').filter(|c| !c.starts_with("btw ") && !c.contains('\n'))
        {
            self.chat.push("you", &prompt);
            return self.command(command);
        }
        let aside = prompt.strip_prefix("/btw ").map(|q| q.trim().to_string());
        self.chat.push("you", &prompt);
        self.chat.began();
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
            // Before the loop borrows the agent: a call names its paths the way
            // somebody standing in this project would.
            let here = rook.workspace.clone();
            let result = agent
                .run_with(&prompt, |progress| {
                    let event = match progress {
                        Progress::Delta(Delta::Text(text)) => TurnEvent::Text(text.clone()),
                        Progress::Delta(Delta::Reasoning(text)) => TurnEvent::Reasoning(text.clone()),
                        Progress::Delta(Delta::ToolCall(call)) => TurnEvent::Tool {
                            name: call.name.clone(),
                            said: tool_line(&call.name, Some(&call.arguments), &here),
                        },
                        Progress::Delegated { task, done, total } => {
                            TurnEvent::Agent(format!("  [{done}/{total}] {task}"))
                        }
                        Progress::Delegating { at, doing } => {
                            TurnEvent::Agent(format!("    {}", rook_core::calls::delegating(at, doing)))
                        }
                        Progress::Step { at, of } => TurnEvent::Step(at, of),
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
                    "{}{}{}[{} steps · {} in / {} out{}]",
                    rook_core::agent::why_it_stopped(&outcome.stopped)
                        .map(|why| format!("  {why}\n"))
                        .unwrap_or_default(),
                    outcome.changed_note().map(|n| format!("  {n}\n")).unwrap_or_default(),
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

    /// Versioning a skill was `rook skills capture` and `rook skills rollback`
    /// in another terminal, with the object id read off a third command's
    /// output. The tab that lists the skills and their versions is where both
    /// belong. Returns whether the key was this tab's.
    fn on_skill_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('c') => self.capture_selected_skill(),
            KeyCode::Char('u') => self.roll_back_selected_skill(),
            _ => return false,
        }
        true
    }

    /// Take a version of the skill under the cursor, as it is on disk now.
    fn capture_selected_skill(&mut self) {
        let Some(name) = self.selected_skill() else { return };
        let said = match self.source.capture_skill(&name, Some("captured from the window".into())) {
            Ok((set, object)) => format!(
                "captured {name} as {} — {} file(s)",
                &object[..12.min(object.len())],
                set.files.len()
            ),
            Err(e) => e.to_string(),
        };
        self.skill_note = said;
        self.reload();
    }

    /// Back to the newest capture — which is what an undo means here, and what
    /// the id in `skills rollback <name> <object>` usually spells.
    ///
    /// Rolling back captures what is there first, so this is itself undoable;
    /// the note names that capture, because it is the only way back.
    fn roll_back_selected_skill(&mut self) {
        let Some(name) = self.selected_skill() else { return };
        let Some(newest) = self.skill_versions.first().map(|v| v.object.clone()) else {
            self.skill_note = format!("{name} has no captured version to go back to — `c` takes one");
            return;
        };
        let said = match self.source.rollback_skill(&name, &newest) {
            Ok(done) => format!(
                "rolled {name} back to {} — {} file(s){}",
                &newest[..12.min(newest.len())],
                done.restored,
                match done.undo {
                    Some(undo) => format!(", and what was there is {}", undo.short()),
                    None => String::new(),
                }
            ),
            Err(e) => e.to_string(),
        };
        self.skill_note = said;
        self.reload();
    }

    /// The Memory tab writes as well as reads. The browser can already forget
    /// and the command line can do both, and what the agent believes is the
    /// one thing a person most needs to correct where they are reading it.
    ///
    /// Returns whether the key was this tab's — while a fact is being typed
    /// every key is, `j` and `q` included.
    fn on_memory_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match (self.adding.is_some(), key.code) {
            (false, KeyCode::Char('a')) => self.adding = Some(Adding::default()),
            (false, KeyCode::Char('A')) => self.adding = Some(Adding { global: true, ..Adding::default() }),
            (false, KeyCode::Char('d')) => self.forget_selected(),
            (false, KeyCode::Char('u')) => self.restore_forgotten(),
            (false, _) => return false,
            (true, KeyCode::Enter) => self.remember_typed(),
            (true, KeyCode::Esc) => self.adding = None,
            (true, code) => {
                if let Some(adding) = &mut self.adding {
                    match code {
                        KeyCode::Backspace => adding.text.backspace(),
                        KeyCode::Delete => adding.text.delete(),
                        KeyCode::Left => adding.text.left(),
                        KeyCode::Right => adding.text.right(),
                        KeyCode::Home => adding.text.home(),
                        KeyCode::End => adding.text.end(),
                        KeyCode::Char(c) => adding.text.insert(c),
                        _ => {}
                    }
                }
            }
        }
        true
    }

    fn remember_typed(&mut self) {
        let Some(adding) = self.adding.take() else { return };
        let text = adding.text.as_str().trim().to_string();
        if text.is_empty() {
            return;
        }
        let workspace = self.source.workspace().to_path_buf();
        let scope = match adding.global {
            true => rook_core::Scope::Global,
            false => rook_core::Scope::Project(workspace.display().to_string()),
        };
        let fact = rook_core::Fact::new(text, scope);
        let id = fact.id.clone();
        use rook_core::memory::Learned;
        let said = match self.source.remember(fact, &workspace) {
            Ok(Learned::Unchanged) => format!("already remembered as [{id}]"),
            Ok(Learned::ScopedElsewhere(scope)) => {
                format!("[{id}] stays scoped to {}; A adds it globally", scope.label())
            }
            Ok(_) => format!("remembered as [{id}]"),
            Err(e) => e.to_string(),
        };
        self.note(said);
    }

    fn forget_selected(&mut self) {
        let Some(fact) = self.fact_state.selected().and_then(|at| self.facts.get(at)).cloned() else {
            return;
        };
        let said = match self.source.forget(&fact.id) {
            Ok(Some(gone)) => {
                let text = gone.text.chars().take(40).collect::<String>();
                self.forgotten = Some(gone);
                format!("forgot {text:?}; u puts it back")
            }
            Ok(None) => format!("no fact [{}]", fact.id),
            Err(e) => e.to_string(),
        };
        self.note(said);
    }

    /// Back with its tags and its pinning, because it goes back as the fact it
    /// was rather than as one typed again from what the screen showed.
    fn restore_forgotten(&mut self) {
        let Some(fact) = self.forgotten.take() else {
            return self.note("nothing forgotten here to put back".into());
        };
        let workspace = self.source.workspace().to_path_buf();
        let id = fact.id.clone();
        let said = match self.source.remember(fact, &workspace) {
            Ok(_) => format!("[{id}] is back"),
            Err(e) => e.to_string(),
        };
        self.note(said);
    }

    fn note(&mut self, said: String) {
        self.fact_note = said;
        self.reload();
    }

    fn move_selection(&mut self, delta: isize) {
        let (state, len) = match self.overlay {
            Some(Overlay::Calls) => (&mut self.call_state, self.calls.len()),
            Some(Overlay::Sessions) => (&mut self.session_state, self.sessions.len()),
            Some(Overlay::Memory) => (&mut self.fact_state, self.facts.len()),
            Some(Overlay::Checkpoints) => (&mut self.checkpoint_state, self.checkpoints.len()),
            Some(Overlay::Docs) => (&mut self.docs_state, self.docs.len()),
            Some(Overlay::Skills) => (&mut self.skill_state, self.skills.len()),
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
        // What the selection is worth reading alongside, loaded with it: a
        // transcript, a skill's versions, a set's pages.
        match self.overlay {
            // A different call, read from the top of its own detail.
            Some(Overlay::Calls) => self.transcript_scroll = 0,
            Some(Overlay::Sessions) => self.load_transcript(),
            Some(Overlay::Skills) => self.load_versions(),
            Some(Overlay::Docs) => self.load_doc_set(),
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        // The conversation and one line under it. No tab bar: what somebody
        // came here to do is talk to the agent, and eight names across the top
        // of every screen are eight names in the way of it.
        let [body, footer] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
        self.draw_chat(f, body);

        // Over the conversation rather than instead of it: what is underneath
        // stays visible at the edges, so opening a pane does not read as
        // having left the place you were.
        if let Some(overlay) = self.overlay {
            // Wide enough that a two-pane view is still readable — these
            // were laid out for a whole screen, and a session's row lost the
            // end of its workspace name at 88 — and short enough that the
            // conversation is visibly still there behind it.
            let area = centred(f.area(), 94, 86);
            f.render_widget(Clear, area);
            match overlay {
                Overlay::Palette => self.draw_palette(f, area),
                Overlay::Calls => self.draw_calls(f, area),
                Overlay::Sessions => self.draw_sessions(f, area),
                Overlay::Memory => self.draw_memory(f, area),
                Overlay::Skills => self.draw_skills(f, area),
                Overlay::Store => self.draw_store(f, area),
                Overlay::Checkpoints => self.draw_checkpoints(f, area),
                Overlay::Docs => self.draw_docs(f, area),
                Overlay::Help => self.draw_help(f, area),
            }
        }

        let keys: &[(&str, &str)] = match self.overlay {
            Some(overlay) => overlay.keys(),
            // The mouse hint says what pressing it gives you, which is also
            // how this window says which of the two it is holding: a wheel
            // that has stopped scrolling reads as an application that has
            // stopped responding, and this is the line that explains it.
            None if self.mouse => {
                &[("^p ", "commands  "), ("^o ", "calls  "), ("^s ", "select  "), ("^c ", "stop  ")]
            }
            None => &[("^p ", "commands  "), ("^o ", "calls  "), ("^s ", "wheel  "), ("^c ", "stop  ")],
        };
        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        for (key, what) in keys {
            spans.push(Span::styled(*key, Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(*what));
        }
        // What this window is talking to and under what rules — the two things
        // a person glances down to check, and neither was on the screen.
        spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(short_model(&self.model), Style::default().fg(Color::LightBlue)));
        spans.push(Span::styled(
            format!("  {}/{}", self.shared.policy.stance().as_str(), self.shared.effort.get().as_str()),
            Style::default().fg(Color::DarkGray),
        ));
        let tail = format!("  {}  {}", spent(self.chat.spent, self.chat.carried, self.usable), self.status);
        spans.push(Span::styled(tail, Style::default().fg(Color::DarkGray)));

        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray)),
            footer,
        );
    }

    /// Everything reachable from here, filtered as it is typed.
    ///
    /// The panes and the slash commands in one list, because from where a
    /// person stands they are one question — "what can I do from here" — and
    /// answering it in two places is how the commands ended up discoverable
    /// only from `/help`, which is where you look after giving up.
    fn draw_palette(&mut self, f: &mut Frame, area: Rect) {
        let [entry, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
        let typed = self.palette.as_str().to_string();
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("› ", Style::default().fg(Color::Cyan)),
                Span::raw(typed.clone()),
            ]))
            .block(bordered(" what would you like to do ")),
            entry,
        );
        f.set_cursor_position((entry.x + 3 + self.palette.column(), entry.y + 1));

        let found = self.palette_entries();
        let items: Vec<ListItem> = found
            .iter()
            .map(|(name, what)| {
                // A column and at least one space after it: `/session
                // [id|last]` is exactly as wide as the column, and ran into
                // its own description.
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{name:<19} "), Style::default().fg(Color::Cyan)),
                    Span::styled(what.clone(), Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        state.select((!items.is_empty()).then_some(self.palette_at.min(items.len().saturating_sub(1))));
        match items.is_empty() {
            true => f.render_widget(
                Paragraph::new("nothing matches that")
                    .style(Style::default().fg(Color::DarkGray))
                    .block(bordered(" ")),
                list,
            ),
            false => f.render_stateful_widget(
                List::new(items)
                    .block(bordered(" "))
                    .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                    .highlight_symbol("▌"),
                list,
                &mut state,
            ),
        }
    }

    /// The panes and the commands that match what has been typed.
    ///
    /// A pane is named as itself; a command keeps its slash, so what lands in
    /// the message box is what would have been typed there anyway.
    fn palette_entries(&self) -> Vec<(String, String)> {
        let typed = self.palette.as_str().trim().trim_start_matches('/').to_lowercase();
        let matches = |name: &str, what: &str| {
            typed.is_empty() || name.contains(&typed) || what.to_lowercase().contains(&typed)
        };
        let mut out: Vec<(String, String)> = Overlay::PANES
            .iter()
            .filter(|pane| matches(pane.name(), pane.what()))
            .map(|pane| (pane.name().to_string(), pane.what().to_string()))
            .collect();
        out.extend(
            crate::chat::commands_matching("/")
                .into_iter()
                .filter(|(name, _, what)| matches(name, what))
                .map(|(name, args, what)| {
                    let name = match args.is_empty() {
                        true => format!("/{name}"),
                        false => format!("/{name} {args}"),
                    };
                    (name, (*what).to_string())
                }),
        );
        out
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
        let mentioned = self.mentioned();
        let blocking = match (&self.chat.pending, &self.chat.asking) {
            (Some(request), _) => approval_height(request, area.width, area.height),
            (_, Some(asking)) => asking.panel().len() as u16 + 2,
            _ if !mentioned.is_empty() => ((mentioned.len() + 2) as u16).min((area.height / 2).max(3)),
            _ if !completing.is_empty() => ((completing.len() + 2) as u16).min((area.height / 2).max(3)),
            _ => 0,
        };
        // The box grows with what is in it, because a pasted paragraph is one
        // prompt and a person editing it has to see it. Capped, since the
        // conversation is what the window is for: past this the box scrolls.
        const MOST_ROWS: u16 = 10;
        let typed = self.chat.input.rows().clamp(1, MOST_ROWS) + 2;
        let [log, ask, input] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(blocking), Constraint::Length(typed)])
                .areas(area);

        let mut lines: Vec<Line> = Vec::new();
        for (kind, body) in &self.chat.log {
            let style = match *kind {
                "you" => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                "tool" => Style::default().fg(Color::Magenta),
                "agent" => Style::default().fg(Color::LightBlue),
                "think" => Style::default().fg(THINKING).add_modifier(Modifier::ITALIC),
                "stat" => Style::default().fg(Color::DarkGray),
                "err" => Style::default().fg(Color::Red),
                _ => Style::default(),
            };
            // A bar down the left of what somebody said, and of what went
            // wrong. Colour alone told these apart, which is a difference a
            // person has to remember rather than see — and on a screen of
            // wrapped paragraphs the eye needs an edge to find where a message
            // starts. The model's own words get none: they are the voice this
            // pane is for, and a marker on everything marks nothing.
            let gutter = match *kind {
                "you" => Some(("▌ ", Style::default().fg(Color::Cyan))),
                "err" => Some(("▌ ", Style::default().fg(Color::Red))),
                "think" => Some(("┆ ", Style::default().fg(THINKING))),
                _ => None,
            };
            // The model's own words are the only ones with Markdown in them:
            // a tool line or a note is written here and says what it says.
            match gutter {
                // Wrapped here rather than by the paragraph, so the bar is on
                // every row of the block and not only its first: a marker that
                // stops after one line marks a line, and what is being marked
                // is a message. These are the kinds with no Markdown in them,
                // which is what makes wrapping them here safe.
                Some((bar, bar_style)) => {
                    let room = log.width.saturating_sub(2 + bar.chars().count() as u16) as usize;
                    for line in wrapped(body, room.max(8)) {
                        lines.push(Line::from(vec![Span::styled(bar, bar_style), Span::styled(line, style)]));
                    }
                }
                None => match *kind {
                    "text" => lines.extend(answer(body, style)),
                    _ => lines.extend(
                        body.split('\n').map(|line| Line::from(Span::styled(line.to_string(), style))),
                    ),
                },
            }
            lines.push(Line::from(""));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Ask it something. ^p opens everything else.",
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
        // Whether this window holds the store or shares one through `rookd`:
        // it decides what the slash commands can do and whose engine runs the
        // turns. It was in the header the tabs sat in, and the header is gone —
        // so it lives here, on the pane it is about.
        let where_from = match self.source.daemon_base() {
            Some(base) => format!(" · via {base}"),
            None => " · this window holds the store".to_string(),
        };
        // Which project, by the directory's own name: two windows on two
        // projects is the ordinary case, and a session from the other one read
        // as this one's until the window said which it was.
        let project = self
            .source
            .workspace()
            .file_name()
            .map(|name| format!("{} · ", name.to_string_lossy()))
            .unwrap_or_default();
        let session = match self.chat.session {
            Some(id) => rook_store::format_session_id(id),
            None => "new session".into(),
        };
        let title = match self.chat.scroll {
            0 => format!(" {project}{session}{where_from} "),
            // Scrolled up, the pane looks exactly like one that has stopped
            // receiving — same border, nothing moving — so the way back
            // displaces the rest rather than being appended to it.
            back => format!(" {project}{session} — {back} lines back, End returns "),
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
        } else if !mentioned.is_empty() {
            let lines: Vec<Line> = mentioned
                .iter()
                .map(|path| Line::from(Span::styled(path.clone(), Style::default().fg(Color::Cyan))))
                .collect();
            f.render_widget(Paragraph::new(lines).block(bordered(" files · tab completes ")), ask);
        }

        let prompt = match (self.chat.busy && self.chat.asking.is_none(), self.chat.since) {
            (true, Some(since)) => format!(
                "  working… {}{}{}  ",
                crate::fmt::elapsed(since.elapsed()),
                match self.chat.step {
                    Some((at, of)) => format!(" · step {at}/{of}"),
                    None => String::new(),
                },
                self.chat.silence(self.patience)
            ),
            (true, None) => "  working… ".to_string(),
            _ => "› ".to_string(),
        };
        // The prompt marks the first row only; the rest are indented to line up
        // under it, so a pasted block reads as one message rather than as a
        // column of fragments.
        let gutter = prompt.chars().count();
        let typing: Vec<Line> = self
            .chat
            .input
            .as_str()
            .split('\n')
            .enumerate()
            .map(|(row, line)| {
                let mark = match row {
                    0 => prompt.clone(),
                    _ => " ".repeat(gutter),
                };
                Line::from(vec![
                    Span::styled(mark, Style::default().fg(Color::DarkGray)),
                    Span::raw(line.to_string()),
                ])
            })
            .collect();
        // Held to the cursor's row: typing at the bottom of a block longer than
        // the box would otherwise write where nothing is shown.
        let (row, column) = self.chat.input.caret();
        let visible = input.height.saturating_sub(2);
        let scroll = row.saturating_sub(visible.saturating_sub(1));
        f.render_widget(Paragraph::new(typing).block(bordered("")).scroll((scroll, 0)), input);
        // Wherever the box takes typing, which is everywhere but an approval:
        // a running turn takes what is typed as an interjection, and hiding the
        // caret there left somebody typing into a box with no sign of it. An
        // approval is answered with a letter and has the keyboard.
        if self.chat.pending.is_none() {
            f.set_cursor_position((
                input.x + 1 + gutter as u16 + column,
                input.y + 1 + row.saturating_sub(scroll),
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
                .block(bordered(" sessions "))
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
                .block(bordered(" transcript "))
                .wrap(Wrap { trim: false })
                .scroll((self.transcript_scroll, 0)),
            right,
        );
    }

    /// What each call was given and what came back.
    ///
    /// A conversation shows a call as one line, which is the right amount while
    /// a turn is running and not enough afterwards: "it edited `service.toml`"
    /// does not say what it wrote there. The log has both halves and this is
    /// where they are read — one gesture from the conversation rather than the
    /// four it took to reach the same bytes through the sessions pane.
    fn draw_calls(&mut self, f: &mut Frame, area: Rect) {
        let [list, detail] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(area);

        let rows: Vec<ListItem> = self
            .calls
            .iter()
            .map(|call| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("#{:<4} ", call.seq), Style::default().fg(Color::DarkGray)),
                    Span::styled(call.doing.clone(), Style::default().fg(Color::Magenta)),
                ]))
            })
            .collect();
        let title = format!(" calls ({}) ", self.calls.len());
        f.render_stateful_widget(
            List::new(rows).block(bordered(&title)).highlight_symbol("▌"),
            list,
            &mut self.call_state,
        );

        let mut lines: Vec<Line> = Vec::new();
        match self.call_state.selected().and_then(|at| self.calls.get(at)) {
            None => lines.push(Line::from(Span::styled(
                match self.chat.session {
                    None => "nothing has been asked in this conversation yet.",
                    Some(_) => "no calls in this conversation — the model has answered from what it knew.",
                },
                Style::default().fg(Color::DarkGray),
            ))),
            Some(call) => {
                lines.push(Line::from(Span::styled(
                    call.name.clone(),
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("given", Style::default().fg(Color::DarkGray))));
                for line in call.given.lines() {
                    lines.push(Line::from(Span::raw(format!("  {line}"))));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("came back", Style::default().fg(Color::DarkGray))));
                match &call.came_back {
                    // A call with no result in the log is one still running, or
                    // one the turn was interrupted in the middle of. Both are
                    // worth saying rather than showing an empty half.
                    None => lines.push(Line::from(Span::styled(
                        "  nothing yet — it is still running, or the turn ended inside it",
                        Style::default().fg(Color::Yellow),
                    ))),
                    Some(came_back) => {
                        for line in came_back.lines() {
                            lines.push(Line::from(Span::raw(format!("  {line}"))));
                        }
                    }
                }
                if call.elided {
                    lines.push(Line::from(Span::styled(
                        "  … elided; `rook store cat` for the whole object",
                        Style::default().fg(Color::Yellow),
                    )));
                }
            }
        }
        f.render_widget(
            Paragraph::new(lines)
                .block(bordered(" what it was given, what came back "))
                .wrap(Wrap { trim: false })
                .scroll((self.transcript_scroll, 0)),
            detail,
        );
    }

    fn draw_memory(&mut self, f: &mut Frame, area: Rect) {
        let typing = if self.adding.is_some() { 3 } else { 0 };
        let [list, entry] = Layout::vertical([Constraint::Min(3), Constraint::Length(typing)]).areas(area);

        let dim = Style::default().fg(Color::DarkGray);
        let scope_of = |fact: &rook_core::Fact| match fact.scope {
            rook_core::Scope::Global => "everywhere".to_string(),
            rook_core::Scope::Project(_) => {
                fact.scope.label().rsplit('/').next().unwrap_or("here").to_string()
            }
        };
        let widest = self.facts.iter().map(|f| scope_of(f).chars().count()).max().unwrap_or(5).clamp(5, 20);

        let mut lines: Vec<Line> = Vec::new();
        if self.facts.is_empty() {
            lines.push(Line::from(Span::styled(
                "nothing remembered yet — the agent writes here as it learns, and `a` adds a fact",
                dim,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("{:<10}{:<4}{:<width$}  fact", "id", "", "scope", width = widest),
                dim,
            )));
        }
        let chosen = self.fact_state.selected();
        for (at, fact) in self.facts.iter().enumerate() {
            let line = Line::from(vec![
                Span::styled(format!("{:<10}", fact.id), dim),
                // A pinned fact is in every request whatever else is relevant,
                // which is worth seeing from across the pane.
                Span::styled(if fact.pinned { "pin " } else { "    " }, Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{:<width$}  ", scope_of(fact), width = widest),
                    Style::default().fg(match fact.scope {
                        rook_core::Scope::Global => Color::Magenta,
                        rook_core::Scope::Project(_) => Color::Cyan,
                    }),
                ),
                Span::raw(fact.text.chars().take(70).collect::<String>()),
                Span::styled(
                    match fact.tags.is_empty() {
                        true => String::new(),
                        false => format!("  {}", fact.tags.join(" ")),
                    },
                    dim,
                ),
            ]);
            lines.push(match Some(at) == chosen {
                true => line.style(Style::default().bg(Color::Rgb(40, 44, 52))),
                false => line,
            });
        }
        let title = match self.fact_note.is_empty() {
            true => format!(" memory ({}) ", self.facts.len()),
            false => format!(" memory ({}) — {} ", self.facts.len(), self.fact_note),
        };
        f.render_widget(Paragraph::new(lines).block(bordered(&title)), list);

        if let Some(adding) = &self.adding {
            let reach = if adding.global { "everywhere" } else { "in this workspace" };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("› ", Style::default().fg(Color::DarkGray)),
                    Span::raw(adding.text.as_str().to_string()),
                ]))
                .block(bordered(&format!(" remember {reach} — enter saves, esc cancels "))),
                entry,
            );
            f.set_cursor_position((entry.x + 3 + adding.text.column(), entry.y + 1));
        }
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
            .block(bordered(" skills "));
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
                .block(bordered(" skills "))
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
            // Every captured version, because a rollback you cannot see is one
            // nobody asks for: `u` goes back to the first of these.
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("versions ({})", self.skill_versions.len()),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            if self.skill_versions.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  none captured — `c` takes one, and `u` goes back to it",
                    Style::default().fg(Color::DarkGray),
                )));
            }
            for (at, version) in self.skill_versions.iter().take(8).enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(
                        match at {
                            0 => "  ▸ ".to_string(),
                            _ => "    ".to_string(),
                        },
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        format!("{:<13}", version.object.chars().take(12).collect::<String>()),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(format!("{:<9}", version.version)),
                    Span::styled(
                        format!("{}  {} file(s)", fmt::ago(version.captured_at), version.files),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
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
        let title = match self.skill_note.is_empty() {
            true => " Detail ".to_string(),
            false => format!(" Detail — {} ", self.skill_note),
        };
        f.render_widget(Paragraph::new(lines).block(bordered(&title)).wrap(Wrap { trim: false }), right);
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
        f.render_widget(Paragraph::new(lines).block(bordered(" store ")).wrap(Wrap { trim: false }), top);

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

    /// Named snapshots of the workspace, and the two things anybody does with
    /// them: take one, and put one back.
    /// The pages of the set under the cursor, loaded with the selection.
    ///
    /// With the selection rather than with the tab, because what a person wants
    /// off this pane is the addresses — and a list of topics with no sources
    /// under it is the same claim without the evidence.
    fn load_doc_set(&mut self) {
        self.doc_set = self
            .docs_state
            .selected()
            .and_then(|at| self.docs.get(at))
            .and_then(|kept| self.source.docs(&kept.topic, Some(&kept.version)).ok().flatten());
    }

    fn draw_docs(&mut self, f: &mut Frame, area: Rect) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);

        let title = match self.docs_note.is_empty() {
            true => format!(" docs ({}) ", self.docs.len()),
            false => format!(" docs ({}) — {} ", self.docs.len(), self.docs_note),
        };
        if self.docs.is_empty() {
            f.render_widget(
                Paragraph::new(vec![
                    Line::from("no documentation gathered yet"),
                    Line::from(""),
                    Line::from("`/docs <topic>` reads a technology's documentation and keeps it"),
                    Line::from("here; `rook docs add <topic>` does the same from a terminal. The"),
                    Line::from("agent gathers on its own when it is asked about something it has"),
                    Line::from("no copy of, rather than answering from what it was trained on."),
                ])
                .style(Style::default().fg(Color::DarkGray))
                .block(bordered(&title)),
                area,
            );
            return;
        }

        let items: Vec<ListItem> = self
            .docs
            .iter()
            .map(|set| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<20}", set.topic)),
                    Span::styled(format!("{:<9}", set.version), Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!("{:>3}p  {}", set.pages, fmt::ago(set.fetched_at)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        f.render_stateful_widget(
            List::new(items)
                .block(bordered(&title))
                .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                .highlight_symbol("▌"),
            left,
            &mut self.docs_state,
        );

        let mut lines = Vec::new();
        if let Some(set) = &self.doc_set {
            lines.push(Line::from(Span::styled(
                format!("{} {}", set.topic, set.version),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "kept as {} · read {}",
                    rook_core::docs::reference(&set.topic, &set.version),
                    fmt::ago(set.fetched_at)
                ),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            // Both addresses, which is the whole point of keeping the reading
            // rather than the page: the local copy is what an answer is made
            // of, and these are what anybody else can check it against.
            for (n, page) in set.pages.iter().enumerate() {
                lines.push(Line::from(Span::raw(format!("{}. {}", n + 1, page.title))));
                lines.push(Line::from(Span::styled(
                    format!("   {}", page.url),
                    Style::default().fg(Color::Blue),
                )));
                // The opening of what was kept, because a list of links is
                // what a browser already gives: the reason for the local copy
                // is that the reading is here.
                const SHOWN: usize = 400;
                let opening: String = page.text.chars().take(SHOWN).collect();
                let elided = page.text.chars().count() > SHOWN;
                lines.push(Line::from(Span::styled(
                    format!("   {opening}{}", if elided { "…" } else { "" }),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(""));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(bordered(" sources ")), right);
    }

    fn draw_checkpoints(&mut self, f: &mut Frame, area: Rect) {
        let asking = self.naming.is_some() || self.restoring.is_some();
        let [list, entry] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(if asking { 3 } else { 0 })])
                .areas(area);

        let items: Vec<ListItem> = self
            .checkpoints
            .iter()
            .map(|(reference, object)| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<40}", named(reference))),
                    Span::styled(
                        object.chars().take(12).collect::<String>(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        let title = match self.checkpoint_note.is_empty() {
            true => format!(" checkpoints ({}) ", self.checkpoints.len()),
            false => format!(" checkpoints ({}) — {} ", self.checkpoints.len(), self.checkpoint_note),
        };
        match self.checkpoints.is_empty() {
            true => f.render_widget(
                Paragraph::new(vec![
                    Line::from("nothing snapshotted yet"),
                    Line::from(""),
                    Line::from("`c` takes one of the whole workspace, under a name you give it."),
                    Line::from("The agent takes its own before every write, and those are what"),
                    Line::from("`/undo` and `session rewind` put back — these are yours."),
                ])
                .style(Style::default().fg(Color::DarkGray))
                .block(bordered(&title)),
                list,
            ),
            false => f.render_stateful_widget(
                List::new(items)
                    .block(bordered(&title))
                    .highlight_style(Style::default().bg(Color::Rgb(40, 44, 52)))
                    .highlight_symbol("▌"),
                list,
                &mut self.checkpoint_state,
            ),
        }

        if let Some(naming) = &self.naming {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("› ", Style::default().fg(Color::DarkGray)),
                    Span::raw(naming.as_str().to_string()),
                ]))
                .block(bordered(" name this checkpoint — enter takes it, esc cancels ")),
                entry,
            );
            f.set_cursor_position((entry.x + 3 + naming.column(), entry.y + 1));
        } else if let Some((name, _)) = &self.restoring {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    // The workspace is in the window's own title bar; a line
                    // that names it again is a line too long to read the
                    // question at the end of.
                    format!("  restore {:?} over the workspace?   y / n", named(name)),
                    Style::default().fg(Color::Yellow),
                )))
                .block(bordered(" this writes over the workspace ")),
                entry,
            );
        }
    }

    /// The two things a checkpoint is for, where they are listed. Returns
    /// whether the key was this tab's — while a name is being typed, or an
    /// answer waited for, every key is.
    fn on_checkpoint_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.restoring.is_some() {
            match key.code {
                KeyCode::Char('y') => self.restore_selected_checkpoint(),
                _ => {
                    self.restoring = None;
                    self.checkpoint_note = "not restored".into();
                }
            }
            return true;
        }
        match (self.naming.is_some(), key.code) {
            (false, KeyCode::Char('c')) => self.naming = Some(Typing::default()),
            (false, KeyCode::Char('R')) => {
                self.restoring =
                    self.checkpoint_state.selected().and_then(|at| self.checkpoints.get(at)).cloned();
            }
            (false, _) => return false,
            (true, KeyCode::Enter) => self.take_checkpoint(),
            (true, KeyCode::Esc) => self.naming = None,
            (true, code) => {
                if let Some(naming) = &mut self.naming {
                    match code {
                        KeyCode::Backspace => naming.backspace(),
                        KeyCode::Delete => naming.delete(),
                        KeyCode::Left => naming.left(),
                        KeyCode::Right => naming.right(),
                        KeyCode::Home => naming.home(),
                        KeyCode::End => naming.end(),
                        KeyCode::Char(c) => naming.insert(c),
                        _ => {}
                    }
                }
            }
        }
        true
    }

    fn take_checkpoint(&mut self) {
        let Some(naming) = self.naming.take() else { return };
        let name = naming.as_str().trim().to_string();
        if name.is_empty() {
            return;
        }
        let said = match self.source.checkpoint(&name, None) {
            Ok((set, object)) => {
                format!("took {name:?} as {} — {} file(s)", &object[..12.min(object.len())], set.files.len())
            }
            Err(e) => e.to_string(),
        };
        self.checkpoint_note = said;
        self.reload();
    }

    /// Into the workspace, which is where it was taken from. The CLI asks for
    /// `--to` because it can be anywhere; here the answer is the directory
    /// this window is about, and the question is whether to write over it.
    fn restore_selected_checkpoint(&mut self) {
        let Some((name, object)) = self.restoring.take() else { return };
        let into = self.source.workspace().to_path_buf();
        let said = match self.source.restore_checkpoint(&object, &into) {
            Ok(files) => format!("restored {:?} — {files} file(s) written", named(&name)),
            Err(e) => e.to_string(),
        };
        self.checkpoint_note = said;
        self.reload();
    }

    fn draw_help(&mut self, f: &mut Frame, area: Rect) {
        // Two columns, coloured by which they are: a page of one grey is read
        // by nobody, and what is being offered here is the left column.
        let two = |line: &str, at: usize, left: Color| {
            // Characters, not bytes: these lines carry arrows and middots, and
            // a split inside one of those is a panic rather than a colour.
            let at = line.char_indices().nth(at).map_or(line.len(), |(at, _)| at);
            Line::from(vec![
                Span::styled(line[..at].to_string(), Style::default().fg(left)),
                Span::styled(line[at..].to_string(), Style::default().fg(Color::DarkGray)),
            ])
        };
        let command = |line: &str| two(line, 33, Color::Cyan);
        let key = |line: &str| two(line, 14, Color::Cyan);
        let text = vec![
            Line::from(Span::styled("rook", Style::default().add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from("The window is the conversation. Everything else — sessions, memory,"),
            Line::from("skills, checkpoints, docs, the store — opens over it from ^p and closes"),
            Line::from("with Esc. Everything here is also on the command line, as tables or --json:"),
            Line::from(""),
            command("  rook store stat                what memory costs, per kind"),
            command("  rook store ls / cat <id>       list and print raw objects"),
            command("  rook store gc --dry-run        what would be collected"),
            command("  rook session ls / show <id>    transcripts by sequence number"),
            command("  rook skills ls / why <name>    what applies here, and why not"),
            command("  rook skills history <name>     every captured version"),
            command("  rook skills rollback <n> <id>  restore one, undoably"),
            command("  rook checkpoint create <name>  snapshot part of the workspace"),
            command("  rook docs ls / show <topic>    documentation gathered here"),
            command("  rook doctor                    detected toolchains and platform"),
            Line::from(""),
            Line::from(Span::styled("keys", Style::default().add_modifier(Modifier::BOLD))),
            key("  ^p          everything reachable, filtered as you type"),
            key("  ^o          what each call was given and what came back"),
            key("  ^s          gives the mouse to the terminal, to select and copy what"),
            key("              was said · press it again to get the wheel back"),
            key("  @           names a file in the workspace · tab completes it"),
            key("  ⌥⏎          a newline in the message · ⏎ sends · paste keeps its lines"),
            key("  Esc         closes what is open; in the chat, clears then quits"),
            key("  j k ↑ ↓     move · Space/PgDn scroll · r reload · wheel scrolls"),
            Line::from(""),
            key("  In sessions: ⏎ continues the one under the cursor, in the chat"),
            Line::from(""),
            key("  In checkpoints: c takes one of the workspace · R restores one over it"),
            key("  In docs: d drops the set under the cursor · /docs <topic> gathers one"),
            Line::from(""),
            key("  In skills:  c captures a version of the one under the cursor"),
            key("              u rolls it back to the newest capture, undoably"),
            Line::from(""),
            key("  In memory:  a adds a fact here · A adds it everywhere"),
            key("              d forgets the selected one · u puts it back"),
            Line::from(""),
            key("  In the chat: Enter sends · Esc clears, then quits"),
            key("              /btw <question> asks without joining the conversation"),
            key("              /continue carries a turn stopped at a limit on from there"),
            key("              y / a / n answer an approval"),
            key("              enter     answer a question, one at a time"),
            key("              /…        tab completes; the list shows as you type"),
            key("              PgUp/PgDn scroll back through the conversation"),
            key("              ↑ / ↓     the prompts already sent, newest first"),
            key("              ← → home end · ctrl-a/e/w/u/k edit the line being typed"),
            key("              F2 / F3   cycle approvals / reasoning effort"),
            key("              ctrl-c    stops a running turn, or quits when none is"),
        ];
        f.render_widget(Paragraph::new(text).block(bordered(" help ")), area);
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

/// The name somebody gave a checkpoint, out of the reference it is stored
/// under: `checkpoint/<name>/<id>`. The whole reference is what the store
/// answers with and what `checkpoint ls` prints; it is not what anybody typed.
fn named(reference: &str) -> String {
    reference
        .strip_prefix("checkpoint/")
        .and_then(|rest| rest.rsplit_once('/'))
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| reference.to_string())
}

/// Text broken to a width, on word boundaries where there are any.
///
/// Written out because what needs wrapping here is what carries a marker down
/// its left edge, and a paragraph that wraps for us puts the marker on the
/// first row only. Characters rather than bytes: the messages this wraps are
/// as often Russian as English, and slicing a byte count through one of those
/// is a panic.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let room = width.saturating_sub(line.chars().count());
            if !line.is_empty() && word.chars().count() + 1 > room {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            // A word longer than the pane is cut where the pane ends rather
            // than pushing the rest of the line off it.
            let mut word: String = word.to_string();
            while word.chars().count() > width {
                let head: String = word.chars().take(width).collect();
                out.push(head.clone());
                word = word.chars().skip(width).collect();
            }
            line.push_str(&word);
        }
        out.push(line);
    }
    out
}

/// What a call is doing, in a few words.
///
/// A transcript of `read_file`, `read_file`, `edit_file` says which tools ran
/// and nothing about the work: the argument that matters is the file, the
/// command, the query. Named tools get the argument that identifies the call;
/// anything else is its own name, which is what it was before.
/// What a call over the socket is doing. An upgrade leaves the running daemon
/// on the old code — which is a case `daemon status` reports on purpose — and
/// one that predates the field sends nothing, where the tool's own name is
/// what a window has always shown.
fn from_daemon(name: &str, doing: &str) -> String {
    match doing.is_empty() {
        true => name.to_string(),
        false => rook_core::calls::within(doing, 72),
    }
}

fn tool_line(name: &str, args: Option<&serde_json::Value>, workspace: &std::path::Path) -> String {
    // The phrase is core's, so the same call reads the same here, in the chat
    // REPL, in `rook run` and in the browser. How much room there is is ours.
    rook_core::calls::within(&rook_core::calls::doing(name, args, workspace), 72)
}

/// A rectangle in the middle of another, by percentage.
///
/// Not the whole screen: what is underneath stays visible at the edges, so an
/// overlay reads as something on top of the conversation rather than as having
/// left it.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let w = area.width * width / 100;
    let h = area.height * height / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

/// The model, as much of it as a footer can carry.
///
/// A spec is `provider/vendor/model-name-and-a-date` often enough that the
/// whole of it would push everything else off the line. The last two segments
/// are the part that differs between the models somebody actually switches
/// between.
fn short_model(spec: &str) -> String {
    let parts: Vec<&str> = spec.split('/').collect();
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        n => parts[n - 2..].join("/"),
    }
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
        assert_eq!(crate::fmt::elapsed(std::time::Duration::from_secs(9)), "9s");
        assert_eq!(crate::fmt::elapsed(std::time::Duration::from_secs(59)), "59s");
        assert_eq!(crate::fmt::elapsed(std::time::Duration::from_secs(60)), "1m00s");
        assert_eq!(crate::fmt::elapsed(std::time::Duration::from_secs(250)), "4m10s");
    }

    /// A call was written when it started and written again when it finished,
    /// so a turn of a dozen calls filled the pane twice over — while the
    /// comment on `running` said the line gained its mark.
    #[test]
    fn a_call_is_one_line_that_gains_its_mark() {
        let mut chat = Chat::default();
        chat.tool_started("read_file", "read src/main.rs");
        assert_eq!(chat.log.len(), 1, "{:?}", chat.log);
        assert_eq!(chat.running(), Some("read src/main.rs"), "and it is what the turn is doing");

        chat.tool_done("read_file", false);
        assert_eq!(chat.log.len(), 1, "still one line: {:?}", chat.log);
        assert!(chat.log[0].1.ends_with('✓'), "{:?}", chat.log);
        assert_eq!(chat.running(), None, "and nothing is running");
    }

    /// Somebody read `1595.2k in` beside `2.4 MiB on disk` and asked whether
    /// their context had passed a million tokens. It had not: that is what
    /// thirty-nine steps had spent between them, and the context was at 29% of
    /// the window. The line answered a question nobody was asking and left the
    /// one they were.
    #[test]
    fn the_footer_says_how_full_the_window_is_before_what_the_turn_has_spent() {
        let said = spent(Some((1_595_200, 24_400, 1_387_800)), 41_000, 175_000);
        assert!(said.starts_with("ctx 41.0k/175.0k · "), "the window's share comes first: {said}");
        assert!(said.contains("1595.2k in"), "and what the turn has spent is still there: {said}");
        assert!(said.contains("(87% cached)"), "as a share, not a second large number: {said}");

        // A window nobody configured is the provider's to report, and a share
        // of a guess is worse than none.
        let unknown = spent(Some((900, 20, 0)), 800, 0);
        assert_eq!(unknown, "900 in / 20 out", "no share, and no claim about the cache either");

        // Before the first reply there is nothing to report at all.
        assert_eq!(spent(None, 0, 175_000), "");
    }

    /// Up walked straight into the history and set the box to the last prompt,
    /// so a half-written message of several rows was gone at the first press of
    /// a key that, in every editor, moves within it.
    #[test]
    fn up_walks_the_rows_before_it_walks_the_history() {
        let mut typing = Typing::default();
        typing.paste("first\nsecond\nthird");

        assert!(typing.up(), "from the last row there is one above");
        assert_eq!(typing.caret(), (1, 5), "and the column is kept where the row is long enough");
        assert!(typing.up());
        assert_eq!(typing.caret(), (0, 5));
        assert!(!typing.up(), "at the top the box has nothing more to offer");

        assert!(typing.down());
        assert_eq!(typing.caret(), (1, 5));
        typing.end();
        assert!(!typing.down(), "and at the end of the last row, nothing below");

        // A short row takes the cursor to where it ends rather than past it.
        let mut ragged = Typing::default();
        ragged.paste("a very long first row\nshort");
        ragged.end();
        assert!(ragged.up());
        assert_eq!(ragged.caret(), (0, 5), "five characters along, which is where `short` reached");

        // One row is where it always was: nothing above, so the key is history.
        let mut one = Typing::default();
        one.set("just this");
        assert!(!one.up());
        assert!(!one.down());
    }

    /// A pasted paragraph was sent one line at a time: the terminal delivers a
    /// pasted newline as the Enter key, so the first line went as a prompt and
    /// the rest chased it as prompts of their own. Bracketed paste is what tells
    /// a paste from typing; this is the half that keeps the newlines.
    #[test]
    fn a_pasted_paragraph_is_one_message_and_not_one_per_line() {
        let mut typing = Typing::default();
        for c in "look at ".chars() {
            typing.insert(c);
        }
        typing.paste("first line\r\nsecond line\rthird line\n");

        assert_eq!(
            typing.as_str(),
            "look at first line\nsecond line\nthird line\n",
            "every ending is a newline: a bare `\\r` left in would draw over the line before it"
        );
        assert_eq!(typing.rows(), 4, "and the box grows to hold them");
        assert_eq!(typing.caret(), (3, 0), "with the cursor after the last one");

        // Typing goes on where the paste left off, on the row it left off on.
        for c in "and that".chars() {
            typing.insert(c);
        }
        assert_eq!(typing.caret(), (3, 8));
        assert!(typing.as_str().ends_with("third line\nand that"));
    }

    /// The cursor is a row and a column now, and both are counted in characters
    /// — a byte offset put it in the middle of a letter in anybody's language
    /// but English.
    #[test]
    fn the_caret_is_counted_in_characters_on_the_row_it_is_on() {
        let mut typing = Typing::default();
        typing.paste("посмотри\nна файл");
        assert_eq!(typing.caret(), (1, 7), "seven characters, not thirteen bytes");
        typing.home();
        assert_eq!(typing.caret(), (0, 0));
    }

    /// Naming a file meant knowing its path and typing it, so the short way to
    /// ask about one was to leave the window and come back with a paste.
    #[test]
    fn a_file_is_named_at_the_cursor_and_the_sentence_goes_on() {
        let mut typing = Typing::default();
        for c in "why does ".chars() {
            typing.insert(c);
        }
        assert_eq!(typing.mentioning(), None, "an ordinary sentence names no file");

        for c in "@serv".chars() {
            typing.insert(c);
        }
        assert_eq!(typing.mentioning(), Some("serv"), "what follows the mark is the fragment");

        typing.mention("src/service.rs");
        assert_eq!(typing.as_str(), "why does @src/service.rs ", "the path lands where the mark was");
        assert_eq!(typing.mentioning(), None, "and the mention is over, so the pane closes");

        // In the middle of a line the space is already there, and a second one
        // is a gap nobody typed.
        let mut middle = Typing::default();
        for c in "read @a and then some".chars() {
            middle.insert(c);
        }
        for _ in 0.."read @a and then some".len() - "read @a".len() {
            middle.left();
        }
        middle.mention("alpha.rs");
        assert_eq!(middle.as_str(), "read @alpha.rs and then some");

        // Typing goes on after it, in the middle of the line.
        for c in "fail?".chars() {
            typing.insert(c);
        }
        assert_eq!(typing.as_str(), "why does @src/service.rs fail?");
    }

    /// An address is not a mention, and neither is a mention somebody has gone
    /// back past: the pane offers what the cursor is in.
    #[test]
    fn a_mark_with_a_word_against_it_is_not_a_file() {
        let mut typing = Typing::default();
        for c in "mail vart@example.com".chars() {
            typing.insert(c);
        }
        assert_eq!(typing.mentioning(), None, "an address is one word with a mark inside it");

        let mut earlier = Typing::default();
        for c in "read @a.rs and @b.rs".chars() {
            earlier.insert(c);
        }
        assert_eq!(earlier.mentioning(), Some("b.rs"), "the one the cursor is in");
        // Back to just after the first `a.rs`, which `"read @a.rs"` is the
        // length of.
        for _ in 0..("read @a.rs and @b.rs".len() - "read @a.rs".len()) {
            earlier.left();
        }
        assert_eq!(earlier.mentioning(), Some("a.rs"), "and it follows the cursor back");
    }

    /// Several files share a prefix; completing to it is how a directory of
    /// near-identical names narrows without the list being read.
    #[test]
    fn several_files_complete_as_far_as_they_agree() {
        let mut typing = Typing::default();
        for c in "read @ser".chars() {
            typing.insert(c);
        }
        typing.narrow("service");
        assert_eq!(typing.as_str(), "read @service", "grown, not finished");
        assert_eq!(typing.mentioning(), Some("service"), "so more can be typed");
    }

    /// Two reads in one turn produce two `read_file` results that are told
    /// apart by the order they were logged in and by nothing else — the same
    /// rule the conversation marks its lines by. Paired the wrong way, the
    /// pane says a call returned somebody else's bytes, which is worse than
    /// saying nothing.
    #[test]
    fn a_result_is_paired_with_the_call_it_answers_and_not_the_first_of_its_name() {
        let calls = paired(logged(&[
            ("tool-call", "read_file", r#"{"path":"a.rs"}"#),
            ("tool-call", "read_file", r#"{"path":"b.rs"}"#),
            ("tool-call", "run_command", r#"{"command":"cargo test"}"#),
            ("tool-result", "read_file", "contents of a"),
            ("tool-result", "run_command", "ok"),
            ("tool-result", "read_file", "contents of b"),
        ]));

        // Newest first, which is what the question is almost always about.
        let answered: Vec<(&str, &str)> =
            calls.iter().map(|c| (c.given.as_str(), c.came_back.as_deref().unwrap_or("nothing"))).collect();
        assert_eq!(
            answered,
            vec![
                (r#"{"command":"cargo test"}"#, "ok"),
                (r#"{"path":"b.rs"}"#, "contents of b"),
                (r#"{"path":"a.rs"}"#, "contents of a"),
            ],
            "each call keeps its own answer"
        );
    }

    /// A turn stopped in the middle of a call leaves it with no result. Saying
    /// so beats an empty half that reads as a call that returned nothing.
    #[test]
    fn a_call_the_turn_ended_inside_says_that_nothing_came_back() {
        let calls = paired(logged(&[("tool-call", "run_command", r#"{"command":"sleep 90"}"#)]));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].came_back, None, "and it is not invented");
    }

    /// Entries as the log hands them over, with the fields the pairing reads.
    fn logged(events: &[(&str, &str, &str)]) -> Vec<TranscriptEntry> {
        events
            .iter()
            .enumerate()
            .map(|(seq, (kind, label, body))| TranscriptEntry {
                seq: seq as u64,
                ts: 0,
                kind: (*kind).to_string(),
                label: (*label).to_string(),
                object: String::new(),
                bytes: body.len() as u64,
                stored_bytes: body.len() as u64,
                tokens_in: 0,
                tokens_out: 0,
                truncated: false,
                body: (*body).to_string(),
                doing: String::new(),
            })
            .collect()
    }

    /// `^c` on a turn that was waiting for an answer left the question on
    /// screen. An approval takes the whole keyboard until it is answered, so
    /// the window read as frozen; a question captures Enter, so nothing could
    /// be sent. Both belong to the turn that asked and end with it.
    #[test]
    fn a_turn_that_ends_takes_what_it_was_waiting_for_with_it() {
        let mut chat = Chat {
            busy: true,
            pending: Some(rook_tools::policy::ApprovalRequest {
                id: "1".into(),
                tool: "run_command".into(),
                action: "rm -rf build".into(),
                preview: None,
                kind: Vec::new(),
            }),
            ..Chat::default()
        };

        chat.ended();
        assert!(!chat.busy, "the window stops waiting");
        assert!(chat.pending.is_none(), "and the approval goes with the turn that asked");
        assert!(
            chat.log.iter().any(|(_, body)| body.contains("what it was waiting for is gone")),
            "and it is said, because a question that vanishes is its own confusion: {:?}",
            chat.log
        );

        // A turn nobody was asked about says nothing extra: a note on every
        // ending is a note nobody reads.
        let mut quiet = Chat { busy: true, ..Chat::default() };
        quiet.ended();
        assert!(quiet.log.is_empty(), "{:?}", quiet.log);
    }

    /// A feature reachable from one front end and not another is a defect here,
    /// and this one was invisible: the window holding the store showed the
    /// work, the window over the socket showed the tool's name.
    #[test]
    fn a_window_over_the_socket_reads_a_call_the_same_as_one_holding_the_store() {
        // The same call, both ways in: the daemon sends the phrase, and the
        // window that holds the store works it out itself.
        let here = std::path::Path::new("/nowhere");
        let arguments = serde_json::json!({ "path": "src/main.rs" });
        let held = tool_line("read_file", Some(&arguments), here);
        let over = from_daemon("read_file", &rook_core::calls::doing("read_file", Some(&arguments), here));
        assert_eq!(held, over, "one turn, one reading");
        assert_eq!(held, "read src/main.rs");

        // A daemon older than the field: the name, which is what it sent
        // before, rather than an empty line where a call was.
        assert_eq!(from_daemon("read_file", ""), "read_file", "and never nothing at all");
    }

    /// Several calls are announced before any of them runs, and they finish in
    /// that order. The finish carries only the tool's name, so what pairs it
    /// with its line is the order it was announced in.
    #[test]
    fn calls_announced_together_mark_the_lines_they_belong_to() {
        let mut chat = Chat::default();
        chat.tool_started("read_file", "read a.rs");
        chat.tool_started("read_file", "read b.rs");
        chat.tool_started("run_command", "run cargo test");

        chat.tool_done("read_file", false);
        chat.tool_done("read_file", true);
        chat.tool_done("run_command", false);

        let marks: Vec<&str> = chat.log.iter().map(|(_, body)| body.as_str()).collect();
        assert_eq!(
            marks,
            vec!["  · read a.rs ✓", "  · read b.rs ✗", "  · run cargo test ✓"],
            "each line gets its own call's answer"
        );
    }

    /// A transcript of `read_file`, `read_file`, `edit_file` says which tools
    /// ran and nothing about the work.
    #[test]
    fn a_call_says_what_it_is_doing_and_not_only_its_name() {
        // Paths already relative to it, which is the ordinary case; the
        // trimming of an absolute one is core's, and tested there.
        let here = std::path::Path::new("/nowhere");
        let path = serde_json::json!({ "path": "src/main.rs" });
        assert_eq!(tool_line("read_file", Some(&path), here), "read src/main.rs");
        assert_eq!(tool_line("edit_file", Some(&path), here), "edit src/main.rs");
        let command = serde_json::json!({ "command": "cargo test -p rook-core" });
        assert_eq!(tool_line("run_command", Some(&command), here), "run cargo test -p rook-core");
        // A refactor names files rather than a path, and the first of them is
        // what identifies the call.
        let files = serde_json::json!({ "files": [{ "path": "a.rs" }, { "path": "b.rs" }] });
        assert_eq!(tool_line("edit_file", Some(&files), here), "edit a.rs");
        // A tool nothing here knows about keeps its own name, which is what
        // every tool had before.
        assert_eq!(tool_line("some_mcp_tool", Some(&path), here), "some_mcp_tool");
        assert_eq!(tool_line("read_file", None, here), "read_file");
        // And a line stays a line: a command that fills the pane pushes the
        // answer off it.
        let long = serde_json::json!({ "command": "x".repeat(200) });
        assert!(tool_line("run_command", Some(&long), here).chars().count() <= 72);
    }

    /// A marker that stops after the first row marks a row, and what is being
    /// marked is a message.
    #[test]
    fn a_wrapped_message_is_broken_on_words_and_counts_characters() {
        let lines = wrapped("одно два три четыре пять", 12);
        assert!(lines.iter().all(|line| line.chars().count() <= 12), "{lines:?}");
        assert_eq!(lines.join(" "), "одно два три четыре пять", "and nothing is lost");

        // A word wider than the pane is cut at the pane rather than pushing
        // the rest of the line off it.
        let long = wrapped("ααααααααααααααααα", 8);
        assert!(long.iter().all(|line| line.chars().count() <= 8), "{long:?}");
        assert_eq!(long.concat(), "ααααααααααααααααα");
    }

    /// A minute of nothing on the screen is a build running, a model thinking,
    /// or a turn that has died, and all three drew the same `working…`. Two of
    /// the three can be named, which is enough to know whether to wait.
    ///
    /// And the wait is named with its end. On a local model a large context is
    /// minutes of prompt processing before the first token — thirteen of them,
    /// measured, at the size a sub-agent reaches — so the honest answer to "has
    /// it stopped?" is not only how long it has been quiet but how long it will
    /// stay before the turn gives up on the stream.
    #[test]
    fn a_quiet_turn_says_what_it_is_waiting_on_and_for_how_much_longer() {
        let a_while = std::time::Duration::from_secs(90);
        let patience = std::time::Duration::from_secs(1200);
        let mut chat = Chat::default();
        assert_eq!(
            waiting_on(a_while, chat.running(), patience),
            " · nothing from the model for 1m30s of 20m00s"
        );

        // A tool has a timeout of its own, so the model's would be the wrong
        // number to put beside it.
        chat.push("tool", "  · run_command");
        assert_eq!(waiting_on(a_while, chat.running(), patience), " · run_command running 1m30s");

        // A call that has finished is not what the turn is waiting on.
        chat.push("tool", "  · run_command ✓");
        assert_eq!(
            waiting_on(a_while, chat.running(), patience),
            " · nothing from the model for 1m30s of 20m00s"
        );

        let mut waiting = Chat { heard: Some(std::time::Instant::now()), ..Chat::default() };
        assert_eq!(waiting.silence(patience), "", "an ordinary pause between tokens says nothing");
        waiting.heard = None;
        assert_eq!(waiting.silence(patience), "", "and neither does a turn nobody started");
    }

    /// The commands were discoverable only from `/help`, which is where you
    /// look after giving up. Typing `/se` narrows to what could follow it.
    #[test]
    fn typing_a_slash_narrows_to_the_commands_that_could_follow() {
        let all = crate::chat::commands_matching("/");
        assert!(all.len() > 10, "everything, for a bare slash: {}", all.len());

        let se: Vec<&str> = crate::chat::commands_matching("/se").iter().map(|(n, ..)| *n).collect();
        assert_eq!(se, ["session", "secrets", "search"], "everything sharing the prefix is offered");

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

    /// A window drawing `working…` over a long silence asks the daemon whether
    /// the turn is still there, and asks once.
    ///
    /// Being told a turn has ended is one message, and the message was lost:
    /// the turn's ending went into a queue and the task that emptied the queue
    /// was aborted in the same breath, so a window that had streamed eleven
    /// minutes of work went on drawing `working…` over a finished turn while
    /// the daemon beside it answered `turns_running: 0`. The race is fixed;
    /// this is the window noticing for itself, because the shape that failure
    /// takes — patience — is the one nobody can tell from work.
    #[test]
    fn a_window_left_drawing_working_asks_the_daemon_whether_the_turn_is_still_there() {
        let patience = std::time::Duration::from_secs(90);
        let long_ago = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(600))
            .unwrap_or_else(std::time::Instant::now);
        let (remote, _held) = mpsc::unbounded_channel::<ClientMessage>();

        let mut chat = Chat {
            busy: true,
            heard: Some(long_ago),
            session: Some(1),
            remote: Some(remote),
            ..Chat::default()
        };
        assert!(chat.worth_asking_if_alive(patience), "ten minutes of silence is worth one question");

        chat.asked_if_alive = true;
        assert!(!chat.worth_asking_if_alive(patience), "and the question is asked once, not every frame");

        chat.asked_if_alive = false;
        chat.heard = Some(std::time::Instant::now());
        assert!(
            !chat.worth_asking_if_alive(patience),
            "an ordinary pause between tokens is not a reason to ask"
        );

        // And a window whose daemon has gone is not left asking a closed
        // channel: the send fails, which is itself the answer.
        let (dead, gone) = mpsc::unbounded_channel::<ClientMessage>();
        drop(gone);
        assert!(
            dead.send(ClientMessage::Attach { session: "x".into() }).is_err(),
            "the precondition is a channel nobody is reading"
        );

        let mut quiet = Chat { busy: true, heard: Some(long_ago), session: Some(1), ..Chat::default() };
        assert!(
            !quiet.worth_asking_if_alive(patience),
            "a turn this process runs itself has no daemon to ask and cannot lose the ending on the way"
        );
        quiet.busy = false;
        assert!(!quiet.worth_asking_if_alive(patience), "and a window with no turn in flight asks nothing");
    }

    /// A window routed through a daemon reads the configuration file, not the
    /// built-in defaults.
    ///
    /// It read the defaults, under a comment saying it read the file. The
    /// visible cost was a footer promising to give up on a silent model after
    /// ninety seconds while the daemon behind it waited the twenty minutes the
    /// file asked for — and a local model filling a two-hundred-thousand-token
    /// prompt is silent for minutes at a time, so the number a person reads to
    /// decide whether to keep waiting was the wrong one by a factor of
    /// thirteen.
    #[test]
    fn a_window_with_no_store_of_its_own_still_reads_the_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[agent]\nstream_idle_timeout_secs = 1200\n").unwrap();

        let built_in = rook_core::Config::default().agent.stream_idle_timeout_secs;
        assert_ne!(built_in, 1200, "the file has to say something the default does not");

        let seen = config_seen_by(None, file);
        assert_eq!(
            seen.agent.stream_idle_timeout_secs, 1200,
            "the window would have drawn a patience of {built_in}s over a turn waiting twenty minutes"
        );
    }
}
