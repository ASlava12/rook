//! Responses over real HTTP, including stateless tool/reasoning replay.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rook_llm::openai::{Config, responses::Responses};
use rook_llm::{Assembler, Delta, Effort, EffortUse, Message, Provider, Request, StopReason, ToolSpec};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Reply {
    status: u16,
    body: String,
    stream: bool,
    fragment: usize,
}
fn reply(value: Value) -> Reply {
    Reply { status: 200, body: value.to_string(), stream: false, fragment: usize::MAX }
}
fn answer(output: Value) -> Value {
    json!({"id":"resp_1", "object":"response", "status":"completed", "model":"gpt-6-astra-20260901",
        "output":output, "usage":{"input_tokens":321,"output_tokens":54,"input_tokens_details":{"cached_tokens":123,"cache_write_tokens":45}}})
}
fn text_item(text: &str) -> Value {
    json!({"type":"message", "id":"msg_1", "status":"completed", "role":"assistant",
        "content":[{"type":"output_text", "text":text, "annotations":[]}]})
}
fn tool(id: &str, name: &str, args: &str) -> Value {
    json!({"type":"function_call", "id":format!("fc_{id}"), "call_id":id,
        "name":name, "arguments":args,"status":"completed"})
}
fn reasoning() -> Value {
    json!({"type":"reasoning", "id":"rs_1", "summary":[{"type":"summary_text","text":"Проверю."}],
        "encrypted_content":"opaque-signed-state", "future_field":{"keep":true}})
}
fn frame(event: Value) -> String {
    format!("data: {event}\r\n\r\n")
}

struct Seen {
    head: String,
    body: Value,
    at: std::time::Instant,
}
async fn serve(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            loop {
                let mut buf = [0; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0 && raw.len() + n < 4 * 1024 * 1024);
                raw.extend_from_slice(&buf[..n]);
                let Some(split) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&raw[..split]).to_string();
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                if raw.len() < split + 4 + length {
                    continue;
                }
                let body = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&raw[split + 4..split + 4 + length]).unwrap()
                };
                captured.lock().unwrap().push(Seen { head, body, at: std::time::Instant::now() });
                break;
            }
            let mime = if reply.stream { "text/event-stream" } else { "application/json" };
            let header = format!(
                "HTTP/1.1 {} Reply\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.status,
                reply.body.len()
            );
            if socket.write_all(header.as_bytes()).await.is_err() {
                continue;
            }
            for chunk in reply.body.as_bytes().chunks(reply.fragment) {
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
                if reply.fragment < 100 {
                    tokio::task::yield_now().await;
                }
            }
        }
    });
    (format!("http://{addr}/prefix/v1"), seen)
}
fn provider(url: String) -> Responses {
    let mut config = Config::new(url, Some("private-key".into()), 128_000);
    config.stream_idle_timeout = Duration::from_secs(2);
    Responses::new("my-source", "gpt-6-astra", config).unwrap()
}

#[tokio::test]
async fn complete_response_replays_ordered_output_and_preserves_tool_schemas() {
    let output = json!([
        reasoning(),
        text_item("Проверка"),
        tool("call_a", "inspect", r#"{"path":"a"}"#),
        tool("call_b", "inspect", r#"{"path":"b"}"#)
    ]);
    let (url, seen) =
        serve(vec![reply(answer(output.clone())), reply(answer(json!([text_item("Готово")])))]).await;
    let provider = provider(url);
    let schema = json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer"}},"required":["path"]});
    let mut request = Request::new(vec![Message::system("rules"), Message::user("inspect")]);
    request.effort = Some(Effort::Max);
    request.tools = vec![ToolSpec {
        name: "inspect".into(),
        description: "Read a path".into(),
        parameters: schema.clone(),
    }];
    let response = provider.complete(request.clone()).await.unwrap();
    assert_eq!(response.message.content, "Проверка");
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.model, "gpt-6-astra-20260901");
    assert_eq!(
        (
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.cache_read_tokens,
            response.usage.cache_write_tokens
        ),
        (321, 54, 123, 45)
    );
    assert_eq!(response.message.tool_calls[0].id, "call_a");
    // This checks the transport envelope's serialization, not durable core replay.
    let restored: Message = serde_json::from_value(serde_json::to_value(response.message).unwrap()).unwrap();
    request.messages.push(restored);
    request
        .messages
        .extend([Message::tool_result("call_a", "first"), Message::tool_result("call_b", "second")]);
    provider.complete(request).await.unwrap();
    let seen = seen.lock().unwrap();
    let first = &seen[0];
    assert!(first.head.starts_with("POST /prefix/v1/responses "));
    assert!(first.head.to_ascii_lowercase().contains("authorization: bearer private-key"));
    assert_eq!(first.body["store"], false);
    assert_eq!(first.body["reasoning"]["effort"], "max");
    assert_eq!(first.body["tools"][0]["parameters"], schema);
    assert_eq!(first.body["tools"][0]["strict"], false, "optional fields must remain optional");
    assert!(first.body.get("temperature").is_none());
    assert!(first.body.get("messages").is_none());
    assert!(first.body.get("previous_response_id").is_none());
    assert!(first.body.get("max_tokens").is_none());
    assert_eq!(first.body["max_output_tokens"], 4096);
    let input = seen[1].body["input"].as_array().unwrap();
    assert_eq!(&input[2..6], output.as_array().unwrap().as_slice(), "IDs, order and unknown fields survive");
    assert_eq!(input[6]["call_id"], "call_a");
    assert_eq!(input[7]["call_id"], "call_b");
    assert_eq!(input.len(), 8, "replay does not duplicate text or calls");
}

#[tokio::test]
async fn compatible_aliases_preserve_reasoning_but_foreign_or_edited_envelopes_cannot_handoff() {
    let output = json!([reasoning(), text_item("inspect"), tool("call_a", "inspect", r#"{"path":"a"}"#)]);
    let (url, seen) =
        serve(vec![reply(answer(output.clone())), reply(answer(json!([text_item("done")])))]).await;
    let response = provider(url.clone()).complete(Request::new(vec![Message::user("go")])).await.unwrap();
    let config = || Config::new(url.clone(), Some("private-key".into()), 128_000);
    let alias = Responses::new("another-alias", "gpt-6-astra", config()).unwrap();
    assert!(alias.can_replay_reasoning(std::slice::from_ref(&response.message)));
    let mut wrong_role = response.message.clone();
    wrong_role.role = rook_llm::Role::User;
    assert!(!alias.can_replay_reasoning(&[wrong_role]));
    let different_model = Responses::new("different", "gpt-6-sol", config()).unwrap();
    assert!(!different_model.can_replay_reasoning(std::slice::from_ref(&response.message)));
    let foreign = Responses::new(
        "foreign",
        "gpt-6-astra",
        Config::new(format!("{url}/other"), Some("private-key".into()), 128_000),
    )
    .unwrap();
    assert!(!foreign.can_replay_reasoning(std::slice::from_ref(&response.message)));
    let different_key =
        Responses::new("other-key", "gpt-6-astra", Config::new(url, Some("other-key".into()), 128_000))
            .unwrap();
    assert!(!different_key.can_replay_reasoning(std::slice::from_ref(&response.message)));
    let mut edited = response.message.clone();
    edited.tool_calls[0].arguments["path"] = json!("other");
    assert!(!alias.can_replay_reasoning(&[edited]));
    let mut extra = response.message.clone();
    extra.reasoning.push(json!({"type":"thinking","signature":"foreign"}));
    assert!(!alias.can_replay_reasoning(&[extra]), "every opaque block must survive");
    let mut oversized_scope = response.message.clone();
    oversized_scope.reasoning[0]["rook_responses_scope"] = json!(vec![0; 65536]);
    assert!(oversized_scope.reasoning[0]["rook_responses_scope"].as_array().unwrap().len() > 32);
    assert!(!alias.can_replay_reasoning(&[oversized_scope]));
    alias
        .complete(Request::new(vec![
            Message::user("go"),
            response.message,
            Message::tool_result("call_a", "evidence"),
        ]))
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(&seen[1].body["input"].as_array().unwrap()[1..4], output.as_array().unwrap().as_slice());
}

#[tokio::test]
async fn stream_text_summary_and_calls_match_the_terminal_response_without_duplicates() {
    let output = json!([reasoning(), text_item("Привет"), tool("call_a", "inspect", r#"{"path":"a"}"#)]);
    let events = [
        json!({"type":"response.created","response":{"status":"in_progress"}}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"Пров"}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"ерю."}),
        json!({"type":"response.output_text.delta","delta":"При"}),
        json!({"type":"response.output_text.delta","delta":"вет"}),
        json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":"}),
        json!({"type":"response.output_item.done","output_index":2,"item":output[2]}),
        json!({"type":"response.completed","response":answer(output.clone())}),
    ];
    let body = events.into_iter().map(frame).collect::<String>();
    let (url, seen) = serve(vec![Reply { status: 200, body, stream: true, fragment: 1 }]).await;
    let provider = rook_llm::retry::Retrying::new(Box::new(provider(url)));
    let mut request = Request::new(vec![Message::user("hi")]);
    request.effort = Some(Effort::Max);
    let mut stream = provider.stream(request).await.unwrap();
    let mut assembler = Assembler::default();
    let mut reports = Vec::new();
    while let Some(delta) = stream.next().await {
        let delta = delta.unwrap();
        if let Delta::Effort(report) = &delta {
            reports.push(report.clone());
        }
        assembler.push(delta).unwrap();
    }
    assert_eq!(assembler.reasoning(), "Проверю.");
    let response = assembler.finish();
    assert_eq!(response.message.content, "Привет");
    assert_eq!(response.message.tool_calls.len(), 1);
    assert_eq!(response.message.reasoning[0]["rook_responses_output"], output);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].applied, EffortUse::Parameter { name: "reasoning.effort", value: "max".into() });
    assert_eq!(seen.lock().unwrap()[0].body["stream"], true);
}

#[tokio::test]
async fn broken_or_failed_streams_do_not_release_tool_calls_or_success() {
    let done_call = frame(
        json!({"type":"response.output_item.done","item":tool("call_a","inspect","{}"),"output_index":0}),
    );
    let cases = [
        done_call.clone(),
        format!("{done_call}data: [DONE]\n\n"),
        format!(
            "{done_call}{}",
            frame(
                json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"failed"}}})
            )
        ),
        frame(json!({"type":"error","code":"server_error","message":"failed"})),
        "data: {bad json}\n\n".into(),
        frame(json!({"type":"response.completed","response":{"status":"in_progress","output":[]}})),
        frame(json!({"type":"response.completed","response":{"status":"completed"}})),
    ];
    for body in cases {
        let (url, _) = serve(vec![Reply { status: 200, body, stream: true, fragment: usize::MAX }]).await;
        let mut stream = provider(url).stream(Request::new(vec![])).await.unwrap();
        let mut failed = false;
        while let Some(delta) = stream.next().await {
            match delta {
                Err(_) => failed = true,
                Ok(Delta::ToolCall(_) | Delta::Done { .. }) => panic!("incomplete stream acted"),
                _ => {}
            }
        }
        assert!(failed);
    }
}

#[tokio::test]
async fn nested_retry_advice_is_bounded_case_insensitive_and_preserves_terminal_errors() {
    let cases = [
        ("rate_limit_exceeded", json!({"Retry-After":" 2 "}), 429, Some(2)),
        ("slow_down", json!({"rEtRy-AfTeR":"120"}), 429, Some(120)),
        ("server_error", json!({"retry-after":"0"}), 503, Some(0)),
        ("server_is_overloaded", json!({"retry-after":"3"}), 503, Some(3)),
        ("rate_limit_exceeded", json!({"Retry-After":"-1"}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":"121"}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":"18446744073709551616"}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":"1\r\nX-Evil: yes"}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":2}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":["2"]}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":"2","retry-after":"3"}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":"0".repeat(33)}), 429, None),
        ("rate_limit_exceeded", json!({"Retry-After":"Wed, 21 Oct 2026 07:28:00 GMT"}), 429, None),
        ("invalid_api_key", json!({"Retry-After":"2"}), 400, Some(2)),
        ("invalid_request_error", json!({"Retry-After":"2"}), 400, Some(2)),
    ];
    for (code, headers, status, seconds) in cases {
        let value = json!({"status":"failed", "error":{"code":code,"message":"come back in 99 seconds","headers":headers}});
        let body = frame(json!({"type":"response.failed","response":value}));
        let (url, seen) =
            serve(vec![reply(value), Reply { status: 200, body, stream: true, fragment: 7 }]).await;
        let provider = provider(url);
        let complete = provider.complete(Request::new(vec![])).await.unwrap_err();
        let mut stream = provider.stream(Request::new(vec![])).await.unwrap();
        let streamed = stream.next().await.unwrap().unwrap_err();
        for error in [complete, streamed] {
            match error {
                rook_llm::LlmError::Status { status: actual, retry_after, body } => {
                    assert_eq!(actual, status, "{code}");
                    assert_eq!(retry_after, seconds.map(Duration::from_secs), "{code}: {body}");
                }
                other => panic!("lost status for {code}: {other}"),
            }
        }
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}

#[derive(Default)]
struct AttemptLog {
    starts: std::sync::atomic::AtomicUsize,
    ends: Arc<Mutex<Vec<(rook_llm::AttemptStatus, rook_llm::AttemptFacts)>>>,
    ended: Arc<tokio::sync::Notify>,
}
struct AttemptEnd {
    ends: Arc<Mutex<Vec<(rook_llm::AttemptStatus, rook_llm::AttemptFacts)>>>,
    ended: Arc<tokio::sync::Notify>,
}
impl rook_llm::AttemptObserver for AttemptLog {
    fn start(&self, dispatch: Option<&rook_llm::Dispatch>) -> rook_llm::Result<Box<dyn rook_llm::Attempt>> {
        assert_eq!(dispatch.unwrap().provider, "my-source");
        assert!(self.starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 16);
        Ok(Box::new(AttemptEnd { ends: self.ends.clone(), ended: self.ended.clone() }))
    }
}
impl rook_llm::Attempt for AttemptEnd {
    fn finish(
        &mut self,
        status: rook_llm::AttemptStatus,
        facts: &rook_llm::AttemptFacts,
    ) -> rook_llm::Result<()> {
        let mut ends = self.ends.lock().unwrap();
        assert!(ends.len() < 16);
        ends.push((status, facts.clone()));
        self.ended.notify_one();
        Ok(())
    }
}
fn failed_reply(code: &str, seconds: &str, prefix: &str) -> Reply {
    Reply {
        status: 200,
        stream: true,
        fragment: 11,
        body: format!(
            "{prefix}{}",
            frame(json!({"type":"response.failed","response":{"status":"failed","error":{
            "code":code,"message":"try in 99 seconds","headers":{"Retry-After":seconds}}}}))
        ),
    }
}
fn finished_reply() -> Reply {
    Reply {
        status: 200,
        stream: true,
        fragment: 13,
        body: frame(
            json!({"type":"response.completed","response":answer(json!([text_item("answered once")]))}),
        ),
    }
}

#[tokio::test]
async fn pre_content_stream_refusal_waits_for_server_advice_and_records_each_physical_attempt() {
    let (url, seen) = serve(vec![failed_reply("rate_limit_exceeded", "2", ""), finished_reply()]).await;
    let provider = rook_llm::retry::Retrying::new(Box::new(provider(url)));
    let log = Arc::new(AttemptLog::default());
    let mut request = Request::new(vec![Message::user("answer")]);
    request.effort = Some(Effort::High);
    let mut stream = provider.stream_observed(request, log.clone()).await.unwrap();
    let mut assembler = Assembler::default();
    let mut efforts = 0;
    while let Some(delta) = stream.next().await {
        let delta = delta.unwrap();
        if matches!(delta, Delta::Effort(_)) {
            efforts += 1;
        }
        assembler.push(delta).unwrap();
    }
    assert_eq!(assembler.finish().message.content, "answered once");
    assert_eq!(efforts, 2, "each physical request retains its requested/applied metadata");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[1].at.duration_since(seen[0].at) >= Duration::from_secs(2), "server delay was ignored");
    assert_eq!(seen[0].body, seen[1].body, "retry must preserve the accepted request");
    let ends = log.ends.lock().unwrap();
    assert_eq!(
        ends.iter().map(|e| e.0).collect::<Vec<_>>(),
        [rook_llm::AttemptStatus::Failed, rook_llm::AttemptStatus::Completed]
    );
    assert!(ends[0].1.usage.is_none() && !ends[0].1.completion_confirmed);
    assert!(ends[1].1.usage_reported && ends[1].1.completion_confirmed);
}

#[tokio::test]
async fn streamed_retry_advice_never_repeats_partial_content_or_terminal_refusals() {
    let cases = [
        ("server_error", frame(json!({"type":"response.output_text.delta","delta":"kept text"}))),
        (
            "server_error",
            frame(json!({"type":"response.reasoning_summary_text.delta","delta":"kept thought"})),
        ),
        ("invalid_api_key", String::new()),
        ("context_length_exceeded", String::new()),
        ("insufficient_quota", String::new()),
    ];
    for (code, prefix) in cases {
        let (url, seen) = serve(vec![failed_reply(code, "2", &prefix), finished_reply()]).await;
        let provider = rook_llm::retry::Retrying::new(Box::new(provider(url)));
        let log = Arc::new(AttemptLog::default());
        let mut stream = provider.stream_observed(Request::new(vec![]), log.clone()).await.unwrap();
        let mut assembler = Assembler::default();
        let mut failed = false;
        while let Some(delta) = stream.next().await {
            match delta {
                Ok(delta) => assembler.push(delta).unwrap(),
                Err(error) => {
                    assert!(error.to_string().contains(code));
                    failed = true;
                }
            }
        }
        assert!(failed, "{code}");
        if prefix.contains("kept text") {
            assert_eq!(assembler.finish().message.content, "kept text");
        } else if prefix.contains("kept thought") {
            assert_eq!(assembler.reasoning(), "kept thought");
        }
        assert_eq!(seen.lock().unwrap().len(), 1, "{code} was retried");
        assert_eq!(log.ends.lock().unwrap()[0].0, rook_llm::AttemptStatus::Failed);
    }
}

#[tokio::test]
async fn cancelling_stream_retry_wait_releases_the_failed_attempt_without_another_request() {
    let (url, seen) = serve(vec![failed_reply("server_is_overloaded", "120", ""), finished_reply()]).await;
    let provider = rook_llm::retry::Retrying::new(Box::new(provider(url)));
    let log = Arc::new(AttemptLog::default());
    let mut stream = provider.stream_observed(Request::new(vec![]), log.clone()).await.unwrap();
    {
        let waiting = async {
            while let Some(delta) = stream.next().await {
                delta.unwrap();
            }
        };
        tokio::select! {
            _ = waiting => panic!("stream finished instead of waiting for its retry"),
            result = tokio::time::timeout(Duration::from_secs(10), log.ended.notified()) => {
                result.expect("failed attempt must end before the retry wait");
            }
        }
    }
    drop(stream);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(log.starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(log.ends.lock().unwrap()[0].0, rook_llm::AttemptStatus::Failed);
}

#[tokio::test]
async fn http_and_stream_refusals_share_one_four_attempt_ceiling() {
    let http = Reply {
        status: 503,
        stream: false,
        fragment: usize::MAX,
        body: json!({"error":{"code":"server_error"}}).to_string(),
    };
    let (url, seen) = serve(vec![
        http,
        failed_reply("server_error", "0", ""),
        failed_reply("server_error", "0", ""),
        failed_reply("server_error", "0", ""),
    ])
    .await;
    let provider = rook_llm::retry::Retrying::new(Box::new(provider(url)));
    let log = Arc::new(AttemptLog::default());
    let mut stream = provider.stream_observed(Request::new(vec![]), log.clone()).await.unwrap();
    let mut last = None;
    while let Some(delta) = stream.next().await {
        if let Err(error) = delta {
            last = Some(error);
        }
    }
    assert!(matches!(last, Some(rook_llm::LlmError::Status { status: 503, .. })));
    assert_eq!(seen.lock().unwrap().len(), 4);
    assert_eq!(log.starts.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(
        log.ends.lock().unwrap().iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![rook_llm::AttemptStatus::Failed; 4]
    );
}

#[tokio::test]
async fn refusals_incomplete_output_and_invalid_usage_are_distinguished() {
    let mut incomplete = answer(json!([text_item("partial")]));
    incomplete["status"] = json!("incomplete");
    incomplete["incomplete_details"] = json!({"reason":"max_output_tokens"});
    let refusal = answer(
        json!([{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot answer"}]}]),
    );
    for (value, expected) in [(incomplete, StopReason::MaxTokens), (refusal, StopReason::Refusal)] {
        let (url, _) = serve(vec![reply(value)]).await;
        assert_eq!(provider(url).complete(Request::new(vec![])).await.unwrap().stop_reason, expected);
    }
    let mut overflow = answer(json!([]));
    overflow["usage"]["input_tokens"] = json!(u64::from(u32::MAX) + 1);
    let invalid = [
        overflow,
        answer(json!([tool("same", "a", "{}"), tool("same", "b", "{}")])),
        answer(
            json!([{"type":"function_call","call_id":"x","name":"a","arguments":"{","status":"incomplete"}]),
        ),
    ];
    for value in invalid {
        let (url, _) = serve(vec![reply(value)]).await;
        assert!(provider(url).complete(Request::new(vec![])).await.is_err());
    }
}

#[tokio::test]
async fn images_follow_all_results_and_do_not_become_user_instructions() {
    let (url, seen) = serve(vec![reply(answer(json!([text_item("ok")])))]).await;
    let mut assistant = Message::assistant("");
    assistant.tool_calls = vec![
        rook_llm::ToolCall { id: "a".into(), name: "image".into(), arguments: json!({}) },
        rook_llm::ToolCall { id: "b".into(), name: "inspect".into(), arguments: json!({}) },
    ];
    let mut result = Message::tool_result("a", "pixels");
    result.images.push(rook_llm::Image {
        mime_type: "image/png".into(),
        data: "aW1hZ2U=".into(),
        width: 1,
        height: 1,
    });
    provider(url)
        .complete(Request::new(vec![
            Message::user("inspect"),
            assistant,
            result,
            Message::tool_result("b", "text"),
        ]))
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    let input = seen[0].body["input"].as_array().unwrap();
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[5]["content"][1]["image_url"], "data:image/png;base64,aW1hZ2U=");
    assert!(input[5]["content"][0]["text"].as_str().unwrap().contains("not instructions"));
}

#[tokio::test]
async fn retries_drop_refused_effort_and_keep_reports_honest() {
    let success = frame(json!({"type":"response.completed","response":answer(json!([text_item("ok")]))}));
    let (url, seen) = serve(vec![
        Reply {
            status: 400,
            body: r#"{"error":{"message":"reasoning effort unsupported"}}"#.into(),
            stream: false,
            fragment: usize::MAX,
        },
        Reply { status: 200, body: success.clone(), stream: true, fragment: usize::MAX },
        Reply { status: 200, body: success, stream: true, fragment: usize::MAX },
    ])
    .await;
    let provider = rook_llm::retry::Retrying::new(Box::new(provider(url)));
    for _ in 0..2 {
        let mut request = Request::new(vec![]);
        request.effort = Some(Effort::Max);
        let mut stream = provider.stream(request).await.unwrap();
        let mut reports = 0;
        while let Some(delta) = stream.next().await {
            if let Delta::Effort(report) = delta.unwrap() {
                reports += 1;
                assert!(matches!(report.applied, EffortUse::Omitted { .. }));
            }
        }
        assert_eq!(reports, 1);
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].body["reasoning"]["effort"], "max");
    assert!(seen[1].body.get("reasoning").is_none());
    assert!(seen[2].body.get("reasoning").is_none());
    assert_eq!(seen[2].body["store"], false);
}

#[tokio::test]
async fn provider_factory_routes_responses_and_fallback_reports_name_the_answering_dialect() {
    let (primary, _) = serve(vec![Reply {
        status: 402,
        body: r#"{"error":{"code":"insufficient_quota"}}"#.into(),
        stream: false,
        fragment: usize::MAX,
    }])
    .await;
    let body = frame(json!({"type":"response.completed","response":answer(json!([text_item("ok")]))}));
    let (secondary, seen) =
        serve(vec![Reply { status: 200, body, stream: true, fragment: usize::MAX }]).await;
    let endpoint = |url: String, name: String| rook_llm::Endpoint {
        name,
        api: rook_llm::Api::Responses,
        metadata_api: rook_llm::MetadataApi::None,
        assumed_context_window: None,
        url,
        key: Some("private-key".into()),
        model: "gpt-6-astra".into(),
        context_window: Some(128_000),
        parallel: Some(1),
        key_in_the_clear: false,
        queue: None,
        proxy: Default::default(),
    };
    let preferred = format!("primary-{primary}");
    let fallback = format!("fallback-{secondary}");
    let provider = rook_llm::from_endpoints_with(
        vec![endpoint(primary, preferred), endpoint(secondary, fallback.clone())],
        Duration::from_secs(2),
        rook_llm::Prefer::AsConfigured,
    )
    .unwrap();
    let mut request = Request::new(vec![]);
    request.effort = Some(Effort::Max);
    let mut stream = provider.stream(request).await.unwrap();
    let mut reported = false;
    while let Some(delta) = stream.next().await {
        if let Delta::Effort(report) = delta.unwrap() {
            reported = true;
            assert_eq!(report.provider, fallback);
            assert_eq!(
                report.applied,
                EffortUse::Parameter { name: "reasoning.effort", value: "max".into() }
            );
        }
    }
    assert!(reported);
    assert!(seen.lock().unwrap()[0].head.contains("/responses"));
}

#[tokio::test]
async fn oversized_and_excessive_responses_fail_without_returning_calls() {
    let oversized = " ".repeat(33 * 1024 * 1024);
    for stream in [false, true] {
        let (url, _) =
            serve(vec![Reply { status: 200, body: oversized.clone(), stream, fragment: 64 * 1024 }]).await;
        let provider = provider(url);
        if stream {
            let mut response = provider.stream(Request::new(vec![])).await.unwrap();
            let error = response.next().await.unwrap().unwrap_err();
            assert!(error.to_string().contains("byte budget"));
        } else {
            let error = provider.complete(Request::new(vec![])).await.unwrap_err();
            assert!(error.to_string().contains("bytes"));
        }
    }
    let excessive =
        answer(json!((0..257).map(|i| tool(&i.to_string(), "inspect", "{}")).collect::<Vec<_>>()));
    let (url, _) = serve(vec![reply(excessive)]).await;
    assert!(provider(url).complete(Request::new(vec![])).await.unwrap_err().to_string().contains("256"));
}

#[tokio::test]
async fn changing_the_visible_assistant_message_does_not_replay_its_stale_snapshot() {
    let (url, seen) = serve(vec![
        reply(answer(json!([reasoning(), text_item("original")]))),
        reply(answer(json!([text_item("ok")]))),
    ])
    .await;
    let provider = provider(url);
    let mut message = provider.complete(Request::new(vec![Message::user("hi")])).await.unwrap().message;
    message.content = "edited".into();
    provider.complete(Request::new(vec![Message::user("hi"), message])).await.unwrap();
    let seen = seen.lock().unwrap();
    let input = seen[1].body["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[1]["content"], "edited");
    assert!(!seen[1].body.to_string().contains("opaque-signed-state"));
}

#[test]
fn responses_api_names_and_context_identity_are_distinct_from_chat_completions() {
    assert_eq!(rook_llm::Api::parse("responses"), Some(rook_llm::Api::Responses));
    assert_eq!(rook_llm::Api::parse("openai-responses"), Some(rook_llm::Api::Responses));
    for name in ["responses", "openai-responses"] {
        assert!(rook_llm::PROVIDERS.contains(&name));
        let endpoint = rook_llm::catalog_endpoint_from_spec(&format!("{name}/gpt-6-astra"), None).unwrap();
        assert_eq!(endpoint.api, rook_llm::Api::Responses);
        assert_eq!(endpoint.model, "gpt-6-astra");
    }
    let config = || Config::new("http://127.0.0.1:1/v1".into(), None, 128_000);
    let chat = rook_llm::openai::OpenAiCompatible::new("same", "gpt-6-astra", config()).unwrap();
    let responses = Responses::new("same", "gpt-6-astra", config()).unwrap();
    assert_ne!(chat.context_key(), responses.context_key());
}

async fn quiet_stream() -> (String, tokio::sync::oneshot::Receiver<bool>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (closed, wait) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0 && raw.len() + n < 64 * 1024);
            raw.extend_from_slice(&buf[..n]);
            let Some(split) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&raw[..split]);
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap();
            if raw.len() >= split + 4 + length {
                break;
            }
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        socket
            .write_all(frame(json!({"type":"response.output_text.delta","delta":"started"})).as_bytes())
            .await
            .unwrap();
        let mut buf = [0; 1];
        let result = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buf)).await;
        let _ = closed.send(matches!(result, Ok(Ok(0)) | Ok(Err(_))));
    });
    (format!("http://{address}/v1"), wait)
}

#[tokio::test]
async fn dropping_or_stalling_a_response_releases_its_connection() {
    for stall in [false, true] {
        let (url, closed) = quiet_stream().await;
        let mut config = Config::new(url, None, 128_000);
        config.stream_idle_timeout = Duration::from_millis(30);
        let provider = Responses::new("quiet", "gpt-6-astra", config).unwrap();
        let mut stream = provider.stream(Request::new(vec![])).await.unwrap();
        assert!(matches!(stream.next().await.unwrap().unwrap(),Delta::Text(text) if text=="started"));
        if stall {
            assert!(matches!(stream.next().await.unwrap(), Err(rook_llm::LlmError::Stalled { .. })));
        }
        drop(stream);
        assert!(tokio::time::timeout(Duration::from_secs(4), closed).await.unwrap().unwrap());
    }
}

#[tokio::test]
async fn pro_models_preserve_their_documented_minimum_effort() {
    for (model, values) in [
        ("gpt-5-pro", ["high", "high", "high", "high", "high"]),
        ("gpt-5.2-pro", ["medium", "medium", "high", "xhigh", "xhigh"]),
        ("gpt-5.4-pro", ["medium", "medium", "high", "xhigh", "xhigh"]),
        ("gpt-5.5-pro", ["medium", "medium", "high", "xhigh", "xhigh"]),
    ] {
        for (effort, expected) in Effort::ALL.into_iter().zip(values) {
            let (url, seen) = serve(vec![reply(answer(json!([text_item("ok")])))]).await;
            let provider = Responses::new("pro", model, Config::new(url, None, 128_000)).unwrap();
            assert_eq!(
                provider.effort_use(effort),
                EffortUse::Parameter { name: "reasoning.effort", value: expected.into() }
            );
            let mut request = Request::new(vec![]);
            request.effort = Some(effort);
            provider.complete(request).await.unwrap();
            assert_eq!(seen.lock().unwrap()[0].body["reasoning"]["effort"], expected);
        }
    }
}

#[tokio::test]
async fn moving_to_another_endpoint_keeps_visible_calls_but_withholds_foreign_encrypted_state() {
    let (first, _) =
        serve(vec![reply(answer(json!([reasoning(), tool("original", "inspect", "{}")])))]).await;
    let (second, seen) = serve(vec![reply(answer(json!([text_item("ok")])))]).await;
    let message =
        provider(first).complete(Request::new(vec![Message::user("inspect")])).await.unwrap().message;
    provider(second)
        .complete(Request::new(vec![
            Message::user("inspect"),
            message,
            Message::tool_result("original", "done"),
        ]))
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    let input = seen[0].body["input"].as_array().unwrap();
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[1]["call_id"], "original");
    assert_eq!(input[2]["call_id"], "original");
    assert!(!seen[0].body.to_string().contains("opaque-signed-state"));
}

fn continuity_endpoint(url: String, name: String) -> rook_llm::Endpoint {
    rook_llm::Endpoint {
        name,
        api: rook_llm::Api::Responses,
        metadata_api: rook_llm::MetadataApi::None,
        assumed_context_window: None,
        url,
        key: Some("private-key".into()),
        model: "gpt-6-astra".into(),
        context_window: Some(128_000),
        parallel: Some(1),
        key_in_the_clear: false,
        queue: None,
        proxy: Default::default(),
    }
}

fn continuity_reply(output: Value, stream: bool) -> Reply {
    let response = answer(output);
    let body = if stream {
        frame(json!({"type":"response.completed","response":response}))
    } else {
        response.to_string()
    };
    Reply { status: 200, body, stream, fragment: usize::MAX }
}

async fn finish_request(
    provider: &dyn Provider,
    request: Request,
    stream: bool,
) -> rook_llm::Result<rook_llm::Response> {
    if !stream {
        return provider.complete(request).await;
    }
    let mut response = provider.stream(request).await?;
    let mut assembled = Assembler::default();
    while let Some(delta) = response.next().await {
        assembled.push(delta?)?;
    }
    Ok(assembled.finish())
}

#[tokio::test]
async fn automatic_fallback_preserves_the_origin_or_refuses_without_sending_foreign_state() {
    for stream in [false, true] {
        for secondary_origin in [false, true] {
            let quota = || Reply {
                status: 402,
                body: r#"{"error":{"code":"insufficient_quota"}}"#.into(),
                stream: false,
                fragment: usize::MAX,
            };
            let original = json!([reasoning(), tool("original", "inspect", "{}")]);
            let primary_replies = if secondary_origin {
                vec![quota(), continuity_reply(json!([text_item("must not use this answer")]), stream)]
            } else {
                vec![
                    continuity_reply(original.clone(), stream),
                    quota(),
                    continuity_reply(json!([text_item("recovered")]), stream),
                ]
            };
            let secondary_replies = if secondary_origin {
                vec![
                    continuity_reply(original.clone(), stream),
                    continuity_reply(json!([text_item("ok")]), stream),
                ]
            } else {
                vec![continuity_reply(json!([text_item("must not discard state")]), stream)]
            };
            let (primary, primary_seen) = serve(primary_replies).await;
            let (secondary, secondary_seen) = serve(secondary_replies).await;
            let primary_name = format!("continuity-primary-{primary}");
            let secondary_name = format!("continuity-secondary-{secondary}");
            let provider = rook_llm::from_endpoints_with(
                vec![
                    continuity_endpoint(primary, primary_name.clone()),
                    continuity_endpoint(secondary, secondary_name.clone()),
                ],
                Duration::from_secs(2),
                rook_llm::Prefer::AsConfigured,
            )
            .unwrap();
            let response =
                finish_request(provider.as_ref(), Request::new(vec![Message::user("inspect")]), stream)
                    .await
                    .unwrap();
            assert!(!response.message.reasoning.is_empty());
            // A recovered primary must not steal the next step from the actual origin.
            rook_llm::answering_again(Some(&primary_name));
            let request = Request::new(vec![
                Message::user("inspect"),
                response.message,
                Message::tool_result("original", "done"),
            ]);
            let result = finish_request(provider.as_ref(), request.clone(), stream).await;
            if secondary_origin {
                assert_eq!(result.unwrap().message.content, "ok");
                assert_eq!(primary_seen.lock().unwrap().len(), 1, "foreign primary was contacted");
                let seen = secondary_seen.lock().unwrap();
                assert_eq!(seen.len(), 2);
                let input = seen[1].body["input"].as_array().unwrap();
                assert_eq!(&input[1..3], original.as_array().unwrap());
                assert_eq!(input[3]["call_id"], "original");
                assert_eq!(input[3]["output"], "done");
            } else {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("provider-owned state") && error.contains(&secondary_name), "{error}");
                assert_eq!(primary_seen.lock().unwrap().len(), 2);
                assert!(secondary_seen.lock().unwrap().is_empty(), "foreign fallback was contacted");
                // The compatible primary is now in cooldown. An incompatible
                // healthy fallback must not prevent trying that primary again.
                assert_eq!(
                    finish_request(provider.as_ref(), request, stream).await.unwrap().message.content,
                    "recovered"
                );
                assert_eq!(primary_seen.lock().unwrap().len(), 3);
                assert!(secondary_seen.lock().unwrap().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn automatic_fallback_can_replay_an_unchanged_scope_through_an_alias() {
    for stream in [false, true] {
        let original = json!([reasoning(), tool("original", "inspect", "{}")]);
        let (url, seen) = serve(vec![
            continuity_reply(original.clone(), stream),
            Reply {
                status: 402,
                body: r#"{"error":{"code":"insufficient_quota"}}"#.into(),
                stream: false,
                fragment: usize::MAX,
            },
            continuity_reply(json!([text_item("alias continued")]), stream),
        ])
        .await;
        let provider = rook_llm::from_endpoints_with(
            vec![
                continuity_endpoint(url.clone(), format!("alias-primary-{url}")),
                continuity_endpoint(url.clone(), format!("alias-secondary-{url}")),
            ],
            Duration::from_secs(2),
            rook_llm::Prefer::AsConfigured,
        )
        .unwrap();
        let initial = finish_request(provider.as_ref(), Request::new(vec![Message::user("inspect")]), stream)
            .await
            .unwrap();
        let request = Request::new(vec![
            Message::user("inspect"),
            initial.message,
            Message::tool_result("original", "done"),
        ]);
        assert_eq!(
            finish_request(provider.as_ref(), request, stream).await.unwrap().message.content,
            "alias continued"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        for sent in &seen[1..] {
            let input = sent.body["input"].as_array().unwrap();
            assert_eq!(&input[1..3], original.as_array().unwrap());
            assert_eq!(input[3]["call_id"], "original");
            assert_eq!(input[3]["output"], "done");
        }
    }
}
