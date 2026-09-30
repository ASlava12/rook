//! A support export must explain execution without copying the conversation.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rook_core::{Config, Rook, agent::AgentLoop};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, ToolSpec, Usage};
use rook_store::EventKind;
use serde_json::{Value, json};

struct Model {
    replies: Mutex<VecDeque<Message>>,
    hang: bool,
    fail: bool,
    entered: tokio::sync::Notify,
}
impl Model {
    fn new(replies: Vec<Message>, hang: bool, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            hang,
            fail,
            entered: tokio::sync::Notify::new(),
        })
    }
}
#[async_trait]
impl Provider for Model {
    fn id(&self) -> &str {
        "scripted/diagnostics"
    }
    fn context_window(&self) -> usize {
        32000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.entered.notify_one();
        if self.hang {
            return std::future::pending().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        if self.fail {
            return Err(rook_llm::LlmError::Other("private-provider-error".into()));
        }
        let checker = request.messages.first().is_some_and(|m| m.content.starts_with("Classify whether"));
        let message = if checker {
            Message::assistant(r#"{"action":"finish"}"#)
        } else {
            self.replies.lock().unwrap().pop_front().expect("unexpected extra request")
        };
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Usage::default(), model: self.id().into() })
    }
}
struct SlowTool;
#[async_trait]
impl rook_tools::Tool for SlowTool {
    fn name(&self) -> &str {
        "measured_read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().into(),
            description: "Read test data".into(),
            parameters: json!({"type":"object"}),
        }
    }
    fn risk(&self, _: &Value) -> rook_tools::policy::Risk {
        rook_tools::policy::Risk::ReadOnly
    }
    async fn call(
        &self,
        _: &rook_tools::ToolContext,
        _: &Value,
    ) -> rook_tools::Result<rook_tools::ToolOutcome> {
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok(rook_tools::ToolOutcome::ok("private-tool-result"))
    }
}
fn rook_at(root: &std::path::Path, config: Config) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        root.into(),
    )
}
fn samples(report: &rook_core::diagnostics::Report, phase: &str) -> Vec<Value> {
    report
        .timings
        .iter()
        .filter(|v| v["measurement"]["phase"] == phase)
        .map(|v| v["measurement"].clone())
        .collect()
}

#[tokio::test]
async fn diagnostics_measure_real_execution_bound_export_and_preserve_private_data() {
    // This binary has one test so ROOK_HOME cannot race another fixture.
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("ROOK_HOME", home.path());
    }
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.agent.context_window = Some(32000);
    let rook = rook_at(root.path(), config.clone());
    let session = rook.start_session("private-session-title").unwrap();
    let mut call = Message::assistant("");
    call.tool_calls.push(ToolCall {
        id: "read".into(),
        name: "measured_read".into(),
        arguments: json!({"key":"private-argument"}),
    });
    let model = Model::new(vec![call, Message::assistant("The data was read.")], false, false);
    let mut agent = AgentLoop::new(&rook, model, session);
    agent.tools.register(Arc::new(SlowTool));
    agent.run("private-user-prompt").await.unwrap();
    let report = rook.diagnostics(session, false).unwrap();
    let models = samples(&report, "model_request");
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|s| s["status"] == "completed" && s["duration_ms"].as_u64().unwrap() >= 20));
    let tools = samples(&report, "tool_dispatch");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["status"], "completed");
    assert!(tools[0]["duration_ms"].as_u64().unwrap() >= 30);
    let results: Vec<_> = rook
        .store
        .events(session, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.record.kind == EventKind::ToolResult)
        .collect();
    assert_eq!(results.len(), 1, "timing notes must not duplicate execution receipts");
    assert_eq!(tools[0]["result_seq"], results[0].seq);
    let text = report.json().unwrap();
    for private in [
        "private-session-title",
        "private-user-prompt",
        "private-argument",
        "private-tool-result",
        root.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(private), "leaked {private}");
    }
    assert!(report.logs.is_empty());
    let file = root.path().join("report.json");
    report.save(&file).unwrap();
    assert!(report.save(&file).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o077, 0);
    }
    let end = rook.store.get_session(session).unwrap().unwrap().next_seq;
    let fork = rook.fork_session(session, end).unwrap().id;
    assert_eq!(samples(&rook.diagnostics(fork, false).unwrap(), "tool_dispatch"), tools);
    drop(agent);
    drop(rook);
    let rook = rook_at(root.path(), config);
    assert_eq!(samples(&rook.diagnostics(session, false).unwrap(), "tool_dispatch"), tools);

    let compacted = rook.start_session("compaction timing").unwrap();
    rook.log(compacted, EventKind::UserMessage, "", &"private-older-prompt ".repeat(1000)).unwrap();
    rook.log(compacted, EventKind::AssistantMessage, "", &"older-answer ".repeat(1000)).unwrap();
    rook.log(compacted, EventKind::UserMessage, "", "continue").unwrap();
    let summary = Model::new(vec![Message::assistant("Earlier work summarized.")], false, false);
    let mut summarizer = AgentLoop::new(&rook, summary, compacted);
    summarizer.set_window_for_test(4000);
    summarizer.compact_now().await;
    assert!(rook.last_compaction(compacted).unwrap().1.is_some(), "fixture must actually compact");
    let measured = samples(&rook.diagnostics(compacted, false).unwrap(), "compaction_request");
    assert_eq!(measured.len(), 1);
    assert_eq!(measured[0]["status"], "completed");
    assert!(measured[0]["duration_ms"].as_u64().unwrap() >= 20);
    drop(summarizer);

    let failed = rook.start_session("failure").unwrap();
    assert!(AgentLoop::new(&rook, Model::new(vec![], false, true), failed).run("fail").await.is_err());
    let report = rook.diagnostics(failed, false).unwrap();
    assert_eq!(samples(&report, "model_request")[0]["status"], "failed");
    assert!(!report.json().unwrap().contains("private-provider-error"));
    let cancelled = rook.start_session("cancelled").unwrap();
    let model = Model::new(vec![], true, false);
    let mut agent = AgentLoop::new(&rook, model.clone(), cancelled);
    {
        let mut run = Box::pin(agent.run("wait"));
        tokio::select! {
            result = &mut run => panic!("request should stay pending: {result:?}"),
            _ = model.entered.notified() => {}
        }
    }
    let report = rook.diagnostics(cancelled, false).unwrap();
    assert_eq!(samples(&report, "model_request")[0]["status"], "cancelled");
    drop(agent);
    drop(rook);

    // Exceed actual limits, including a payload too large to be a timing record.
    let mut config = Config::default();
    config.telemetry.diagnostic_events = 8;
    config.telemetry.diagnostic_log_bytes = 512;
    config.agent.model = "probe".into();
    let marker = root.path().join("credential-helper-ran");
    let source: Config = toml::from_str(&format!(
        "[models.probe]\napi='openai'\nurl='http://localhost:1/v1'\nmodel='x'\nkey={}\n",
        serde_json::to_string(&format!("cmd:touch {}", marker.display())).unwrap()
    ))
    .unwrap();
    config.models = source.models;
    let rook = rook_at(root.path(), config);
    for _ in 0..20 {
        rook.log(session, EventKind::Note, "private-label", "private-log-payload").unwrap();
    }
    rook.log(session, EventKind::Note, "rook:timing:v1", &"private-huge-payload".repeat(100000)).unwrap();
    std::fs::create_dir_all(home.path().join("logs")).unwrap();
    let log = format!(
        "{}\naccess_token=secret-token-value\nAuthorization: Bearer bearer-secret-value\n{{\"apiKey\":\"quoted-api-secret\"}}\nhttps://user:password@host/path?key=private-query\n{}\nready\n",
        "old\n".repeat(1000),
        root.path().display()
    );
    assert!(log.len() > rook.config.telemetry.diagnostic_log_bytes);
    std::fs::write(home.path().join("logs/rook.log"), log).unwrap();
    let report = rook.diagnostics(session, true).unwrap();
    assert_eq!(report.events.len(), 8);
    assert!(report.session["tail_from"].as_u64().unwrap() > 0);
    assert!(report.notices.iter().any(|s| s.contains("timing records could not")));
    assert!(report.logs[0]["truncated"].as_bool().unwrap());
    assert!(report.logs[0]["bytes_read"].as_u64().unwrap() <= 512);
    assert!(report.logs[0]["text"].as_str().unwrap().contains("ready"));
    let text = report.json().unwrap();
    for private in [
        "secret-token-value",
        "bearer-secret-value",
        "quoted-api-secret",
        "private-query",
        "private-huge-payload",
        "private-label",
        root.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(private), "leaked {private}");
    }
    assert!(!marker.exists(), "export must never resolve credentials");
    let empty = rook.start_session("old session with no timing instrumentation").unwrap();
    let mut report = rook.diagnostics(empty, false).unwrap();
    assert!(report.timings.is_empty());
    assert!(report.notices.iter().any(|s| s.contains("no measured timings")));
    report.notices.push("x".repeat(1024 * 1024 + 1));
    assert!(report.json().is_err(), "serialization itself has a hard byte bound");
}
