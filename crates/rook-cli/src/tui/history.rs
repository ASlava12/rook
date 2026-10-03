//! One bounded reader worker keeps navigation off the input/streaming thread.
use super::*;
use rook_core::transcript::{Cursor, EntryPage, Matches, Page, PageRequest};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

pub(super) struct History {
    session: Option<u128>,
    departed: Option<u128>,
    epoch: u64,
    pending: bool,
    page: Option<Page>,
    turns: Option<rook_core::turns::Page>,
    tree: Option<rook_core::branches::Page>,
    switch: Option<rook_core::branches::Node>,
    offered_switch: Option<rook_core::branches::Node>,
    navigation_draft: Option<rook_core::branches::Node>,
    review: Option<SummaryReview>,
    saved_summary: Option<String>,
    forked: Option<rook_core::branches::Forked>,
    renamed: Option<rook_core::branches::Node>,
    bookmarks: Option<rook_core::branches::Bookmarks>,
    show_bookmarks: bool,
    hits: Option<Matches>,
    entry: Option<EntryPage>,
    at: usize,
    scroll: u16,
    input: Typing,
    editing: Option<bool>,
    rename_target: Option<u128>,
    mark_target: Option<u64>,
    needle: String,
    note: String,
    quote: Option<String>,
    suggestion: Option<(u128, Result<rook_core::branches::SummaryDraft, String>)>,
    exported: Option<std::result::Result<(PathBuf, crate::commands::html_export::Report), String>>,
    worktree_result: Option<String>,
    send: Option<SyncSender<(u64, Command)>>,
    receive: Receiver<(u64, Result<Update>)>,
}
struct SummaryReview {
    node: rook_core::branches::Node,
    source: u128,
    through: u64,
}
enum Command {
    Worktree(u128, String),
    Export(u128, u64, Option<u64>, PathBuf),
    Draft(u128, u128),
    Suggest(u128, u128),
    Carry(SummaryReview, String),
    Fork(u128, u64),
    Rename(u128, String),
    Bookmarks(u128),
    Mark(u128, u64, String),
    Tree(u128, Option<String>),
    Turns(u128, Option<u64>),
    Page(u128, PageRequest),
    Search(u128, String, Cursor),
    Entry(u128, u64, u64),
    Quote(u128, u64, u64),
}
enum Update {
    Worktree(u128, Result<String, String>),
    Exported(std::result::Result<(PathBuf, crate::commands::html_export::Report), String>),
    Suggested(u128, Result<rook_core::branches::SummaryDraft, String>),
    Carried(SummaryReview, Result<u64, String>),
    Fork(rook_core::branches::Forked),
    Rename(rook_core::branches::Node),
    Bookmarks(rook_core::branches::Bookmarks, bool),
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
            Self::Worktree(parent, args) => {
                Update::Worktree(parent, source.worktree_command(parent, &args).map_err(|e| e.to_string()))
            }
            Self::Export(session, from, through, path) => Update::Exported(
                crate::commands::html_export::save(source, session, from, through, &path)
                    .map(|report| (path, report))
                    .map_err(|error| error.to_string()),
            ),
            Self::Draft(from, target) => Update::Suggested(
                target,
                source.branch_summary_draft(from, target).map_err(|error| error.to_string()),
            ),
            Self::Suggest(from, target) => Update::Suggested(
                target,
                source.branch_summary_suggest(from, target).map_err(|error| error.to_string()),
            ),
            Self::Carry(review, text) => {
                let result = rook_store::parse_session_id(&review.node.id)
                    .ok_or_else(|| "invalid target branch ID".to_string())
                    .and_then(|target| {
                        source
                            .transfer_branch_summary_at(review.source, target, Some(review.through), &text)
                            .map_err(|error| error.to_string())
                    });
                Update::Carried(review, result)
            }
            Self::Fork(session, seq) => Update::Fork(source.branch_from_event(session, seq)?),
            Self::Rename(session, title) => Update::Rename(source.rename_branch(session, &title)?),
            Self::Bookmarks(session) => Update::Bookmarks(source.bookmarks(session)?, true),
            Self::Mark(session, seq, label) => {
                Update::Bookmarks(source.mark_bookmark(session, seq, &label)?, false)
            }
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
            departed: None,
            epoch: 0,
            pending: false,
            page: None,
            turns: None,
            tree: None,
            switch: None,
            offered_switch: None,
            navigation_draft: None,
            review: None,
            saved_summary: None,
            forked: None,
            renamed: None,
            bookmarks: None,
            show_bookmarks: false,
            hits: None,
            entry: None,
            at: 0,
            scroll: 0,
            input: Typing::default(),
            editing: None,
            rename_target: None,
            mark_target: None,
            needle: String::new(),
            note,
            quote: None,
            suggestion: None,
            exported: None,
            worktree_result: None,
            send,
            receive,
        }
    }
    pub(super) fn open(&mut self, session: Option<u128>, departed: Option<u128>) {
        self.open_mode(session, |id| Command::Page(id, PageRequest::default()));
        self.departed = departed;
    }
    pub(super) fn open_entry(&mut self, session: Option<u128>, seq: u64, departed: Option<u128>) {
        self.open_mode(session, |id| Command::Entry(id, seq, 0));
        self.departed = departed;
    }
    pub(super) fn open_turns(&mut self, session: Option<u128>, before: Option<u64>, departed: Option<u128>) {
        self.open_mode(session, |id| Command::Turns(id, before));
        self.departed = departed;
    }
    pub(super) fn open_tree(&mut self, session: Option<u128>, departed: Option<u128>) {
        self.open_mode(session, |id| Command::Tree(id, None));
        self.departed = departed;
    }
    fn open_mode(&mut self, session: Option<u128>, command: impl FnOnce(u128) -> Command) {
        self.epoch = self.epoch.wrapping_add(1);
        self.session = session;
        self.pending = false;
        self.page = None;
        self.turns = None;
        self.tree = None;
        self.switch = None;
        self.offered_switch = None;
        self.navigation_draft = None;
        self.review = None;
        self.departed = None;
        self.hits = None;
        self.entry = None;
        self.bookmarks = None;
        self.show_bookmarks = false;
        self.at = 0;
        self.scroll = 0;
        self.quote = None;
        self.editing = None;
        self.rename_target = None;
        self.mark_target = None;
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
    pub(super) fn suggest(&mut self, source: u128, target: u128) -> bool {
        if self.pending {
            return false;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.ask(Command::Suggest(source, target))
    }
    pub(super) fn export(&mut self, session: u128, from: u64, through: Option<u64>, path: PathBuf) -> bool {
        if self.pending {
            return false;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.ask(Command::Export(session, from, through, path))
    }
    pub(super) fn worktree(&mut self, parent: u128, arguments: &str) -> bool {
        if self.pending || arguments.len() > 256 {
            return false;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.ask(Command::Worktree(parent, arguments.to_owned()))
    }
    pub(super) fn take_worktree(&mut self) -> Option<String> {
        self.worktree_result.take()
    }
    pub(super) fn poll(&mut self) {
        while let Ok((epoch, update)) = self.receive.try_recv() {
            if let Ok(Update::Worktree(parent, result)) = update {
                self.worktree_result = Some(format!(
                    "Worktree operation for parent {}:\n{}",
                    rook_store::format_session_id(parent),
                    result.unwrap_or_else(|e| e)
                ));
                if epoch == self.epoch {
                    self.pending = false;
                }
                continue;
            }
            // A saved summary remains reportable even if another viewer opened
            // before the reply. Only the original view may continue its branch.
            if let Ok(Update::Carried(mut review, result)) = update {
                match result {
                    Ok(event) => {
                        self.saved_summary = Some(format!(
                            "Saved historical summary from {} through #{} in {} at event #{event}. Verify file and test claims in the current workspace.",
                            rook_store::format_session_id(review.source),
                            review.through,
                            review.node.id,
                        ));
                        if epoch == self.epoch {
                            review.node.next_seq = review.node.next_seq.max(event.saturating_add(1));
                            self.switch = Some(review.node);
                            self.review = None;
                        }
                    }
                    Err(error) if epoch == self.epoch => {
                        self.note = format!(
                            "{error} Check target history before retrying an uncertain save; Esc cancels this review."
                        );
                    }
                    Err(_) => {}
                }
                if epoch == self.epoch {
                    self.pending = false;
                }
                continue;
            }
            if let Ok(Update::Exported(result)) = update {
                self.exported = Some(result);
                if epoch == self.epoch {
                    self.pending = false;
                }
                continue;
            }
            // A committed fork survives closing the viewer while it was being
            // created. The app decides whether it can still replace the draft.
            if let Ok(Update::Fork(forked)) = update {
                self.forked = Some(forked);
                if epoch == self.epoch {
                    self.pending = false;
                }
                continue;
            }
            // The write may have committed even if the viewer was closed while
            // its reply was in flight. Keep the session picker in sync.
            if let Ok(Update::Rename(node)) = &update {
                self.renamed = Some(node.clone());
            }
            if epoch != self.epoch {
                continue;
            }
            self.pending = false;
            self.note.clear();
            match update {
                Ok(Update::Worktree(_, _)) => {
                    unreachable!("worktree completion handled before stale read filtering")
                }
                Ok(Update::Fork(_)) => unreachable!("fork completion handled before stale read filtering"),
                Ok(Update::Exported(_)) => {
                    unreachable!("export completion handled before stale read filtering")
                }
                Ok(Update::Carried(_, _)) => unreachable!("summary save handled before stale read filtering"),
                Ok(Update::Rename(node)) => {
                    if let Some(tree) = &mut self.tree {
                        for branch in tree
                            .ancestors
                            .iter_mut()
                            .chain(std::iter::once(&mut tree.selected))
                            .chain(tree.children.iter_mut())
                        {
                            if branch.id == node.id {
                                *branch = node.clone();
                            }
                        }
                    }
                    self.note = format!("Renamed branch {}.", node.id);
                    self.renamed = Some(node);
                }
                Ok(Update::Bookmarks(page, show)) => {
                    self.bookmarks = Some(page);
                    if show {
                        self.show_bookmarks = true;
                        self.at = 0;
                        self.entry = None;
                    } else if self.show_bookmarks {
                        self.at = self
                            .at
                            .min(self.bookmarks.as_ref().map_or(0, |p| p.items.len().saturating_sub(1)));
                    }
                    self.note = if show {
                        "Enter opens a bookmark · m edits · x removes · b returns to history".into()
                    } else {
                        "Bookmark updated.".into()
                    };
                }
                Ok(Update::Tree(page)) => {
                    self.offered_switch = None;
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
                    self.show_bookmarks = false;
                    self.entry = Some(entry);
                    self.scroll = 0;
                }
                Ok(Update::Quote(text)) => self.quote = Some(text),
                Ok(Update::Suggested(target, result)) => {
                    if let Some(node) = self.navigation_draft.take() {
                        match result {
                            Ok(draft)
                                if rook_store::parse_session_id(&node.id) == Some(target)
                                    && rook_store::parse_session_id(&draft.source_session)
                                        == self.departed
                                    && draft.text.len() <= rook_core::branches::SUMMARY_BYTES =>
                            {
                                self.input.set(&draft.text);
                                self.input.home();
                                self.review = self.departed.map(|source| SummaryReview {
                                    node,
                                    source,
                                    through: draft.source_through,
                                });
                                self.offered_switch = None;
                                self.note = "Review and edit · Ctrl+S saves and continues · Enter adds a line · Esc cancels".into();
                            }
                            Ok(_) => {
                                self.note = "Draft has an invalid source or size; request it again.".into()
                            }
                            Err(error) => {
                                self.note = format!("{error} · d excerpts · s model · c skip · Esc cancel")
                            }
                        }
                    } else {
                        self.suggestion = Some((target, result));
                        self.note = "Draft ready in chat · Esc returns to review and save it".into();
                    }
                }
                Err(e) => self.note = e.to_string(),
            }
        }
    }
    pub(super) fn take_quote(&mut self) -> Option<String> {
        self.quote.take()
    }
    pub(super) fn take_saved_summary(&mut self) -> Option<String> {
        self.saved_summary.take()
    }
    pub(super) fn captures_review_key(&self, key: crossterm::event::KeyEvent) -> bool {
        self.review.is_some()
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Enter | KeyCode::Char('s' | 'u' | 'z' | 'y'))
    }
    pub(super) fn wants_continue(&self, key: crossterm::event::KeyEvent) -> bool {
        if self.review.is_some() {
            return key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Enter | KeyCode::Char('s'));
        }
        self.tree.is_some() && self.rename_target.is_none() && key.code == KeyCode::Char('c')
    }
    pub(super) fn take_export(
        &mut self,
    ) -> Option<std::result::Result<(PathBuf, crate::commands::html_export::Report), String>> {
        self.exported.take()
    }
    pub(super) fn take_suggestion(
        &mut self,
    ) -> Option<(u128, Result<rook_core::branches::SummaryDraft, String>)> {
        self.suggestion.take()
    }
    pub(super) fn take_session(&mut self) -> Option<rook_core::branches::Node> {
        self.switch.take()
    }
    pub(super) fn take_forked(&mut self) -> Option<rook_core::branches::Forked> {
        self.forked.take()
    }
    pub(super) fn take_renamed(&mut self) -> Option<rook_core::branches::Node> {
        self.renamed.take()
    }
    pub(super) fn wants_branch(&self, key: crossterm::event::KeyEvent) -> bool {
        self.editing.is_none()
            && self.rename_target.is_none()
            && self.mark_target.is_none()
            && self.tree.is_none()
            && key.code == KeyCode::Char('B')
    }
    fn branch(&self) -> Option<&rook_core::branches::Node> {
        let page = self.tree.as_ref()?;
        page.ancestors.iter().chain(std::iter::once(&page.selected)).chain(page.children.iter()).nth(self.at)
    }
    fn image_hint(&self, entry: &rook_core::TranscriptEntry) -> String {
        match (self.session, entry.image_note) {
            (Some(session), Some(note)) => format!(
                "saved image source #{note} · historical pixels\nExport: rook session image {} {} --index 0 --output NEW_FILE\n",
                rook_store::format_session_id(session),
                entry.seq,
            ),
            _ => String::new(),
        }
    }
    fn target(&self) -> Option<(u64, u64)> {
        if self.show_bookmarks {
            return self.bookmarks.as_ref()?.items.get(self.at).map(|bookmark| (bookmark.seq, 0));
        }
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
        if self.review.is_some() {
            if !self.pending {
                if text.len() <= rook_core::branches::SUMMARY_BYTES.saturating_sub(self.input.text.len()) {
                    self.input.paste(text);
                } else {
                    self.note = "Summary exceeds 16 KiB; paste was not inserted.".into();
                }
            }
            return;
        }
        let maximum: usize = if self.rename_target.is_some() {
            4096
        } else if self.mark_target.is_some() {
            128
        } else {
            256
        };
        if self.editing.is_some() || self.rename_target.is_some() || self.mark_target.is_some() {
            if text.len() <= maximum.saturating_sub(self.input.text.len()) {
                self.input.paste(&text.replace("\r\n", " ").replace(['\r', '\n'], " "));
            } else {
                self.note = format!("Input exceeds {maximum} bytes; paste was not inserted.");
            }
        }
    }
    pub(super) fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.review.is_some() {
            if self.pending {
                self.note = "Saving summary; wait for its result before continuing or retrying.".into();
                return false;
            }
            match key.code {
                KeyCode::Esc => {
                    self.review = None;
                    self.input.set("");
                    self.note = "Review cancelled; nothing saved · c chooses a branch".into();
                }
                KeyCode::Enter | KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let text = self.input.text.trim();
                    if text.is_empty() {
                        self.note = "Write a reviewed summary before saving.".into();
                    } else if let Some(review) = &self.review {
                        let review = SummaryReview {
                            node: review.node.clone(),
                            source: review.source,
                            through: review.through,
                        };
                        self.ask(Command::Carry(review, text.to_string()));
                    }
                }
                KeyCode::Enter if self.input.text.len() < rook_core::branches::SUMMARY_BYTES => {
                    self.input.insert('\n')
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
                KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => self.input.undo(),
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => self.input.redo(),
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && c.len_utf8()
                            <= rook_core::branches::SUMMARY_BYTES.saturating_sub(self.input.text.len()) =>
                {
                    self.input.insert(c)
                }
                _ => {}
            }
            return false;
        }
        if self.editing.is_some() || self.rename_target.is_some() || self.mark_target.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    self.rename_target = None;
                    self.mark_target = None;
                }
                KeyCode::Enter => {
                    if let Some(session) = self.session {
                        let sent = if let Some(id) = self.rename_target {
                            self.ask(Command::Rename(id, self.input.text.clone()))
                        } else if let Some(seq) = self.mark_target {
                            self.ask(Command::Mark(session, seq, self.input.text.clone()))
                        } else if self.editing == Some(true) {
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
                            self.rename_target = None;
                            self.mark_target = None;
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
                        && self.input.text.len() + c.len_utf8()
                            <= if self.rename_target.is_some() {
                                4096
                            } else if self.mark_target.is_some() {
                                128
                            } else {
                                256
                            } =>
                {
                    self.input.insert(c)
                }
                _ => {}
            }
            return false;
        }
        if key.code == KeyCode::Esc {
            if self.offered_switch.take().is_some() {
                self.epoch = self.epoch.wrapping_add(1);
                self.pending = false;
                self.navigation_draft = None;
                self.note = "Transfer cancelled · c chooses a branch to continue".into();
                return false;
            }
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
            if let Some(offered) = self.offered_switch.take() {
                match key.code {
                    KeyCode::Char('c') if !self.pending => {
                        self.switch = Some(offered);
                        return false;
                    }
                    KeyCode::Char('d' | 's') if !self.pending => {
                        if let (Some(source), Some(target)) =
                            (self.departed, rook_store::parse_session_id(&offered.id))
                        {
                            let command = if key.code == KeyCode::Char('d') {
                                Command::Draft(source, target)
                            } else {
                                Command::Suggest(source, target)
                            };
                            if self.ask(command) {
                                self.navigation_draft = Some(offered.clone());
                            }
                        }
                        self.offered_switch = Some(offered);
                        return false;
                    }
                    _ => {
                        self.offered_switch = Some(offered);
                        return false;
                    }
                }
            }
            match key.code {
                KeyCode::Char('e') => {
                    let selected = self.branch().and_then(|branch| {
                        rook_store::parse_session_id(&branch.id).map(|id| (id, branch.title.clone()))
                    });
                    if let Some((id, title)) = selected {
                        self.input.set(&title);
                        self.rename_target = Some(id);
                    }
                }
                KeyCode::Enter => {
                    if let Some(id) = self.branch().and_then(|n| rook_store::parse_session_id(&n.id)) {
                        self.ask(Command::Tree(id, None));
                    }
                }
                KeyCode::Char('c') if !self.pending => {
                    if let Some(branch) = self.branch().cloned() {
                        if let Some(departed) =
                            self.departed.filter(|id| Some(*id) != rook_store::parse_session_id(&branch.id))
                        {
                            self.note = format!(
                                "Summary {} → {}? d excerpts · s model · c skip · Esc cancel",
                                rook_store::format_session_id(departed),
                                branch.id
                            );
                            self.offered_switch = Some(branch);
                        } else {
                            self.switch = Some(branch);
                        }
                    }
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
        if self.show_bookmarks {
            match key.code {
                KeyCode::Char('b') | KeyCode::Char('l') | KeyCode::Char('h') => {
                    self.show_bookmarks = false;
                    self.at = self.page.as_ref().map_or(0, |p| p.items.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    if let Some(bookmark) = self.bookmarks.as_ref().and_then(|p| p.items.get(self.at)) {
                        if bookmark.available {
                            self.ask(Command::Entry(session, bookmark.seq, 0));
                        } else {
                            self.note = format!("Event #{} is no longer available.", bookmark.seq);
                        }
                    }
                }
                KeyCode::Char('m') => {
                    if let Some(bookmark) = self.bookmarks.as_ref().and_then(|p| p.items.get(self.at)) {
                        self.input.set(&bookmark.label);
                        self.mark_target = Some(bookmark.seq);
                    }
                }
                KeyCode::Char('x') => {
                    if let Some(bookmark) = self.bookmarks.as_ref().and_then(|p| p.items.get(self.at)) {
                        self.ask(Command::Mark(session, bookmark.seq, String::new()));
                    }
                }
                KeyCode::Char('B') => {
                    if let Some((seq, _)) = self.target() {
                        self.ask(Command::Fork(session, seq));
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.at = self
                        .at
                        .saturating_add(1)
                        .min(self.bookmarks.as_ref().map_or(0, |p| p.items.len().saturating_sub(1)));
                }
                KeyCode::Up | KeyCode::Char('k') => self.at = self.at.saturating_sub(1),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('m') => {
                if let Some((seq, _)) = self.target() {
                    let label = self
                        .bookmarks
                        .as_ref()
                        .and_then(|p| p.items.iter().find(|b| b.seq == seq))
                        .map_or("", |b| b.label.as_str());
                    self.input.set(label);
                    self.mark_target = Some(seq);
                }
            }
            KeyCode::Char('l') => {
                self.ask(Command::Bookmarks(session));
            }
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
            KeyCode::Char('c') => {
                let note = self.entry.as_ref().and_then(|p| p.entry.change_note).or_else(|| {
                    if self.hits.is_some() || self.turns.is_some() || self.show_bookmarks {
                        return None;
                    }
                    self.page.as_ref()?.items.get(self.at)?.change_note
                });
                if let Some(note) = note {
                    self.ask(Command::Entry(session, note, 0));
                }
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
        if let Some(review) = &self.review {
            let [source, editor, help] =
                Layout::vertical([Constraint::Length(6), Constraint::Min(3), Constraint::Length(4)])
                    .areas(area);
            f.render_widget(Paragraph::new(format!(
                "Historical source {} through #{} → {}\nReview before carrying. File and test observations need verification in the current workspace.",
                rook_store::format_session_id(review.source), review.through, review.node.id,
            )).wrap(Wrap { trim: false }).block(bordered("branch summary source")), source);
            self.input.draw_box(f, editor, "", "review branch summary");
            f.render_widget(Paragraph::new(self.note.as_str()).wrap(Wrap { trim: false }), help);
            return;
        }
        if let Some(page) = &self.tree {
            let heading_height = self.rename_target.map_or(3, |_| self.input.box_height("", area.width, 8));
            let [heading, list, detail, help] = Layout::vertical([
                Constraint::Length(heading_height),
                Constraint::Min(3),
                Constraint::Length(6),
                Constraint::Length(3),
            ])
            .areas(area);
            if self.rename_target.is_some() {
                self.input.draw_box(f, heading, "", "rename branch · Enter saves");
            } else {
                f.render_widget(
                    Paragraph::new(
                        "Explore a conversation branch. Switching leaves workspace files as they are.",
                    )
                    .wrap(Wrap { trim: false })
                    .block(bordered("conversation tree")),
                    heading,
                );
            }
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
        let editing = self.editing.is_some() || self.mark_target.is_some();
        let top_height = if editing {
            self.input.box_height("", area.width, 8)
        } else if self.turns.is_some() {
            5
        } else {
            3
        };
        let [top, body, note] =
            Layout::vertical([Constraint::Length(top_height), Constraint::Min(3), Constraint::Length(2)])
                .areas(area);
        let title = if self.mark_target.is_some() {
            "bookmark label · Enter saves · empty removes"
        } else if self.show_bookmarks {
            "bookmarks · Enter opens · m edits · x removes · b history"
        } else {
            match self.editing {
                Some(true) => "jump to event #",
                Some(false) => "find literal text",
                None if self.turns.is_some() => "turn results · t refresh · n older · h history",
                None => "history · c changes · m mark · l bookmarks · / find · g jump · B branch",
            }
        };
        let text = if self.editing.is_some() || self.mark_target.is_some() {
            self.input.text.clone()
        } else if let Some(turns) = &self.turns {
            rook_core::turns::describe(turns)
        } else {
            self.session.map(rook_store::format_session_id).unwrap_or_default()
        };
        if editing {
            self.input.draw_box(f, top, "", title);
        } else {
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(bordered(title)), top);
        }
        let [list, detail] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(body);
        let rows: Vec<ListItem> = if self.show_bookmarks {
            self.bookmarks
                .as_ref()
                .map(|page| {
                    page.items
                        .iter()
                        .map(|bookmark| {
                            ListItem::new(format!(
                                "#{} {}{}",
                                bookmark.seq,
                                bookmark.label,
                                if bookmark.available { "" } else { " · unavailable" }
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else if let Some(hits) = &self.hits {
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
                        .map(|e| {
                            let mark = self
                                .bookmarks
                                .as_ref()
                                .and_then(|p| p.items.iter().find(|b| b.seq == e.seq))
                                .map(|b| format!(" · {}", b.label))
                                .unwrap_or_default();
                            ListItem::new(format!("#{} {} {}{mark}", e.seq, e.kind, e.label))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut selected = ListState::default();
        selected.select(Some(self.at));
        f.render_stateful_widget(
            List::new(rows)
                .block(bordered(if self.show_bookmarks {
                    "bookmarks"
                } else if self.hits.is_some() {
                    "matches"
                } else {
                    "events"
                }))
                .highlight_style(Style::default().bg(Color::DarkGray)),
            list,
            &mut selected,
        );
        let text = if self.show_bookmarks {
            self.bookmarks
                .as_ref()
                .and_then(|p| p.items.get(self.at))
                .map(|b| {
                    format!(
                        "#{} · {}\n\n{}",
                        b.seq,
                        b.label,
                        if b.available {
                            "Enter reads this event."
                        } else {
                            "The saved event is no longer available; x removes this label."
                        }
                    )
                })
                .unwrap_or_else(|| "No bookmarks in this session. Mark an event with m.".into())
        } else if let Some(page) = &self.entry {
            format!(
                "#{} · byte {} / {}{}\n{}\n{}\n{}{}{}",
                page.entry.seq,
                page.offset,
                page.total_bytes,
                if page.next_offset.is_some() { " · n reads more" } else { "" },
                page.entry.tool_measurement.map(|m| m.text()).unwrap_or_default(),
                page.entry.tool_details.as_ref().map(|d| d.text()).unwrap_or_default(),
                page.entry
                    .change_note
                    .map(|seq| format!("saved changes #{seq} · c opens preview\n"))
                    .unwrap_or_default(),
                self.image_hint(&page.entry),
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
                        "#{} {}{}\n{}\n{}\n{}{}{}",
                        e.seq,
                        e.kind,
                        if e.truncated { " · Enter reads full body in pages" } else { "" },
                        e.tool_measurement.map(|m| m.text()).unwrap_or_default(),
                        e.tool_details.as_ref().map(|d| d.text()).unwrap_or_default(),
                        e.change_note
                            .map(|seq| format!("saved changes #{seq} · c opens preview\n"))
                            .unwrap_or_default(),
                        self.image_hint(e),
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

#[cfg(test)]
mod branch_offer_tests {
    use super::*;

    fn tree() -> (History, std::sync::mpsc::Receiver<(u64, Command)>) {
        let store_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(store_dir.path()).unwrap(),
            rook_core::Config::default(),
            rook_skills::Environment::bare("windows", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            workspace.path().to_path_buf(),
        );
        let source = crate::source::Source::Local(std::sync::Arc::new(rook));
        let mut history = History::new(&source);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        history.send = Some(send);
        let node = |id: u128| rook_core::branches::Node {
            id: rook_store::format_session_id(id),
            parent: None,
            title: format!("branch {id}"),
            workspace: "test".into(),
            title_truncated: false,
            workspace_truncated: false,
            forked_at: None,
            delegated: false,
            next_seq: 1,
            updated_at: 0,
        };
        history.session = Some(1);
        history.departed = Some(1);
        history.tree = Some(rook_core::branches::Page {
            ancestors: vec![],
            selected: node(1),
            children: vec![node(2)],
            next: None,
            earlier_ancestor: None,
            missing_parent: None,
            scanned_sessions: 2,
        });
        history.at = 1;
        (history, receive)
    }

    fn key(char: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(KeyCode::Char(char), KeyModifiers::NONE)
    }

    #[test]
    fn switching_other_branches_offers_review_or_explicit_skip() {
        let (mut history, receive) = tree();
        history.key(key('c'));
        assert!(history.switch.is_none());
        assert_eq!(history.offered_switch.as_ref().unwrap().id, rook_store::format_session_id(2));
        assert!(receive.try_recv().is_err(), "an offer must not request or save a summary");

        history.key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(history.offered_switch.is_none());
        assert!(history.switch.is_none());

        history.key(key('c'));
        history.key(key('c'));
        assert_eq!(history.take_session().unwrap().id, rook_store::format_session_id(2));
        assert!(receive.try_recv().is_err(), "skip must not request or save a summary");
    }

    #[test]
    fn offered_excerpts_and_model_drafts_use_the_departed_source() {
        for (choice, is_draft) in [('d', true), ('s', false)] {
            let (mut history, receive) = tree();
            history.key(key('c'));
            history.key(key(choice));
            assert!(history.switch.is_none());
            let (_, command) = receive.try_recv().unwrap();
            assert!(matches!(command, Command::Draft(1, 2)) == is_draft);
            assert!(matches!(command, Command::Suggest(1, 2)) != is_draft);
        }
    }

    fn draft() -> rook_core::branches::SummaryDraft {
        rook_core::branches::SummaryDraft {
            source_session: rook_store::format_session_id(1),
            source_through: 7,
            text: "Historical finding\nReview before carrying".into(),
            scanned_events: 3,
            omitted_earlier: false,
            source_from: 5,
            common_ancestor: Some(rook_store::format_session_id(3)),
            scope_known: true,
        }
    }

    fn deliver(history: &mut History, update: Update) {
        let (send, receive) = sync_channel(1);
        history.receive = receive;
        send.send((history.epoch, Ok(update))).unwrap();
        history.poll();
    }

    fn review() -> (History, Receiver<(u64, Command)>) {
        let (mut history, commands) = tree();
        history.key(key('c'));
        history.key(key('s'));
        assert!(matches!(commands.try_recv().unwrap().1, Command::Suggest(1, 2)));
        deliver(&mut history, Update::Suggested(2, Ok(draft())));
        assert!(history.suggestion.is_none(), "navigation draft belongs in its own editor");
        (history, commands)
    }

    #[test]
    fn navigation_review_edits_multiline_text_and_saves_only_on_confirmation() {
        let (mut history, commands) = review();
        assert!(
            history.captures_review_key(crossterm::event::KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            )),
            "the summary confirmation must take precedence over global mouse selection"
        );
        assert_eq!(history.review.as_ref().unwrap().through, 7);
        assert!(history.switch.is_none());
        history.input.end();
        history.key(crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        history.paste("Reviewed conclusion: проверено 🦀");
        let edited = history.input.text.clone();
        assert!(commands.try_recv().is_err(), "editing must not write or call a model");
        history.key(crossterm::event::KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let (_, command) = commands.try_recv().unwrap();
        let Command::Carry(review, text) = command else { panic!("explicit save must carry the review") };
        assert_eq!(text, edited);
        assert_eq!(review.source, 1);
        assert_eq!(review.through, 7);
        assert_eq!(review.node.id, rook_store::format_session_id(2));
        history.key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(history.review.is_some(), "an in-flight save cannot be dismissed and retried");
        assert!(history.switch.is_none());
        deliver(&mut history, Update::Carried(review, Ok(12)));
        assert!(history.review.is_none());
        assert_eq!(history.take_session().unwrap().next_seq, 13);
        assert!(history.take_saved_summary().unwrap().contains("through #7"));
    }

    #[test]
    fn navigation_review_refuses_oversized_input_before_copying_and_cancel_does_not_write() {
        let (mut history, commands) = review();
        let oversized = "я".repeat(rook_core::branches::SUMMARY_BYTES);
        assert!(oversized.len() > rook_core::branches::SUMMARY_BYTES);
        let before = history.input.text.clone();
        history.paste(&oversized);
        assert_eq!(history.input.text, before);
        history.input.set(&"x".repeat(rook_core::branches::SUMMARY_BYTES - 1));
        history.key(key('я'));
        assert_eq!(history.input.text.len(), rook_core::branches::SUMMARY_BYTES - 1);
        history.key(key('x'));
        history.key(key('x'));
        assert_eq!(history.input.text.len(), rook_core::branches::SUMMARY_BYTES);
        history.key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(history.review.is_none());
        assert!(history.switch.is_none());
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn cancelled_navigation_ignores_late_drafts_and_wrong_source_drafts_are_rejected() {
        let (mut history, commands) = tree();
        history.key(key('c'));
        history.key(key('s'));
        let (epoch, _) = commands.try_recv().unwrap();
        history.key(key('j'));
        assert!(history.offered_switch.is_some(), "unrelated keys must keep the pending offer");
        history.key(crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let (send, receive) = sync_channel(1);
        history.receive = receive;
        send.send((epoch, Ok(Update::Suggested(2, Ok(draft()))))).unwrap();
        history.poll();
        assert!(history.review.is_none());
        assert!(history.suggestion.is_none());
        history.key(key('c'));
        history.key(key('s'));
        commands.try_recv().unwrap();
        let mut wrong = draft();
        wrong.source_session = rook_store::format_session_id(9);
        deliver(&mut history, Update::Suggested(2, Ok(wrong)));
        assert!(history.review.is_none());
        assert!(history.offered_switch.is_some());
    }

    #[test]
    fn failed_review_save_keeps_edits_and_a_stale_success_reports_without_switching() {
        let (mut history, commands) = review();
        let edited = history.input.text.clone();
        history.key(crossterm::event::KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let Command::Carry(review, _) = commands.try_recv().unwrap().1 else { panic!("expected save") };
        deliver(&mut history, Update::Carried(review, Err("source changed; request a new draft".into())));
        assert_eq!(history.input.text, edited);
        assert!(history.review.is_some());
        assert!(history.switch.is_none());
        assert!(history.note.contains("Check target history"));
        let review = history.review.take().unwrap();
        let old_epoch = history.epoch;
        history.epoch += 1;
        let (send, receive) = sync_channel(1);
        history.receive = receive;
        send.send((old_epoch, Ok(Update::Carried(review, Ok(9))))).unwrap();
        history.poll();
        assert!(history.take_saved_summary().is_some());
        assert!(history.switch.is_none(), "a stale save reply cannot select another conversation");
    }

    #[test]
    fn every_history_entry_point_retains_the_actual_departed_conversation_for_tree_navigation() {
        let (mut history, commands) = tree();
        history.open(Some(2), Some(1));
        assert!(matches!(commands.try_recv().unwrap().1, Command::Page(2, _)));
        assert_eq!(history.departed, Some(1));
        history.open_entry(Some(3), 4, Some(1));
        assert!(matches!(commands.try_recv().unwrap().1, Command::Entry(3, 4, 0)));
        assert_eq!(history.departed, Some(1));
        history.open_turns(Some(2), None, Some(1));
        assert!(matches!(commands.try_recv().unwrap().1, Command::Turns(2, None)));
        assert_eq!(history.departed, Some(1));
        history.open(Some(2), None);
        commands.try_recv().unwrap();
        assert_eq!(history.departed, None, "browsing before opening a chat cannot invent a source");
    }
}
