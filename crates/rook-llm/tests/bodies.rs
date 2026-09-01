//! How much of an endpoint's answer reaches memory.
//!
//! `base_url` is configuration, so the size of a reply is decided by whatever
//! is on the other end of it. Reading it all and then measuring is a cap that
//! has already been paid.

use std::time::Duration;

use rook_llm::openai::{Config, OpenAiCompatible};
use rook_llm::{Provider, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers `status` and then writes for as long as anyone is reading.
async fn endless(status: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut scratch = [0u8; 8192];
                let _ = socket.read(&mut scratch).await;
                // No length: the client cannot know when to stop, which is the
                // whole question. Chunked so it stays a valid HTTP body.
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                );
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let chunk = format!("{:x}\r\n{}\r\n", 64 * 1024, "x".repeat(64 * 1024));
                while socket.write_all(chunk.as_bytes()).await.is_ok() {}
            });
        }
    });
    format!("http://{addr}/v1")
}

fn provider(url: String) -> OpenAiCompatible {
    OpenAiCompatible::new("test/model", "m", Config::new(url, None, 8192)).unwrap()
}

#[tokio::test]
async fn a_reply_that_never_ends_is_refused_rather_than_held() {
    let url = endless("200 OK").await;
    let refused =
        tokio::time::timeout(Duration::from_secs(120), provider(url).complete(Request::new(Vec::new())))
            .await
            .expect("a body with no end must not be read to the end")
            .unwrap_err()
            .to_string();

    assert!(refused.contains("still sending"), "{refused}");
    assert!(refused.contains("33554432"), "and says the bound it passed: {refused}");
}

#[tokio::test]
async fn a_failure_answered_with_an_endless_body_costs_a_sentence_to_report() {
    let url = endless("500 Internal Server Error").await;
    let refused =
        tokio::time::timeout(Duration::from_secs(60), provider(url).complete(Request::new(Vec::new())))
            .await
            .expect("an error body with no end must not be read to the end")
            .unwrap_err()
            .to_string();

    assert!(refused.contains("500"), "{refused}");
    assert!(refused.len() < 4096, "the message is {} bytes of what a broken server sent", refused.len());
    assert!(refused.contains('…'), "and says it was cut: {refused}");
}
