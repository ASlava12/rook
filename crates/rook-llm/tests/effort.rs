//! The status shown to a person must agree with the accepted HTTP request.
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use rook_llm::retry::Retrying;
use rook_llm::{Assembler, Delta, Effort, EffortReport, EffortUse, Message, Provider, Request};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const OPENAI: &str = "data: {\"model\":\"test\",\"choices\":[{\"delta\":{\"content\":\"answered\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
const GOOGLE: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"answered\"}]},\"finishReason\":\"STOP\"}]}\n\n";
const ANTHROPIC: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"test\",\"usage\":{\"input_tokens\":1}}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answered\"}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

async fn server(replies: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    tokio::spawn(async move {
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0 && raw.len() + n < 64 * 1024);
                raw.extend_from_slice(&buf[..n]);
                let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let header = String::from_utf8_lossy(&raw[..split]);
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")?
                            .trim()
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap();
                if raw.len() < split + 4 + length {
                    continue;
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&raw[split + 4..split + 4 + length]).unwrap());
                break;
            }
            let content_type = if status == 200 { "text/event-stream" } else { "application/json" };
            let response = format!(
                "HTTP/1.1 {status} Reply\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}"), seen)
}

fn openai(url: String, model: &str) -> Retrying {
    Retrying::new(Box::new(
        rook_llm::openai::OpenAiCompatible::new(
            "route/model",
            model,
            rook_llm::openai::Config::new(url, None, 8192),
        )
        .unwrap(),
    ))
}

async fn observed(provider: &dyn Provider, effort: Effort) -> EffortReport {
    let mut request = Request::new(vec![Message::user("hello")]);
    request.effort = Some(effort);
    let mut stream = provider.stream(request).await.unwrap();
    let mut reports = Vec::new();
    let mut assembler = Assembler::default();
    while let Some(delta) = stream.next().await {
        let delta = delta.unwrap();
        if let Delta::Effort(report) = &delta {
            reports.push(report.clone());
        }
        assembler.push(delta).unwrap();
    }
    assert_eq!(assembler.finish().message.content, "answered", "metadata never becomes model text");
    assert_eq!(reports.len(), 1, "one report for the accepted attempt");
    let report = reports.pop().unwrap();
    assert_eq!(report.requested, effort);
    report
}

#[tokio::test]
async fn every_reported_effort_mapping_matches_the_bytes_the_dialect_sends() {
    for effort in Effort::ALL {
        for dialect in ["openai", "anthropic", "google"] {
            let response = match dialect {
                "anthropic" => ANTHROPIC,
                "google" => GOOGLE,
                _ => OPENAI,
            };
            let (url, seen) = server(vec![(200, response)]).await;
            let provider: Box<dyn Provider> = match dialect {
                "anthropic" => Box::new(Retrying::new(Box::new(
                    rook_llm::anthropic::Anthropic::new(
                        "anthropic/test",
                        "claude-opus-5",
                        rook_llm::anthropic::Config::new(url, "k".into(), "claude-opus-5"),
                    )
                    .unwrap(),
                ))),
                "google" => Box::new(Retrying::new(Box::new(
                    rook_llm::google::Google::new(
                        "google/test",
                        "gemini-2.5-pro",
                        rook_llm::google::Config::new(url, "k".into(), "gemini-2.5-pro"),
                    )
                    .unwrap(),
                ))),
                _ => Box::new(openai(url, "gpt-5")),
            };
            let report = observed(provider.as_ref(), effort).await;
            assert_eq!(report.provider, provider.id());
            let EffortUse::Parameter { name, value } = report.applied else { panic!("known mapping") };
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            let wire = seen[0].pointer(&format!("/{}", name.replace('.', "/"))).unwrap();
            let wire = wire.as_str().map(str::to_string).unwrap_or_else(|| wire.to_string());
            assert_eq!(value, wire, "{dialect} at {effort:?}");
            if dialect == "openai" && effort == Effort::Max {
                assert_eq!(value, "high");
            }
            if dialect == "google" && effort == Effort::Max {
                assert_eq!(value, "32768");
            }
        }
    }
}

#[tokio::test]
async fn a_refused_effort_is_reported_as_omitted_including_later_requests() {
    let (url, seen) =
        server(vec![(400, r#"{"error":"unsupported reasoning_effort"}"#), (200, OPENAI), (200, OPENAI)])
            .await;
    let provider = openai(url, "gpt-5");
    assert!(provider.takes_effort());
    for _ in 0..2 {
        let report = observed(&provider, Effort::Max).await;
        assert_eq!(report.applied, EffortUse::Omitted { reason: "endpoint refused the effort parameter" });
        assert!(!provider.takes_effort());
        assert_eq!(provider.effort_use(Effort::Max), report.applied);
    }
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["reasoning_effort"], "high", "the first attempt must carry the field");
    assert!(requests[1].get("reasoning_effort").is_none());
    assert!(requests[2].get("reasoning_effort").is_none());
}

#[tokio::test]
async fn an_unmapped_model_does_not_claim_its_requested_effort_was_sent() {
    let (url, seen) = server(vec![(200, OPENAI)]).await;
    let provider = openai(url, "local-model");
    let report = observed(&provider, Effort::High).await;
    assert!(matches!(report.applied, EffortUse::Omitted { .. }));
    assert!(seen.lock().unwrap()[0].get("reasoning_effort").is_none());
}

#[tokio::test]
async fn a_concurrent_refusal_does_not_rewrite_another_requests_report() {
    struct Concurrent {
        high_arrived: Arc<tokio::sync::Notify>,
        finish_high: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl Provider for Concurrent {
        fn id(&self) -> &str {
            "concurrent"
        }
        fn context_window(&self) -> usize {
            8192
        }
        fn effort_use(&self, effort: Effort) -> EffortUse {
            EffortUse::Parameter { name: "reasoning_effort", value: effort.as_str().into() }
        }
        async fn complete(&self, _: Request) -> rook_llm::Result<rook_llm::Response> {
            panic!("this fixture streams")
        }
        async fn stream(&self, request: Request) -> rook_llm::Result<rook_llm::ResponseStream> {
            if request.effort == Some(Effort::Low) {
                return Err(rook_llm::LlmError::Status {
                    status: 400,
                    body: "effort refused".into(),
                    retry_after: None,
                });
            }
            if request.effort == Some(Effort::High) {
                self.high_arrived.notify_one();
                self.finish_high.notified().await;
            }
            Ok(Box::pin(futures_util::stream::iter(vec![Ok(Delta::Text("answered".into()))])))
        }
    }
    let high_arrived = Arc::new(tokio::sync::Notify::new());
    let finish_high = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(Retrying::new(Box::new(Concurrent {
        high_arrived: high_arrived.clone(),
        finish_high: finish_high.clone(),
    })));
    let other = provider.clone();
    let high = tokio::spawn(async move { observed(other.as_ref(), Effort::High).await });
    high_arrived.notified().await;
    let low = observed(provider.as_ref(), Effort::Low).await;
    assert!(matches!(low.applied, EffortUse::Omitted { .. }));
    assert!(!provider.takes_effort(), "shared state has learned the refusal");
    finish_high.notify_one();
    let high = high.await.unwrap();
    assert_eq!(high.applied, EffortUse::Parameter { name: "reasoning_effort", value: "high".into() });
}

/// Expected values come from each provider's model-specific API contract, not
/// from the same mapper that builds the body. Exercise accepted streaming
/// requests, including the status displayed by the retry wrapper.
#[tokio::test]
async fn model_specific_levels_and_reports_match_the_http_contract() {
    const THREE: [&str; 5] = ["low", "medium", "high", "high", "high"];
    const FOUR: [&str; 5] = ["low", "medium", "high", "xhigh", "xhigh"];
    const FIVE: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
    const NO_XHIGH: [&str; 5] = ["low", "medium", "high", "high", "max"];
    const PRO_BUDGET: [&str; 5] = ["1024", "8192", "24576", "32768", "32768"];
    const FLASH_BUDGET: [&str; 5] = ["1024", "8192", "24576", "24576", "24576"];
    const NONE: [&str; 5] = [""; 5];
    let cases = [
        ("openai", "gpt-5.1", THREE),
        ("openai", "o3-mini", THREE),
        ("openai", "gpt-5.1-codex-max", FOUR),
        ("openai", "gpt-5.2-2025-12-11", FOUR),
        ("openai", "gpt-5.3-codex", FOUR),
        ("openai", "gpt-5.4-mini", FOUR),
        ("openai", "gpt-5.5", FOUR),
        ("openai", "gpt-5.6-sol", FIVE),
        ("openai", "gpt-5.6-terra", FIVE),
        ("openai", "gpt-6-astra", FIVE),
        ("openai", "gpt-6.1-sol", FIVE),
        ("openai", "gpt-5.20", THREE),
        ("openai", "local-model", NONE),
        ("anthropic", "claude-opus-4-5-20251101", THREE),
        ("anthropic", "claude-opus-4-6", NO_XHIGH),
        ("anthropic", "claude-sonnet-4-6", NO_XHIGH),
        ("anthropic", "claude-mythos-preview", NO_XHIGH),
        ("anthropic", "claude-opus-4-7", FIVE),
        ("anthropic", "claude-opus-4-8", FIVE),
        ("anthropic", "claude-opus-5-5", FIVE),
        ("anthropic", "claude-sonnet-5", FIVE),
        ("anthropic", "claude-fable-5-1", FIVE),
        ("anthropic", "claude-mythos-5-1", FIVE),
        ("anthropic", "claude-haiku-4-5", NONE),
        ("anthropic", "claude-opus-4-60", NONE),
        ("google", "gemini-2.5-pro", PRO_BUDGET),
        ("google", "gemini-2.5-flash", FLASH_BUDGET),
        ("google", "gemini-2.5-flash-lite-preview", FLASH_BUDGET),
        ("google", "gemini-robotics-er-1.6-preview", FLASH_BUDGET),
        ("google", "gemini-3-pro-preview", ["low", "high", "high", "high", "high"]),
        ("google", "gemini-3-flash-preview", THREE),
        ("google", "gemini-3.1-pro-preview", THREE),
        ("google", "gemini-3.5-flash", THREE),
        ("google", "gemini-3.8-flash", THREE),
        ("google", "gemini-robotics-er-2-preview", THREE),
        ("google", "gemini-3.1-flash-lite-image", ["minimal", "high", "high", "high", "high"]),
        ("google", "gemini-2.0-flash", NONE),
        ("google", "gemini-2.5-flash-image", NONE),
        ("google", "gemini-2.5-flash-preview-tts", NONE),
        ("google", "gemini-30-flash", NONE),
        ("google", "unknown-model", NONE),
    ];
    for (dialect, model, expected) in cases {
        for (effort, expected) in Effort::ALL.into_iter().zip(expected) {
            let response = match dialect {
                "anthropic" => ANTHROPIC,
                "google" => GOOGLE,
                _ => OPENAI,
            };
            let (url, seen) = server(vec![(200, response)]).await;
            let provider: Box<dyn Provider> = match dialect {
                "anthropic" => Box::new(Retrying::new(Box::new(
                    rook_llm::anthropic::Anthropic::new(
                        "anthropic/test",
                        model,
                        rook_llm::anthropic::Config::new(url, "k".into(), model),
                    )
                    .unwrap(),
                ))),
                "google" => Box::new(Retrying::new(Box::new(
                    rook_llm::google::Google::new(
                        "google/test",
                        model,
                        rook_llm::google::Config::new(url, "k".into(), model),
                    )
                    .unwrap(),
                ))),
                _ => Box::new(openai(url, model)),
            };
            assert_eq!(provider.takes_effort(), !expected.is_empty(), "{model}");
            let preview = provider.effort_use(effort);
            let report = observed(provider.as_ref(), effort).await;
            assert_eq!(report.applied, preview, "{model} {effort:?}");
            let bodies = seen.lock().unwrap();
            assert_eq!(bodies.len(), 1, "{model}: no retry should be needed");
            let body = &bodies[0];
            if expected.is_empty() {
                assert!(matches!(report.applied, EffortUse::Omitted { .. }), "{model}");
                assert!(body.get("reasoning_effort").is_none());
                assert!(body.get("output_config").is_none());
                assert!(body.pointer("/generationConfig/thinkingConfig").is_none());
                continue;
            }
            let EffortUse::Parameter { name, value } = report.applied else { panic!("{model}") };
            assert_eq!(value, expected, "{model} {effort:?}");
            let wire = body.pointer(&format!("/{}", name.replace('.', "/"))).unwrap();
            assert_eq!(
                wire.as_str().map(str::to_owned).unwrap_or_else(|| wire.to_string()),
                expected,
                "{model} {effort:?}"
            );
            if dialect == "openai" {
                assert!(body.get("max_tokens").is_none());
                assert!(body["max_completion_tokens"].is_number());
                assert!(body.get("temperature").is_none());
            }
            if dialect == "google" {
                let config = body.pointer("/generationConfig/thinkingConfig").unwrap();
                assert_eq!(config.as_object().unwrap().len(), 1, "only one thinking control: {model}");
                if expected.parse::<i32>().is_ok() {
                    assert!(wire.is_number(), "budgets are JSON numbers");
                    assert!(name.ends_with("thinkingBudget"));
                } else {
                    assert!(wire.is_string(), "levels are JSON strings");
                    assert!(name.ends_with("thinkingLevel"));
                }
            }
            if model == "claude-opus-4-5-20251101" {
                assert!(body.get("thinking").is_none(), "effort does not imply adaptive thinking");
            }
        }
    }
}

#[tokio::test]
async fn google_output_refusal_reduces_the_limit_without_losing_the_thinking_level() {
    let (url, seen) = server(vec![
        (400, r#"{"error":{"message":"maxOutputTokens exceeds output limit"}}"#),
        (200, GOOGLE),
        (200, GOOGLE),
    ])
    .await;
    let model = "gemini-3-flash-preview";
    let provider = Retrying::new(Box::new(
        rook_llm::google::Google::new(
            "google/test",
            model,
            rook_llm::google::Config::new(url, "k".into(), model),
        )
        .unwrap(),
    ));
    for _ in 0..2 {
        let report = observed(&provider, Effort::Max).await;
        assert_eq!(
            report.applied,
            EffortUse::Parameter {
                name: "generationConfig.thinkingConfig.thinkingLevel",
                value: "high".into(),
            }
        );
    }
    let bodies = seen.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    let initial = Request::new(Vec::new()).max_output_tokens;
    for (body, expected) in bodies.iter().zip([initial, initial / 2, initial / 2]) {
        assert_eq!(body["generationConfig"]["maxOutputTokens"], expected);
        assert_eq!(body["generationConfig"]["thinkingConfig"]["thinkingLevel"], "high");
        assert!(body["generationConfig"]["thinkingConfig"].get("thinkingBudget").is_none());
    }
}
