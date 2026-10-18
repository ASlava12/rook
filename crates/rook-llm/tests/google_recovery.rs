//! Native HTTP failures must not replay an answer or lose failed-attempt usage.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use futures_util::StreamExt;
use rook_llm::{
    Attempt, AttemptFacts, AttemptObserver, AttemptStatus, Delta, LlmError, Message, Provider, Request,
    StopReason,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<Vec<Value>>, stream: bool) -> Self {
        assert!(!replies.is_empty());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1beta", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let saved = requests.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                assert!(n < 8, "recovery must remain bounded");
                let mut raw = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let read = tokio::time::timeout(Duration::from_secs(10), socket.read(&mut chunk))
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(read > 0 && raw.len() + read <= 1024 * 1024);
                    raw.extend_from_slice(&chunk[..read]);
                    let Some(head_end) = raw.windows(4).position(|p| p == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&raw[..head_end]);
                    let size: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse().unwrap())
                        })
                        .unwrap();
                    if raw.len() < head_end + 4 + size {
                        continue;
                    }
                    saved
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&raw[head_end + 4..head_end + 4 + size]).unwrap());
                    break;
                }
                let reply = &replies[n.min(replies.len() - 1)];
                let body = if stream {
                    reply.iter().map(|v| format!("data: {v}\n\n")).collect::<String>()
                } else {
                    assert_eq!(reply.len(), 1);
                    reply[0].to_string()
                };
                let mime = if stream { "text/event-stream" } else { "application/json" };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(wire.as_bytes()).await.unwrap();
            }
        });
        Self { url, requests, count, task }
    }
    fn provider(&self) -> rook_llm::retry::Retrying {
        let mut config =
            rook_llm::google::Config::new(self.url.clone(), "owned-fixture-key".into(), "gemini-2.5-flash");
        config.proxy = rook_llm::Proxy::Direct;
        config.stream_idle_timeout = Duration::from_secs(10);
        rook_llm::retry::Retrying::new(Box::new(
            rook_llm::google::Google::new("google/recovery", "gemini-2.5-flash", config).unwrap(),
        ))
    }
}

#[derive(Clone, Default)]
struct Receipts(Arc<Mutex<Vec<(AttemptStatus, AttemptFacts)>>>, Arc<tokio::sync::Notify>);
impl AttemptObserver for Receipts {
    fn start(&self, _: Option<&rook_llm::Dispatch>) -> rook_llm::Result<Box<dyn Attempt>> {
        Ok(Box::new(self.clone()))
    }
}
impl Attempt for Receipts {
    fn finish(&mut self, status: AttemptStatus, facts: &AttemptFacts) -> rook_llm::Result<()> {
        self.0.lock().unwrap().push((status, facts.clone()));
        self.1.notify_one();
        Ok(())
    }
}
fn request() -> Request {
    Request::new(vec![Message::user("Answer the saved task")])
}
fn malformed() -> Value {
    json!({"candidates":[{"content":{"parts":[{"text":"failed frame must not escape"},{"functionCall":{"name":"write_file","args":{"path":"never"}}}]},"finishReason":"MALFORMED_RESPONSE"}],"usageMetadata":{"promptTokenCount":17,"candidatesTokenCount":3}})
}
fn answered() -> Value {
    json!({"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":18,"candidatesTokenCount":4}})
}
fn assert_retry(f: &Fixture, receipts: &Receipts) {
    assert_eq!(f.count.load(Ordering::SeqCst), 2);
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1], "recovery preserves the original task and tools");
    let ends = receipts.0.lock().unwrap();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0].0, AttemptStatus::Failed);
    assert!(ends[0].1.usage_reported);
    assert!(!ends[0].1.completion_confirmed);
    let usage = ends[0].1.usage.as_ref().unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (17, 3));
    assert_eq!(ends[1].0, AttemptStatus::Completed);
}

#[tokio::test]
async fn a_buffered_malformed_generation_retries_without_delivering_its_calls_and_retains_usage() {
    let f = Fixture::new(vec![vec![malformed()], vec![answered()]], false).await;
    let receipts = Receipts::default();
    let answer = f.provider().complete_observed(request(), Arc::new(receipts.clone())).await.unwrap();
    assert_eq!(answer.response.message.content, "ok");
    assert!(answer.response.message.tool_calls.is_empty());
    assert_retry(&f, &receipts);
}

#[tokio::test]
async fn a_stream_retries_only_when_the_malformed_frame_precedes_all_delivered_content() {
    let f = Fixture::new(vec![vec![malformed()], vec![answered()]], true).await;
    let receipts = Receipts::default();
    let mut stream = f.provider().stream_observed(request(), Arc::new(receipts.clone())).await.unwrap();
    let mut text = String::new();
    let mut completed = false;
    while let Some(delta) = stream.next().await {
        match delta.unwrap() {
            Delta::Text(s) => text.push_str(&s),
            Delta::ToolCall(_) => panic!("a malformed frame cannot expose a tool call"),
            Delta::Done { .. } => completed = true,
            _ => {}
        }
    }
    assert_eq!(text, "ok");
    assert!(completed);
    assert_retry(&f, &receipts);
}

#[tokio::test]
async fn delivered_text_reasoning_or_a_call_prevents_automatic_replay_after_malformed_output() {
    for part in [
        json!({"text":"partial"}),
        json!({"thought":true,"text":"partial"}),
        json!({"functionCall":{"name":"read_file","args":{}}}),
    ] {
        let f = Fixture::new(
            vec![vec![json!({"candidates":[{"content":{"parts":[part]}}]}), malformed()], vec![answered()]],
            true,
        )
        .await;
        let receipts = Receipts::default();
        let mut stream = f.provider().stream_observed(request(), Arc::new(receipts.clone())).await.unwrap();
        let mut delivered = 0;
        let mut failed = false;
        while let Some(delta) = stream.next().await {
            match delta {
                Ok(Delta::Text(_) | Delta::Reasoning(_) | Delta::ToolCall(_)) => delivered += 1,
                Ok(Delta::Done { .. }) => panic!("malformed output is not a completion"),
                Err(LlmError::IncompleteResponse { .. }) => failed = true,
                Err(error) => panic!("unexpected failure: {error}"),
                _ => {}
            }
        }
        assert_eq!(delivered, 1, "only the earlier valid frame is delivered");
        assert!(failed);
        assert_eq!(f.count.load(Ordering::SeqCst), 1, "a partial answer is never replayed");
        let ends = receipts.0.lock().unwrap();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].0, AttemptStatus::Failed);
        assert_eq!(ends[0].1.usage.as_ref().unwrap().output_tokens, 3);
        assert!(!ends[0].1.completion_confirmed);
    }
}

#[tokio::test]
async fn repeatedly_malformed_streams_stop_at_the_shared_attempt_limit_without_completion() {
    let f = Fixture::new(vec![vec![malformed()]], true).await;
    let receipts = Receipts::default();
    let mut stream = f.provider().stream_observed(request(), Arc::new(receipts.clone())).await.unwrap();
    let mut failed = false;
    while let Some(delta) = stream.next().await {
        match delta {
            Err(LlmError::IncompleteResponse { .. }) => failed = true,
            Ok(Delta::Dispatch(_) | Delta::Effort(_)) => {}
            other => panic!("no response content or completion may escape: {other:?}"),
        }
    }
    assert!(failed);
    assert_eq!(f.count.load(Ordering::SeqCst), 4);
    let ends = receipts.0.lock().unwrap();
    assert_eq!(ends.len(), 4);
    assert!(ends.iter().all(|(status, facts)| *status == AttemptStatus::Failed
        && !facts.completion_confirmed
        && facts.usage_reported));
}

#[tokio::test]
async fn omitted_usage_is_unknown_and_a_safety_stop_is_never_retried() {
    let mut bad = malformed();
    bad["usageMetadata"] = json!({"promptTokenCount":17});
    let f = Fixture::new(vec![vec![bad]], false).await;
    let receipts = Receipts::default();
    // The direct adapter makes a single observed attempt, without the wrapper.
    let mut config =
        rook_llm::google::Config::new(f.url.clone(), "owned-fixture-key".into(), "gemini-2.5-flash");
    config.proxy = rook_llm::Proxy::Direct;
    let p = rook_llm::google::Google::new("google/recovery", "gemini-2.5-flash", config).unwrap();
    assert!(matches!(
        p.complete_observed(request(), Arc::new(receipts.clone())).await,
        Err(LlmError::IncompleteResponse { usage: None, .. })
    ));
    {
        let ends = receipts.0.lock().unwrap();
        assert_eq!(ends.len(), 1);
        assert!(ends[0].1.usage.is_none());
        assert!(!ends[0].1.usage_reported && !ends[0].1.completion_confirmed);
    }

    for finish in [
        "SAFETY",
        "RECITATION",
        "LANGUAGE",
        "BLOCKLIST",
        "PROHIBITED_CONTENT",
        "SPII",
        "IMAGE_SAFETY",
        "IMAGE_PROHIBITED_CONTENT",
        "IMAGE_RECITATION",
        "ESCALATION",
        "PUP_LIMITED_DISABLED",
    ] {
        let f = Fixture::new(vec![vec![json!({"candidates":[{"finishReason":finish}]})]], false).await;
        let answer = f.provider().complete(request()).await.unwrap();
        assert_eq!(answer.stop_reason, StopReason::Refusal, "{finish} is a refusal");
        assert_eq!(f.count.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn refused_or_invalid_function_call_finishes_never_expose_calls_or_retry() {
    for finish in [
        "SAFETY",
        "RECITATION",
        "LANGUAGE",
        "BLOCKLIST",
        "PROHIBITED_CONTENT",
        "SPII",
        "IMAGE_SAFETY",
        "IMAGE_PROHIBITED_CONTENT",
        "IMAGE_RECITATION",
        "ESCALATION",
        "PUP_LIMITED_DISABLED",
        "MALFORMED_FUNCTION_CALL",
        "UNEXPECTED_TOOL_CALL",
        "TOO_MANY_TOOL_CALLS",
        "MISSING_THOUGHT_SIGNATURE",
    ] {
        let mut bad = malformed();
        bad["candidates"][0]["finishReason"] = json!(finish);
        for streaming in [false, true] {
            let f = Fixture::new(vec![vec![bad.clone()], vec![answered()]], streaming).await;
            let result = if streaming {
                let mut stream = f.provider().stream(request()).await.unwrap();
                let mut failed = false;
                while let Some(delta) = stream.next().await {
                    match delta {
                        Ok(Delta::Dispatch(_) | Delta::Effort(_)) => {}
                        Err(_) => failed = true,
                        other => panic!("{finish} cannot expose a call or completion: {other:?}"),
                    }
                }
                failed
            } else {
                f.provider().complete(request()).await.is_err()
            };
            assert!(result, "{finish} with a call is not an executable generation");
            assert_eq!(f.count.load(Ordering::SeqCst), 1, "{finish} must not be retried");
        }
    }
}

#[tokio::test]
async fn a_later_safety_finish_aborts_a_stream_with_previously_delivered_calls() {
    let f = Fixture::new(
        vec![vec![
            json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"write_file","args":{}}}]}}]}),
            json!({"candidates":[{"finishReason":"SAFETY"}]}),
        ]],
        true,
    )
    .await;
    let mut stream = f.provider().stream(request()).await.unwrap();
    let mut calls = 0;
    let mut failed = false;
    while let Some(delta) = stream.next().await {
        match delta {
            Ok(Delta::ToolCall(_)) => calls += 1,
            Ok(Delta::Dispatch(_) | Delta::Effort(_)) => {}
            Err(_) => failed = true,
            other => panic!("a refusal must prevent dispatch of preceding calls: {other:?}"),
        }
    }
    assert_eq!(calls, 1, "the call preceded the refused finish");
    assert!(failed);
    assert_eq!(f.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_the_stream_during_backoff_never_starts_a_detached_attempt() {
    let f = Fixture::new(vec![vec![malformed()], vec![answered()]], true).await;
    let receipts = Receipts::default();
    let mut stream = f.provider().stream_observed(request(), Arc::new(receipts.clone())).await.unwrap();
    // Wait for an observed failed attempt, rather than guessing how quickly
    // the local HTTP fixture responds on a loaded CI worker.
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = receipts.1.notified() => {},
            _ = async {
                while let Some(delta) = stream.next().await {
                    match delta.unwrap() {
                        Delta::Dispatch(_) | Delta::Effort(_) => {},
                        other => panic!("unexpected response during backoff: {other:?}"),
                    }
                }
                panic!("recovery ended before backoff");
            } => {
                panic!("stream ended before its failure was observed");
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(f.count.load(Ordering::SeqCst), 1);
    drop(stream);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(f.count.load(Ordering::SeqCst), 1);
}
