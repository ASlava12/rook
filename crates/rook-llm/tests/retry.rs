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
