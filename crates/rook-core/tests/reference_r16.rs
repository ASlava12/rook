use rook_core::{
    Config, ModelSource, Rook,
    agent::AgentLoop,
    conversation_export::{Profile, Request as ExportRequest},
    model_selection::Request as Pick,
};
use rook_store::EventKind;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn open(root: &std::path::Path, config: Config) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "0.15.0"),
        rook_skills::SkillIndex::default(),
        root.into(),
    )
}
fn config() -> Config {
    let mut config = Config::default();
    config.memory.enabled = false;
    config.web.enabled = false;
    config.agent.install_servers = false;
    config.agent.plan_first = false;
    config.agent.one_script = false;
    for name in ["A", "B"] {
        config.models.insert(
            name.into(),
            ModelSource {
                model: name.into(),
                api: "openai".into(),
                url: "http://127.0.0.1:9/v1".into(),
                context_window: Some(8192),
                ..Default::default()
            },
        );
    }
    config.agent.model = "A".into();
    config
}

#[test]
fn acknowledged_model_picks_survive_reopen_and_retry_a_cannot_replace_b() {
    let root = tempfile::tempdir().unwrap();
    let config = config();
    let rook = open(root.path(), config.clone());
    let session = rook.start_session("model picks").unwrap();
    let a = Pick { effort: None, id: "request-A".into(), expected_revision: 0, model: "A".into() };
    let a_receipt = rook.select_model(session, &a).unwrap();
    let b = Pick { effort: None, id: "request-B".into(), expected_revision: 1, model: "B".into() };
    let b_receipt = rook.select_model(session, &b).unwrap();
    assert!(rook.model_selection(session).unwrap().pending);
    assert_eq!(rook.select_model(session, &a).unwrap(), a_receipt);
    assert_eq!(rook.model_selection(session).unwrap().selected.as_deref(), Some("B"));
    assert!(rook.select_model(session, &Pick { model: "B".into(), ..a.clone() }).is_err());
    assert!(rook.select_model(session, &Pick { id: "stale".into(), ..a.clone() }).is_err());
    drop(rook);
    let rook = open(root.path(), config.clone());
    assert_eq!(rook.select_model(session, &b).unwrap(), b_receipt);
    rook.log(session, EventKind::UserMessage, "", "preserve this event").unwrap();
    let before = rook.store.get_session(session).unwrap().unwrap();
    let running_b = rook.provider_for_turn(session, Some("A")).unwrap();
    assert_eq!(running_b.configured_source(), Some("B"));
    let after = rook.store.get_session(session).unwrap().unwrap();
    assert_eq!(after.model, "B");
    assert_eq!(after.event_count, before.event_count);
    assert!(!rook.model_selection(session).unwrap().pending);
    let c = Pick { effort: None, id: "request-C".into(), expected_revision: 2, model: "A".into() };
    rook.select_model(session, &c).unwrap();
    assert_eq!(running_b.configured_source(), Some("B"), "a live provider is immutable");
    drop(rook);
    let mut changed = config;
    changed.models.get_mut("A").unwrap().url = "http://127.0.0.1:10/v1".into();
    let rook = open(root.path(), changed);
    assert!(rook.provider_for_turn(session, None).err().unwrap().to_string().contains("route"));
    assert_eq!(
        rook.select_model(session, &a).unwrap(),
        a_receipt,
        "an exact retry needs neither the current revision nor its old route"
    );
    assert_eq!(rook.model_selection(session).unwrap().revision, 3);
    rook.select_model(
        session,
        &Pick { effort: None, id: "confirm-new-route".into(), expected_revision: 3, model: "A".into() },
    )
    .unwrap();
    assert_eq!(rook.provider_for_turn(session, None).unwrap().configured_source(), Some("A"));
}

#[test]
fn live_attention_indexes_waiting_owners_instead_of_paging_through_old_sessions() {
    use rook_core::attention::{Live, Query};
    use std::collections::BTreeMap;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    for _ in 0..8 {
        rook.start_session("old session without input").unwrap();
    }
    let parent = rook.start_session("waiting parent").unwrap();
    let child = rook.start_session("waiting child").unwrap();
    rook.store.update_session(child, |meta| meta.parent = Some(parent)).unwrap();
    let waiting = Live { generation: "current".into(), questions: 1, ..Default::default() };
    let mut live = BTreeMap::from([(child, waiting.clone())]);
    let page = rook.live_attention_page(&Query { limit: Some(1), ..Default::default() }, &live).unwrap();
    assert_eq!(page.scanned, 1);
    assert_eq!(page.items[0].session, rook_store::format_session_id(child));
    assert!(page.next.is_none());
    live.insert(parent, waiting);
    let page = rook.live_attention_page(&Query::default(), &live).unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].session, rook_store::format_session_id(parent));
    assert_eq!(page.items[1].depth, 1);
    assert!(!page.items[1].prefix.is_empty());
}

#[test]
fn export_profiles_omit_internal_payloads_keep_the_selected_prefix_and_escape_rendered_text() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let session = rook.start_session("export").unwrap();
    let start = rook.log(session, EventKind::UserMessage, "", "<script>'question'</script>").unwrap();
    rook.log(session, EventKind::Reasoning, "", "INTERNAL_THOUGHT").unwrap();
    rook.log(session, EventKind::ToolCall, "run_command", "TOOL_ARGUMENTS").unwrap();
    rook.log(session, EventKind::ToolResult, "run_command", &"🦀&".repeat(4000)).unwrap();
    rook.log(session, EventKind::Compaction, "", "INTERNAL_COMPACTION").unwrap();
    let end = rook.log(session, EventKind::AssistantMessage, "SENSITIVE_LABEL", "visible answer").unwrap();
    rook.log(session, EventKind::AssistantMessage, "", "OUTSIDE_RANGE").unwrap();
    let request = ExportRequest { from: start, through: Some(end), profile: Profile::Share };
    let share = rook.conversation_export(session, &request).unwrap();
    assert_eq!(share.events, 2);
    assert_eq!(share.omitted["reasoning"], 1);
    for hidden in [
        "INTERNAL_THOUGHT",
        "TOOL_ARGUMENTS",
        "INTERNAL_COMPACTION",
        "OUTSIDE_RANGE",
        "SENSITIVE_LABEL",
        "<script>",
    ] {
        assert!(!share.html.contains(hidden), "{hidden}");
    }
    assert!(share.html.contains("&lt;script&gt;&#39;question&#39;&lt;/script&gt;"));
    assert!(share.html.contains("Omission manifest"));
    let tools = rook
        .conversation_export(session, &ExportRequest { profile: Profile::ShareTools, ..request.clone() })
        .unwrap();
    assert_eq!(tools.events, 4);
    assert_eq!(tools.shortened, 1);
    assert!(tools.html.contains("TOOL_ARGUMENTS") && tools.html.contains("Show tool content"));
    assert!(!tools.html.contains("INTERNAL_THOUGHT"));
    let archive =
        rook.conversation_export(session, &ExportRequest { profile: Profile::Archive, ..request }).unwrap();
    assert!(archive.html.contains("INTERNAL_THOUGHT") && archive.html.contains("INTERNAL_COMPACTION"));
    assert!(
        rook.conversation_export(
            session,
            &ExportRequest { from: end, through: Some(start), ..Default::default() }
        )
        .is_err()
    );
    for _ in 0..513 {
        rook.log(session, EventKind::Note, "", "OMITTED_TOO").unwrap();
    }
    assert!(
        rook.conversation_export(session, &ExportRequest::default()).unwrap_err().to_string().contains("512"),
        "omitting events must not bypass the scan cap"
    );
}

#[test]
fn attention_pages_do_not_turn_saved_questions_into_live_answer_authority() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let parent = rook.start_session("parent").unwrap();
    let child = rook.start_session("child").unwrap();
    rook.store.update_session(child, |meta| meta.parent = Some(parent)).unwrap();
    let mut tracker = rook_core::activity::Tracker::new(child, 30);
    tracker.check(false, true);
    rook_core::activity::begin(&rook.store, child, &tracker.snapshot(true)).unwrap();
    let offline = rook.attention_page(&Default::default(), false, |_| None).unwrap();
    assert_eq!(offline.items.len(), 1);
    assert!(offline.items[0].live.is_none());
    assert_eq!(offline.items[0].parent, Some(rook_store::format_session_id(parent)));
    assert!(rook.attention_page(&Default::default(), true, |_| None).unwrap().items.is_empty());
    let live = rook
        .attention_page(&Default::default(), true, |id| {
            (id == child).then(|| rook_core::attention::Live {
                generation: "owner".into(),
                questions: 2,
                secrets: 1,
                ..Default::default()
            })
        })
        .unwrap();
    assert_eq!(live.items[0].live.as_ref().unwrap().secrets, 1);
    let json = serde_json::to_string(&live).unwrap();
    assert!(!json.contains("question body") && !json.contains("secret_value"));
    let one = rook
        .attention_page(&rook_core::attention::Query { after: None, limit: Some(1) }, false, |_| None)
        .unwrap();
    assert_eq!(one.scanned, 1);
    assert!(one.next.is_some());
    let two = rook
        .attention_page(&rook_core::attention::Query { after: one.next, limit: Some(1) }, false, |_| None)
        .unwrap();
    assert_eq!(two.scanned, 1);
    assert!(
        rook.attention_page(
            &rook_core::attention::Query { after: Some("bad cursor".into()), limit: None },
            false,
            |_| None
        )
        .is_err()
    );
}

struct ToolsModel {
    calls: AtomicUsize,
    seen: Mutex<Vec<Vec<String>>>,
}
#[async_trait::async_trait]
impl rook_llm::Provider for ToolsModel {
    fn id(&self) -> &str {
        "fixture/equipment"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request.tools.iter().map(|t| t.name.clone()).collect());
        Ok(rook_llm::Response {
            message: rook_llm::Message::assistant("done"),
            usage: Default::default(),
            stop_reason: rook_llm::StopReason::EndTurn,
            model: self.id().into(),
        })
    }
}
#[tokio::test]
async fn recipe_preflight_spends_no_model_request_when_equipment_is_missing_and_empty_means_none() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    std::fs::create_dir_all(root.path().join(".rook/recipes")).unwrap();
    let path = root.path().join(".rook/recipes/probe.toml");
    std::fs::write(&path, "version=1\nprompt='do work'\n[equipment]\nrequire_mcp=['missing-server']\n")
        .unwrap();
    let model = Arc::new(ToolsModel { calls: AtomicUsize::new(0), seen: Mutex::new(Vec::new()) });
    let options = rook_proto::TurnOptions {
        recipe: Some(rook_proto::RecipeInvocation { path: "probe".into(), parameters: Default::default() }),
        ..Default::default()
    };
    let mut agent = AgentLoop::new(&rook, model.clone(), rook.start_session("missing").unwrap());
    agent.options = options.clone();
    let error = agent.run("run it").await.unwrap_err().to_string();
    assert!(error.contains("missing-server"), "{error}");
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    std::fs::write(&path, "version=1\nprompt='answer only'\n[equipment]\ntools=[]\nmcp=[]\n").unwrap();
    let mut agent = AgentLoop::new(&rook, model.clone(), rook.start_session("empty").unwrap());
    agent.options = options;
    agent.run("run it").await.unwrap();
    assert!(model.calls.load(Ordering::SeqCst) > 0);
    assert!(model.seen.lock().unwrap().iter().all(Vec::is_empty));
}

struct UnadvertisedWrite(AtomicUsize);
#[async_trait::async_trait]
impl rook_llm::Provider for UnadvertisedWrite {
    fn id(&self) -> &str {
        "fixture/unadvertised-write"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
        let mut message = rook_llm::Message::assistant("done");
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            message.tool_calls.push(rook_llm::ToolCall {
                id: "denied-write".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path":"should-not-exist.txt","content":"forbidden"}),
            });
        }
        let stop_reason = if message.tool_calls.is_empty() {
            rook_llm::StopReason::EndTurn
        } else {
            rook_llm::StopReason::ToolUse
        };
        Ok(rook_llm::Response { message, usage: Default::default(), stop_reason, model: self.id().into() })
    }
}

#[tokio::test]
async fn recipe_restrictions_block_actual_calls_even_when_the_model_ignores_advertisements() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    std::fs::create_dir_all(root.path().join(".rook/recipes")).unwrap();
    std::fs::write(
        root.path().join(".rook/recipes/closed.toml"),
        "version=1\nprompt='answer only'\n[equipment]\ntools=[]\nmcp=[]\n",
    )
    .unwrap();
    let session = rook.start_session("closed equipment").unwrap();
    let mut agent = AgentLoop::new(&rook, Arc::new(UnadvertisedWrite(AtomicUsize::new(0))), session);
    agent.options.recipe =
        Some(rook_proto::RecipeInvocation { path: "closed".into(), parameters: Default::default() });
    agent.run("try a write").await.unwrap();
    assert!(!root.path().join("should-not-exist.txt").exists());
    let events = rook.store.events(session, 0, 128).unwrap();
    assert!(events.iter().any(|event| {
        event.record.kind == EventKind::ToolResult
            && String::from_utf8_lossy(&rook.store.get(&event.record.body).unwrap())
                .contains("unavailable under the selected recipe equipment")
    }));
}
