//! Native wire capture, including failures, retries and a dropped stream.
use rook_llm::{
    Attempt, AttemptFacts, AttemptObserver, AttemptStatus, Message, Provider, Request, TraceContext,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEY: &str = "private-provider-credential-42";
#[derive(Default)]
struct Log {
    starts: AtomicUsize,
    ended: Mutex<Vec<(AttemptStatus, AttemptFacts)>>,
}
struct Recorder {
    log: Arc<Log>,
    propagate: bool,
}
struct Receipt {
    log: Arc<Log>,
    span: String,
    propagate: bool,
}
impl AttemptObserver for Recorder {
    fn start(&self, _: Option<&rook_llm::Dispatch>) -> rook_llm::Result<Box<dyn Attempt>> {
        let n = self.log.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Box::new(Receipt { log: self.log.clone(), span: format!("{n:016x}"), propagate: self.propagate }))
    }
}
impl Attempt for Receipt {
    fn trace_context(&self) -> Option<(TraceContext, bool)> {
        Some((TraceContext::bounded(&"a".repeat(32), &self.span, None).unwrap(), self.propagate))
    }
    fn finish(&mut self, status: AttemptStatus, facts: &AttemptFacts) -> rook_llm::Result<()> {
        self.log.ended.lock().unwrap().push((status, facts.clone()));
        Ok(())
    }
}
struct Server {
    url: String,
    seen: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new(dialect: &str, failures: usize, stream: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let dialect = dialect.to_owned();
        let task = tokio::spawn(async move {
            let mut count = 0;
            while let Ok((mut socket, _)) = listener.accept().await {
                count += 1;
                assert!(count <= 16);
                let mut raw = Vec::new();
                let (head, body) = loop {
                    let mut part = [0; 4096];
                    let n = socket.read(&mut part).await.unwrap();
                    assert!(n > 0 && raw.len() + n <= 1024 * 1024);
                    raw.extend_from_slice(&part[..n]);
                    let Some(split) = raw.windows(4).position(|p| p == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
                    let size: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|n| n.trim().parse().unwrap())
                        })
                        .unwrap();
                    if raw.len() < split + 4 + size {
                        continue;
                    }
                    break (head, serde_json::from_slice(&raw[split + 4..split + 4 + size]).unwrap());
                };
                log.lock().unwrap().push((head, body));
                if dialect == "closed" {
                    continue;
                }
                let status = if count <= failures { 503 } else { 200 };
                let body = if status != 200 {
                    r#"{"error":{"message":"temporarily overloaded"}}"#.to_owned()
                } else if stream {
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into()
                } else {
                    match dialect.as_str() {
                    "openai" => r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
                    "responses" => r#"{"id":"resp_1","status":"completed","model":"test","output":[{"type":"message","id":"m1","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
                    "anthropic" => r#"{"id":"msg_1","type":"message","role":"assistant","model":"test","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#,
                    "google" => r#"{"candidates":[{"content":{"parts":[{"text":"ok"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1},"modelVersion":"test"}"#,
                    _ => unreachable!(),
                }.to_owned()
                };
                let mime = if stream { "text/event-stream" } else { "application/json" };
                let oversized = "x".repeat(129);
                assert!(oversized.len() > 128);
                let reply = format!(
                    "HTTP/1.1 {status} Reply\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nx-request-id: req-{count}\r\nrequest-id: {KEY}\r\ncf-ray: {oversized}\r\nSet-Cookie: private-cookie\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        Self { url, seen, task }
    }
}
fn provider(dialect: &str, url: &str) -> Box<dyn Provider> {
    match dialect {
        "openai" | "responses" => {
            let mut config = rook_llm::openai::Config::new(url.into(), Some(KEY.into()), 32000);
            config.proxy = rook_llm::Proxy::Direct;
            if dialect == "openai" {
                Box::new(rook_llm::openai::OpenAiCompatible::new("local", "qwen", config).unwrap())
            } else {
                Box::new(rook_llm::openai::responses::Responses::new("local", "gpt-6.1", config).unwrap())
            }
        }
        "anthropic" => {
            let mut config = rook_llm::anthropic::Config::new(url.into(), KEY.into(), "claude");
            config.proxy = rook_llm::Proxy::Direct;
            Box::new(rook_llm::anthropic::Anthropic::new("local", "claude", config).unwrap())
        }
        "google" => {
            let mut config = rook_llm::google::Config::new(url.into(), KEY.into(), "gemini-flash");
            config.proxy = rook_llm::Proxy::Direct;
            Box::new(rook_llm::google::Google::new("local", "gemini-flash", config).unwrap())
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn native_dialects_capture_bounded_response_ids_and_only_propagate_when_enabled() {
    for dialect in ["openai", "responses", "anthropic", "google"] {
        for propagate in [false, true] {
            let server = Server::new(dialect, 0, false).await;
            let log = Arc::new(Log::default());
            let observer = Arc::new(Recorder { log: log.clone(), propagate });
            provider(dialect, &server.url)
                .complete_observed(Request::new(vec![Message::user("private-prompt")]), observer)
                .await
                .unwrap();
            let ended = log.ended.lock().unwrap();
            assert_eq!(ended.len(), 1);
            assert_eq!(
                ended[0].1.provider_request_ids,
                std::collections::BTreeMap::from([("x-request-id".into(), "req-1".into())])
            );
            let seen = server.seen.lock().unwrap();
            assert_eq!(seen[0].0.to_lowercase().contains("traceparent:"), propagate, "{dialect}");
            assert!(!seen[0].1.to_string().contains("trace_id"));
            assert!(!format!("{:?}", ended[0].1).contains(KEY));
        }
    }
}

#[tokio::test]
async fn physical_retries_have_distinct_spans_and_dropping_a_stream_keeps_its_response_id() {
    let server = Server::new("openai", 1, false).await;
    let log = Arc::new(Log::default());
    let observer = Arc::new(Recorder { log: log.clone(), propagate: true });
    let retrying = rook_llm::retry::Retrying::new(provider("openai", &server.url));
    retrying.complete_observed(Request::new(vec![Message::user("retry")]), observer).await.unwrap();
    {
        let ended = log.ended.lock().unwrap();
        assert_eq!(ended.len(), 2);
        assert_eq!((ended[0].0, ended[1].0), (AttemptStatus::Failed, AttemptStatus::Completed));
        assert_eq!(ended[0].1.provider_request_ids["x-request-id"], "req-1");
        assert_eq!(ended[1].1.provider_request_ids["x-request-id"], "req-2");
        let seen = server.seen.lock().unwrap();
        let traces: Vec<_> = seen
            .iter()
            .map(|(head, _)| head.lines().find(|l| l.starts_with("traceparent:")).unwrap())
            .collect();
        assert_ne!(traces[0], traces[1]);
    }

    let server = Server::new("openai", 0, true).await;
    let log = Arc::new(Log::default());
    let observer = Arc::new(Recorder { log: log.clone(), propagate: false });
    let provider = provider("openai", &server.url);
    let stream =
        provider.stream_observed(Request::new(vec![Message::user("cancel")]), observer).await.unwrap();
    drop(stream);
    let ended = log.ended.lock().unwrap();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].0, AttemptStatus::Interrupted);
    assert_eq!(ended[0].1.provider_request_ids["x-request-id"], "req-1");
}

#[tokio::test]
async fn failover_keeps_the_actual_leaf_attempt_and_its_provider_request_id() {
    let failed = Server::new("closed", 0, false).await;
    let answered = Server::new("openai", 0, false).await;
    let endpoint = |server: &Server| rook_llm::Endpoint {
        name: format!("correlation:{}", server.url),
        api: rook_llm::Api::OpenAi,
        metadata_api: rook_llm::MetadataApi::Auto,
        assumed_context_window: Some(32000),
        url: server.url.clone(),
        key: Some(KEY.into()),
        model: "qwen".into(),
        context_window: Some(32000),
        parallel: None,
        key_in_the_clear: false,
        proxy: rook_llm::Proxy::Direct,
        queue: None,
    };
    let provider = rook_llm::from_endpoints_with(
        vec![endpoint(&failed), endpoint(&answered)],
        std::time::Duration::from_secs(5),
        rook_llm::Prefer::AsConfigured,
    )
    .unwrap();
    let log = Arc::new(Log::default());
    provider
        .complete_observed(
            Request::new(vec![Message::user("fail over")]),
            Arc::new(Recorder { log: log.clone(), propagate: true }),
        )
        .await
        .unwrap();
    let ended = log.ended.lock().unwrap();
    assert_eq!(ended.len(), 2);
    assert_eq!(ended[0].0, AttemptStatus::Failed);
    assert!(ended[0].1.provider_request_ids.is_empty());
    assert_eq!(ended[1].0, AttemptStatus::Completed);
    assert_eq!(ended[1].1.provider_request_ids["x-request-id"], "req-1");
    assert_eq!(answered.seen.lock().unwrap().len(), 1);
}
