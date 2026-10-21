//! Effects require a confirmed generation, not merely parseable tool JSON.
use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy)]
enum Mode {
    UnconfirmedTool,
    ConfirmedTool,
    UnconfirmedText,
    UnconfirmedClassifier,
    UnconfirmedRepair,
    WrongBody,
    PromptedTool,
    ReasoningTool,
}

async fn endpoint(mode: Mode) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    tokio::spawn(async move {
        'connections: loop {
            let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept()).await
            else {
                return;
            };
            let mut input = Vec::new();
            let mut scratch = [0_u8; 8192];
            let body = loop {
                let Ok(Ok(n)) =
                    tokio::time::timeout(std::time::Duration::from_secs(10), socket.read(&mut scratch)).await
                else {
                    return;
                };
                if n == 0 {
                    return;
                }
                assert!(input.len() + n <= 1024 * 1024);
                input.extend_from_slice(&scratch[..n]);
                let Some(boundary) = input.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&input[..boundary]);
                if headers.starts_with("GET") {
                    let data = r#"{"data":[]}"#;
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).as_bytes()).await.unwrap();
                    continue 'connections;
                }
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_string)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                if input.len() >= boundary + 4 + size {
                    break serde_json::from_slice::<serde_json::Value>(
                        &input[boundary + 4..boundary + 4 + size],
                    )
                    .unwrap();
                }
            };
            let index = counted.fetch_add(1, Ordering::SeqCst);
            assert!(index < 4, "an unconfirmed response must not start another model request");
            let streaming = body["stream"].as_bool().unwrap_or(false);
            let (mime, data) = if index == 0 {
                let mut choice = serde_json::json!({"index":0,"delta":{"content":"Partial assistant text."}});
                if matches!(mode, Mode::UnconfirmedTool | Mode::ConfirmedTool) {
                    choice["delta"]["tool_calls"] = serde_json::json!([{
                    "index":0,"id":"unconfirmed-call","type":"function","function":{
                        "name":"write_file","arguments":"{\"path\":\"must-not-exist.txt\",\"content\":\"controlled effect\"}"}}]);
                }
                if matches!(mode, Mode::PromptedTool | Mode::ReasoningTool) {
                    let call = serde_json::json!({"name":"write_file","arguments":{"path":"must-not-exist.txt","content":"controlled effect"}}).to_string();
                    if matches!(mode, Mode::PromptedTool) {
                        choice["delta"]["content"] = serde_json::json!(call);
                    } else {
                        choice["delta"]["content"] = serde_json::Value::Null;
                        choice["delta"]["reasoning_content"] = serde_json::json!(call);
                    }
                }
                if matches!(mode, Mode::ConfirmedTool | Mode::UnconfirmedClassifier | Mode::UnconfirmedRepair)
                {
                    choice["finish_reason"] = serde_json::json!(if matches!(mode, Mode::ConfirmedTool) {
                        "tool_calls"
                    } else {
                        "stop"
                    });
                }
                let chunk = serde_json::json!({"model":"fixture","choices":[choice],"usage":{"prompt_tokens":11,"completion_tokens":7}});
                // Clean HTTP EOF, complete JSON arguments, but no terminal SSE
                // evidence in the negative case. The positive case deliberately
                // has finish_reason without [DONE], which is sufficient.
                if matches!(mode, Mode::WrongBody) {
                    ("application/json", chunk.to_string())
                } else {
                    ("text/event-stream", format!("data: {chunk}\n\n"))
                }
            } else if streaming {
                let chunk = serde_json::json!({"model":"fixture","choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}});
                ("text/event-stream", format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            } else {
                let repair = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("Repair only the JSON representation");
                let confirmed = !matches!(
                    (mode, repair),
                    (Mode::UnconfirmedClassifier, false) | (Mode::UnconfirmedRepair, true)
                );
                let reply = serde_json::json!({"model":"fixture","choices":[{"message":{"role":"assistant","content":if repair { "{\"ok\":true}" } else { "{\"action\":\"finish\"}" }},"finish_reason":confirmed.then_some("stop")}],"usage":{"prompt_tokens":1,"completion_tokens":1}});
                ("application/json", reply.to_string())
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len()).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (format!("http://{address}/v1"), requests)
}

fn engine(home: &std::path::Path, workspace: &std::path::Path) -> Rook {
    let mut config = Config::default();
    config.agent.install_servers = false;
    config.agent.plan_first = false;
    config.agent.one_script = false;
    config.agent.max_steps = 3;
    config.sandbox.stance = rook_tools::policy::Stance::Free;
    Rook::from_parts(
        rook_store::Store::open(home.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare(std::env::consts::OS, std::env::consts::ARCH, "0.1"),
        Default::default(),
        workspace.to_path_buf(),
    )
}

#[tokio::test]
async fn complete_tool_arguments_without_terminal_evidence_cannot_write_a_file() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("unconfirmed generation").unwrap();
    let (url, requests) = endpoint(Mode::UnconfirmedTool).await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "fixture",
        "fixture",
        rook_llm::openai::Config::new(url, None, 32768),
    )
    .unwrap();
    let result =
        AgentLoop::new(&rook, Arc::new(provider), session).run("Write the controlled fixture file.").await;
    assert!(
        !workspace.path().join("must-not-exist.txt").exists(),
        "unconfirmed generation executed write_file; result={result:?}"
    );
    assert!(result.is_err(), "EOF is not a successful model decision: {result:?}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "do not classify or retry an incomplete generation inside this turn"
    );
    let metadata = rook.store.get_session(session).unwrap().unwrap();
    assert_eq!(
        (metadata.tokens_in, metadata.tokens_out),
        (11, 7),
        "observed usage survives refusal before effects"
    );
    let history = rook.transcript(session, 0, 100, 4096).unwrap();
    let route: serde_json::Value =
        serde_json::from_str(&history.iter().find(|e| e.label == "rook:model-route:v1").unwrap().body)
            .unwrap();
    assert_eq!(route["complete"], false);
    assert_eq!(route["usage_reported"], true);
    let attempt: serde_json::Value = serde_json::from_str(
        &history.iter().rev().find(|e| e.label == "rook:model-attempt:v1").unwrap().body,
    )
    .unwrap();
    assert_eq!(attempt["state"], "incomplete");
    assert_eq!(attempt["completion_confirmed"], false);
}

#[tokio::test]
async fn an_unconfirmed_signed_assistant_envelope_cannot_survive_for_replay() {
    struct SignedIncomplete;
    #[async_trait::async_trait]
    impl rook_llm::Provider for SignedIncomplete {
        fn id(&self) -> &str {
            "signed-incomplete"
        }
        fn context_window(&self) -> usize {
            32768
        }
        async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            panic!("an incomplete stream must not request classification")
        }
        async fn stream(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::ResponseStream> {
            use rook_llm::{Delta, StopReason, ToolCall};
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(Delta::Text("Retained partial text.".into())),
                Ok(Delta::Reasoning("Retained partial reasoning.".into())),
                Ok(Delta::ReasoningDone(
                    serde_json::json!({"type":"thinking", "thinking":"partial", "signature":"INCOMPLETE_SIGNATURE"}),
                )),
                Ok(Delta::ToolCall(ToolCall {
                    id: "pending-call".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path":"must-not-exist.txt","content":"controlled effect"}),
                })),
                Ok(Delta::ResponseMetadata { usage_reported: true, completion_confirmed: false }),
                Ok(Delta::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: rook_llm::Usage { input_tokens: 11, output_tokens: 7, ..Default::default() },
                    model: "signed".into(),
                }),
            ])))
        }
    }
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("incomplete provider envelope").unwrap();
    let result = AgentLoop::new(&rook, Arc::new(SignedIncomplete), session).run("Write the fixture.").await;
    assert!(result.is_err());
    assert!(!workspace.path().join("must-not-exist.txt").exists());
    let history = rook.transcript(session, 0, 100, 4096).unwrap();
    assert!(!history.iter().any(|e| e.label == "rook:assistant-state:v1" || e.label == "rook:call-state:v1"));
    assert!(!history.iter().any(|e| e.body.contains("INCOMPLETE_SIGNATURE")));
    assert!(history.iter().any(|e| e.body == "Retained partial text."));
    assert!(history.iter().any(|e| e.body == "Retained partial reasoning."));
    let metadata = rook.store.get_session(session).unwrap().unwrap();
    assert_eq!((metadata.tokens_in, metadata.tokens_out), (11, 7));
}

#[tokio::test]
async fn an_explicit_finish_reason_still_admits_the_file_write_without_done_sentinel() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("confirmed generation").unwrap();
    let (url, _) = endpoint(Mode::ConfirmedTool).await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "fixture",
        "fixture",
        rook_llm::openai::Config::new(url, None, 32768),
    )
    .unwrap();
    let result = AgentLoop::new(&rook, Arc::new(provider), session)
        .run("Write the controlled fixture file.")
        .await
        .unwrap();
    assert_eq!(std::fs::read(workspace.path().join("must-not-exist.txt")).unwrap(), b"controlled effect");
    assert_eq!(result.tools_called, ["write_file"]);
}

#[tokio::test]
async fn unconfirmed_text_is_retained_but_cannot_complete_the_task() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("partial text").unwrap();
    let (url, requests) = endpoint(Mode::UnconfirmedText).await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "fixture",
        "fixture",
        rook_llm::openai::Config::new(url, None, 32768),
    )
    .unwrap();
    let result =
        AgentLoop::new(&rook, Arc::new(provider), session).run("Answer the controlled question.").await;
    assert!(result.is_err(), "unconfirmed text cannot become a successful end_turn: {result:?}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let events = rook.store.events(session, 0, 100).unwrap();
    assert!(events.iter().any(|event| event.record.kind == rook_store::EventKind::AssistantMessage
        && rook.store.get(&event.record.body).unwrap() == b"Partial assistant text."));
}

#[tokio::test]
async fn a_non_sse_success_body_is_not_a_confirmed_stream() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("wrong stream body").unwrap();
    let (url, requests) = endpoint(Mode::WrongBody).await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "fixture",
        "fixture",
        rook_llm::openai::Config::new(url, None, 32768),
    )
    .unwrap();
    let result =
        AgentLoop::new(&rook, Arc::new(provider), session).run("Answer the controlled question.").await;
    assert!(result.is_err(), "HTTP 200 without a recognized terminal frame is not success: {result:?}");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_parseable_unconfirmed_classifier_cannot_finish_the_task() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("unconfirmed classifier").unwrap();
    let (url, requests) = endpoint(Mode::UnconfirmedClassifier).await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "fixture",
        "fixture",
        rook_llm::openai::Config::new(url, None, 32768),
    )
    .unwrap();
    let result = AgentLoop::new(&rook, Arc::new(provider), session)
        .run("Answer the controlled question.")
        .await
        .unwrap();
    assert_eq!(result.stopped, "completion_unchecked", "parseable finish JSON is not terminal confirmation");
    assert_eq!(result.reply, "Partial assistant text.");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        2,
        "only retry classification later, never rerun working turn here"
    );
}

#[tokio::test]
async fn a_parseable_unconfirmed_output_repair_cannot_replace_an_artifact() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("result.json"), b"original artifact").unwrap();
    let rook = engine(home.path(), workspace.path());
    let session = rook.start_session("unconfirmed output repair").unwrap();
    let (url, requests) = endpoint(Mode::UnconfirmedRepair).await;
    let provider = rook_llm::openai::OpenAiCompatible::new(
        "fixture",
        "fixture",
        rook_llm::openai::Config::new(url, None, 32768),
    )
    .unwrap();
    let mut agent = AgentLoop::new(&rook, Arc::new(provider), session);
    agent.options.output = Some("result.json".into());
    agent.options.output_schema =
        Some(serde_json::json!({"type":"object","required":["ok"],"properties":{"ok":{"type":"boolean"}}}));
    agent.options.schema_retries = 1;
    let result = agent.run("Return the controlled JSON result.").await;
    assert_eq!(
        std::fs::read(workspace.path().join("result.json")).unwrap(),
        b"original artifact",
        "unconfirmed repair changed the requested output"
    );
    assert!(result.is_err());
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3,
        "the unconfirmed repair actually reached the native endpoint"
    );
}

#[tokio::test]
async fn unconfirmed_textual_or_reasoning_calls_are_never_promoted_to_effects() {
    for mode in [Mode::PromptedTool, Mode::ReasoningTool] {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rook = engine(home.path(), workspace.path());
        let session = rook.start_session("unconfirmed prompted call").unwrap();
        let (url, requests) = endpoint(mode).await;
        let provider = rook_llm::openai::OpenAiCompatible::new(
            "fixture",
            "fixture",
            rook_llm::openai::Config::new(url, None, 32768),
        )
        .unwrap();
        let result = AgentLoop::new(&rook, Arc::new(provider), session)
            .run("Write the controlled fixture file.")
            .await;
        assert!(!workspace.path().join("must-not-exist.txt").exists());
        assert!(result.is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }
}
