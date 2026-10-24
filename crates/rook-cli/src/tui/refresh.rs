//! One read worker, one current request and one coalesced replacement.
use super::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

pub(super) type Read<T> = std::result::Result<T, String>;
type DocumentRead = ((String, String), Read<Option<rook_core::DocSet>>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Backend {
    Local(usize),
    Daemon(String),
}
impl Backend {
    pub fn of(source: &crate::source::Source) -> Self {
        match source {
            crate::source::Source::Local(rook) => Self::Local(Arc::as_ptr(rook) as usize),
            crate::source::Source::Daemon(daemon) => Self::Daemon(daemon.base.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub backend: Backend,
    pub workspace: PathBuf,
    pub live: Option<u128>,
    pub session: Option<u128>,
    pub skill: Option<String>,
    pub docs: Option<(String, String)>,
}
pub(super) struct Query {
    pub scope: Scope,
    pub selected: Option<SessionSummary>,
}
pub(super) struct Snapshot {
    pub scope: Scope,
    pub sessions: Read<Vec<SessionSummary>>,
    pub skills: Read<Vec<SkillCard>>,
    pub facts: Read<Vec<rook_core::Fact>>,
    pub stats: Read<StoreStats>,
    pub checkpoints: Read<Vec<(String, String)>>,
    pub objects: Read<Vec<rook_store::ObjectRow>>,
    pub docs: Read<Vec<rook_core::docs::Kept>>,
    pub endpoints: Option<Read<Vec<String>>>,
    pub unchosen: Read<Option<Vec<String>>>,
    pub selected: Option<SessionSummary>,
    pub transcript: Option<Read<Vec<TranscriptEntry>>>,
    pub changes: Option<Read<rook_core::changes::Changes>>,
    pub versions: Option<(String, Read<Vec<rook_core::SkillVersionRecord>>)>,
    pub doc_set: Option<DocumentRead>,
    pub calls: Option<Read<(Vec<Call>, bool)>>,
}

// Count serialized bytes without retaining a second copy of the snapshot.
struct Counter {
    used: usize,
    limit: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.used) {
            return Err(std::io::Error::other("panel snapshot exceeds 64 MiB; use a bounded history view"));
        }
        self.used += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Budget(usize);
impl Budget {
    fn keep<T: serde::Serialize>(&mut self, value: Result<T>) -> Read<T> {
        let value = value.map_err(|e| rook_llm::truncate(&e.to_string(), 256))?;
        let mut count = Counter { used: 0, limit: self.0 };
        serde_json::to_writer(&mut count, &value).map_err(|e| rook_llm::truncate(&e.to_string(), 256))?;
        self.0 -= count.used;
        Ok(value)
    }
}

impl Query {
    fn read(self, source: &crate::source::Source, epoch: u64, current: &AtomicU64) -> Option<Snapshot> {
        let mut budget = Budget(64 * 1024 * 1024);
        macro_rules! read {
            ($operation:expr) => {{
                if current.load(Ordering::Acquire) != epoch {
                    return None;
                }
                budget.keep($operation)
            }};
        }
        let sessions = read!(source.sessions());
        let selected = match &sessions {
            Ok(rows) => self
                .scope
                .session
                .and_then(|id| rows.iter().find(|s| s.meta.id == id))
                .or_else(|| rook_core::session_list::rows(rows).first().and_then(|r| rows.get(r.index)))
                .cloned(),
            Err(_) => self.selected,
        };
        let skills = read!(source.catalog(&self.scope.workspace));
        let skill = match &skills {
            Ok(rows) => self
                .scope
                .skill
                .as_ref()
                .and_then(|name| rows.iter().find(|s| &s.name == name))
                .or_else(|| rows.first())
                .map(|s| s.name.clone()),
            Err(_) => self.scope.skill.clone(),
        };
        let docs = read!(source.docs_kept());
        let doc = match &docs {
            Ok(rows) => self
                .scope
                .docs
                .as_ref()
                .and_then(|key| rows.iter().find(|d| (&d.topic, &d.version) == (&key.0, &key.1)))
                .or_else(|| rows.first())
                .map(|d| (d.topic.clone(), d.version.clone())),
            Err(_) => self.scope.docs.clone(),
        };
        let here = self.scope.workspace.display().to_string();
        let facts = read!(
            source.memory().map(|rows| rows.into_iter().filter(|f| f.scope.applies_in(&here)).collect())
        );
        let stats = read!(source.stats());
        let checkpoints = read!(source.checkpoints());
        let objects = read!(source.objects(None, 300));
        let transcript = if let Some(s) = &selected {
            Some(read!(source.transcript(s.meta.id, 0, 500, 4_000)))
        } else {
            None
        };
        let changes =
            if let Some(s) = &selected { Some(read!(source.changes(s.meta.id, false))) } else { None };
        let versions = if let Some(name) = skill {
            Some((name.clone(), read!(source.skill_history(&name))))
        } else {
            None
        };
        let doc_set = if let Some(key) = doc {
            let set = read!(source.docs(&key.0, Some(&key.1)));
            Some((key, set))
        } else {
            None
        };
        let calls = if let Some(id) = self.scope.live { Some(read!(recent_calls(source, id))) } else { None };
        let endpoints = if let Some(r) = source.here() {
            Some(read!(Ok(r.config.models.keys().cloned().collect::<Vec<_>>())))
        } else {
            None
        };
        let unchosen = read!(Ok(source
            .here()
            .and_then(|r| rook_core::models::unchosen(&r.config, &rook_core::paths::config_file()))));
        Some(Snapshot {
            scope: self.scope,
            sessions,
            skills,
            facts,
            stats,
            checkpoints,
            objects,
            docs,
            selected,
            transcript,
            changes,
            versions,
            doc_set,
            calls,
            endpoints,
            unchosen,
        })
    }
}

pub(super) struct Refresh {
    backend: Backend,
    send: Option<SyncSender<(u64, Query)>>,
    receive: Receiver<(u64, Option<Snapshot>)>,
    current: Arc<AtomicU64>,
    epoch: u64,
    active: bool,
    desired: Option<Query>,
    pub note: String,
}
impl Refresh {
    pub fn new(source: &crate::source::Source) -> Self {
        let (send, commands) = sync_channel::<(u64, Query)>(1);
        let (updates, receive) = sync_channel(1);
        let current = Arc::new(AtomicU64::new(0));
        let observed = current.clone();
        let reader = source.panel_reader();
        let (send, note) = match reader {
            Ok(mut reader) => match std::thread::Builder::new().name("panel-reader".into()).spawn(move || {
                while let Ok((epoch, query)) = commands.recv() {
                    if reader.workspace() != query.scope.workspace {
                        reader = reader.in_workspace(query.scope.workspace.clone());
                    }
                    let snapshot = query.read(&reader, epoch, &observed);
                    if updates.send((epoch, snapshot)).is_err() {
                        break;
                    }
                }
            }) {
                Ok(_) => (Some(send), String::new()),
                Err(error) => (None, format!("panel reader: {error}")),
            },
            Err(error) => (None, format!("panel reader: {error}")),
        };
        Self {
            backend: Backend::of(source),
            send,
            receive,
            current,
            epoch: 0,
            active: false,
            desired: None,
            note,
        }
    }
    pub fn request(&mut self, query: Query) {
        if self.send.is_none() {
            return;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.current.store(self.epoch, Ordering::Release);
        if query.scope.backend != self.backend {
            self.desired = None;
            self.note = "panel backend changed; reopen the window".into();
            return;
        }
        self.desired = Some(query);
        self.note = "loading panels (each daemon read: at most 15s)".into();
        self.dispatch();
    }
    fn dispatch(&mut self) {
        if self.active {
            return;
        }
        let Some(send) = self.send.as_ref() else { return };
        if let Some(query) = self.desired.take() {
            match send.try_send((self.epoch, query)) {
                Ok(()) => self.active = true,
                Err(_) => {
                    self.send = None;
                    self.note = "panel reader stopped; reopen the window".into();
                }
            }
        }
    }
    pub fn poll(&mut self) -> Option<Snapshot> {
        let mut ready = None;
        loop {
            match self.receive.try_recv() {
                Ok((epoch, snapshot)) => {
                    self.active = false;
                    if epoch == self.epoch {
                        ready = snapshot;
                    }
                    self.dispatch();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.active = false;
                    self.send = None;
                    self.note = "panel reader stopped; reopen the window".into();
                    break;
                }
            }
        }
        ready
    }
}
impl Drop for Refresh {
    fn drop(&mut self) {
        self.current.store(u64::MAX, Ordering::Release);
        self.send.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::sync::{Mutex, atomic::AtomicBool};

    struct Server {
        source: crate::source::Source,
        hold: Arc<AtomicBool>,
        fail: Arc<AtomicBool>,
        seen: Arc<Mutex<Vec<String>>>,
        started: Receiver<()>,
        release: SyncSender<()>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Server {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let rook = rook_core::Rook::from_parts(
                rook_store::Store::open(home.path().join("store")).unwrap(),
                Default::default(),
                rook_skills::Environment::bare("test", "test", "1"),
                Default::default(),
                PathBuf::from("."),
            );
            rook.start_session("first fixture session").unwrap();
            rook.start_session("second fixture session").unwrap();
            let sessions =
                serde_json::to_vec(&rook_proto::Page::new(rook.session_summaries().unwrap())).unwrap();
            let stats = serde_json::to_vec(&rook.stats().unwrap()).unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let source =
                crate::source::Source::panel_fixture(format!("http://{}", listener.local_addr().unwrap()));
            let hold = Arc::new(AtomicBool::new(false));
            let fail = Arc::new(AtomicBool::new(false));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (began, started) = sync_channel(1);
            let (release, resume) = sync_channel(1);
            let (held, failed, requests, ended) = (hold.clone(), fail.clone(), seen.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                while !ended.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                    let (mut socket, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    // Accepted sockets can inherit nonblocking mode on Windows.
                    // Wait for complete headers instead of retiring a live client
                    // when its first packet has not arrived yet.
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut header = Vec::new();
                    let mut byte = [0];
                    while !header.ends_with(b"\r\n\r\n") {
                        assert!(header.len() < 16384);
                        if socket.read_exact(&mut byte).is_err() {
                            break;
                        }
                        header.push(byte[0]);
                    }
                    // A client may open then retire a connection without a
                    // request, particularly when its old read is cancelled.
                    if !header.ends_with(b"\r\n\r\n") {
                        continue;
                    }
                    let request = String::from_utf8(header).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    requests.lock().unwrap().push(path.clone());
                    if path == "/api/sessions" && held.swap(false, Ordering::AcqRel) {
                        began.send(()).unwrap();
                        resume.recv_timeout(Duration::from_secs(30)).unwrap();
                    }
                    let refused = path == "/api/sessions" && failed.load(Ordering::Acquire);
                    let missing = path.contains("/history");
                    let (status, body): (&str, &[u8]) = if refused {
                        ("503 Unavailable", b"owned session read failed")
                    } else if missing {
                        ("404 Not Found", b"old history endpoint")
                    } else if path == "/api/sessions" {
                        ("200 OK", &sessions)
                    } else if path == "/api/store/stats" {
                        ("200 OK", &stats)
                    } else if path.contains("/changes?") {
                        ("200 OK", br#"{"files":[],"written_by_commands":[],"watched":true}"#)
                    } else {
                        ("200 OK", br#"{"items":[],"next_cursor":null,"total":null}"#)
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes());
                    let _ = socket.write_all(body);
                }
            });
            Self { source, hold, fail, seen, started, release, stop, thread: Some(thread) }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let _ = self.release.try_send(());
            let joined = self.thread.take().unwrap().join();
            if !std::thread::panicking() {
                joined.unwrap();
            }
        }
    }
    fn query(source: &crate::source::Source, workspace: &str) -> Query {
        Query {
            selected: None,
            scope: Scope {
                backend: Backend::of(source),
                workspace: workspace.into(),
                live: None,
                session: None,
                skill: None,
                docs: None,
            },
        }
    }
    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "panel worker did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn panel_refresh_rejects_a_different_backend_before_sending_a_read() {
        let server = Server::new();
        let mut refresh = Refresh::new(&server.source);
        let mut request = query(&server.source, "same-workspace");
        request.scope.backend = Backend::Daemon("http://different-owned-fixture.invalid".into());
        refresh.request(request);
        assert!(refresh.poll().is_none());
        assert!(!refresh.active && refresh.desired.is_none());
        assert!(refresh.note.contains("backend changed"));
        assert!(server.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn panel_refresh_budget_counts_encoded_bytes_and_does_not_charge_a_failed_panel() {
        let mut budget = Budget(8);
        assert!(budget.keep(Ok("abcdefg")).is_err()); // Includes JSON quotes.
        assert_eq!(budget.0, 8);
        assert_eq!(budget.keep(Ok("界")).unwrap(), "界");
        assert_eq!(budget.0, 3); // UTF-8 bytes plus quotes.
        assert_eq!(budget.keep(Ok(123)).unwrap(), 123);
        assert_eq!(budget.0, 0);
        assert!(budget.keep(Ok(0)).is_err());
    }

    #[test]
    fn panel_details_survive_read_failure_only_while_the_selected_identity_survives() {
        let server = Server::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut app = App::new(server.source.reader().unwrap(), runtime, true);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            app.poll_refresh();
            if !app.sessions.is_empty() && !app.refresh.active {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "panel initialization: {}; sessions={}; active={}; desired={}; requests={:?}",
                app.refresh.note,
                app.sessions.len(),
                app.refresh.active,
                app.refresh.desired.is_some(),
                server.seen.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let skill = |name: &str| SkillCard {
            name: name.into(),
            description: String::new(),
            version: "1".into(),
            source: "owned fixture".into(),
            keywords: Vec::new(),
            applicable: true,
            mismatches: Vec::new(),
            body_tokens: 0,
        };
        let docs = |topic: &str| rook_core::docs::Kept {
            topic: topic.into(),
            version: "1".into(),
            pages: 0,
            bytes: 0,
            fetched_at: 0,
            object: "owned fixture".into(),
        };
        app.skills = vec![skill("old-skill")];
        app.skill_state.select(Some(0));
        app.skill_versions = vec![rook_core::SkillVersionRecord {
            reference: "old-skill".into(),
            object: "old-object".into(),
            version: "1".into(),
            captured_at: 0,
            files: 0,
            bytes: 0,
            note: None,
        }];
        app.docs = vec![docs("old-docs")];
        app.docs_state.select(Some(0));
        app.doc_set = Some(rook_core::DocSet::new("old-docs", "1", Vec::new()));

        for replaced in [false, true] {
            let mut snapshot = Query { scope: app.refresh_scope(), selected: None }
                .read(&server.source, 0, &AtomicU64::new(0))
                .unwrap();
            let name = if replaced { "replacement-skill" } else { "old-skill" };
            let topic = if replaced { "replacement-docs" } else { "old-docs" };
            snapshot.skills = Ok(vec![skill(name)]);
            snapshot.versions = Some((name.into(), Err("owned history unavailable".into())));
            snapshot.docs = Ok(vec![docs(topic)]);
            snapshot.doc_set = Some(((topic.into(), "1".into()), Err("owned pages unavailable".into())));
            app.apply_refresh(snapshot);
            assert_eq!(app.selected_skill().as_deref(), Some(name));
            assert_eq!(app.docs[app.docs_state.selected().unwrap()].topic, topic);
            assert_eq!(app.skill_versions.is_empty(), replaced);
            assert_eq!(app.doc_set.is_none(), replaced);
            assert!(
                app.refresh.note.contains("skill versions:")
                    && app.refresh.note.contains("documentation pages:")
            );
        }
    }

    #[test]
    fn panel_refresh_coalesces_requests_and_rejects_the_delayed_old_scope() {
        let server = Server::new();
        server.hold.store(true, Ordering::Release);
        let mut refresh = Refresh::new(&server.source);
        refresh.request(query(&server.source, "first"));
        server.started.recv_timeout(Duration::from_secs(30)).unwrap();
        for i in 0..100 {
            refresh.request(query(&server.source, &format!("newest-{i}")));
        }
        assert!(refresh.poll().is_none());
        server.release.send(()).unwrap();
        let mut result = None;
        until(|| {
            result = refresh.poll();
            result.is_some()
        });
        let snapshot = result.unwrap();
        assert_eq!(snapshot.scope.workspace, PathBuf::from("newest-99"));
        assert_eq!(snapshot.sessions.as_ref().unwrap().len(), 2);
        assert!(
            snapshot.skills.is_ok()
                && snapshot.stats.is_ok()
                && snapshot.transcript.as_ref().unwrap().is_ok()
        );
        let seen = server.seen.lock().unwrap();
        assert_eq!(
            seen.iter().filter(|p| *p == "/api/sessions").count(),
            2,
            "stale work must stop after its held read: {seen:?}"
        );
        assert!(seen.iter().any(|p| p.contains("workspace=newest-99")));
        assert!(!seen.iter().any(|p| p.contains("workspace=first")));
    }
    #[test]
    fn question_can_be_answered_before_a_panel_read_finishes() {
        let server = Server::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut app = App::new(server.source.reader().unwrap(), runtime, true);
        until(|| {
            app.poll_refresh();
            !app.sessions.is_empty() && !app.refresh.active
        });
        let active = app.sessions[0].meta.id;
        let browsing = app.sessions[1].meta.id;
        app.chat.session = Some(active);
        app.chat.busy = true;
        let (send, mut controls) = mpsc::unbounded_channel();
        app.chat.remote = Some(send);
        app.session_state.select(Some(1));
        server.hold.store(true, Ordering::Release);
        app.reload();
        server.started.recv_timeout(Duration::from_secs(30)).unwrap();

        app.heard_from_daemon(ChatEvent::Ask {
            id: "held-panel-question".into(),
            questions: vec![rook_proto::AskQuestion {
                question: "Choose how to continue while panels wait".into(),
                choices: vec!["KEEP_BRANCH".into(), "CHANGE_BRANCH".into()],
                multi: false,
            }],
        });
        assert!(controls.try_recv().is_err(), "showing a question must not answer it");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let visible: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(visible.contains("Choose how to continue while panels wait"));
        app.on_key(crossterm::event::KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        app.on_key(crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            controls.try_recv().unwrap(),
            ClientMessage::Answers { id, answers }
                if id == "held-panel-question" && answers == vec![vec!["KEEP_BRANCH".to_owned()]]
        ));
        assert!(controls.try_recv().is_err(), "one input sends exactly one answer");
        assert!(app.chat.asking.is_none());
        assert_eq!(app.chat.session, Some(active));
        assert_eq!(app.sessions[app.session_state.selected().unwrap()].meta.id, browsing);
        assert!(app.refresh.poll().is_none(), "answer must precede panel completion");
        server.release.send(()).unwrap();
        until(|| {
            app.poll_refresh();
            !app.refresh.active
        });
    }

    #[test]
    fn failed_panel_refresh_retains_data_and_processes_draft_stream_and_stop_before_io_finishes() {
        let server = Server::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut app = App::new(server.source.reader().unwrap(), runtime, true);
        until(|| {
            app.poll_refresh();
            !app.sessions.is_empty() && !app.refresh.active
        });
        assert_eq!(app.sessions.len(), 2);
        let active = app.sessions[0].meta.id;
        let browsing = app.sessions[1].meta.id;
        let private = tempfile::tempdir().unwrap();
        app.stop_retry =
            Some(stop_retry::Journal::new(private.path().into(), app.source.workspace()).unwrap());
        app.chat.session = Some(active);
        app.chat.turn_id = Some(rook_store::format_session_id(rook_store::new_session_id()));
        app.chat.busy = true;
        let (send, mut controls) = mpsc::unbounded_channel();
        app.chat.remote = Some(send);
        app.chat.input.set("UNSENT_PANEL_DRAFT");
        server.hold.store(true, Ordering::Release);
        server.fail.store(true, Ordering::Release);
        app.reload();
        server.started.recv_timeout(Duration::from_secs(30)).unwrap();
        app.session_state.select(Some(1));
        app.load_transcript();
        app.to_loop.send(TurnEvent::Text("STREAM_WHILE_PANEL_READ_BLOCKED".into())).unwrap();
        app.drain_turn_events();
        app.on_action(Action::Left);
        app.on_action(Action::Stop);
        assert!(
            matches!(controls.try_recv().unwrap(), ClientMessage::Stop { session, .. } if session == Some(rook_store::format_session_id(active)))
        );
        assert_eq!(app.chat.input.as_str(), "UNSENT_PANEL_DRAFT");
        assert!(app.chat.log.iter().any(|(_, text)| text.contains("STREAM_WHILE_PANEL_READ_BLOCKED")));
        assert!(app.refresh.poll().is_none(), "the held backend has not been released yet");
        server.release.send(()).unwrap();
        until(|| {
            app.poll_refresh();
            !app.refresh.active && app.refresh.desired.is_none()
        });
        assert_eq!(app.sessions.len(), 2, "read failure must not erase a successful list");
        assert_eq!(app.sessions[app.session_state.selected().unwrap()].meta.id, browsing);
        assert_eq!(app.chat.session, Some(active));
        assert_eq!(app.chat.input.as_str(), "UNSENT_PANEL_DRAFT");
        assert!(
            app.refresh.note.contains("sessions:") && app.refresh.note.contains("retained"),
            "{}",
            app.refresh.note
        );
        app.overlay = Some(Overlay::Sessions);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let visible: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(visible.contains("refresh failed"), "panel failure must be visible: {visible}");
    }
}
