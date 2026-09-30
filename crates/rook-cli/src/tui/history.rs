//! One bounded reader worker keeps navigation off the input/streaming thread.
use super::*;
use rook_core::transcript::{Cursor, EntryPage, Matches, Page, PageRequest};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

pub(super) struct History {
    session: Option<u128>,
    epoch: u64,
    pending: bool,
    page: Option<Page>,
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
    Page(u128, PageRequest),
    Search(u128, String, Cursor),
    Entry(u128, u64, u64),
    Quote(u128, u64, u64),
}
enum Update {
    Page(Page),
    Search(Matches),
    Entry(EntryPage),
    Quote(String),
}
impl Command {
    fn read(self, source: &crate::source::Source) -> Result<Update> {
        Ok(match self {
            Self::Page(session, q) => Update::Page(source.transcript_page(session, &q)?),
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
        self.epoch = self.epoch.wrapping_add(1);
        self.session = session;
        self.pending = false;
        self.page = None;
        self.hits = None;
        self.entry = None;
        self.at = 0;
        self.scroll = 0;
        self.quote = None;
        self.editing = None;
        if let Some(session) = session {
            self.ask(Command::Page(session, PageRequest::default()));
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
            if epoch != self.epoch {
                continue;
            }
            self.pending = false;
            self.note.clear();
            match update {
                Ok(Update::Page(page)) => {
                    self.at = page.items.len().saturating_sub(1);
                    self.page = Some(page);
                    self.hits = None;
                    self.entry = None;
                    self.scroll = 0;
                }
                Ok(Update::Search(hits)) => {
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
    fn target(&self) -> Option<(u64, u64)> {
        if let Some(entry) = &self.entry {
            return Some((entry.entry.seq, entry.offset));
        }
        if let Some(hits) = &self.hits {
            return hits.hits.get(self.at).map(|h| (h.seq, h.offset));
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
            return true;
        }
        let Some(session) = self.session else {
            return false;
        };
        match key.code {
            KeyCode::Char('/') => {
                self.editing = Some(false);
                self.input.set(&self.needle);
            }
            KeyCode::Char('g') => {
                self.editing = Some(true);
                self.input.set("");
            }
            KeyCode::Char('r') => {
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
        let [top, body, note] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(2)]).areas(area);
        let title = match self.editing {
            Some(true) => "jump to event #",
            Some(false) => "find literal text",
            None => "history · / find · g jump · q quote to draft",
        };
        let text = if self.editing.is_some() {
            self.input.text.clone()
        } else {
            self.session.map(rook_store::format_session_id).unwrap_or_default()
        };
        f.render_widget(Paragraph::new(text).block(bordered(title)), top);
        let [list, detail] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(body);
        let rows: Vec<ListItem> = if let Some(hits) = &self.hits {
            hits.hits
                .iter()
                .map(|h| ListItem::new(format!("#{} {} {}", h.seq, h.kind, h.snippet.replace('\n', " "))))
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
