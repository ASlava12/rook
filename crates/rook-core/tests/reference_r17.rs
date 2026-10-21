use rook_core::{Config, ModelSource, Rook, agent::AgentLoop, model_selection::Request as Pick};
use rook_store::EventKind;
use std::sync::{Arc, Mutex};

fn open(root: &std::path::Path, config: Config) -> Rook {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };
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
    config.model_catalog.cache_enabled = false;
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
fn model_and_effort_are_one_retryable_pair_and_an_admitted_pair_is_immutable() {
    let root = tempfile::tempdir().unwrap();
    let config = config();
    let rook = open(root.path(), config.clone());
    let session = rook.start_session("paired settings").unwrap();
    let a = Pick {
        id: "A-low".into(),
        expected_revision: 0,
        model: "A".into(),
        effort: Some(rook_llm::Effort::Low),
    };
    rook.select_model(session, &a).unwrap();
    let running = rook.provider_for_turn(session, None).unwrap();
    let b = Pick {
        id: "B-high".into(),
        expected_revision: 1,
        model: "B".into(),
        effort: Some(rook_llm::Effort::High),
    };
    let receipt = rook.select_model(session, &b).unwrap();
    assert_eq!(running.turn_effort(), Some(rook_llm::Effort::Low));
    assert_eq!(rook.model_selection(session).unwrap().selected_effort, Some(rook_llm::Effort::High));
    assert_eq!(rook.model_selection(session).unwrap().last_effective_effort, Some(rook_llm::Effort::Low));
    assert!(rook.select_model(session, &Pick { effort: Some(rook_llm::Effort::Max), ..b.clone() }).is_err());
    drop(rook);
    let rook = open(root.path(), config);
    assert_eq!(rook.select_model(session, &b).unwrap(), receipt);
    rook.select_model(session, &a).unwrap();
    assert_eq!(rook.model_selection(session).unwrap().selected.as_deref(), Some("B"));
    let next = rook.provider_for_turn(session, None).unwrap();
    assert_eq!(next.turn_effort(), Some(rook_llm::Effort::High));
    assert!(!rook.model_selection(session).unwrap().pending);
    // A legacy model-only pick preserves the previously selected effort.
    rook.select_model(
        session,
        &Pick { id: "legacy".into(), expected_revision: 2, model: "A".into(), effort: None },
    )
    .unwrap();
    assert_eq!(rook.provider_for_turn(session, None).unwrap().turn_effort(), Some(rook_llm::Effort::High));
}

#[test]
fn legacy_json_selection_receipts_migrate_without_reinterpreting_an_old_retry() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let session = rook.start_session("legacy").unwrap();
    let key = format!("model-selection/{session:032x}");
    rook.store.kv_set(&key, br#"{"version":1,"revision":1,"applied_revision":0,"selected":"A","route":"0000000000000000000000000000000000000000000000000000000000000000","effective":null,"receipts":[{"id":"old","model":"A","revision":1,"expected_revision":0,"status":"queued"}]}"#).unwrap();
    let view = rook.model_selection(session).unwrap();
    assert_eq!(view.selected_effort, None);
    assert_eq!(
        rook.select_model(
            session,
            &Pick { id: "old".into(), expected_revision: 0, model: "A".into(), effort: None }
        )
        .unwrap()
        .revision,
        1
    );
    assert!(
        rook.select_model(
            session,
            &Pick {
                id: "old".into(),
                expected_revision: 0,
                model: "A".into(),
                effort: Some(rook_llm::Effort::High)
            }
        )
        .is_err()
    );
}

#[test]
fn last_answer_follows_the_saved_branch_and_internal_summary_is_an_explicit_selection() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let session = rook.start_session("copy").unwrap();
    let first = rook.log(session, EventKind::AssistantMessage, "", "first answer").unwrap();
    let fork = rook.fork_session(session, first + 1).unwrap();
    rook.log(session, EventKind::AssistantMessage, "", "later parent answer").unwrap();
    rook.log(fork.id, EventKind::Reasoning, "", "PRIVATE_REASONING").unwrap();
    rook.log(fork.id, EventKind::ToolResult, "", "PRIVATE_TOOL_PAYLOAD").unwrap();
    let summary = rook
        .log(fork.id, EventKind::Compaction, "", r#"{"through_seq":0,"summary":"internal summary"}"#)
        .unwrap();
    let answer = rook.last_answer(fork.id, false).unwrap();
    assert_eq!(answer.text, "first answer");
    assert_eq!(answer.seq, first);
    assert_eq!(rook.last_answer(session, false).unwrap().text, "later parent answer");
    let answer = rook.last_answer(fork.id, true).unwrap();
    assert_eq!(answer.text, "internal summary");
    assert_eq!(answer.seq, summary);
    rook.log(fork.id, EventKind::AssistantMessage, "", &"🙂".repeat(30000)).unwrap();
    let answer = rook.last_answer(fork.id, false).unwrap();
    assert!(answer.truncated);
    assert!(answer.text.len() < 66000);
    assert!(!answer.text.contains("PRIVATE_"));
}

#[test]
fn explicit_empty_main_fallback_does_not_inherit_global_priorities_and_invalid_chains_fail() {
    let mut config = config();
    config.models.get_mut("B").unwrap().priority = Some(1);
    let vault = rook_core::Vault::empty();
    assert_eq!(rook_core::models::endpoints_for(&config, &vault, "A").unwrap().len(), 2);
    config.agent.fallbacks.main = Some(vec![]);
    assert_eq!(rook_core::models::endpoints_for(&config, &vault, "A").unwrap().len(), 1);
    config.agent.fallbacks.compaction = Some(vec!["B".into()]);
    config.agent.fallbacks.errand = Some(vec![]);
    assert!(config.validation_errors().is_empty(), "{:?}", config.validation_errors());
    config.agent.fallbacks.errand = Some(vec!["B".into(), "B".into()]);
    assert!(config.validation_errors().iter().any(|e| e.contains("fallbacks.errand")));
    assert!(rook_core::models::errand_provider_for(&config, &vault, "A").is_err());
}

struct Summarizer {
    pending: bool,
    requests: Arc<Mutex<Vec<rook_llm::Request>>>,
}
#[async_trait::async_trait]
impl rook_llm::Provider for Summarizer {
    fn id(&self) -> &str {
        "fixture/summary"
    }
    fn context_window(&self) -> usize {
        8192
    }
    async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
        unreachable!("summary streams")
    }
    async fn stream(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::ResponseStream> {
        self.requests.lock().unwrap().push(request);
        if self.pending {
            return Ok(Box::pin(futures_util::stream::pending()));
        }
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(rook_llm::Delta::Text("goal and unresolved work retained".into())),
            Ok(rook_llm::Delta::Done {
                stop_reason: rook_llm::StopReason::EndTurn,
                usage: Default::default(),
                model: "fixture".into(),
            }),
        ])))
    }
}
fn seed(rook: &Rook, session: u128) {
    for index in 0..16 {
        rook.log(
            session,
            if index % 2 == 0 { EventKind::UserMessage } else { EventKind::AssistantMessage },
            "",
            &format!("entry {index} trusted goal and unfinished work. ").repeat(120),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn compaction_phase_ceiling_discards_an_unfinished_summary_and_keeps_the_source() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config();
    config.agent.compaction_timeout_secs = Some(1);
    let rook = open(root.path(), config);
    let session = rook.start_session("deadline").unwrap();
    seed(&rook, session);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Summarizer { pending: true, requests: requests.clone() });
    let agent = AgentLoop::new(&rook, model, session);
    tokio::time::timeout(std::time::Duration::from_secs(30), agent.compact_now()).await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1, "the real summary stream must have started");
    let (_, summary) = rook.last_compaction(session).unwrap();
    assert!(summary.unwrap().contains("compaction phase deadline expired"));
    assert!(
        rook.transcript(session, 0, 100, 2048)
            .unwrap()
            .iter()
            .any(|entry| entry.kind == "user" && entry.body.contains("trusted goal"))
    );
}
#[tokio::test]
async fn explicit_summary_output_budget_uses_the_summarizers_window_and_default_remains_2048() {
    for budget in [None, Some(4096)] {
        let root = tempfile::tempdir().unwrap();
        let mut config = config();
        config.agent.compaction_output_tokens = budget;
        let rook = open(root.path(), config);
        let session = rook.start_session("budget").unwrap();
        seed(&rook, session);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = Arc::new(Summarizer { pending: false, requests: requests.clone() });
        AgentLoop::new(&rook, model, session).compact_now().await;
        assert!(
            rook.last_compaction(session).unwrap().1.unwrap().contains("goal and unresolved work retained")
        );
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].max_output_tokens, budget.unwrap_or(2048));
        assert!(
            requests[0]
                .messages
                .iter()
                .map(|m| rook_core::context::estimate_tokens(&m.content))
                .sum::<usize>()
                + requests[0].max_output_tokens as usize
                <= 8192
        );
    }
}

#[tokio::test]
async fn dropping_compaction_cancels_its_stream_and_never_commits_a_late_note() {
    let root = tempfile::tempdir().unwrap();
    let observer = root.path().join("unexpected-observer");
    let mut config = config();
    config.hooks.push(rook_core::hooks::HookConfig {
        event: rook_core::hooks::Event::PostCompaction,
        command: format!("echo called > \"{}\"", observer.display()),
        ..Default::default()
    });
    let rook = open(root.path(), config);
    let session = rook.start_session("cancel summary").unwrap();
    seed(&rook, session);
    let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent =
        AgentLoop::new(&rook, Arc::new(Summarizer { pending: true, requests: requests.clone() }), session);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(100), agent.compact_now()).await.is_err());
    assert_eq!(requests.lock().unwrap().len(), 1);
    tokio::task::yield_now().await;
    assert!(
        !rook
            .store
            .events(session, before, 128)
            .unwrap()
            .iter()
            .any(|event| event.record.kind == EventKind::Compaction)
    );
    assert_eq!(rook.last_compaction(session).unwrap().1, None);
    assert!(!observer.exists(), "a cancelled candidate has no committed observer event");
    assert!(rook.transcript(session, 0, 128, 1024).unwrap().iter().any(|entry| entry.kind == "user"));
}

async fn answer_server() -> (String, tokio::sync::oneshot::Receiver<serde_json::Value>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut scratch = [0; 8192];
        let streaming = loop {
            let n = socket.read(&mut scratch).await.unwrap();
            assert!(n > 0 && raw.len() + n < 512 * 1024);
            raw.extend_from_slice(&scratch[..n]);
            let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&raw[..split]);
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|n| n.trim().parse().ok())
                })
                .unwrap();
            if raw.len() < split + 4 + length {
                continue;
            }
            let request: serde_json::Value =
                serde_json::from_slice(&raw[split + 4..split + 4 + length]).unwrap();
            let streaming = request["stream"] == true;
            tx.send(request).unwrap();
            break streaming;
        };
        let (mime, body) = if streaming {
            (
                "text/event-stream",
                format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({
                        "id":"r17", "model":"opaque-alias", "choices":[{"index":0,"delta":{
                            "content":"goal, unfinished work and saved source retained"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":20,"completion_tokens":10}
                    })
                ),
            )
        } else {
            ("application/json", r#"{"id":"r17","model":"opaque-alias","choices":[{"message":{"role":"assistant","content":"goal, unfinished work and saved source retained"},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":10}}"#.to_string())
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    (url, rx)
}

#[tokio::test]
async fn purpose_fallbacks_use_distinct_real_routes_and_alias_effort_matches_the_admitted_pair() {
    use rook_llm::Effort;
    let (main_url, main) = answer_server().await;
    let (summary_url, summary) = answer_server().await;
    let (errand_url, errand) = answer_server().await;
    let mut config = config();
    for (name, url, window) in [
        ("main-backup", main_url, 16000),
        ("summary-backup", summary_url, 32000),
        ("errand-backup", errand_url, 64000),
    ] {
        config.models.insert(
            name.into(),
            ModelSource {
                api: "openai".into(),
                url,
                model: "opaque-alias".into(),
                context_window: Some(window),
                reasoning: Some(rook_llm::reasoning::Override {
                    control: Some(rook_llm::reasoning::Control::OpenaiEffort),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
    }
    config.agent.fallbacks.main = Some(vec!["main-backup".into()]);
    config.agent.fallbacks.compaction = Some(vec!["summary-backup".into()]);
    config.agent.fallbacks.errand = Some(vec!["errand-backup".into()]);
    config.models.get_mut("A").unwrap().compaction.keep_recent_tokens = Some(256);
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config);
    let session = rook.start_session("purpose routing").unwrap();
    rook.select_model(
        session,
        &Pick { id: "paired".into(), expected_revision: 0, model: "A".into(), effort: Some(Effort::High) },
    )
    .unwrap();
    let selected = rook.provider_for_turn(session, None).unwrap();
    assert_eq!(selected.turn_effort(), Some(Effort::High));
    let mut request = rook_llm::Request::new(vec![rook_llm::Message::user("main")]);
    request.effort = selected.turn_effort();
    tokio::time::timeout(std::time::Duration::from_secs(20), selected.complete(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(main.await.unwrap()["reasoning_effort"], "high");
    seed(&rook, session);
    AgentLoop::new(&rook, Arc::from(rook.provider_for_turn(session, None).unwrap()), session)
        .compact_now()
        .await;
    assert_eq!(summary.await.unwrap()["reasoning_effort"], "low");
    let asked =
        rook_core::models::errand_provider_for(&rook.config, &rook_core::Vault::empty(), "A").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        asked.complete(rook_llm::Request::new(vec![rook_llm::Message::user("errand")])),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(errand.await.unwrap()["model"], "opaque-alias");
    assert!(
        rook.transcript(session, 0, 128, 4096)
            .unwrap()
            .iter()
            .any(|entry| entry.label == "rook:model-aux:v1" && entry.body.contains("summary-backup"))
    );
}

#[tokio::test]
async fn an_inherited_errand_primary_still_uses_its_own_explicit_backup_chain() {
    struct Parent(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl rook_llm::Provider for Parent {
        fn id(&self) -> &str {
            "fixture/parent"
        }
        fn configured_source(&self) -> Option<&str> {
            Some("A")
        }
        fn context_window(&self) -> usize {
            8192
        }
        async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            let classification =
                request.messages.first().is_some_and(|m| m.content.starts_with("Classify whether"));
            let first = !classification && self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let mut message = rook_llm::Message::assistant(if classification {
                r#"{"action":"finish"}"#
            } else {
                "parent done"
            });
            if first {
                message.content.clear();
                message.tool_calls.push(rook_llm::ToolCall {
                    id: "errand".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({"task":"inspect without side effects"}),
                });
            }
            Ok(rook_llm::Response {
                message,
                stop_reason: if first {
                    rook_llm::StopReason::ToolUse
                } else {
                    rook_llm::StopReason::EndTurn
                },
                usage: Default::default(),
                model: "fixture".into(),
            })
        }
    }
    for explicit in [false, true] {
        let (url, request) = answer_server().await;
        let mut config = config();
        config.agent.fallbacks.main = Some(vec![]);
        config.agent.fallbacks.errand = explicit.then(|| vec!["errand-only".into()]);
        config.models.insert(
            "errand-only".into(),
            ModelSource {
                priority: Some(1),
                api: "openai".into(),
                url,
                model: "errand-only-model".into(),
                context_window: Some(8192),
                ..Default::default()
            },
        );
        let root = tempfile::tempdir().unwrap();
        let rook = open(root.path(), config);
        let session = rook.start_session("inherited errand primary").unwrap();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            AgentLoop::new(&rook, Arc::new(Parent(Default::default())), session).run("delegate once"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome.delegated.len(), 1);
        assert_eq!(request.await.unwrap()["model"], "errand-only-model");
    }
}

#[test]
fn optional_reasoning_and_fallback_collections_are_reachable_through_typed_config_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path,"[agent]\nmodel='desk'\n[models.desk]\nmodel='opaque-alias'\napi='openai'\nurl='http://127.0.0.1:9/v1'\n").unwrap();
    Config::set_in(&path, "models.desk.reasoning.control", "openai-effort").unwrap();
    Config::set_in(&path, "models.desk.reasoning.effort_levels", r#"["low","high"]"#).unwrap();
    Config::set_in(&path, "agent.fallbacks.main", "[]").unwrap();
    let read = || toml::from_str::<Config>(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(read().agent.fallbacks.main, Some(vec![]));
    assert_eq!(
        read().models["desk"].reasoning.as_ref().unwrap().effort_levels,
        Some(vec![rook_llm::Effort::Low, rook_llm::Effort::High])
    );
    let mut editor = rook_core::config::edit::Editor::open(path.clone()).unwrap();
    let model = vec!["models".into(), "desk".into()];
    assert!(
        editor
            .entries(&model)
            .unwrap()
            .iter()
            .find(|e| e.path.last().is_some_and(|p| p == "reasoning"))
            .unwrap()
            .value
            .is_object()
    );
    let errand = vec!["agent".into(), "fallbacks".into(), "errand".into()];
    assert!(editor.can_add(&errand));
    assert!(editor.entries(&errand).unwrap().is_empty());
    let child = editor.add(&errand, "desk").unwrap();
    editor.save().unwrap();
    assert_eq!(read().agent.fallbacks.errand, Some(vec!["desk".into()]));
    editor.remove(&child).unwrap();
    editor.save().unwrap();
    assert_eq!(
        read().agent.fallbacks.errand,
        Some(vec![]),
        "removing the last item explicitly disables fallback"
    );
    editor.remove(&errand).unwrap();
    editor.save().unwrap();
    assert_eq!(read().agent.fallbacks.errand, None, "removing the setting restores inheritance");
}

#[test]
fn changing_reasoning_controls_refreshes_capabilities_without_invalidating_the_saved_model_choice() {
    let root = tempfile::tempdir().unwrap();
    let mut config = config();
    let rook = open(root.path(), config.clone());
    let session = rook.start_session("capability edit").unwrap();
    rook.select_model(
        session,
        &Pick {
            id: "selected".into(),
            model: "A".into(),
            effort: Some(rook_llm::Effort::High),
            expected_revision: 0,
        },
    )
    .unwrap();
    drop(rook);
    config.models.get_mut("A").unwrap().reasoning = Some(rook_llm::reasoning::Override {
        control: Some(rook_llm::reasoning::Control::OpenaiEffort),
        ..Default::default()
    });
    let rook = open(root.path(), config.clone());
    let provider = rook.provider_for_turn(session, None).unwrap();
    assert_eq!(provider.turn_effort(), Some(rook_llm::Effort::High));
    assert!(matches!(provider.effort_use(rook_llm::Effort::High), rook_llm::EffortUse::Parameter { .. }));
    drop(rook);
    config.models.get_mut("A").unwrap().url = "http://127.0.0.1:10/v1".into();
    assert!(
        open(root.path(), config).provider_for_turn(session, None).is_err(),
        "physical route changes still require an explicit source choice"
    );
}

#[tokio::test]
async fn configured_google_budget_status_matches_the_actual_request_after_runtime_decoration() {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1beta", listener.local_addr().unwrap());
    let (sent, received) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut scratch = [0; 4096];
        loop {
            let n = socket.read(&mut scratch).await.unwrap();
            assert!(n > 0 && raw.len() + n < 65536);
            raw.extend_from_slice(&scratch[..n]);
            let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&raw[..split]);
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|n| n.trim().parse().ok())
                })
                .unwrap();
            if raw.len() < split + 4 + length {
                continue;
            }
            sent.send(
                serde_json::from_slice::<serde_json::Value>(&raw[split + 4..split + 4 + length]).unwrap(),
            )
            .unwrap();
            break;
        }
        let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]},\"finishReason\":\"STOP\"}]}\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let mut vault = rook_core::Vault::load_from(root.path().join("keys.toml")).unwrap();
    vault.keep("fixture-key", "not-a-real-key").unwrap();
    let mut config = config();
    config.agent.fallbacks.main = Some(vec![]);
    config.models.insert(
        "budget-alias".into(),
        ModelSource {
            api: "google".into(),
            model: "opaque-alias".into(),
            url,
            key: "secret:fixture-key".into(),
            context_window: Some(8192),
            reasoning: Some(rook_llm::reasoning::Override {
                control: Some(rook_llm::reasoning::Control::GoogleBudget),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let provider = rook_core::models::provider_for(&config, &vault, "budget-alias").unwrap();
    let mut request = rook_llm::Request::new(vec![rook_llm::Message::user("hi")]);
    request.effort = Some(rook_llm::Effort::High);
    request.max_output_tokens = 1024;
    let mut stream = provider.stream(request).await.unwrap();
    let mut report = None;
    while let Some(delta) = stream.next().await {
        if let rook_llm::Delta::Effort(status) = delta.unwrap() {
            report = Some(status.applied);
        }
    }
    let body = received.await.unwrap();
    let actual = body["generationConfig"]["thinkingConfig"]["thinkingBudget"].as_u64().unwrap();
    assert_eq!(actual, 896, "the output-room clamp must actually be reached");
    assert_eq!(
        report,
        Some(rook_llm::EffortUse::Parameter {
            name: "generationConfig.thinkingConfig.thinkingBudget",
            value: actual.to_string(),
        })
    );
}

#[tokio::test]
async fn malformed_google_frames_never_reach_engine_tool_dispatch_and_failed_usage_stays_saved() {
    for (finish, recover, earlier_call) in [
        ("MALFORMED_RESPONSE", true, false),
        ("SAFETY", false, false),
        ("SAFETY", false, true),
        ("MALFORMED_FUNCTION_CALL", false, false),
        ("UNEXPECTED_TOOL_CALL", false, false),
    ] {
        check_google_fault(finish, recover, earlier_call).await;
    }
}

async fn check_google_fault(finish: &'static str, recover: bool, earlier_call: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1beta", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let mut requests = Vec::new();
        for attempt in 0..if recover { 3 } else { 1 } {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            loop {
                let mut scratch = [0; 8192];
                let n = socket.read(&mut scratch).await.unwrap();
                assert!(n > 0 && raw.len() + n <= 512 * 1024);
                raw.extend_from_slice(&scratch[..n]);
                let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&raw[..split]);
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|s| s.trim().parse().ok())
                    })
                    .unwrap();
                if raw.len() < split + 4 + length {
                    continue;
                }
                requests.push(
                    serde_json::from_slice::<serde_json::Value>(&raw[split + 4..split + 4 + length]).unwrap(),
                );
                break;
            }
            let response = if attempt == 0 {
                serde_json::json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"write_file","args":{"path":"must-not-exist.txt","content":"failed frame"}}}]},"finishReason":finish}],"usageMetadata":{"promptTokenCount":17,"candidatesTokenCount":3}})
            } else if attempt == 1 {
                serde_json::json!({"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":18,"candidatesTokenCount":4}})
            } else {
                serde_json::json!({"candidates":[{"content":{"parts":[{"text":"{\"action\":\"finish\"}"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":2}})
            };
            let (mime, body) = if attempt < 2 {
                let prefix = if attempt == 0 && earlier_call {
                    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"write_file\",\"args\":{\"path\":\"must-not-exist.txt\",\"content\":\"earlier frame\"}}}]}}]}\n\n"
                } else {
                    ""
                };
                ("text/event-stream", format!("{prefix}data: {response}\n\n"))
            } else {
                ("application/json", response.to_string())
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let session = rook.start_session("Gemini malformed recovery").unwrap();
    let mut google = rook_llm::google::Config::new(url, "owned-fixture-key".into(), "gemini-2.5-flash");
    google.proxy = rook_llm::Proxy::Direct;
    let provider = rook_llm::google::Google::new("google/recovery", "gemini-2.5-flash", google).unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        AgentLoop::new(&rook, Arc::new(rook_llm::retry::Retrying::new(Box::new(provider))), session)
            .run("Reply with ok"),
    )
    .await
    .unwrap();
    if recover {
        assert_eq!(result.unwrap().reply, "ok");
    } else {
        assert!(result.is_err(), "{finish} cannot complete an executable generation");
    }
    let sent = served.await.unwrap();
    assert_eq!(sent.len(), if recover { 3 } else { 1 });
    if recover {
        assert_eq!(sent[0], sent[1]);
    }
    assert!(!root.path().join("must-not-exist.txt").exists());
    let events = rook.store.events(session, 0, 128).unwrap();
    assert!(
        !events.iter().any(|event| event.record.kind == EventKind::ToolCall),
        "a failed-frame call must not enter dispatch or the journal"
    );
    let attempts: Vec<serde_json::Value> = events
        .iter()
        .filter(|event| event.record.label == "rook:model-attempt:v1")
        .map(|event| serde_json::from_slice(&rook.store.get(&event.record.body).unwrap()).unwrap())
        .collect();
    let failed: Vec<_> = attempts.iter().filter(|attempt| attempt["state"] == "failed").collect();
    assert_eq!(failed.len(), 1, "attempt records: {attempts:#?}");
    assert_eq!(failed[0]["usage"]["input_tokens"], 17);
    assert_eq!(failed[0]["usage"]["output_tokens"], 3);
    assert_eq!(failed[0]["usage_reported"], true);
    assert_eq!(failed[0]["completion_confirmed"], false);
    assert_eq!(
        attempts
            .iter()
            .filter(|attempt| attempt["state"] == "completed" && attempt["purpose"] == "main")
            .count(),
        usize::from(recover)
    );
    assert_eq!(
        attempts
            .iter()
            .filter(|attempt| attempt["state"] == "completed" && attempt["purpose"] == "completion_check")
            .count(),
        usize::from(recover)
    );
}
