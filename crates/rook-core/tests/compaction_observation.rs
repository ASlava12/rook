//! Real compaction attempts, their durable IDs and output admission.
use async_trait::async_trait;
use rook_core::agent::{
    AgentLoop,
    observe::{Event, Observer},
};
use rook_core::{Config, Rook};
use rook_llm::{Delta, Provider, Request, Response, StopReason, Usage};
use rook_skills::{Environment, SkillIndex};
use rook_store::{EventKind, Store};
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq)]
enum Observed {
    Start(u128),
    Saved(u128, u64, String),
    Failed(u128),
    Cancelled(u128),
}
#[derive(Default)]
struct Watch(Mutex<Vec<Observed>>);
impl Observer for Watch {
    fn observe(&self, event: Event<'_>) {
        let event = match event {
            Event::CompactionStarted { id, .. } => Observed::Start(id),
            Event::CompactionEnded { id, result, .. } => match result {
                Some(Ok((seq, summary))) => Observed::Saved(id, seq, summary.into()),
                Some(Err(_)) => Observed::Failed(id),
                None => Observed::Cancelled(id),
            },
            _ => return,
        };
        self.0.lock().unwrap().push(event);
    }
}
struct Summary {
    deltas: Vec<Delta>,
    hang: bool,
    entered: Arc<tokio::sync::Notify>,
    requests: Arc<Mutex<Vec<Request>>>,
}
#[async_trait]
impl Provider for Summary {
    fn id(&self) -> &str {
        "fixture/summary"
    }
    fn context_window(&self) -> usize {
        4000
    }
    async fn complete(&self, _: Request) -> rook_llm::Result<Response> {
        unreachable!("compaction must stream")
    }
    async fn stream(&self, request: Request) -> rook_llm::Result<rook_llm::ResponseStream> {
        self.requests.lock().unwrap().push(request);
        self.entered.notify_one();
        if self.hang {
            return Ok(Box::pin(futures_util::stream::pending()));
        }
        Ok(Box::pin(futures_util::stream::iter(self.deltas.clone().into_iter().map(Ok))))
    }
}
fn rook(store: Store, workspace: &std::path::Path, config: Config) -> Rook {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };
    Rook::from_parts(
        store,
        config,
        Environment::bare("linux", "x86_64", "0.1.0"),
        SkillIndex::default(),
        workspace.to_owned(),
    )
}
fn history(rook: &Rook, session: u128) {
    for i in 0..12 {
        let kind = if i % 2 == 0 { EventKind::UserMessage } else { EventKind::AssistantMessage };
        rook.log(session, kind, "turn", &format!("evidence entry {i}: ").repeat(140)).unwrap();
    }
    assert!(rook.context_usage(session, Some(4000)).unwrap().needs_compaction);
}

#[tokio::test]
async fn summary_admits_combined_text_and_reasoning_before_assembly_and_ignores_opaque_state() {
    for oversized in [false, true] {
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.agent.max_compaction_summary_bytes = 1024;
        let root = rook(Store::open(store.path()).unwrap(), workspace.path(), config.clone());
        let session = root.start_session("summary bound").unwrap();
        history(&root, session);
        let text = "accepted evidence ".repeat(50);
        let thought = if oversized { "thinking ".repeat(30) } else { "short thinking".into() };
        assert_eq!(
            text.len() + thought.len() > 1024,
            oversized,
            "the combined bound must actually be crossed"
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Summary {
            deltas: vec![
                Delta::ReasoningDone(serde_json::json!({"signature":"DO_NOT_REPLAY_OPAQUE_STATE"})),
                Delta::Text(text.clone()),
                Delta::Reasoning(thought),
                Delta::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    model: "fixture".into(),
                },
            ],
            hang: false,
            entered: Default::default(),
            requests: requests.clone(),
        };
        let watch = Arc::new(Watch::default());
        let mut agent = AgentLoop::new(&root, Arc::new(provider), session);
        agent.set_window_for_test(4000);
        agent.observer = Some(watch.clone());
        agent.compact_now().await;
        assert_eq!(requests.lock().unwrap()[0].max_output_tokens, 2048);
        let saved = root
            .store
            .events(session, 0, 1000)
            .unwrap()
            .into_iter()
            .find(|e| e.record.kind == EventKind::Compaction)
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&root.store.get(&saved.record.body).unwrap()).unwrap();
        assert!(!body.to_string().contains("DO_NOT_REPLAY_OPAQUE_STATE"));
        let summary = body["summary"].as_str().unwrap();
        if oversized {
            assert!(
                summary.contains("max_compaction_summary_bytes"),
                "fallback identifies the exceeded bound: {summary}"
            );
            assert!(!summary.contains(&text));
        } else {
            assert_eq!(summary, text);
        }
        let id = rook_store::parse_session_id(body["compaction_id"].as_str().unwrap()).unwrap();
        assert_eq!(
            *watch.0.lock().unwrap(),
            vec![Observed::Start(id), Observed::Saved(id, saved.seq, summary.into())]
        );
        let through = root.last_compaction(session).unwrap();
        assert!(through.0 > 0);
        drop(agent);
        drop(root);
        let reopened = rook(Store::open(store.path()).unwrap(), workspace.path(), config);
        assert_eq!(reopened.last_compaction(session).unwrap(), through);
        let reopened_event = reopened.store.events(session, saved.seq, 1).unwrap().remove(0);
        let reopened_body: serde_json::Value =
            serde_json::from_slice(&reopened.store.get(&reopened_event.record.body).unwrap()).unwrap();
        assert_eq!(reopened_body["compaction_id"], body["compaction_id"]);
    }
}

#[tokio::test]
async fn compaction_cancellation_and_unsummarizable_history_never_claim_saved_completion() {
    let store = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = rook(Store::open(store.path()).unwrap(), workspace.path(), Config::default());
    let session = root.start_session("cancellation").unwrap();
    let watch = Arc::new(Watch::default());
    let entered = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(Summary {
        deltas: vec![],
        hang: true,
        entered: entered.clone(),
        requests: Default::default(),
    });
    let mut agent = AgentLoop::new(&root, provider, session);
    agent.observer = Some(watch.clone());
    agent.set_window_for_test(4000);
    agent.compact_now().await;
    {
        let events = watch.0.lock().unwrap();
        let Observed::Start(id) = events[0] else { panic!("no start") };
        assert_eq!(events[1], Observed::Failed(id));
    }
    history(&root, session);
    let mut compacting = Box::pin(agent.compact_now());
    tokio::select! {
        _ = &mut compacting => panic!("controlled pending stream completed"),
        _ = entered.notified() => {}
    }
    // Drop the owning future, not only a pinned reference to it.
    drop(compacting);
    drop(agent);
    let events = watch.0.lock().unwrap();
    let Observed::Start(id) = events[2] else { panic!("no actual attempt") };
    assert_eq!(events[3], Observed::Cancelled(id));
    assert!(
        !root.store.events(session, 0, 1000).unwrap().iter().any(|e| e.record.kind == EventKind::Compaction)
    );
}

#[test]
fn summary_byte_limit_has_a_default_and_rejects_out_of_range_configuration() {
    let mut config = Config::default();
    assert_eq!(config.agent.max_compaction_summary_bytes, 64 * 1024);
    for (value, valid) in [(1023, false), (1024, true), (1024 * 1024, true), (1024 * 1024 + 1, false)] {
        config.agent.max_compaction_summary_bytes = value;
        assert_eq!(
            !config
                .validation_errors()
                .iter()
                .any(|error| error.starts_with("agent.max_compaction_summary_bytes")),
            valid
        );
    }
}

#[tokio::test]
async fn completion_is_not_announced_when_a_generated_summary_cannot_be_appended() {
    struct PausedSummary {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl Provider for PausedSummary {
        fn id(&self) -> &str {
            "fixture/save-failure"
        }
        fn context_window(&self) -> usize {
            4000
        }
        async fn complete(&self, _: Request) -> rook_llm::Result<Response> {
            unreachable!()
        }
        async fn stream(&self, request: Request) -> rook_llm::Result<rook_llm::ResponseStream> {
            assert!(request.messages[0].content.starts_with("You are compacting"));
            self.entered.notify_one();
            self.release.notified().await;
            Ok(Box::pin(futures_util::stream::iter([
                Ok(Delta::Text("completed model summary".into())),
                Ok(Delta::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    model: "fixture".into(),
                }),
            ])))
        }
    }
    let store = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let root = rook(Store::open(store.path()).unwrap(), workspace.path(), Config::default());
    let session = root.start_session("append failure").unwrap();
    history(&root, session);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let watch = Arc::new(Watch::default());
    let mut agent = AgentLoop::new(
        &root,
        Arc::new(PausedSummary { entered: entered.clone(), release: release.clone() }),
        session,
    );
    agent.set_window_for_test(4000);
    agent.observer = Some(watch.clone());
    let mut compacting = Box::pin(agent.compact_now());
    tokio::select! {
        _ = &mut compacting => panic!("the model must actually be in flight"),
        _ = entered.notified() => {}
    }
    // The owned scratch session disappears after its transcript was admitted,
    // proving the completion path fails at persistence rather than initial load.
    root.store.delete_session(session).unwrap();
    release.notify_one();
    compacting.await;
    assert!(root.store.get_session(session).unwrap().is_none());
    let events = watch.0.lock().unwrap();
    let Observed::Start(id) = events[0] else { panic!("no started attempt") };
    assert_eq!(*events, vec![Observed::Start(id), Observed::Failed(id)]);
}
