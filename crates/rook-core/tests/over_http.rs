//! A whole turn over a real socket, against a server that speaks the
//! OpenAI dialect and checks what it was sent.
//!
//! Every other loop test hands the agent a scripted provider, which skips the
//! wire entirely: a request the agent builds wrongly — a mis-shaped tool schema,
//! an unpaired tool_call_id, a role a server rejects — would pass all of them
//! and fail against the first real model.

use std::sync::{Arc, Mutex};

use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The bodies the server was sent, in order.
type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// Answers each request from `replies` in turn, recording what it was asked.
async fn serve(replies: Vec<serde_json::Value>) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Default::default();
    let recorded = seen.clone();

    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut raw = Vec::new();
            let mut scratch = [0u8; 16384];
            // Read until the body is complete, which for these requests is one
            // read — but a large tool schema can split across frames.
            loop {
                let Ok(n) = socket.read(&mut scratch).await else { return };
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&scratch[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .or_else(|| head.lines().find_map(|l| l.strip_prefix("Content-Length: ")))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if body.len() >= length {
                        recorded.lock().unwrap().push(serde_json::from_str(body).unwrap());
                        break;
                    }
                }
            }
            // The loop streams, so the answer goes back as server-sent events —
            // the same shape a real provider uses.
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                .await;
            let _ = socket.write_all(format!("data: {reply}\n\ndata: [DONE]\n\n").as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn answer(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "1",
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": content }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
    })
}

fn tool_call(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "1",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": name, "arguments": arguments.to_string() }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
    })
}

struct Fixture {
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    rook: Rook,
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };
    let store = Store::open(home.path().join("store")).unwrap();
    let mut config = Config::default();
    config.agent.lazy_tools = true;
    let (skills, _) = SkillIndex::discover(&[]);
    let rook = Rook::from_parts(
        store,
        config,
        Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        workspace.path().to_path_buf(),
    );
    Fixture { _home: home, workspace, rook }
}

fn provider(base: &str) -> Arc<dyn rook_llm::Provider> {
    let config = rook_llm::openai::Config::new(base.to_string(), None, 32_768);
    Arc::new(rook_llm::openai::OpenAiCompatible::new("test/model", "model", config).unwrap())
}

#[tokio::test]
async fn a_turn_with_a_tool_call_round_trips_over_http() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("notes.txt"), "the answer is 42\n").unwrap();

    let (base, seen) = serve(vec![
        tool_call("read_file", serde_json::json!({"path": "notes.txt"})),
        answer("the answer is 42"),
    ])
    .await;

    let session = f.rook.start_session("http").unwrap();
    let outcome =
        AgentLoop::new(&f.rook, provider(&base), session).run("what is in notes.txt?").await.unwrap();

    assert_eq!(outcome.reply, "the answer is 42");
    assert_eq!(outcome.tools_called, ["read_file"]);

    let sent = seen.lock().unwrap();
    assert_eq!(sent.len(), 2, "one request to ask, one to report the result");

    // What the first request has to carry for any real server to accept it.
    let first = &sent[0];
    assert_eq!(first["model"], "model");
    assert_eq!(first["messages"][0]["role"], "system");
    assert_eq!(first["messages"].as_array().unwrap().last().unwrap()["role"], "user");
    let tools = first["tools"].as_array().unwrap();
    let read = tools.iter().find(|t| t["function"]["name"] == "read_file").unwrap();
    assert_eq!(read["type"], "function");
    assert!(read["function"]["parameters"]["properties"]["path"].is_object(), "{read}");

    // And what the second must carry so the server can pair the result up.
    let second = &sent[1];
    let messages = second["messages"].as_array().unwrap();
    let call = messages.iter().find(|m| m["role"] == "assistant" && !m["tool_calls"].is_null()).unwrap();
    assert_eq!(call["tool_calls"][0]["id"], "call_1");
    let result = messages.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(result["tool_call_id"], "call_1", "an unpaired id is what a real server rejects");
    assert!(result["content"].as_str().unwrap().contains("42"), "{result}");
}

#[tokio::test]
async fn a_plain_answer_needs_only_one_request() {
    let f = fixture();
    let (base, seen) = serve(vec![answer("nothing to do")]).await;

    let session = f.rook.start_session("http").unwrap();
    let outcome = AgentLoop::new(&f.rook, provider(&base), session).run("say hello").await.unwrap();

    assert_eq!(outcome.reply, "nothing to do");
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(outcome.input_tokens, 10, "usage is read back off the wire");
    assert_eq!(outcome.output_tokens, 5);
}

#[tokio::test]
async fn a_provider_that_answers_with_an_error_ends_the_turn_with_the_reason() {
    let f = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = [0u8; 8192];
        let _ = socket.read(&mut scratch).await;
        let body = r#"{"error":{"message":"model not found","type":"invalid_request_error"}}"#;
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
    });

    let session = f.rook.start_session("http").unwrap();
    let err = AgentLoop::new(&f.rook, provider(&format!("http://{addr}/v1")), session)
        .run("anything")
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("404"), "the status a real server sent must survive: {err}");
    assert!(err.contains("model not found"), "and so must its reason: {err}");
}

#[tokio::test]
async fn the_prompt_is_logged_before_the_provider_is_called() {
    let f = fixture();
    // Nothing is listening: the turn fails, and the prompt must still be in the
    // log, or a failed first turn loses what was asked.
    let session = f.rook.start_session("http").unwrap();
    let _ = AgentLoop::new(&f.rook, provider("http://127.0.0.1:1/v1"), session)
        .run("remember this question")
        .await;

    let events = f.rook.transcript(session, 0, 10, 200).unwrap();
    assert!(
        events.iter().any(|e| e.body.contains("remember this question")),
        "{:?}",
        events.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_session_killed_mid_tool_call_can_still_be_resumed() {
    let f = fixture();
    let session = f.rook.start_session("crashed").unwrap();
    f.rook.log(session, rook_store::EventKind::UserMessage, "prompt", "do it").unwrap();
    // What a process killed between logging a call and logging its result
    // leaves behind. Every provider refuses a request where an assistant asked
    // for a tool and nothing answered.
    f.rook.log(session, rook_store::EventKind::ToolCall, "read_file", r#"{"path":"a.txt"}"#).unwrap();

    let (base, seen) = serve(vec![answer("carrying on")]).await;
    let outcome = AgentLoop::new(&f.rook, provider(&base), session).run("and again").await.unwrap();

    assert_eq!(outcome.reply, "carrying on");

    let sent = seen.lock().unwrap();
    let messages = sent[0]["messages"].as_array().unwrap();
    let asked = messages.iter().position(|m| !m["tool_calls"].is_null()).unwrap();
    let answered = messages
        .iter()
        .position(|m| m["role"] == "tool" && m["tool_call_id"] == messages[asked]["tool_calls"][0]["id"]);

    assert!(
        answered.is_some_and(|i| i == asked + 1),
        "the call must be answered, and immediately: {:?}",
        sent[0]["messages"]
    );
    assert!(
        messages[answered.unwrap()]["content"].as_str().unwrap().contains("did not finish"),
        "and say what happened rather than leave a blank"
    );
}

#[tokio::test]
async fn several_unanswered_calls_are_each_answered_in_place() {
    let f = fixture();
    let session = f.rook.start_session("crashed twice").unwrap();
    for i in 0..2 {
        f.rook.log(session, rook_store::EventKind::ToolCall, "read_file", &format!("{{\"n\":{i}}}")).unwrap();
    }
    f.rook.log(session, rook_store::EventKind::UserMessage, "prompt", "carry on").unwrap();

    let (base, seen) = serve(vec![answer("fine")]).await;
    AgentLoop::new(&f.rook, provider(&base), session).run("again").await.unwrap();

    let sent = seen.lock().unwrap();
    let messages = sent[0]["messages"].as_array().unwrap();
    let calls = messages.iter().filter(|m| !m["tool_calls"].is_null()).count();
    let results = messages.iter().filter(|m| m["role"] == "tool").count();

    assert_eq!(calls, 2, "{:?}", sent[0]["messages"]);
    assert_eq!(results, 2, "one answer each, not one for the pair: {:?}", sent[0]["messages"]);
}
