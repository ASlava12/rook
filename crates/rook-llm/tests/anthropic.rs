//! The Anthropic dialect, against a socket that records what was sent.
//!
//! The differences from the OpenAI shape are all in the request body, so a test
//! that only checks the reply would pass while sending something the API
//! rejects.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rook_llm::anthropic::{Anthropic, Config};
use rook_llm::{Delta, LlmError, Message, Provider, Request, Role, StopReason, ToolCall, ToolSpec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one request, records its body, and replies with `body`.
async fn serve(
    status: &'static str,
    content_type: &'static str,
    body: &'static str,
) -> (String, Arc<Mutex<Option<serde_json::Value>>>) {
    let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let recorder = seen.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut scratch = [0u8; 8192];
        loop {
            let n = match socket.read(&mut scratch).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            raw.extend_from_slice(&scratch[..n]);
            let text = String::from_utf8_lossy(&raw).to_string();
            let Some(split) = text.find("\r\n\r\n") else { continue };
            let length: usize = text
                .lines()
                .find_map(|l| {
                    l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            if raw.len() < split + 4 + length {
                continue;
            }
            *recorder.lock().unwrap() = serde_json::from_str(&text[split + 4..]).ok();
            break;
        }
        let response = if content_type == "text/event-stream" {
            format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n{body}")
        } else {
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });

    (format!("http://{addr}"), seen)
}

fn provider(url: String) -> Anthropic {
    Anthropic::new("anthropic/claude-opus-5", "claude-opus-5", Config::new(url, "k".into(), "claude-opus-5"))
        .unwrap()
}

const DONE: &str = r#"{"id":"msg_1","model":"claude-opus-5","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":9,"output_tokens":2}}"#;

/// The hour is the better deal exactly when a conversation outlives five
/// minutes, and it is not the default because a scripted single turn never
/// reads the cache it wrote.
#[tokio::test]
async fn a_longer_cache_lifetime_is_asked_for_only_when_it_is_asked_for() {
    let cached = |ttl| async move {
        let (url, seen) = serve("200 OK", "application/json", DONE).await;
        let mut request = Request::new(vec![Message::system("be terse").cacheable(), Message::user("hello")]);
        request.cache_ttl = ttl;
        provider(url).complete(request).await.unwrap();
        seen.lock().unwrap().clone().unwrap()["system"][0]["cache_control"].clone()
    };

    assert_eq!(cached(rook_llm::CacheTtl::FiveMinutes).await["type"], "ephemeral");
    // The default is unnamed on the wire: a model that does not offer the choice
    // rejects the request over an unknown field.
    assert!(cached(rook_llm::CacheTtl::FiveMinutes).await.get("ttl").is_none());
    assert_eq!(cached(rook_llm::CacheTtl::OneHour).await["ttl"], "1h");
}

#[tokio::test]
async fn the_system_prompt_is_lifted_out_of_the_message_list() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let request = Request::new(vec![
        Message::system("be terse"),
        Message::system("and precise"),
        Message::user("hello"),
    ]);
    provider(url).complete(request).await.unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["system"][0]["text"], "be terse\n\nand precise", "both system turns must be merged");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "no system message may remain in the list");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn tool_results_are_blocks_in_one_user_message() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let assistant = Message {
        role: Role::Assistant,
        content: "checking".into(),
        tool_calls: vec![
            ToolCall { id: "a".into(), name: "one".into(), arguments: serde_json::json!({}) },
            ToolCall { id: "b".into(), name: "two".into(), arguments: serde_json::json!({}) },
        ],
        tool_call_id: None,
        cache: false,
        reasoning: Vec::new(),
    };
    let request = Request::new(vec![
        Message::user("do both"),
        assistant,
        Message::tool_result("a", "first"),
        Message::tool_result("b", "second"),
    ]);
    provider(url).complete(request).await.unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "the two results must share one user message: {messages:#?}");

    let blocks = messages[1]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[1]["type"], "tool_use", "a call is a content block, not a parallel array");
    assert_eq!(blocks[1]["id"], "a");

    let results = messages[2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2, "splitting results teaches the model to stop calling in parallel");
    assert_eq!(results[0]["type"], "tool_result");
    assert_eq!(results[0]["tool_use_id"], "a");
    assert_eq!(results[1]["tool_use_id"], "b");
}

#[tokio::test]
async fn a_tool_is_advertised_with_input_schema() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let mut request = Request::new(vec![Message::user("hi")]);
    request.tools = vec![ToolSpec {
        name: "read_file".into(),
        description: "Read a file.".into(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
    }];
    provider(url).complete(request).await.unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    let tool = &body["tools"][0];
    assert_eq!(tool["name"], "read_file");
    assert!(tool["input_schema"].is_object(), "the field is input_schema, not parameters");
    assert!(tool["parameters"].is_null());
    assert!(body["max_tokens"].is_number(), "max_tokens is required");
}

#[tokio::test]
async fn an_assistant_turn_with_nothing_in_it_is_dropped() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let request =
        Request::new(vec![Message::user("hi"), Message::assistant("   "), Message::user("still there?")]);
    provider(url).complete(request).await.unwrap();

    let messages = seen.lock().unwrap().clone().unwrap()["messages"].as_array().unwrap().len();
    assert_eq!(messages, 2, "an empty assistant block is rejected by the API");
}

#[tokio::test]
async fn a_completed_response_carries_text_tool_calls_and_usage() {
    let body = r#"{"id":"m","model":"claude-opus-5","content":[
        {"type":"text","text":"looking"},
        {"type":"tool_use","id":"toolu_1","name":"read_file","input":{"path":"a.txt"}}
    ],"stop_reason":"tool_use","usage":{"input_tokens":11,"output_tokens":4}}"#;
    let (url, _) = serve("200 OK", "application/json", Box::leak(body.to_string().into_boxed_str())).await;

    let response = provider(url).complete(Request::new(vec![Message::user("go")])).await.unwrap();
    assert_eq!(response.message.content, "looking");
    assert_eq!(response.message.tool_calls[0].name, "read_file");
    assert_eq!(response.message.tool_calls[0].arguments["path"], "a.txt");
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.usage.input_tokens, 11);
}

#[tokio::test]
async fn a_stream_assembles_text_thinking_and_a_tool_call() {
    let events = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5\",\"usage\":{\"input_tokens\":20}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"weighing it\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Rea\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ding.\"}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pa\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"th\\\":\\\"a.txt\\\"}\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (url, _) = serve("200 OK", "text/event-stream", events).await;

    let mut stream = provider(url).stream(Request::new(vec![Message::user("go")])).await.unwrap();
    let (mut text, mut thinking) = (String::new(), String::new());
    let mut calls = Vec::new();
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut done = None;
    while let Some(delta) = stream.next().await {
        match delta.unwrap() {
            Delta::Text(t) => text.push_str(&t),
            Delta::Reasoning(t) => thinking.push_str(&t),
            Delta::ToolCall(c) => calls.push(c),
            Delta::Done { stop_reason, usage, model } => done = Some((stop_reason, usage, model)),
            Delta::ReasoningDone(block) => blocks.push(block),
        }
    }

    assert_eq!(text, "Reading.");
    assert_eq!(thinking, "weighing it", "thinking is reported apart from the answer");
    assert_eq!(calls.len(), 1, "the fragments must assemble into one call");
    assert_eq!(calls[0].id, "toolu_9");
    assert_eq!(calls[0].arguments["path"], "a.txt");

    let (stop, usage, model) = done.unwrap();
    assert_eq!(stop, StopReason::ToolUse);
    assert_eq!(usage.input_tokens, 20, "usage arrives split across message_start and message_delta");
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(model, "claude-opus-5");
}

#[tokio::test]
async fn an_error_event_mid_stream_surfaces_rather_than_ending_quietly() {
    let events = concat!(
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    );
    let (url, _) = serve("200 OK", "text/event-stream", events).await;

    let mut stream = provider(url).stream(Request::new(vec![Message::user("go")])).await.unwrap();
    let mut failure = None;
    while let Some(delta) = stream.next().await {
        if let Err(e) = delta {
            failure = Some(e);
            break;
        }
    }
    assert!(failure.is_some(), "an error event must not look like the end of the stream");
    assert!(failure.unwrap().to_string().contains("Overloaded"));
}

#[tokio::test]
async fn an_http_error_carries_the_status_and_the_body() {
    let (url, _) = serve("400 Bad Request", "application/json", r#"{"error":{"message":"bad model"}}"#).await;
    let Err(err) = provider(url).complete(Request::new(vec![Message::user("hi")])).await else {
        panic!("a 400 must not look like a reply")
    };
    match err {
        LlmError::Status { status, body, .. } => {
            assert_eq!(status, 400);
            assert!(body.contains("bad model"), "{body}");
        }
        other => panic!("expected a status error, got {other:?}"),
    }
}

#[tokio::test]
async fn models_are_listed_with_their_context_window() {
    let body =
        r#"{"data":[{"id":"claude-opus-5","display_name":"Claude Opus 5","max_input_tokens":1000000}]}"#;
    let (url, _) = serve("200 OK", "application/json", body).await;
    let models = provider(url).models().await.unwrap();
    assert_eq!(models[0].id, "claude-opus-5");
    assert_eq!(
        models[0].context_window,
        Some(1_000_000),
        "the field is max_input_tokens; there is no context_window"
    );
}

#[test]
fn an_unknown_model_gets_the_smaller_window_rather_than_an_optimistic_guess() {
    let window = |model: &str| {
        Anthropic::new("x", model, Config::new("http://x".into(), "k".into(), model))
            .unwrap()
            .context_window()
    };
    assert_eq!(window("claude-opus-5"), 1_000_000);
    assert_eq!(window("claude-sonnet-5"), 1_000_000);
    assert_eq!(window("claude-haiku-4-5"), 200_000);
    assert_eq!(
        window("something-unreleased"),
        200_000,
        "budgeting against a window the model lacks fails the request; budgeting low only wastes it"
    );
}

#[tokio::test]
async fn a_stalled_stream_gives_up() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = [0u8; 8192];
        let _ = socket.read(&mut scratch).await;
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
            .await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut config = Config::new(format!("http://{addr}"), "k".into(), "claude-opus-5");
    config.stream_idle_timeout = Duration::from_millis(150);
    let provider = Anthropic::new("x", "claude-opus-5", config).unwrap();

    let mut stream = provider.stream(Request::new(vec![Message::user("go")])).await.unwrap();
    let mut failure = None;
    while let Some(delta) = stream.next().await {
        if let Err(e) = delta {
            failure = Some(e);
            break;
        }
    }
    assert!(matches!(failure, Some(LlmError::Stalled { .. })), "{failure:?}");
}

#[tokio::test]
async fn a_breakpoint_on_the_system_block_caches_the_tools_with_it() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let mut request =
        Request::new(vec![Message::system("a large stable preamble").cacheable(), Message::user("hello")]);
    request.tools = vec![ToolSpec {
        name: "t".into(),
        description: "d".into(),
        parameters: serde_json::json!({ "type": "object" }),
    }];
    provider(url).complete(request).await.unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    assert!(body["system"].is_array(), "a breakpoint needs the block form, not a bare string");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert!(
        body["tools"][0]["cache_control"].is_null(),
        "tools render before system, so one marker covers both"
    );
}

#[tokio::test]
async fn an_unmarked_system_prompt_carries_no_breakpoint() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    provider(url).complete(Request::new(vec![Message::system("short"), Message::user("hi")])).await.unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    assert!(
        body["system"][0]["cache_control"].is_null(),
        "marking a prefix too small to cache only pays the write premium"
    );
}

#[tokio::test]
async fn a_marked_conversation_turn_carries_the_breakpoint_on_its_last_block() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let assistant = Message {
        role: Role::Assistant,
        content: "done".into(),
        tool_calls: vec![ToolCall { id: "a".into(), name: "t".into(), arguments: serde_json::json!({}) }],
        tool_call_id: None,
        cache: true,
        reasoning: Vec::new(),
    };
    provider(url)
        .complete(Request::new(vec![Message::user("go"), assistant, Message::user("next")]))
        .await
        .unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    let blocks = body["messages"][1]["content"].as_array().unwrap();
    assert!(blocks[0]["cache_control"].is_null());
    assert_eq!(
        blocks.last().unwrap()["cache_control"]["type"],
        "ephemeral",
        "the breakpoint belongs on the last block of the marked turn"
    );
}

#[tokio::test]
async fn cache_hits_are_reported_back() {
    let body = r#"{"id":"m","model":"claude-opus-5","content":[{"type":"text","text":"hi"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":12,"output_tokens":3,"cache_read_input_tokens":9000,"cache_creation_input_tokens":40}}"#;
    let (url, _) = serve("200 OK", "application/json", Box::leak(body.to_string().into_boxed_str())).await;

    let usage = provider(url).complete(Request::new(vec![Message::user("hi")])).await.unwrap().usage;
    assert_eq!(usage.cache_read_tokens, 9000, "without this there is no way to tell caching works");
    assert_eq!(usage.cache_write_tokens, 40);
}

#[tokio::test]
async fn a_current_model_gets_adaptive_thinking_and_a_visible_summary() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let mut request = Request::new(vec![Message::user("hi")]);
    request.effort = Some(rook_llm::Effort::XHigh);
    provider(url).complete(request).await.unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(
        body["thinking"]["display"], "summarized",
        "the default is omitted, which streams empty thinking blocks"
    );
    assert_eq!(body["output_config"]["effort"], "xhigh");
    assert!(
        body["thinking"]["budget_tokens"].is_null(),
        "budget_tokens is rejected outright on current models"
    );
}

#[tokio::test]
async fn a_model_outside_the_documented_families_is_sent_neither() {
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let mut request = Request::new(vec![Message::user("hi")]);
    request.effort = Some(rook_llm::Effort::Max);
    Anthropic::new("x", "claude-haiku-4-5", Config::new(url, "k".into(), "claude-haiku-4-5"))
        .unwrap()
        .complete(request)
        .await
        .unwrap();

    let body = seen.lock().unwrap().clone().unwrap();
    assert!(
        body["thinking"].is_null() && body["output_config"].is_null(),
        "guessing wrong here fails every request rather than degrading: {body}"
    );
}

#[test]
fn an_unreadable_effort_setting_falls_back_rather_than_failing_a_turn() {
    assert_eq!(rook_llm::Effort::parse("xhigh"), Some(rook_llm::Effort::XHigh));
    assert_eq!(rook_llm::Effort::parse("  MAX "), Some(rook_llm::Effort::Max));
    assert_eq!(rook_llm::Effort::parse("very"), None);
    assert_eq!(rook_llm::Effort::default().as_str(), "high");
}

/// The round trip this dialect refuses without: a turn thinks, calls a tool,
/// and continues. The thinking block has to come back beside the call it led
/// to, whole and signed and before it — the API answers a request that dropped
/// it with a 400, and every turn with a tool call has one.
#[tokio::test]
async fn thinking_comes_back_beside_the_call_it_led_to() {
    let events = concat!(
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"the file\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" first\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqQBCgIYAh\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (url, _) = serve("200 OK", "text/event-stream", events).await;

    let mut stream =
        provider(url).stream(Request::new(vec![Message::user("what is in a.txt?")])).await.unwrap();
    let mut assembler = rook_llm::Assembler::default();
    while let Some(delta) = stream.next().await {
        assembler.push(delta.unwrap()).unwrap();
    }
    let thought = assembler.finish().message;

    assert_eq!(thought.reasoning.len(), 1, "the block is kept: {:?}", thought.reasoning);
    assert_eq!(thought.reasoning[0]["type"], "thinking");
    assert_eq!(thought.reasoning[0]["thinking"], "the file first", "assembled from its deltas");
    assert_eq!(thought.reasoning[0]["signature"], "EqQBCgIYAh", "and signed, or it is refused back");

    // The turn goes on: the assistant message and the tool's answer.
    let (url, seen) = serve("200 OK", "application/json", DONE).await;
    let answer = Message {
        role: Role::Tool,
        content: "hello".into(),
        tool_calls: vec![],
        tool_call_id: Some("toolu_1".into()),
        cache: false,
        reasoning: Vec::new(),
    };
    provider(url)
        .complete(Request::new(vec![Message::user("what is in a.txt?"), thought, answer]))
        .await
        .unwrap();

    let sent = seen.lock().unwrap().clone().unwrap();
    let assistant = sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("the assistant turn is replayed");
    let blocks = assistant["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking", "first, as the API orders them: {blocks:?}");
    assert_eq!(blocks[0]["signature"], "EqQBCgIYAh", "verbatim, signature and all");
    assert!(
        blocks.iter().any(|b| b["type"] == "tool_use"),
        "and still beside the call it led to: {blocks:?}"
    );
}

/// A redacted block is opaque and goes back as it came: reconstructing one is
/// not possible and not needed.
#[tokio::test]
async fn a_redacted_thinking_block_is_carried_whole() {
    let events = concat!(
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"EroBCkYIA\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (url, _) = serve("200 OK", "text/event-stream", events).await;

    let mut stream = provider(url).stream(Request::new(vec![Message::user("go")])).await.unwrap();
    let mut assembler = rook_llm::Assembler::default();
    while let Some(delta) = stream.next().await {
        assembler.push(delta.unwrap()).unwrap();
    }
    let message = assembler.finish().message;

    assert_eq!(message.reasoning.len(), 1, "{:?}", message.reasoning);
    assert_eq!(message.reasoning[0]["type"], "redacted_thinking");
    assert_eq!(message.reasoning[0]["data"], "EroBCkYIA");
}

/// A block the stream never signed is one it did not finish, and sending it
/// back is a refused request. Dropped rather than guessed at.
#[tokio::test]
async fn an_unsigned_thinking_block_is_not_carried() {
    let events = concat!(
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"cut off\"}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (url, _) = serve("200 OK", "text/event-stream", events).await;

    let mut stream = provider(url).stream(Request::new(vec![Message::user("go")])).await.unwrap();
    let mut assembler = rook_llm::Assembler::default();
    while let Some(delta) = stream.next().await {
        assembler.push(delta.unwrap()).unwrap();
    }
    let shown = assembler.reasoning().to_string();
    let message = assembler.finish().message;

    assert!(message.reasoning.is_empty(), "nothing to send back: {:?}", message.reasoning);
    assert_eq!(shown, "cut off", "and the person still saw what there was");
}

/// The non-streaming path answers with the same blocks, so a caller that does
/// not stream is not the one that gets refused.
#[tokio::test]
async fn a_whole_response_carries_its_thinking_too() {
    const THOUGHT: &str = r#"{"model":"claude-opus-5","stop_reason":"tool_use","content":[
        {"type":"thinking","thinking":"weighing it","signature":"sig-1"},
        {"type":"text","text":"reading"},
        {"type":"tool_use","id":"toolu_2","name":"read_file","input":{"path":"a.txt"}}],
        "usage":{"input_tokens":5,"output_tokens":6}}"#;
    let (url, _) = serve("200 OK", "application/json", THOUGHT).await;

    let response = provider(url).complete(Request::new(vec![Message::user("go")])).await.unwrap();

    assert_eq!(response.message.content, "reading");
    assert_eq!(response.message.tool_calls[0].id, "toolu_2");
    assert_eq!(response.message.tool_calls[0].arguments["path"], "a.txt");
    assert_eq!(response.message.reasoning.len(), 1, "{:?}", response.message.reasoning);
    assert_eq!(response.message.reasoning[0]["signature"], "sig-1");
}
