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
    refusals: Vec<(&'static str, &'static str)>,
) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
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
async fn paced(seconds: &'static str) -> (String, Arc<AtomicUsize>) {
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
            let response = match n {
                0 => {
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
    let (url, seen) = paced("2").await;

    let started = std::time::Instant::now();
    let answered = provider(url).complete(Request::new(Vec::new())).await;
    let waited = started.elapsed();

    assert_eq!(seen.load(Ordering::SeqCst), 2, "asked again after waiting");
    assert_eq!(answered.expect("the second request answers").message.content, "answered");
    // A sleep does not finish early, so the floor is the assertion: without
    // reading the header the first wait is one second.
    assert!(waited >= std::time::Duration::from_secs(2), "waited only {waited:?}");
}
