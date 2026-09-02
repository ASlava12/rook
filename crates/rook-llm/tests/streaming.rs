//! The SSE path, over a real socket.
//!
//! Frames are deliberately split across writes: reassembling them is the part
//! that breaks, and it cannot be exercised by feeding the parser whole frames.

use std::time::Duration;

use futures_util::StreamExt;
use rook_llm::openai::{Config, OpenAiCompatible};
use rook_llm::{Delta, LlmError, Message, Provider, Request, Role, StopReason};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve one request, writing `pieces` with a pause between each.
async fn serve(pieces: Vec<&'static str>, gap: Duration, then_hang: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = [0u8; 8192];
        let _ = socket.read(&mut scratch).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        for piece in pieces {
            socket.write_all(piece.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(gap).await;
        }
        if then_hang {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
    format!("http://{addr}/v1")
}

fn provider(base_url: String, idle: Duration) -> OpenAiCompatible {
    let mut config = Config::new(base_url, None, 8192);
    config.stream_idle_timeout = idle;
    OpenAiCompatible::new("test/model", "model", config).unwrap()
}

fn request() -> Request {
    Request::new(vec![Message::user("hi")])
}

#[tokio::test]
async fn text_deltas_arrive_in_order_across_split_frames() {
    let url = serve(
        vec![
            // A frame cut in half mid-JSON, then completed by the next write.
            r#"data: {"model":"m","choices":[{"delta":{"content":"Hel"#,
            r#"lo "}}]}"#,
            "\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"world\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        ],
        Duration::from_millis(5),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut text = String::new();
    let mut done = None;
    while let Some(delta) = stream.next().await {
        match delta.unwrap() {
            Delta::Text(t) => text.push_str(&t),
            Delta::Done { stop_reason, usage, model } => done = Some((stop_reason, usage, model)),
            _ => {}
        }
    }
    assert_eq!(text, "Hello world");
    let (stop, usage, model) = done.expect("the stream must always end with Done");
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(usage.input_tokens, 7);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(model, "m");
}

#[tokio::test]
async fn a_tool_call_is_emitted_once_whole_not_in_fragments() {
    let url = serve(
        vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            "\n\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "\n\ndata: [DONE]\n\n",
        ],
        Duration::from_millis(5),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut calls = Vec::new();
    let mut stop = None;
    while let Some(delta) = stream.next().await {
        match delta.unwrap() {
            Delta::ToolCall(c) => calls.push(c),
            Delta::Done { stop_reason, .. } => stop = Some(stop_reason),
            _ => {}
        }
    }
    assert_eq!(calls.len(), 1, "fragments must not surface as separate calls");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].id, "c1");
    assert_eq!(calls[0].arguments["path"], "a.txt");
    assert_eq!(stop, Some(StopReason::ToolUse));
}

/// Some gateways repeat `id` and `name` on every continuation chunk with
/// nothing in them. Taken at face value the name is wiped, and a call with no
/// name is dropped — so the model asked for a tool and nothing happened, with
/// nothing anywhere saying so.
#[tokio::test]
async fn a_later_chunk_that_repeats_the_name_as_empty_does_not_erase_it() {
    let url = serve(
        vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            "\n\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"","arguments":"th\":\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "\n\ndata: [DONE]\n\n",
        ],
        Duration::from_millis(5),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut calls = Vec::new();
    while let Some(delta) = stream.next().await {
        if let Delta::ToolCall(c) = delta.unwrap() {
            calls.push(c);
        }
    }
    assert_eq!(calls.len(), 1, "the call must survive a chunk that names nothing");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].id, "c1");
    assert_eq!(calls[0].arguments["path"], "a.txt");
}

/// And the other direction, which is what goose had: a provider that sends the
/// index first and the name only once it knows it.
#[tokio::test]
async fn a_name_that_arrives_late_is_still_the_calls_name() {
    let url = serve(
        vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":""}}]}}]}"#,
            "\n\n",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c2","function":{"name":"list_dir","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "\n\ndata: [DONE]\n\n",
        ],
        Duration::from_millis(5),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut calls = Vec::new();
    while let Some(delta) = stream.next().await {
        if let Delta::ToolCall(c) = delta.unwrap() {
            calls.push(c);
        }
    }
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "list_dir");
    assert_eq!(calls[0].id, "c2");
}

#[tokio::test]
async fn a_stalled_stream_gives_up_instead_of_hanging() {
    let url = serve(
        vec!["data: {\"choices\":[{\"delta\":{\"content\":\"start\"}}]}\n\n"],
        Duration::from_millis(5),
        true,
    )
    .await;

    let mut stream = provider(url, Duration::from_millis(150)).stream(request()).await.unwrap();
    let mut error = None;
    while let Some(delta) = stream.next().await {
        if let Err(e) = delta {
            error = Some(e);
            break;
        }
    }
    assert!(
        matches!(error, Some(LlmError::Stalled { .. })),
        "a silent connection must surface as a stall, not as a hang: {error:?}"
    );
}

#[tokio::test]
async fn malformed_frames_are_skipped_rather_than_killing_the_stream() {
    let url = serve(
        vec![
            ": this is an SSE comment\n\n",
            "data: not json at all\n\n",
            "event: ping\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"survived\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ],
        Duration::from_millis(2),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut text = String::new();
    while let Some(delta) = stream.next().await {
        if let Delta::Text(t) = delta.unwrap() {
            text.push_str(&t);
        }
    }
    assert_eq!(text, "survived");
}

#[tokio::test]
async fn reasoning_is_reported_separately_from_the_answer() {
    let url = serve(
        vec![
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ],
        Duration::from_millis(2),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let (mut text, mut reasoning) = (String::new(), String::new());
    while let Some(delta) = stream.next().await {
        match delta.unwrap() {
            Delta::Text(t) => text.push_str(&t),
            Delta::Reasoning(t) => reasoning.push_str(&t),
            _ => {}
        }
    }
    assert_eq!(reasoning, "thinking");
    assert_eq!(text, "answer");
}

#[tokio::test]
async fn an_http_error_is_reported_before_any_delta() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = [0u8; 8192];
        let _ = socket.read(&mut scratch).await;
        let body = r#"{"error":{"message":"model not found"}}"#;
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
    });

    let result = provider(format!("http://{addr}/v1"), Duration::from_secs(5)).stream(request()).await;
    let Err(err) = result else {
        panic!("a 404 must fail the call, not produce an empty stream");
    };
    match err {
        LlmError::Status { status, body } => {
            assert_eq!(status, 404);
            assert!(body.contains("model not found"), "{body}");
        }
        other => panic!("expected a status error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_stream_that_never_sends_a_separator_is_cut_off_rather_than_buffered_forever() {
    // 64k of frame-less noise: the shape that turns a naive rescan quadratic and
    // an unbounded buffer into an out-of-memory.
    let noise: &'static str = Box::leak("data: {\"x\":1}".repeat(700_000).into_boxed_str());
    let url = serve(vec![noise, noise, noise], Duration::from_millis(1), true).await;

    let started = std::time::Instant::now();
    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut error = None;
    while let Some(delta) = stream.next().await {
        if let Err(e) = delta {
            error = Some(e);
            break;
        }
    }
    assert!(matches!(error, Some(LlmError::Decode(_))), "expected the frame cap to fire, got {error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cutting it off took {:?}, which means it was still rescanning",
        started.elapsed()
    );
}

/// The frame cap bounds one SSE event and says nothing about how many arrive.
/// A provider that never ends the stream is held only by the request timeout,
/// which bounds the time and not the memory.
#[test]
fn assembling_a_reply_that_never_ends_stops_rather_than_growing() {
    let mut assembler = rook_llm::Assembler::default();
    let megabyte = "x".repeat(1 << 20);

    let mut pushed = 0usize;
    let refusal = loop {
        pushed += megabyte.len();
        if let Err(e) = assembler.push(rook_llm::Delta::Text(megabyte.clone())) {
            break e.to_string();
        }
        assert!(pushed <= 64 << 20, "no cap was reached after {pushed} bytes");
    };

    assert!(pushed > 32 << 20, "the cap has to be passed for this to test anything: {pushed} bytes");
    assert!(refusal.contains("provider"), "and the message says whose fault it is: {refusal}");
}

/// The agent produces consecutive user turns honestly — a compaction summary in
/// front of the first replayed message, a loaded skill beside the prompt that
/// asked for it. Hosted APIs take the pair; a chat template on a self-hosted
/// server often does not, and this is aimed at local models.
#[test]
fn consecutive_user_turns_are_folded_into_one() {
    let request = Request::new(vec![
        Message::system("rules"),
        Message::user("a summary of earlier work"),
        Message::user("and the prompt itself"),
        Message::assistant("an answer"),
        Message::user("a follow-up"),
    ]);

    let roles: Vec<Role> = request.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [Role::System, Role::User, Role::Assistant, Role::User],
        "two user turns in a row must arrive as one"
    );
    assert_eq!(request.messages[1].content, "a summary of earlier work\n\nand the prompt itself");
}

/// A tool result is a user turn on the wire and is not one here: the dialects
/// have their own rules for them, and folding one into a prompt loses the id it
/// answers.
#[test]
fn a_tool_result_is_not_folded_into_the_prompt_before_it() {
    let request =
        Request::new(vec![Message::user("do the thing"), Message::tool_result("call_1", "it is done")]);

    assert_eq!(request.messages.len(), 2, "{:?}", request.messages);
    assert_eq!(request.messages[1].tool_call_id.as_deref(), Some("call_1"));
}

/// The dialect says `content` is a string. Several servers that implement it
/// send a list of parts instead, and a field typed as a string makes the whole
/// frame fail to parse — which is skipped, so the reply arrives empty with
/// nothing anywhere saying why. This is aimed at self-hosted servers, where that
/// is the likeliest shape to meet.
#[tokio::test]
async fn a_content_delta_shaped_as_a_list_is_read_the_same_as_a_string() {
    let url = serve(
        vec![
            "data: {\"choices\":[{\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"in \"},{\"type\":\"text\",\"text\":\"parts\"}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" and a string\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ],
        Duration::from_millis(2),
        false,
    )
    .await;

    let mut stream = provider(url, Duration::from_secs(5)).stream(request()).await.unwrap();
    let mut text = String::new();
    while let Some(delta) = stream.next().await {
        if let Delta::Text(t) = delta.unwrap() {
            text.push_str(&t);
        }
    }
    assert_eq!(text, "in parts and a string", "both shapes are the same text");
}
