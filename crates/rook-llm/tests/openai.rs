//! The OpenAI dialect's request body, against a socket that records what was
//! sent.
//!
//! The reply says nothing about which fields went out, so a test that only
//! reads the answer passes while the request drops what the user asked for.

use std::sync::{Arc, Mutex};

use rook_llm::openai::{Config, OpenAiCompatible};
use rook_llm::{Effort, Message, Provider, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DONE: &str = r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;

/// Serves one request and records its body.
async fn serve() -> (String, Arc<Mutex<Option<serde_json::Value>>>) {
    let seen: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let recorder = seen.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut scratch = [0u8; 8192];
        loop {
            let n = match socket.read(&mut scratch).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            raw.extend_from_slice(&scratch[..n]);
            let text = String::from_utf8_lossy(&raw).to_string();
            let Some(split) = text.find("\r\n\r\n") else { continue };
            let length: usize = text
                .lines()
                .find_map(|l| {
                    l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            if raw.len() < split + 4 + length {
                continue;
            }
            *recorder.lock().unwrap() = serde_json::from_str(&text[split + 4..]).ok();
            break;
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{DONE}",
            DONE.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });

    (format!("http://{addr}/v1"), seen)
}

async fn sent(model: &str, effort: Option<Effort>) -> serde_json::Value {
    let (url, seen) = serve().await;
    let mut request = Request::new(vec![Message::user("hi")]);
    request.effort = effort;
    OpenAiCompatible::new("test/model", model, Config::new(url, None, 8192))
        .unwrap()
        .complete(request)
        .await
        .unwrap();
    seen.lock().unwrap().clone().unwrap()
}

/// The setting exists in three front ends and reached two of the three dialects.
#[tokio::test]
async fn the_effort_the_user_asked_for_reaches_a_model_that_has_one() {
    let body = sent("gpt-5", Some(Effort::Low)).await;
    assert_eq!(body["reasoning_effort"], "low", "{body}");

    // Five rungs against four: the two above `high` are the same request here,
    // and a value the API does not know is a rejected request, not more effort.
    let body = sent("o3-mini", Some(Effort::Max)).await;
    assert_eq!(body["reasoning_effort"], "high", "{body}");
}

/// Three cases arrive as one string, and collapsing them makes a truncated call
/// read as a call that forgot an argument.
#[test]
fn arguments_that_are_absent_are_not_arguments_that_are_broken() {
    use rook_llm::parse_arguments;

    assert_eq!(parse_arguments("{\"path\":\"a.rs\"}")["path"], "a.rs");
    assert_eq!(parse_arguments(""), serde_json::json!({}), "a call with no arguments has none");
    assert_eq!(parse_arguments("   "), serde_json::json!({}));
    assert!(parse_arguments("{\"path\":\"a.r").is_null(), "cut off at the output limit");
}

/// Most of what speaks this dialect is not OpenAI, and a strict server rejects
/// an unknown field rather than ignoring it.
#[tokio::test]
async fn nothing_is_said_about_effort_to_a_model_that_has_none() {
    let body = sent("llama-3.1-8b", Some(Effort::Max)).await;
    assert!(body.get("reasoning_effort").is_none(), "{body}");

    let body = sent("gpt-5", None).await;
    assert!(body.get("reasoning_effort").is_none(), "a request that asked for nothing says nothing: {body}");
}
