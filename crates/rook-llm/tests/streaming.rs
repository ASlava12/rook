//! The SSE path, over a real socket.
//!
//! Frames are deliberately split across writes: reassembling them is the part
//! that breaks, and it cannot be exercised by feeding the parser whole frames.

use std::time::Duration;

use futures_util::StreamExt;
use rook_llm::openai::{Config, OpenAiCompatible};
use rook_llm::{Delta, LlmError, Message, Provider, Request, StopReason};
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
