//! Asking again when the provider said "not now".
//!
//! A turn that has been running for minutes should not end because the endpoint
//! was busy for a second, and it should not sit there re-asking a question that
//! will be answered the same way every time.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rook_llm::openai::{Config, OpenAiCompatible};
use rook_llm::retry::Retrying;
use rook_llm::{LlmError, Provider, Request};

struct CreditStream {
    seen: Arc<std::sync::Mutex<Vec<u32>>>,
    opening_error: bool,
    partial: bool,
}

#[async_trait::async_trait]
impl Provider for CreditStream {
    fn id(&self) -> &str {
        "credit-stream"
    }
    fn context_window(&self) -> usize {
        8192
    }
    fn dispatch_identity(&self) -> Option<rook_llm::Dispatch> {
        rook_llm::Dispatch::bounded("credit-stream", "m", true)
    }
    async fn complete(&self, _: Request) -> rook_llm::Result<rook_llm::Response> {
        Err(LlmError::Other("stream fixture only".into()))
    }
    async fn stream(&self, request: Request) -> rook_llm::Result<rook_llm::ResponseStream> {
        let first = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(request.max_output_tokens);
            seen.len() == 1
        };
        let error =
            LlmError::Status { status: 402, body: "can only afford 2048 tokens".into(), retry_after: None };
        if first && self.opening_error {
            return Err(error);
        }
        let mut deltas = Vec::new();
        if first {
            if self.partial {
                deltas.push(Ok(rook_llm::Delta::Text("partial".into())));
            }
            deltas.push(Err(error));
        } else {
            deltas.push(Ok(rook_llm::Delta::Text("answered".into())));
            deltas.push(Ok(rook_llm::Delta::Done {
                stop_reason: rook_llm::StopReason::EndTurn,
                usage: Default::default(),
                model: "m".into(),
            }));
        }
        Ok(Box::pin(futures_util::stream::iter(deltas)))
    }
}

#[tokio::test]
async fn credit_refusals_retry_at_stream_open_or_first_delta_but_never_after_text() {
    use futures_util::StreamExt;
    for (opening_error, partial) in [(true, false), (false, false), (false, true)] {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = Retrying::new(Box::new(CreditStream { seen: seen.clone(), opening_error, partial }));
        let mut stream = provider.stream(Request::new(Vec::new())).await.unwrap();
        let mut text = String::new();
        let mut failed = false;
        while let Some(delta) = stream.next().await {
            match delta {
                Ok(rook_llm::Delta::Text(value)) => text.push_str(&value),
                Err(LlmError::Status { status: 402, .. }) => failed = true,
                Err(error) => panic!("unexpected stream error: {error}"),
                _ => {}
            }
        }
        assert_eq!(failed, partial);
        assert_eq!(text, if partial { "partial" } else { "answered" });
        assert_eq!(*seen.lock().unwrap(), if partial { vec![4096] } else { vec![4096, 1984] });
    }
}

#[tokio::test]
async fn a_credit_limited_refusal_retries_once_without_remembering_the_balance() {
    let (url, sent) = recording(vec![(
        "402 Payment Required",
        r#"{"error":{"message":"Requested 20000 tokens, but can only afford 7,117. Add credits."}}"#,
    )])
    .await;
    let provider = provider(url);
    let mut request = Request::new(Vec::new());
    request.max_output_tokens = 20_000;
    provider.complete(request.clone()).await.unwrap();
    provider.complete(request).await.unwrap();
    let bodies = sent.lock().unwrap();
    let caps: Vec<_> = bodies
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body).unwrap()["max_tokens"].as_u64().unwrap())
        .collect();
    assert_eq!(caps, [20_000, 7053, 20_000], "credit is not a permanent model ceiling");
}

#[tokio::test]
async fn a_second_credit_refusal_is_returned_without_another_budget_retry() {
    let (url, sent) = recording(vec![
        ("402 Payment Required", r#"{"error":{"message":"can only afford 2048 tokens"}}"#),
        ("402 Payment Required", r#"{"error":{"message":"can only afford 1500 tokens"}}"#),
    ])
    .await;
    let error = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();
    assert!(matches!(error, LlmError::Status { status: 402, .. }));
    assert_eq!(sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn payment_refusals_without_a_useful_smaller_integer_budget_are_not_retried() {
    for message in [
        "insufficient funds",
        "can only afford 100 tokens",
        "can only afford 1087 tokens",
        "can only afford 4096 tokens",
        "can only afford 999999999999999999 tokens",
        "can only afford 2,04 tokens",
        "can only afford 2000.5 tokens",
        "can only afford 2000abc",
    ] {
        let body = serde_json::json!({"error":{"message":message}}).to_string();
        let (url, sent) = recording(vec![("402 Payment Required", body)]).await;
        let error = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();
        assert!(matches!(error, LlmError::Status { status: 402, .. }), "{message}");
        assert_eq!(sent.lock().unwrap().len(), 1, "{message}");
    }
}

#[tokio::test]
async fn an_affordable_budget_in_a_non_payment_refusal_does_not_trigger_credit_recovery() {
    let (url, sent) =
        recording(vec![("400 Bad Request", r#"{"error":{"message":"can only afford 2048 tokens"}}"#)]).await;
    assert!(matches!(
        provider(url).complete(Request::new(Vec::new())).await,
        Err(LlmError::Status { status: 400, .. })
    ));
    assert_eq!(sent.lock().unwrap().len(), 1);
}

/// Answers with `refusals` in order, then serves a real reply for ever after.
/// Returns the endpoint and a counter of how many requests it saw.
async fn flaky(refusals: Vec<&'static str>) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = seen.clone();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 8192];
            let _ = socket.read(&mut scratch).await;
            let n = counted.fetch_add(1, Ordering::SeqCst);
            let (status, body) = match refusals.get(n) {
                Some(status) => (*status, r#"{"error":"not now"}"#),
                None => (
                    "200 OK",
                    r#"{"choices":[{"message":{"role":"assistant","content":"answered"},
                        "finish_reason":"stop"}],"model":"m"}"#,
                ),
            };
            // This answers one request per connection and then drops it, and a
            // client not told so is entitled to reuse the socket and find it
            // gone — which a retry does by definition.
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn provider(url: String) -> Retrying {
    let inner = OpenAiCompatible::new("test/model", "m", Config::new(url, None, 8192)).unwrap();
    Retrying::new(Box::new(inner))
}

#[tokio::test]
async fn an_overloaded_provider_is_waited_out_rather_than_ending_the_turn() {
    let (url, seen) = flaky(vec!["429 Too Many Requests", "503 Service Unavailable"]).await;

    let answered = provider(url).complete(Request::new(Vec::new())).await;

    assert_eq!(seen.load(Ordering::SeqCst), 3, "it has to have been refused twice to test anything");
    let answered = answered.expect("a rate limit means later, not no");
    assert_eq!(answered.message.content, "answered");
}

#[tokio::test]
async fn a_refusal_that_will_not_change_is_reported_at_once() {
    let (url, seen) = flaky(vec!["400 Bad Request"; 8]).await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();

    assert_eq!(seen.load(Ordering::SeqCst), 1, "asking a 400 again only delays the message");
    match refused {
        LlmError::Status { status, .. } => assert_eq!(status, 400),
        other => panic!("the status has to survive the wrapper: {other}"),
    }
}

/// The point of a ceiling: an endpoint that is down stays down, and a turn that
/// waits for ever is worse than one that says so.
#[tokio::test]
async fn an_endpoint_that_stays_down_gives_up_and_says_which_status() {
    let (url, seen) = flaky(vec!["529 Overloaded"; 16]).await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();

    let tries = seen.load(Ordering::SeqCst);
    assert!(tries > 1, "it has to have tried again for the ceiling to be the thing that stopped it");
    assert!(tries <= 4, "and stopped at the ceiling rather than going on: {tries} tries");
    assert!(refused.to_string().contains("529"), "the last status is the real one: {refused}");
}

/// Answers each request with the next entry, then serves a real reply for ever
/// after. Keeps every request body, so a test can say what was actually sent.
async fn recording(
    refusals: Vec<(&'static str, impl Into<String>)>,
) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let refusals: Vec<_> = refusals.into_iter().map(|(status, body)| (status, body.into())).collect();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let sent: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let kept = sent.clone();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = vec![0u8; 16 << 10];
            let read = socket.read(&mut scratch).await.unwrap_or(0);
            let whole = String::from_utf8_lossy(&scratch[..read]).into_owned();
            let body = whole.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
            let n = {
                let mut kept = kept.lock().unwrap();
                kept.push(body);
                kept.len() - 1
            };
            let (status, body) = match refusals.get(n) {
                Some((status, body)) => (*status, (*body).to_string()),
                None => (
                    "200 OK",
                    r#"{"choices":[{"message":{"role":"assistant","content":"answered"},
                        "finish_reason":"stop"}],"model":"m"}"#
                        .to_string(),
                ),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{addr}/v1"), sent)
}

/// A model name is what decides whether the effort is sent, so the test needs
/// one this dialect maps.
fn reasoning_provider(url: String) -> Retrying {
    let inner = OpenAiCompatible::new("test/gpt-5-mini", "gpt-5-mini", Config::new(url, None, 8192)).unwrap();
    Retrying::new(Box::new(inner))
}

fn thinking_hard() -> Request {
    let mut request = Request::new(Vec::new());
    request.effort = Some(rook_llm::Effort::High);
    request
}

/// A route that will not take the effort refuses the whole request, and a turn
/// that has been running for minutes ends on a field the user never set. The
/// same request answers the same for ever; a request without that field need
/// not, so it is asked once more.
#[tokio::test]
async fn an_effort_the_endpoint_refuses_is_dropped_and_the_request_asked_again() {
    let (url, sent) = recording(vec![(
        "400 Bad Request",
        r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort'"}}"#,
    )])
    .await;

    let answered = reasoning_provider(url).complete(thinking_hard()).await;

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2, "asked again, once: {sent:?}");
    assert!(sent[0].contains("reasoning_effort"), "the precondition: the first request carried it");
    assert!(!sent[1].contains("reasoning_effort"), "and the second did not: {}", sent[1]);
    assert_eq!(answered.expect("the second request is the answer").message.content, "answered");
}

/// One refusal per process, not one per step: the endpoint's own answer
/// overrides the guess the model name made, for the rest of the session.
#[tokio::test]
async fn an_endpoint_that_refused_the_effort_is_not_asked_with_it_again() {
    let (url, sent) = recording(vec![(
        "400 Bad Request",
        r#"{"error":{"message":"Reasoning is mandatory for this endpoint and cannot be disabled"}}"#,
    )])
    .await;
    let provider = reasoning_provider(url);

    provider.complete(thinking_hard()).await.expect("the retry answers");
    provider.complete(thinking_hard()).await.expect("and so does the next turn");

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 3, "one refusal, its retry, and a later turn: {}", sent.len());
    assert!(!sent[2].contains("reasoning_effort"), "the later turn pays nothing for it: {}", sent[2]);
}

/// Asking for the room the window leaves is the default, and an endpoint with
/// a ceiling of its own refuses it. What that ceiling is appears in no API and
/// differs per model, so it is learned from the refusal: asked for half, and
/// remembered, rather than guessed from a name or kept in a table.
#[tokio::test]
async fn a_reply_too_large_for_the_endpoint_is_asked_for_smaller() {
    let (url, sent) = recording(vec![(
        "400 Bad Request",
        r#"{"error":{"message":"max_tokens: 20000 > 8192, which is the maximum for this model"}}"#,
    )])
    .await;
    let provider = provider(url);
    let mut asking = Request::new(Vec::new());
    asking.max_output_tokens = 20_000;

    provider.complete(asking.clone()).await.expect("the smaller request answers");
    provider.complete(asking).await.expect("and so does the next turn");

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 3, "the refusal, its retry, and a later turn: {}", sent.len());
    assert!(sent[0].contains("20000"), "the precondition: it asked for the room it had");
    assert!(sent[1].contains("10000"), "halved, because the endpoint said too big and not what fits");
    // One refusal per process: a later turn pays nothing to find out again.
    assert!(!sent[2].contains("20000"), "the ceiling is remembered: {}", sent[2]);
}

/// A 400 about anything else is still a refusal that will not change, and one
/// naming the effort on a request that never had one cannot loop.
#[tokio::test]
async fn a_refusal_that_names_the_effort_without_one_sent_is_reported_at_once() {
    let (url, sent) = recording(vec![("400 Bad Request", r#"{"error":"reasoning is mandatory"}"#); 8]).await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();

    assert_eq!(sent.lock().unwrap().len(), 1, "there was nothing to drop, so nothing to ask again");
    assert!(refused.to_string().contains("400"), "{refused}");
}

/// Tool definitions are the other thing the agent adds rather than the user.
/// Dropping them would leave an agent that cannot act and does not say why, so
/// the message names the setting that puts them in the prompt instead.
#[tokio::test]
async fn a_refusal_about_tools_names_the_setting_that_answers_it() {
    let (url, _) = recording(vec![
        (
            "400 Bad Request",
            r#"{"error":{"message":"this endpoint does not support the tools parameter"}}"#,
        );
        4
    ])
    .await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err().to_string();

    assert!(refused.contains("native_tools = false"), "the message has to say what to do: {refused}");
}

/// Answers 429 with the wait it wants, then a real reply. The header is what
/// this is about, so the server sends it and nothing else does.
async fn paced(
    header: impl Fn() -> String + Send + 'static,
) -> (String, Arc<std::sync::Mutex<Vec<std::time::SystemTime>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counted = seen.clone();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 8192];
            let _ = socket.read(&mut scratch).await;
            let n = {
                let mut seen = counted.lock().unwrap();
                assert!(seen.len() < 8, "the fixture admits at most eight requests");
                seen.push(std::time::SystemTime::now());
                seen.len() - 1
            };
            let response = match n {
                0 => {
                    let seconds = header();
                    let body = r#"{"error":"slow down"}"#;
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\n\
                         Retry-After: {seconds}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                _ => {
                    let body = r#"{"choices":[{"message":{"role":"assistant","content":"answered"},
                        "finish_reason":"stop"}],"model":"m"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
            };
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{addr}/v1"), seen)
}

/// A rate limiter that names its window has given the only number worth
/// waiting. Doubling from a second spends every try inside that window and
/// ends the turn on a refusal the server had already explained how to avoid.
#[tokio::test]
async fn the_wait_is_the_one_the_provider_asked_for() {
    let (url, seen) = paced(|| "2".into()).await;

    let started = std::time::Instant::now();
    let answered = provider(url).complete(Request::new(Vec::new())).await;
    let waited = started.elapsed();

    assert_eq!(seen.lock().unwrap().len(), 2, "asked again after waiting");
    assert_eq!(answered.expect("the second request answers").message.content, "answered");
    // A sleep does not finish early, so the floor is the assertion: without
    // reading the header the first wait is one second.
    assert!(waited >= std::time::Duration::from_secs(2), "waited only {waited:?}");
}

#[tokio::test]
async fn a_real_http_date_refusal_is_not_retried_before_its_deadline() {
    use std::time::{Duration, SystemTime};
    let deadline = Arc::new(std::sync::Mutex::new(None));
    let target = deadline.clone();
    let (url, seen) = paced(move || {
        // Construct the date at refusal time: runner delays before the request
        // cannot consume the waiting interval this test is meant to exercise.
        let header = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(4));
        *target.lock().unwrap() = Some(httpdate::parse_http_date(&header).unwrap());
        header
    })
    .await;
    let answered = provider(url).complete(Request::new(Vec::new())).await.unwrap();
    assert_eq!(answered.message.content, "answered");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let deadline = deadline.lock().unwrap().unwrap();
    assert!(seen[0] < deadline, "the refusal must actually specify a future deadline");
    assert!(
        seen[1] >= deadline,
        "the second physical request arrived before Retry-After: {seen:?}, deadline {deadline:?}"
    );
}

/// Answers the same status and body for ever, counting the asks.
async fn always(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 8192];
            let _ = socket.read(&mut scratch).await;
            counted.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{addr}/v1"), seen)
}

/// Ported from openclaw #140991: a gateway between here and the model answers
/// its upstream's 400 with a 500, and a status-only reading of that sends the
/// same wrong request four times with a backoff between them before the message
/// that names what is wrong arrives. The code in the body is a verdict whatever
/// carried it.
#[tokio::test]
async fn a_deterministic_refusal_wrapped_in_a_gateway_error_is_not_asked_again() {
    let (url, seen) = always(
        "502 Bad Gateway",
        r#"{"error":{"type":"invalid_request_error","message":"unknown parameter: reasoning"}}"#,
    )
    .await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();

    assert_eq!(seen.load(Ordering::SeqCst), 1, "the same wrong request was sent again");
    assert!(refused.to_string().contains("invalid_request_error"), "and it says what: {refused}");
}

/// The other half, which is what the retry is for: a gateway error that says
/// nothing about the request is still *later* rather than *no*.
#[tokio::test]
async fn a_gateway_error_that_names_nothing_is_still_waited_out() {
    let (url, seen) = flaky(vec!["502 Bad Gateway", "500 Internal Server Error"]).await;

    let answered = provider(url).complete(Request::new(Vec::new())).await;

    assert_eq!(seen.load(Ordering::SeqCst), 3, "it has to have been refused twice to test anything");
    assert_eq!(answered.expect("a gateway hiccup means later").message.content, "answered");
}

/// And the compaction path: the too-long refusal reaches the loop even when a
/// gateway wrapped it, because the loop shrinks the window on that answer and
/// otherwise ends the turn on a failure it could have recovered from.
#[test]
fn a_wrapped_context_refusal_is_still_read_as_one() {
    let wrapped = LlmError::Status {
        status: 500,
        body: r#"{"error":{"code":"context_length_exceeded","message":"maximum context length is 8192 tokens"}}"#
            .into(),
        retry_after: None,
    };
    assert!(rook_llm::retry::names_the_context(&wrapped), "a compaction is the answer to this");
}

/// An empty wallet is *no* wearing the clothes of *later*. OpenAI answers 429,
/// which is the rate limiter's status and is waited out by default — so four
/// attempts and seven seconds were spent discovering that the account still had
/// no money, before an error that nothing about waiting could have helped.
#[tokio::test]
async fn an_account_out_of_credit_is_not_waited_out() {
    let (url, seen) = always("429 Too Many Requests", r#"{"error":{"code":"insufficient_quota"}}"#).await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();

    assert_eq!(seen.load(Ordering::SeqCst), 1, "waiting cannot add money");
    assert!(refused.to_string().contains("insufficient_quota"), "and it says why: {refused}");
}

/// The other spelling of it, which arrives as a 400 the retry already declines
/// to repeat — asserted so that the two stay told apart by what they say rather
/// than by which of the rules happens to catch them.
#[tokio::test]
async fn the_other_spelling_of_an_empty_wallet_is_not_waited_out_either() {
    let (url, seen) = always(
        "400 Bad Request",
        r#"{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}"#,
    )
    .await;

    let refused = provider(url).complete(Request::new(Vec::new())).await.unwrap_err();

    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert!(refused.to_string().contains("credit balance"), "{refused}");
}

#[tokio::test]
async fn reasoning_completion_limits_are_reduced_and_remembered_after_a_refusal() {
    let (url, sent) = recording(vec![(
        "400 Bad Request",
        r#"{"error":{"message":"max_completion_tokens exceeds the model output limit"}}"#,
    )])
    .await;
    let provider = reasoning_provider(url);
    let mut request = thinking_hard();
    request.max_output_tokens = 20_000;
    provider.complete(request.clone()).await.unwrap();
    provider.complete(request).await.unwrap();
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 3);
    for (raw, expected) in sent.iter().zip([20_000, 10_000, 10_000]) {
        let body: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(body["max_completion_tokens"], expected);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());
        assert_eq!(body["reasoning_effort"], "high", "only the output limit was refused");
    }
}
