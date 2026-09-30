//! One worker and one pending operation keep queue I/O off the terminal thread.
use super::*;
use rook_proto::queue::{Change, Entry, Page, Query};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

enum Command {
    Page(Query),
    Read(String, bool),
    Change(Change, bool),
}
enum Update {
    Page(Page),
    Read(Entry, bool),
    Changed(Entry, bool),
}
impl Command {
    fn run(self, source: &crate::source::Source, session: u128) -> Result<Update> {
        Ok(match self {
            Self::Page(query) => Update::Page(source.queue_page(session, &query)?),
            Self::Read(reference, edit) => Update::Read(source.queue_read(session, &reference)?, edit),
            Self::Change(change, restore) => Update::Changed(source.queue_change(session, change)?, restore),
        })
    }
}

pub(super) struct Queue {
    session: Option<u128>,
    pending: bool,
    page: Option<Page>,
    entry: Option<Entry>,
    at: usize,
    all: bool,
    editing: bool,
    input: Typing,
    note: String,
    restored: Option<(u128, String)>,
    notice: Option<(rook_proto::queue::Notice, String)>,
    send: Option<SyncSender<(u128, Command)>>,
    receive: Receiver<(u128, Result<Update>)>,
}

impl Queue {
    pub(super) fn new(source: &crate::source::Source) -> Self {
        let (send, commands) = sync_channel::<(u128, Command)>(1);
        let (updates, receive) = sync_channel(1);
        let result = source.reader().and_then(|reader| {
            Ok(std::thread::Builder::new().name("queue-editor".into()).spawn(move || {
                while let Ok((session, command)) = commands.recv() {
                    if updates.send((session, command.run(&reader, session))).is_err() {
                        break;
                    }
                }
            })?)
        });
        let (send, note) = match result {
            Ok(_) => (Some(send), String::new()),
            Err(error) => (None, error.to_string()),
        };
        Self {
            session: None,
            pending: false,
            page: None,
            entry: None,
            at: 0,
            all: false,
            editing: false,
            input: Typing::default(),
            note,
            restored: None,
            notice: None,
            send,
            receive,
        }
    }

    pub(super) fn open(&mut self, session: Option<u128>) {
        // Do not discard a mutation's result by switching this worker to another
        // session while it is in flight. Closing the panel does not cancel it.
        if self.pending {
            self.note = "Finishing the current queue operation…".into();
            return;
        }
        self.session = session;
        self.entry = None;
        self.editing = false;
        self.page = None;
        self.at = 0;
        if session.is_some() {
            self.refresh(None);
        } else {
            self.note = "Start or open a session first.".into();
        }
    }

    fn ask(&mut self, command: Command) {
        if self.pending {
            return;
        }
        let Some(session) = self.session else {
            return;
        };
        match self.send.as_ref().map(|send| send.try_send((session, command))) {
            Some(Ok(())) => {
                self.pending = true;
                self.note = "Reading or updating the queue…".into();
            }
            _ => self.note = "Queue worker unavailable; reopen the window.".into(),
        }
    }
    fn refresh(&mut self, after: Option<String>) {
        self.ask(Command::Page(Query { after, include_finished: self.all }));
    }

    pub(super) fn take_notice(&mut self) -> Option<(rook_proto::queue::Notice, String)> {
        self.notice.take()
    }

    pub(super) fn poll(&mut self, current: Option<u128>) -> Option<String> {
        while let Ok((session, result)) = self.receive.try_recv() {
            self.pending = false;
            match result {
                Ok(Update::Page(page)) => {
                    self.page = Some(page);
                    self.entry = None;
                    self.at = 0;
                    self.note = "e edits · d withdraws · q withdraws into draft · a toggles finished · r refreshes · n next · Esc closes".into();
                }
                Ok(Update::Read(entry, edit)) => {
                    self.editing = edit && entry.receipt.queued();
                    if self.editing {
                        self.input.set(&entry.receipt.text);
                        self.note = "Ctrl-S saves · Enter inserts a newline · Esc cancels editing".into();
                    } else {
                        self.note = if entry.receipt.queued() {
                            "e edits · d withdraws · q withdraws into draft · r refreshes"
                        } else {
                            "This receipt is immutable; r refreshes the queue."
                        }
                        .into();
                    }
                    self.entry = Some(entry);
                }
                Ok(Update::Changed(entry, restore)) => {
                    self.editing = false;
                    self.notice = Some((
                        rook_proto::queue::Notice::new(
                            rook_store::format_session_id(session),
                            entry.reference.clone(),
                            &entry.receipt,
                        ),
                        entry.receipt.text.clone(),
                    ));
                    if restore {
                        self.restored = Some((session, entry.receipt.text));
                    }
                    self.refresh(None);
                }
                Err(error) => {
                    self.note =
                        format!("{error} · draft retained; Esc cancels editing, r refreshes the queue")
                }
            }
        }
        if self.restored.as_ref().is_some_and(|(session, _)| Some(*session) == current) {
            self.restored.take().map(|(_, text)| text)
        } else {
            None
        }
    }

    fn selected(&self) -> Option<&Entry> {
        self.entry.as_ref().or_else(|| self.page.as_ref()?.items.get(self.at))
    }
    pub(super) fn paste(&mut self, text: &str) {
        if self.editing && !self.pending {
            let limit = self.page.as_ref().map_or(0, |page| page.max_message_bytes);
            if text.len() <= limit.saturating_sub(self.input.text.len()) {
                self.input.paste(text);
            } else {
                self.note =
                    format!("Message exceeds work.max_message_bytes ({limit}); paste was not inserted.");
            }
        }
    }
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.pending {
            return key.code == KeyCode::Esc;
        }
        if self.editing {
            match key.code {
                KeyCode::Esc => {
                    self.editing = false;
                    self.entry = None;
                    self.note = "Edit cancelled; r refreshes.".into();
                }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(entry) = &self.entry {
                        self.ask(Command::Change(
                            Change::Edit {
                                reference: entry.reference.clone(),
                                revision: entry.receipt.revision,
                                text: self.input.text.clone(),
                            },
                            false,
                        ));
                    }
                }
                KeyCode::Backspace => self.input.backspace(),
                KeyCode::Delete => self.input.delete(),
                KeyCode::Left => self.input.left(),
                KeyCode::Right => self.input.right(),
                KeyCode::Up => {
                    self.input.up();
                }
                KeyCode::Down => {
                    self.input.down();
                }
                KeyCode::Home => self.input.home(),
                KeyCode::End => self.input.end(),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.input.set(""),
                KeyCode::Enter => self.paste("\n"),
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.paste(c.encode_utf8(&mut [0; 4]))
                }
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Char('r') => self.refresh(None),
            KeyCode::Char('a') => {
                self.all = !self.all;
                self.refresh(None);
            }
            KeyCode::Char('n') => {
                if let Some(next) = self.page.as_ref().and_then(|page| page.next.clone()) {
                    self.refresh(Some(next));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.entry = None;
                self.at =
                    (self.at + 1).min(self.page.as_ref().map_or(0, |p| p.items.len().saturating_sub(1)));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.entry = None;
                self.at = self.at.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(entry) = self.selected() {
                    self.ask(Command::Read(entry.reference.clone(), key.code == KeyCode::Char('e')));
                }
            }
            KeyCode::Char('d') | KeyCode::Char('q') => {
                if self.restored.is_some() {
                    self.note =
                        "Switch to the previous message's session to receive its restored draft first."
                            .into();
                } else if let Some(entry) = self.selected() {
                    self.ask(Command::Change(
                        Change::Withdraw {
                            reference: entry.reference.clone(),
                            revision: entry.receipt.revision,
                        },
                        key.code == KeyCode::Char('q'),
                    ));
                }
            }
            _ => {}
        }
        false
    }

    pub(super) fn draw(&self, f: &mut Frame, area: Rect) {
        let [rows, body, note] =
            Layout::vertical([Constraint::Percentage(35), Constraint::Min(4), Constraint::Length(4)])
                .areas(area);
        let items: Vec<_> = self
            .page
            .as_ref()
            .map(|page| {
                page.items
                    .iter()
                    .map(|entry| {
                        ListItem::new(format!(
                            "{} · r{} · {}",
                            crate::commands::queue::status(&entry.receipt),
                            entry.receipt.revision,
                            entry.receipt.text.lines().next().unwrap_or_default()
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut selected = ListState::default();
        selected.select(Some(self.at));
        f.render_stateful_widget(
            List::new(items)
                .block(Block::bordered().title(if self.all {
                    " Message queue · all receipts "
                } else {
                    " Message queue · pending "
                }))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            rows,
            &mut selected,
        );
        if self.editing {
            let block = Block::bordered().title(" Edit pending message · Ctrl-S saves ");
            let inside = block.inner(body);
            let (lines, (x, y)) = self.input.view("", inside.width, inside.height);
            f.render_widget(block, body);
            f.render_widget(Paragraph::new(lines), inside);
            if inside.width > 0 && inside.height > 0 {
                f.set_cursor_position((inside.x + x, inside.y + y));
            }
        } else {
            let text = self
                .selected()
                .map(|entry| {
                    format!(
                        "{} · revision {}{}\n\n{}",
                        entry.reference,
                        entry.receipt.revision,
                        if entry.truncated { " · shortened; Enter reads full text" } else { "" },
                        entry.receipt.text
                    )
                })
                .unwrap_or_else(|| "No messages on this page. r refreshes.".into());
            f.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::bordered().title(format!(
                    " Message · {} ",
                    self.session.map(rook_store::format_session_id).unwrap_or_default()
                ))),
                body,
            );
        }
        f.render_widget(Paragraph::new(self.note.as_str()).wrap(Wrap { trim: false }), note);
    }
}
