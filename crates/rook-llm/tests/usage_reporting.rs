//! Missing counters and explicit zero must remain distinguishable over the wire.
use futures_util::StreamExt;
use rook_llm::{Assembler, Message, Provider, Request};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn serve(body: String) -> String {
    serve_as(body, "text/event-stream").await
}

async fn serve_as(body: String, mime: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0 && raw.len() + n <= 32768);
            raw.extend_from_slice(&buf[..n]);
            let Some(split) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") else { continue };
            let head = String::from_utf8_lossy(&raw[..split]);
            let length: usize = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            if raw.len() >= split + 4 + length {
                break;
            }
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(body.as_bytes()).await.unwrap();
    });
    format!("http://{address}")
}

fn frame(value: Value) -> String {
    format!("data: {value}\n\n")
}

fn native(dialect: &str, url: String) -> Box<dyn Provider> {
    match dialect {
        "chat" => Box::new(
            rook_llm::openai::OpenAiCompatible::new(
                "native",
                "physical",
                rook_llm::openai::Config::new(url, None, 65536),
            )
            .unwrap(),
        ),
        "responses" => Box::new(
            rook_llm::openai::responses::Responses::new(
                "native",
                "physical",
                rook_llm::openai::Config::new(url, None, 65536),
            )
            .unwrap(),
        ),
        "anthropic" => Box::new(
            rook_llm::anthropic::Anthropic::new(
                "native",
                "physical",
                rook_llm::anthropic::Config::new(url, "fixture".into(), "physical"),
            )
            .unwrap(),
        ),
        _ => Box::new(
            rook_llm::google::Google::new(
                "native",
                "physical",
                rook_llm::google::Config::new(url, "fixture".into(), "physical"),
            )
            .unwrap(),
        ),
    }
}

#[tokio::test]
async fn nonstreaming_facts_preserve_native_identity_counter_presence_and_terminal_evidence() {
    for dialect in ["chat", "responses", "anthropic", "google"] {
        for (input, output, terminal) in [
            (Some(0), Some(7), true),
            (None, Some(7), true),
            (Some(9), None, true),
            (Some(0), Some(0), true),
            (Some(9), Some(7), false),
        ] {
            let (input_key, output_key) = match dialect {
                "chat" => ("prompt_tokens", "completion_tokens"),
                "google" => ("promptTokenCount", "candidatesTokenCount"),
                _ => ("input_tokens", "output_tokens"),
            };
            let mut usage = json!({});
            if let Some(input) = input {
                usage[input_key] = json!(input);
            }
            if let Some(output) = output {
                usage[output_key] = json!(output);
            }
            let body = match dialect {
                "chat" => {
                    json!({"model":"server-reported","choices":[{"message":{"role":"assistant","content":"reply"},"finish_reason":terminal.then_some("stop")}],"usage":usage})
                }
                "responses" => {
                    json!({"model":"server-reported","status":if terminal {"completed"} else {"in_progress"},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"reply"}]}],"usage":usage})
                }
                "anthropic" => {
                    json!({"model":"server-reported","content":[{"type":"text","text":"reply"}],"stop_reason":terminal.then_some("end_turn"),"usage":usage})
                }
                _ => {
                    json!({"modelVersion":"server-reported","candidates":[{"content":{"parts":[{"text":"reply"}]},"finishReason":terminal.then_some("STOP")}],"usageMetadata":usage})
                }
            };
            let provider = native(dialect, serve_as(body.to_string(), "application/json").await);
            let result = provider.complete_with_metadata(Request::new(vec![Message::user("go")])).await;
            if dialect == "responses" && !terminal {
                assert!(result.is_err(), "an in-progress response is not a completed generation");
                continue;
            }
            let completed = result.unwrap();
            assert_eq!(completed.usage_reported, input.is_some() && output.is_some(), "{dialect}");
            assert_eq!(completed.completion_confirmed, terminal, "{dialect}");
            let dispatch = completed.dispatch.unwrap();
            assert_eq!((dispatch.provider.as_str(), dispatch.model.as_str()), ("native", "physical"));
            assert_eq!(dispatch.input_includes_cache, dialect != "anthropic");
            assert_eq!(completed.response.model, "server-reported");
            assert_eq!(completed.response.usage.input_tokens, input.unwrap_or(0));
            assert_eq!(completed.response.usage.output_tokens, output.unwrap_or(0));
            assert_eq!(completed.response.message.content, "reply");
        }
    }
}

async fn check(dialect: &str) {
    for (input, output, reported, complete) in [
        (Some(0), Some(7), true, true),
        (None, Some(7), false, true),
        (Some(9), None, false, true),
        (Some(0), Some(0), true, true),
        (Some(9), Some(7), true, false),
    ] {
        let (input_key, output_key) = match dialect {
            "chat" => ("prompt_tokens", "completion_tokens"),
            "google" => ("promptTokenCount", "candidatesTokenCount"),
            _ => ("input_tokens", "output_tokens"),
        };
        let mut usage = json!({});
        if let Some(input) = input {
            usage[input_key] = json!(input);
        }
        if let Some(output) = output {
            usage[output_key] = json!(output);
        }
        let body = match dialect {
            "chat" => {
                frame(
                    json!({"choices":[{"delta":{"content":"reply"},"finish_reason":complete.then_some("stop")}],"usage":usage}),
                ) + if complete { "data: [DONE]\n\n" } else { "" }
            }
            "responses" if !complete => frame(json!({"type":"response.output_text.delta","delta":"reply"})),
            "responses" => frame(
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"reply"}]}],"usage":usage}}),
            ),
            "anthropic" => {
                // A fully cached prompt can legitimately report zero fresh input.
                let mut start = json!({"cache_read_input_tokens":80});
                if let Some(input) = input {
                    start["input_tokens"] = json!(input);
                }
                frame(json!({"type":"message_start","message":{"usage":start}}))
                    + &frame(
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"reply"}}),
                    )
                    + &frame(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":usage}))
                    + &if complete { frame(json!({"type":"message_stop"})) } else { String::new() }
            }
            // Separate chunks exercise retention even when the later one omits input.
            _ => {
                let mut start = json!({});
                if let Some(input) = input {
                    start["promptTokenCount"] = json!(input);
                }
                usage.as_object_mut().unwrap().remove("promptTokenCount");
                frame(json!({"candidates":[],"usageMetadata":start}))
                    + &frame(
                        json!({"candidates":[{"content":{"parts":[{"text":"reply"}]},"finishReason":complete.then_some("STOP")}],"usageMetadata":usage}),
                    )
            }
        };
        let url = serve(body).await;
        let provider: Box<dyn Provider> = match dialect {
            "chat" => Box::new(
                rook_llm::openai::OpenAiCompatible::new(
                    "test",
                    "model",
                    rook_llm::openai::Config::new(url, None, 65536),
                )
                .unwrap(),
            ),
            "responses" => Box::new(
                rook_llm::openai::responses::Responses::new(
                    "test",
                    "model",
                    rook_llm::openai::Config::new(url, None, 65536),
                )
                .unwrap(),
            ),
            "anthropic" => Box::new(
                rook_llm::anthropic::Anthropic::new(
                    "test",
                    "model",
                    rook_llm::anthropic::Config::new(url, "fixture".into(), "model"),
                )
                .unwrap(),
            ),
            _ => Box::new(
                rook_llm::google::Google::new(
                    "test",
                    "model",
                    rook_llm::google::Config::new(url, "fixture".into(), "model"),
                )
                .unwrap(),
            ),
        };
        let mut stream = provider.stream(Request::new(vec![Message::user("go")])).await.unwrap();
        let mut assembler = Assembler::default();
        let mut error = false;
        while let Some(delta) = stream.next().await {
            match delta {
                Ok(delta) => assembler.push(delta).unwrap(),
                Err(_) => error = true,
            }
        }
        assert_eq!(error, dialect == "responses" && !complete);
        if error {
            assert!(!assembler.completion_confirmed(), "Responses EOF is not completion");
            continue;
        }
        assert!(assembler.has_done(), "{dialect}");
        assert_eq!(
            assembler.completion_confirmed(),
            complete,
            "{dialect}: transport EOF is not a terminal marker"
        );
        assert_eq!(assembler.usage_reported(), reported, "{dialect}: input={input:?}, output={output:?}");
        let response = assembler.finish();
        assert_eq!(response.usage.input_tokens, input.unwrap_or(0));
        assert_eq!(response.usage.output_tokens, output.unwrap_or(0));
        assert_eq!(response.message.content, "reply");
    }
}

#[tokio::test]
async fn chat_distinguishes_omitted_usage_from_zero() {
    check("chat").await;
}
#[tokio::test]
async fn responses_distinguishes_omitted_usage_from_zero() {
    check("responses").await;
}
#[tokio::test]
async fn anthropic_distinguishes_omitted_usage_from_zero_fresh_input() {
    check("anthropic").await;
}
#[tokio::test]
async fn google_retains_split_counters_and_distinguishes_omission_from_zero() {
    check("google").await;
}
