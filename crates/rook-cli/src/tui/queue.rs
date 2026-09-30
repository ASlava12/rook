//! One worker and one pending operation keep queue I/O off the terminal thread.
use super::*;
use rook_proto::queue::{Change, Entry, Page, Query};
use std::cell::{Cell, RefCell};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

enum Command {
    Prepare(bool),
    Submit(Change),
    Page(Query),
    Peek,
    Read(String, bool),
    Change(Change, bool),
}
enum Update {
    Prepared(String, usize),
    Submitted(Entry),
    Page(Page),
    Peek(Preview),
    Read(Entry, bool),
    Changed(Entry, bool),
}
#[derive(Default)]
struct DetailIndex {
    width: u16,
    rows: usize,
    /// One byte offset every 256 rendered rows, bounded by message bytes.
    checkpoints: Vec<(usize, usize)>,
}
#[derive(Default)]
struct Preview {
    next: Option<String>,
    total: usize,
}

impl Preview {
    fn from_page(page: Page) -> Self {
        // The page is already bounded by the store. Retain only one short line
        // in the TUI so a queued paste never gets copied into every frame.
        Self {
            next: page.items.first().map(|entry| {
                entry
                    .receipt
                    .text
                    .chars()
                    .take(160)
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect()
            }),
            total: page.total,
        }
    }
}
impl Command {
    fn run(self, source: &crate::source::Source, session: u128) -> Result<Update> {
        Ok(match self {
            Self::Prepare(follow_up) => {
                let page = source.queue_page(session, &Query::default())?;
                Update::Prepared(
                    if follow_up {
                        page.follow_up_target.ok_or_else(|| {
                            anyhow::anyhow!("start a turn or goal before queuing a follow-up")
                        })?
                    } else {
                        page.submission_target
                    },
                    page.max_message_bytes,
                )
            }
            Self::Submit(change) => Update::Submitted(source.queue_change(session, change)?),
            Self::Page(query) => Update::Page(source.queue_page(session, &query)?),
            Self::Peek => Update::Peek(Preview::from_page(source.queue_page(session, &Query::default())?)),
            Self::Read(reference, edit) => Update::Read(source.queue_read(session, &reference)?, edit),
            Self::Change(change, restore) => Update::Changed(source.queue_change(session, change)?, restore),
        })
    }
}

pub(super) struct Queue {
    attempt: Option<(u128, Change)>,
    status: Option<String>,
    max_text: usize,
    session: Option<u128>,
    pending: bool,
    pending_peek: bool,
    submit_after_peek: bool,
    open_after_pending: Option<u128>,
    page: Option<Page>,
    preview: Preview,
    preview_session: Option<u128>,
    preview_dirty: bool,
    preview_at: Option<std::time::Instant>,
    entry: Option<Entry>,
    at: usize,
    detail_scroll: Cell<usize>,
    detail_max: Cell<usize>,
    detail_index: RefCell<DetailIndex>,
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
    pub(super) fn new(source: &crate::source::Source, max_text: usize) -> Self {
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
            attempt: None,
            status: None,
            max_text: max_text.min(8 * 1024 * 1024),
            session: None,
            pending: false,
            pending_peek: false,
            submit_after_peek: false,
            open_after_pending: None,
            page: None,
            preview: Preview::default(),
            preview_session: None,
            preview_dirty: true,
            preview_at: None,
            entry: None,
            at: 0,
            detail_scroll: Cell::new(0),
            detail_max: Cell::new(0),
            detail_index: RefCell::new(DetailIndex::default()),
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
            self.open_after_pending = session;
            self.note = "Finishing the current queue operation…".into();
            return;
        }
        if let Some((session, _)) = &self.attempt {
            self.session = Some(*session);
            return;
        }
        self.session = session;
        self.entry = None;
        self.editing = false;
        self.page = None;
        self.at = 0;
        self.detail_scroll.set(0);
        self.detail_index.get_mut().width = 0;
        if session.is_some() {
            self.refresh(None);
        } else {
            self.note = "Start or open a session first.".into();
        }
    }

    /// Reserve the only in-flight submission before copying its bounded text.
    /// Preparing only reads the target; the first write happens after it is
    /// retained here, so retries can never resolve a different goal generation.
    pub(super) fn submit(&mut self, session: u128, text: &str, follow_up: bool) -> Result<()> {
        anyhow::ensure!(
            (!self.pending || self.pending_peek) && self.attempt.is_none() && !self.editing,
            "queue request pending; /queue opens it (u retries a failed send); your new draft is retained"
        );
        anyhow::ensure!(
            !text.trim().is_empty() && text.len() <= self.max_text,
            "message must fit work.max_message_bytes ({})",
            self.max_text
        );
        let id = rook_store::format_session_id(rook_store::new_session_id());
        self.attempt = Some((
            session,
            if follow_up {
                Change::FollowUp { target: String::new(), id, text: text.into() }
            } else {
                Change::Submit { target: String::new(), id, text: text.into() }
            },
        ));
        self.preview_dirty = true;
        if self.pending_peek {
            self.submit_after_peek = true;
        } else {
            self.retry();
        }
        Ok(())
    }

    fn retry(&mut self) {
        if self.pending {
            return;
        }
        let Some((session, change)) = &self.attempt else { return };
        self.session = Some(*session);
        let prepare = matches!(change, Change::Submit { target, .. } | Change::FollowUp { target, .. } if target.is_empty());
        let command = if prepare {
            Command::Prepare(matches!(change, Change::FollowUp { .. }))
        } else {
            Command::Submit(change.clone())
        };
        self.ask(command);
        if !self.pending {
            self.status = Some(format!("{} · /queue retains this send for retry", self.note));
        }
    }

    pub(super) fn take_status(&mut self) -> Option<String> {
        self.status.take()
    }

    fn ask(&mut self, command: Command) {
        if self.pending {
            return;
        }
        let Some(session) = self.session else {
            return;
        };
        let quiet = matches!(command, Command::Peek);
        match self.send.as_ref().map(|send| send.try_send((session, command)).is_ok()) {
            Some(true) => {
                self.pending = true;
                self.pending_peek = quiet;
                if !quiet {
                    self.note = "Reading or updating the queue…".into();
                }
            }
            _ if !quiet => self.note = "Queue worker unavailable; reopen the window.".into(),
            _ => {}
        }
    }
    fn refresh(&mut self, after: Option<String>) {
        self.ask(Command::Page(Query { after, include_finished: self.all }));
    }

    /// Read a bounded queue snapshot off the terminal thread. Receipt notices
    /// invalidate it promptly; a short interval also catches other clients.
    pub(super) fn sync_preview(&mut self, current: Option<u128>, busy: bool) {
        if self.preview_session != current {
            self.preview = Preview::default();
            self.preview_session = current;
            self.preview_dirty = true;
            self.preview_at = None;
        }
        if !busy || current.is_none() || self.pending {
            return;
        }
        if !self.preview_dirty
            && self.preview_at.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(2))
        {
            return;
        }
        self.session = current;
        self.ask(Command::Peek);
        if self.pending {
            self.preview_dirty = false;
            self.preview_at = Some(std::time::Instant::now());
        }
    }

    pub(super) fn preview(&self, current: Option<u128>) -> Option<(usize, &str)> {
        if self.preview_session != current {
            return None;
        }
        if let Some(next) = self.preview.next.as_deref() {
            return Some((self.preview.total + usize::from(self.attempt.is_some()), next));
        }
        if self.preview.total > 0 {
            return Some((self.preview.total, "Open /queue to inspect the next message"));
        }
        self.attempt.as_ref().and_then(|(session, change)| {
            if Some(*session) != current {
                return None;
            }
            match change {
                Change::Submit { text, .. } | Change::FollowUp { text, .. } => Some((1, text.as_str())),
                _ => None,
            }
        })
    }

    pub(super) fn invalidate_preview(&mut self) {
        self.preview_dirty = true;
        self.preview = Preview::default();
    }

    pub(super) fn take_notice(&mut self) -> Option<(rook_proto::queue::Notice, String)> {
        self.notice.take()
    }

    pub(super) fn poll(&mut self, current: Option<u128>) -> Option<String> {
        while let Ok((session, result)) = self.receive.try_recv() {
            self.pending = false;
            self.pending_peek = false;
            match result {
                Ok(Update::Prepared(target, limit)) => {
                    if let Some((
                        _,
                        Change::Submit { target: saved, text, .. }
                        | Change::FollowUp { target: saved, text, .. },
                    )) = &mut self.attempt
                    {
                        if target.is_empty() || text.len() > limit {
                            self.note = "Cannot send: daemon needs an update or message exceeds its byte limit. c discards this local attempt.".into();
                            self.status = Some(self.note.clone());
                            continue;
                        }
                        *saved = target;
                        self.retry();
                    }
                }
                Ok(Update::Submitted(entry)) => {
                    self.attempt = None;
                    self.invalidate_preview();
                    self.notice = Some((
                        rook_proto::queue::Notice::new(
                            rook_store::format_session_id(session),
                            entry.reference.clone(),
                            &entry.receipt,
                        ),
                        format!("↩ {}", entry.receipt.text),
                    ));
                    self.note = "Submission confirmed. r refreshes the queue.".into();
                }
                Ok(Update::Page(page)) => {
                    self.page = Some(page);
                    self.entry = None;
                    self.at = 0;
                    self.detail_scroll.set(0);
                    self.detail_index.get_mut().width = 0;
                    self.note = format!(
                        "{}e edits · d withdraws · q withdraws into draft · a toggles finished · r refreshes · n next · Esc closes",
                        self.page
                            .as_ref()
                            .and_then(|p| p.follow_up_status.as_ref())
                            .map(|s| format!("{s}\n"))
                            .unwrap_or_default()
                    );
                }
                Ok(Update::Peek(preview)) => {
                    if Some(session) == current && !self.preview_dirty {
                        self.preview = preview;
                    }
                }
                Ok(Update::Read(entry, edit)) => {
                    self.detail_scroll.set(0);
                    self.detail_index.get_mut().width = 0;
                    self.editing = edit
                        && entry.receipt.queued()
                        && entry.receipt.follow_up.as_ref().is_none_or(|f| f.reserved.is_none());
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
                    self.invalidate_preview();
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
                    if self.attempt.is_some() {
                        self.note = format!(
                            "{error} · /queue: u retries the same ID and target; c forgets the local attempt (it may already be queued)."
                        );
                        self.status = Some(self.note.clone());
                    } else {
                        self.note =
                            format!("{error} · draft retained; Esc cancels editing, r refreshes the queue");
                    }
                }
            }
        }
        if !self.pending && std::mem::take(&mut self.submit_after_peek) {
            self.retry();
        }
        if !self.pending
            && let Some(session) = self.open_after_pending.take()
            && Some(session) == current
        {
            self.open(Some(session));
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
    pub(super) fn scroll(&mut self, wheel: MouseEventKind) {
        if self.editing {
            return;
        }
        match wheel {
            MouseEventKind::ScrollUp => self.detail_scroll.set(self.detail_scroll.get().saturating_sub(3)),
            MouseEventKind::ScrollDown => {
                self.detail_scroll.set(self.detail_scroll.get().saturating_add(3).min(self.detail_max.get()))
            }
            _ => {}
        }
    }
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.pending {
            return key.code == KeyCode::Esc;
        }
        if self.attempt.is_some() {
            match key.code {
                KeyCode::Esc => return true,
                KeyCode::Char('u') => self.retry(),
                KeyCode::Char('c') => {
                    self.attempt = None;
                    self.note =
                        "Local retry forgotten; the message may already be in the saved queue. r refreshes."
                            .into();
                }
                _ => {}
            }
            return false;
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
            KeyCode::PageUp => self.detail_scroll.set(self.detail_scroll.get().saturating_sub(8)),
            KeyCode::PageDown => {
                self.detail_scroll.set(self.detail_scroll.get().saturating_add(8).min(self.detail_max.get()))
            }
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
                self.detail_scroll.set(0);
                self.detail_index.get_mut().width = 0;
                self.at =
                    (self.at + 1).min(self.page.as_ref().map_or(0, |p| p.items.len().saturating_sub(1)));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.entry = None;
                self.detail_scroll.set(0);
                self.detail_index.get_mut().width = 0;
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
                            "{}{} · r{} · {}",
                            if entry.receipt.follow_up.is_some() { "follow-up · " } else { "" },
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
        if let Some((session, Change::Submit { target, id, text } | Change::FollowUp { target, id, text })) =
            &self.attempt
        {
            f.render_widget(Paragraph::new(format!("Session {}\nTarget {target} · ID {id}\n{text}\n\nu retries the same send · c forgets the local retry (may already be queued)", rook_store::format_session_id(*session)))
                .wrap(Wrap { trim: false }).block(Block::bordered().title(" Unconfirmed submission ")), body);
        } else if self.editing {
            let block = Block::bordered().title(" Edit pending message · Ctrl-S saves ");
            let inside = block.inner(body);
            let (lines, (row, column)) = self.input.view("", inside.width, inside.height);
            f.render_widget(block, body);
            f.render_widget(Paragraph::new(lines), inside);
            if inside.width > 0 && inside.height > 0 {
                f.set_cursor_position((inside.x + column, inside.y + row));
            }
        } else {
            let block = Block::bordered().title(format!(
                " Message · {} · PgUp/PgDn scroll ",
                self.session.map(rook_store::format_session_id).unwrap_or_default()
            ));
            let inside = block.inner(body);
            f.render_widget(block, body);
            if let Some(entry) = self.selected() {
                let header = format!(
                    "{} · revision {}{}{}",
                    entry.reference,
                    entry.receipt.revision,
                    entry
                        .receipt
                        .follow_up
                        .as_ref()
                        .map(|follow_up| format!(
                            " · follow-up after {}{}{}",
                            follow_up.after,
                            if follow_up.reserved.is_some() { " · reserved" } else { "" },
                            follow_up
                                .blocked
                                .as_ref()
                                .map(|reason| format!(" · stopped: {reason}"))
                                .unwrap_or_default()
                        ))
                        .unwrap_or_default(),
                    if entry.truncated { " · shortened; Enter reads full text" } else { "" },
                );
                let header_height = inside.height.min(2);
                let header_area = Rect { height: header_height, ..inside };
                f.render_widget(Paragraph::new(header).wrap(Wrap { trim: false }), header_area);
                let text_area =
                    Rect { y: inside.y + header_height, height: inside.height - header_height, ..inside };
                let geometry = wrapping::Geometry::new("", text_area.width);
                let mut index = self.detail_index.borrow_mut();
                if index.width != text_area.width || index.rows == 0 {
                    let mut rows = 0;
                    let mut checkpoints = Vec::new();
                    let mut next = 0;
                    geometry.walk(&entry.receipt.text, |byte, row, column, _, _| {
                        rows = row + 1;
                        if column == 0 && row >= next {
                            checkpoints.push((row, byte));
                            next = row.saturating_add(256);
                        }
                        true
                    });
                    *index = DetailIndex { width: text_area.width, rows, checkpoints };
                }
                let maximum = index.rows.saturating_sub(usize::from(text_area.height));
                self.detail_max.set(maximum);
                let offset = self.detail_scroll.get().min(maximum);
                self.detail_scroll.set(offset);
                let &(row, byte) =
                    index.checkpoints.iter().rev().find(|(row, _)| *row <= offset).unwrap_or(&(0, 0));
                let lines = geometry.window(&entry.receipt.text[byte..], offset - row, text_area.height);
                f.render_widget(Paragraph::new(lines), text_area);
            } else {
                f.render_widget(Paragraph::new("No messages on this page. r refreshes."), inside);
            }
        }
        f.render_widget(Paragraph::new(self.note.as_str()).wrap(Wrap { trim: false }), note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestWindow = (Queue, Receiver<(u128, Command)>, SyncSender<(u128, Result<Update>)>);

    fn window(limit: usize) -> TestWindow {
        let (send, commands) = sync_channel(1);
        let (updates, receive) = sync_channel(1);
        (
            Queue {
                session: None,
                pending: false,
                pending_peek: false,
                submit_after_peek: false,
                open_after_pending: None,
                page: None,
                preview: Preview::default(),
                preview_session: None,
                preview_dirty: true,
                preview_at: None,
                entry: None,
                at: 0,
                detail_scroll: Cell::new(0),
                detail_max: Cell::new(0),
                detail_index: RefCell::new(DetailIndex::default()),
                all: false,
                editing: false,
                input: Typing::default(),
                note: String::new(),
                restored: None,
                notice: None,
                send: Some(send),
                receive,
                attempt: None,
                status: None,
                max_text: limit,
            },
            commands,
            updates,
        )
    }

    fn screen(queue: &Queue) -> String {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(52, 20)).unwrap();
        terminal.draw(|frame| queue.draw(frame, frame.area())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..20)
            .map(|y| (0..52).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn pinned_preview_tracks_first_receipt_and_ignores_stale_snapshots() {
        let (mut queue, commands, updates) = window(4096);
        queue.sync_preview(Some(42), true);
        assert!(matches!(commands.try_recv(), Ok((42, Command::Peek))));
        let page: Page = serde_json::from_value(serde_json::json!({
            "items": [{"reference": "session.first", "truncated": false,
                "receipt": {"id": "first", "text": "first\nmessage", "submitted_at": 1,
                            "applied_at": null, "session": null}}],
            "next": null, "total": 2, "max_message_bytes": 4096,
            "submission_target": "session"
        }))
        .unwrap();
        updates.send((42, Ok(Update::Peek(Preview::from_page(page))))).unwrap();
        queue.poll(Some(42));
        assert_eq!(queue.preview(Some(42)), Some((2, "first message")));

        queue.invalidate_preview();
        assert_eq!(queue.preview(Some(42)), None);
        queue.sync_preview(Some(42), true);
        assert!(matches!(commands.try_recv(), Ok((42, Command::Peek))));
        queue.invalidate_preview();
        updates.send((42, Ok(Update::Peek(Preview { next: Some("stale".into()), total: 1 })))).unwrap();
        queue.poll(Some(42));
        assert_eq!(queue.preview(Some(42)), None);

        queue.sync_preview(Some(43), true);
        assert!(matches!(commands.try_recv(), Ok((43, Command::Peek))));
        assert_eq!(queue.preview(Some(42)), None);
    }

    #[test]
    fn opening_queue_during_background_preview_still_loads_the_panel() {
        let (mut queue, commands, updates) = window(4096);
        queue.sync_preview(Some(42), true);
        assert!(matches!(commands.try_recv(), Ok((42, Command::Peek))));
        queue.open(Some(42));
        updates.send((42, Ok(Update::Peek(Preview::default())))).unwrap();
        queue.poll(Some(42));
        assert!(matches!(commands.try_recv(), Ok((42, Command::Page(_)))));
    }

    #[test]
    fn submission_waits_for_background_preview_without_losing_the_draft() {
        let (mut queue, commands, updates) = window(4096);
        queue.sync_preview(Some(42), true);
        assert!(matches!(commands.try_recv(), Ok((42, Command::Peek))));
        queue.submit(42, "send this next", false).unwrap();
        assert_eq!(queue.preview(Some(42)), Some((1, "send this next")));
        updates.send((42, Ok(Update::Peek(Preview::default())))).unwrap();
        queue.poll(Some(42));
        assert!(matches!(commands.try_recv(), Ok((42, Command::Prepare(false)))));
        assert!(queue.attempt.is_some());
    }

    #[test]
    fn read_only_queue_detail_scrolls_to_the_last_line_and_back_without_changing_the_receipt() {
        let (mut queue, _, _) = window(4096);
        queue.session = Some(42);
        let text = (0..30).map(|n| format!("line-{n:02}")).collect::<Vec<_>>().join("\n");
        queue.entry = Some(
            serde_json::from_value(serde_json::json!({
                "reference": "session.long", "truncated": false,
                "receipt": { "id": "long", "text": text, "submitted_at": 1,
                             "applied_at": null, "session": null }
            }))
            .unwrap(),
        );
        let initial = screen(&queue);
        assert!(initial.contains("line-00"), "{initial}");
        queue.key(crossterm::event::KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        let scrolled = screen(&queue);
        assert!(!scrolled.contains("line-00") && scrolled.contains("line-08"), "{scrolled}");
        for _ in 0..30 {
            queue.key(crossterm::event::KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        let end = screen(&queue);
        assert!(end.contains("line-29"), "{end}");
        assert_eq!(queue.detail_scroll.get(), queue.detail_max.get());
        queue.scroll(MouseEventKind::ScrollUp);
        assert!(queue.detail_scroll.get() < queue.detail_max.get());
        assert_eq!(queue.entry.as_ref().unwrap().receipt.text, text);
    }

    #[test]
    fn queue_detail_can_reach_past_the_terminal_widget_scroll_limit() {
        let (mut queue, _, _) = window(8 * 1024 * 1024);
        queue.session = Some(42);
        let text = format!("{}TAIL_MARK", "x\n".repeat(66_000));
        queue.entry = Some(
            serde_json::from_value(serde_json::json!({
                "reference": "session.long", "truncated": false,
                "receipt": { "id": "long", "text": text, "submitted_at": 1,
                             "applied_at": null, "session": null }
            }))
            .unwrap(),
        );
        screen(&queue);
        assert!(queue.detail_index.borrow().rows > u16::MAX as usize);
        for _ in 0..9_000 {
            queue.key(crossterm::event::KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        let end = screen(&queue);
        assert!(end.contains("TAIL_MARK"), "last line remained unreachable: {end}");
    }

    #[test]
    fn an_uncertain_send_retries_the_same_target_id_and_text_even_after_switching_sessions() {
        for follow_up in [false, true] {
            let (mut queue, commands, updates) = window(16);
            queue.submit(42, "correct this", follow_up).unwrap();
            assert!(
                matches!(commands.try_recv().unwrap(), (42, Command::Prepare(mode)) if mode == follow_up)
            );
            updates.send((42, Ok(Update::Prepared("goal.original".into(), 16)))).unwrap();
            queue.poll(Some(42));
            let (session, Command::Submit(first)) = commands.try_recv().unwrap() else {
                panic!("not submitted")
            };
            assert_eq!(session, 42);
            updates.send((42, Err(anyhow::anyhow!("response lost after commit")))).unwrap();
            queue.poll(Some(42));
            assert!(queue.take_status().unwrap().contains("same ID and target"));
            queue.open(Some(43));
            queue.retry();
            let (session, Command::Submit(retry)) = commands.try_recv().unwrap() else {
                panic!("retry must not prepare a new target")
            };
            assert_eq!(session, 42);
            assert_eq!(serde_json::to_value(first).unwrap(), serde_json::to_value(retry).unwrap());
        }
    }

    #[test]
    fn one_bounded_pending_send_refuses_another_before_copying_it() {
        let (mut queue, commands, _) = window(4);
        assert!(queue.submit(42, "🙂a", false).is_err());
        assert!(queue.attempt.is_none());
        assert!(commands.try_recv().is_err());
        queue.submit(42, "🙂", false).unwrap();
        assert!(queue.pending);
        assert!(queue.submit(42, "next", false).is_err());
        let Some((42, Change::Submit { text, .. })) = queue.attempt else { panic!("lost first message") };
        assert_eq!(text, "🙂");
    }
}
