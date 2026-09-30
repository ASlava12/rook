//! One bounded reader worker keeps navigation off the input/streaming thread.
use super::*;
use rook_core::transcript::{Cursor, EntryPage, Matches, Page, PageRequest};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

pub(super) struct History {
    session: Option<u128>,
    epoch: u64,
    pending: bool,
    page: Option<Page>,
    turns: Option<rook_core::turns::Page>,
    tree: Option<rook_core::branches::Page>,
    switch: Option<rook_core::branches::Node>,
    forked: Option<rook_core::branches::Forked>,
    hits: Option<Matches>,
    entry: Option<EntryPage>,
    at: usize,
    scroll: u16,
    input: Typing,
    editing: Option<bool>,
    needle: String,
    note: String,
    quote: Option<String>,
    send: Option<SyncSender<(u64, Command)>>,
    receive: Receiver<(u64, Result<Update>)>,
}
enum Command {
    Fork(u128, u64),
    Tree(u128, Option<String>),
    Turns(u128, Option<u64>),
    Page(u128, PageRequest),
    Search(u128, String, Cursor),
    Entry(u128, u64, u64),
    Quote(u128, u64, u64),
}
enum Update {
    Fork(rook_core::branches::Forked),
    Tree(rook_core::branches::Page),
    Turns(rook_core::turns::Page),
    Page(u128, Page),
    Search(Matches),
    Entry(EntryPage),
    Quote(String),
}
impl Command {
    fn read(self, source: &crate::source::Source) -> Result<Update> {
        Ok(match self {
            Self::Fork(session, seq) => Update::Fork(source.branch_from_event(session, seq)?),
            Self::Tree(session, after) => Update::Tree(source.branch_page(session, after.as_deref())?),
            Self::Turns(session, before) => Update::Turns(source.turn_results(session, before)?),
            Self::Page(session, q) => Update::Page(session, source.transcript_page(session, &q)?),
            Self::Search(session, q, cursor) => {
                Update::Search(source.transcript_search(session, &q, cursor)?)
            }
            Self::Entry(session, seq, offset) => {
                Update::Entry(source.transcript_entry(session, seq, offset)?)
            }
            Self::Quote(session, seq, offset) => {
                Update::Quote(source.transcript_quote(session, seq, offset)?.text)
            }
        })
    }
}
impl History {
    pub(super) fn new(source: &crate::source::Source) -> Self {
        let (send, commands) = sync_channel::<(u64, Command)>(1);
        let (updates, receive) = sync_channel(1);
        let reader = source.reader();
        let (send, note) = match reader {
            Ok(reader) => match std::thread::Builder::new().name("history-reader".into()).spawn(move || {
                while let Ok((epoch, command)) = commands.recv() {
                    if updates.send((epoch, command.read(&reader))).is_err() {
                        break;
                    }
                }
            }) {
                Ok(_) => (Some(send), String::new()),
                Err(e) => (None, e.to_string()),
            },
            Err(e) => (None, e.to_string()),
        };
        Self {
            session: None,
            epoch: 0,
            pending: false,
            page: None,
            turns: None,
            tree: None,
            switch: None,
            forked: None,
            hits: None,
            entry: None,
            at: 0,
            scroll: 0,
            input: Typing::default(),
            editing: None,
            needle: String::new(),
            note,
            quote: None,
            send,
            receive,
        }
    }
    pub(super) fn open(&mut self, session: Option<u128>) {
        self.open_mode(session, |id| Command::Page(id, PageRequest::default()));
    }
    pub(super) fn open_turns(&mut self, session: Option<u128>, before: Option<u64>) {
        self.open_mode(session, |id| Command::Turns(id, before));
    }
    pub(super) fn open_tree(&mut self, session: Option<u128>) {
        self.open_mode(session, |id| Command::Tree(id, None));
    }
    fn open_mode(&mut self, session: Option<u128>, command: impl FnOnce(u128) -> Command) {
        self.epoch = self.epoch.wrapping_add(1);
        self.session = session;
        self.pending = false;
        self.page = None;
        self.turns = None;
        self.tree = None;
        self.switch = None;
        self.hits = None;
        self.entry = None;
        self.at = 0;
        self.scroll = 0;
        self.quote = None;
        self.editing = None;
        if let Some(session) = session {
            self.ask(command(session));
        } else {
            self.note = "This conversation has no saved session yet.".into();
        }
    }
    fn ask(&mut self, command: Command) -> bool {
        if self.pending {
            self.note = "Still reading; Esc closes the viewer.".into();
            return false;
        }
        if self.send.as_ref().is_some_and(|s| s.try_send((self.epoch, command)).is_ok()) {
            self.pending = true;
            self.note = "Reading history…".into();
            true
        } else {
            self.note = "History reader is busy or unavailable; try again.".into();
            false
        }
    }
    pub(super) fn poll(&mut self) {
        while let Ok((epoch, update)) = self.receive.try_recv() {
            // A committed fork survives closing the viewer while it was being
            // created. The app decides whether it can still replace the draft.
            if let Ok(Update::Fork(forked)) = update {
                self.forked = Some(forked);
                if epoch == self.epoch {
                    self.pending = false;
                }
                continue;
            }
            if epoch != self.epoch {
                continue;
            }
            self.pending = false;
            self.note.clear();
            match update {
                Ok(Update::Fork(_)) => unreachable!("fork completion handled before stale read filtering"),
                Ok(Update::Tree(page)) => {
                    self.session = rook_store::parse_session_id(&page.selected.id);
                    self.at = page.ancestors.len();
                    self.tree = Some(page);
                    self.turns = None;
                    self.page = None;
                    self.hits = None;
                    self.entry = None;
                    self.scroll = 0;
                    self.note = "Enter explores · c continues selected · h reads history · n scans children · u earlier ancestors".into();
                }
                Ok(Update::Turns(page)) => {
                    self.tree = None;
                    self.turns = Some(page);
                    self.page = None;
                    self.hits = None;
                    self.entry = None;
                    self.at = 0;
                    self.scroll = 0;
                    self.note =
                        "t refreshes results · n scans older · h opens history · Enter reads full result"
                            .into();
                }
                Ok(Update::Page(session, page)) => {
                    self.session = Some(session);
                    self.tree = None;
                    self.turns = None;
                    self.at = page.items.len().saturating_sub(1);
                    self.page = Some(page);
                    self.hits = None;
                    self.entry = None;
                    self.scroll = 0;
                }
                Ok(Update::Search(hits)) => {
                    self.tree = None;
                    self.turns = None;
                    self.note = if hits.next.is_some() {
                        "Search page complete; n continues the scan."
                    } else {
                        "Search complete."
                    }
                    .into();
                    self.hits = Some(hits);
                    self.entry = None;
                    self.at = 0;
                    self.scroll = 0;
                }
                Ok(Update::Entry(entry)) => {
                    self.entry = Some(entry);
                    self.scroll = 0;
                }
                Ok(Update::Quote(text)) => self.quote = Some(text),
                Err(e) => self.note = e.to_string(),
            }
        }
    }
    pub(super) fn take_quote(&mut self) -> Option<String> {
        self.quote.take()
    }
    pub(super) fn take_session(&mut self) -> Option<rook_core::branches::Node> {
        self.switch.take()
    }
    pub(super) fn take_forked(&mut self) -> Option<rook_core::branches::Forked> {
        self.forked.take()
    }
    pub(super) fn wants_branch(&self, key: crossterm::event::KeyEvent) -> bool {
        self.editing.is_none() && self.tree.is_none() && key.code == KeyCode::Char('B')
    }
    fn branch(&self) -> Option<&rook_core::branches::Node> {
        let page = self.tree.as_ref()?;
        page.ancestors.iter().chain(std::iter::once(&page.selected)).chain(page.children.iter()).nth(self.at)
    }
    fn target(&self) -> Option<(u64, u64)> {
        if let Some(entry) = &self.entry {
            return Some((entry.entry.seq, entry.offset));
        }
        if let Some(hits) = &self.hits {
            return hits.hits.get(self.at).map(|h| (h.seq, h.offset));
        }
        if let Some(turns) = &self.turns {
            return turns.items.get(self.at).map(|e| (e.result_seq, 0));
        }
        self.page.as_ref()?.items.get(self.at).map(|e| (e.seq, 0))
    }
    pub(super) fn paste(&mut self, text: &str) {
        if self.editing.is_some() && self.input.text.len() + text.len() <= 256 {
            self.input.paste(&text.replace('\n', " "));
        }
    }
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if let Some(jump) = self.editing {
            match key.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Enter => {
                    if let Some(session) = self.session {
                        let sent = if jump {
                            match self.input.text.trim().trim_start_matches('#').parse() {
                                Ok(seq) => self.ask(Command::Entry(session, seq, 0)),
                                Err(_) => {
                                    self.note = "Enter an event number, for example 1200.".into();
                                    false
                                }
                            }
                        } else {
                            let q = self.input.text.trim().to_string();
                            if q.is_empty() {
                                false
                            } else if self.ask(Command::Search(session, q.clone(), Cursor::default())) {
                                self.needle = q;
                                true
                            } else {
                                false
                            }
                        };
                        if sent {
                            self.editing = None;
                        }
                    }
                }
                KeyCode::Backspace => self.input.backspace(),
                KeyCode::Delete => self.input.delete(),
                KeyCode::Left => self.input.left(),
                KeyCode::Right => self.input.right(),
                KeyCode::Home => self.input.home(),
                KeyCode::End => self.input.end(),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.input.set(""),
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && self.input.text.len() + c.len_utf8() <= 256 =>
                {
                    self.input.insert(c)
                }
                _ => {}
            }
            return false;
        }
        if key.code == KeyCode::Esc {
            self.epoch = self.epoch.wrapping_add(1);
            self.pending = false;
            self.quote = None;
            self.switch = None;
            return true;
        }
        let Some(session) = self.session else {
            return false;
        };
        if self.tree.is_some() {
            match key.code {
                KeyCode::Enter => {
                    if let Some(id) = self.branch().and_then(|n| rook_store::parse_session_id(&n.id)) {
                        self.ask(Command::Tree(id, None));
                    }
                }
                KeyCode::Char('c') if !self.pending => {
                    self.switch = self.branch().cloned();
                }
                KeyCode::Char('h') => {
                    if let Some(id) = self.branch().and_then(|n| rook_store::parse_session_id(&n.id)) {
                        self.ask(Command::Page(id, PageRequest::default()));
                    }
                }
                KeyCode::Char('n') => {
                    if let Some(after) = self.tree.as_ref().and_then(|p| p.next.clone()) {
                        self.ask(Command::Tree(session, Some(after)));
                    }
                }
                KeyCode::Char('u') => {
                    if let Some(id) = self
                        .tree
                        .as_ref()
                        .and_then(|p| p.earlier_ancestor.as_deref())
                        .and_then(rook_store::parse_session_id)
                    {
                        self.ask(Command::Tree(id, None));
                    }
                }
                KeyCode::Char('r') => {
                    self.ask(Command::Tree(session, None));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(p) = &self.tree {
                        self.at = self.at.saturating_add(1).min(p.ancestors.len() + p.children.len());
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.at = self.at.saturating_sub(1);
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('B') => {
                if let Some((seq, _)) = self.target() {
                    self.ask(Command::Fork(session, seq));
                }
            }
            KeyCode::Char('v') => {
                self.ask(Command::Tree(session, None));
            }
            KeyCode::Char('/') => {
                self.editing = Some(false);
                self.input.set(&self.needle);
            }
            KeyCode::Char('g') => {
                self.editing = Some(true);
                self.input.set("");
            }
            KeyCode::Char('r') => {
                self.ask(if self.turns.is_some() {
                    Command::Turns(session, None)
                } else {
                    Command::Page(session, PageRequest::default())
                });
            }
            KeyCode::Char('t') => {
                self.ask(Command::Turns(session, None));
            }
            KeyCode::Char('h') => {
                self.ask(Command::Page(session, PageRequest::default()));
            }
            KeyCode::Char('b') => {
                self.entry = None;
                self.hits = None;
                self.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
                let len = self
                    .hits
                    .as_ref()
                    .map(|h| h.hits.len())
                    .or_else(|| self.turns.as_ref().map(|p| p.items.len()))
                    .or_else(|| self.page.as_ref().map(|p| p.items.len()))
                    .unwrap_or(0);
                self.at = if matches!(key.code, KeyCode::Down | KeyCode::Char('j')) {
                    self.at.saturating_add(1).min(len.saturating_sub(1))
                } else {
                    self.at.saturating_sub(1)
                };
                self.entry = None;
                self.scroll = 0;
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(12),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(12),
            KeyCode::Enter => {
                if let Some((seq, offset)) = self.target() {
                    self.ask(Command::Entry(session, seq, offset));
                }
            }
            KeyCode::Char('q') => {
                if let Some((seq, offset)) = self.target() {
                    self.ask(Command::Quote(session, seq, offset));
                }
            }
            KeyCode::Char('n') => {
                let command = if let Some(entry) = &self.entry {
                    entry.next_offset.map(|offset| Command::Entry(session, entry.entry.seq, offset))
                } else if let Some(hits) = &self.hits {
                    hits.next.map(|cursor| Command::Search(session, self.needle.clone(), cursor))
                } else if let Some(turns) = &self.turns {
                    turns.before.map(|before| Command::Turns(session, Some(before)))
                } else {
                    self.page.as_ref().and_then(|p| p.next).map(|from| {
                        Command::Page(session, PageRequest { from: Some(from), ..Default::default() })
                    })
                };
                if let Some(command) = command {
                    self.ask(command);
                }
            }
            KeyCode::Char('p') => {
                let command = if let Some(entry) = &self.entry {
                    entry.previous_offset.map(|offset| Command::Entry(session, entry.entry.seq, offset))
                } else if self.hits.is_none() {
                    self.page.as_ref().and_then(|p| p.previous).map(|before| {
                        Command::Page(session, PageRequest { before: Some(before), ..Default::default() })
                    })
                } else {
                    None
                };
                if let Some(command) = command {
                    self.ask(command);
                }
            }
            _ => {}
        }
        false
    }
    pub(super) fn draw(&self, f: &mut Frame, area: Rect) {
        if let Some(page) = &self.tree {
            let [heading, list, detail, help] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(6),
                Constraint::Length(2),
            ])
            .areas(area);
            f.render_widget(
                Paragraph::new(
                    "Explore a conversation branch. Switching leaves workspace files as they are.",
                )
                .wrap(Wrap { trim: false })
                .block(bordered("conversation tree")),
                heading,
            );
            let rows = page
                .ancestors
                .iter()
                .enumerate()
                .chain(std::iter::once((page.ancestors.len(), &page.selected)))
                .chain(page.children.iter().map(|n| (page.ancestors.len() + 1, n)))
                .map(|(depth, n)| {
                    ListItem::new(format!(
                        "{}{} {}",
                        "  ".repeat(depth.min(8)),
                        if n.id == page.selected.id { ">" } else { "└" },
                        rook_core::branches::label(n)
                    ))
                })
                .collect::<Vec<_>>();
            let mut selected = ListState::default();
            selected.select(Some(self.at));
            f.render_stateful_widget(
                List::new(rows)
                    .block(bordered("ancestors → selected → children"))
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                list,
                &mut selected,
            );
            let text = self
                .branch()
                .map(|n| {
                    format!(
                        "{}\n{}{}\nNext event #{}{}{}",
                        n.id,
                        n.workspace,
                        if n.workspace_truncated { "…" } else { "" },
                        n.next_seq,
                        page.missing_parent
                            .as_ref()
                            .map(|id| format!("\nParent unavailable: {id}"))
                            .unwrap_or_default(),
                        if page.next.is_some() {
                            "\nn scans more children; this is a bounded scan page."
                        } else {
                            ""
                        }
                    )
                })
                .unwrap_or_default();
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), detail);
            f.render_widget(Paragraph::new(self.note.as_str()).wrap(Wrap { trim: false }), help);
            return;
        }
        let [top, body, note] = Layout::vertical([
            Constraint::Length(if self.turns.is_some() { 5 } else { 3 }),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .areas(area);
        let title = match self.editing {
            Some(true) => "jump to event #",
            Some(false) => "find literal text",
            None if self.turns.is_some() => "turn results · t refresh · n older · h history",
            None => "history · / find · g jump · q quote · B branch · t turn results",
        };
        let text = if self.editing.is_some() {
            self.input.text.clone()
        } else if let Some(turns) = &self.turns {
            rook_core::turns::describe(turns)
        } else {
            self.session.map(rook_store::format_session_id).unwrap_or_default()
        };
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(bordered(title)), top);
        let [list, detail] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(body);
        let rows: Vec<ListItem> = if let Some(hits) = &self.hits {
            hits.hits
                .iter()
                .map(|h| ListItem::new(format!("#{} {} {}", h.seq, h.kind, h.snippet.replace('\n', " "))))
                .collect()
        } else if let Some(turns) = &self.turns {
            turns
                .items
                .iter()
                .map(|e| {
                    ListItem::new(format!(
                        "#{} {}{}",
                        e.result_seq,
                        e.summary.stopped,
                        e.summary.follow_up.as_ref().map(|id| format!(" · {id}")).unwrap_or_default()
                    ))
                })
                .collect()
        } else {
            self.page
                .as_ref()
                .map(|p| {
                    p.items
                        .iter()
                        .map(|e| ListItem::new(format!("#{} {} {}", e.seq, e.kind, e.label)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut selected = ListState::default();
        selected.select(Some(self.at));
        f.render_stateful_widget(
            List::new(rows)
                .block(bordered(if self.hits.is_some() { "matches" } else { "events" }))
                .highlight_style(Style::default().bg(Color::DarkGray)),
            list,
            &mut selected,
        );
        let text = if let Some(page) = &self.entry {
            format!(
                "#{} · byte {} / {}{}\n\n{}",
                page.entry.seq,
                page.offset,
                page.total_bytes,
                if page.next_offset.is_some() { " · n reads more" } else { "" },
                page.entry.body
            )
        } else if let Some(hits) = &self.hits {
            hits.hits
                .get(self.at)
                .map(|h| format!("#{} · Enter opens at the match\n\n{}", h.seq, h.snippet))
                .unwrap_or_else(|| {
                    "No matches on this scan page. n continues when a cursor is available.".into()
                })
        } else if let Some(turns) = &self.turns {
            turns.items.get(self.at).map(rook_core::turns::entry_text).unwrap_or_else(|| {
                "No recorded outcomes on this scan page. n scans older events when available.".into()
            })
        } else {
            self.page
                .as_ref()
                .and_then(|p| p.items.get(self.at))
                .map(|e| {
                    format!(
                        "#{} {}{}\n\n{}",
                        e.seq,
                        e.kind,
                        if e.truncated { " · Enter reads full body in pages" } else { "" },
                        e.body
                    )
                })
                .unwrap_or_else(|| "No events on this page.".into())
        };
        f.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(bordered("message")),
            detail,
        );
        f.render_widget(Paragraph::new(self.note.as_str()).wrap(Wrap { trim: false }), note);
    }
}
