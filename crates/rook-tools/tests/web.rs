//! Reading a page.
//!
//! Against a server on loopback: a test that reaches the internet tests the
//! internet, and this one is about what the tool does with what it gets.

use rook_tools::policy::Risk;
use rook_tools::{Tool, ToolContext, web::Fetch};

async fn serve(status: &'static str, kind: &'static str, body: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 4096];
            let _ = socket.read(&mut scratch).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}/page")
}

fn ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir())
}

async fn fetch(url: &str) -> rook_tools::ToolOutcome {
    Fetch::new(std::time::Duration::from_secs(5))
        .unwrap()
        .call(&ctx(), &serde_json::json!({ "url": url }))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_page_comes_back_as_prose_without_its_markup() {
    let url = serve(
        "200 OK",
        "text/html; charset=utf-8",
        "<html><head><style>p{color:red}</style><script>alert('x')</script></head>\
         <body><h1>Title</h1><p>First line.</p><td>a</td><td>b</td></body></html>",
    )
    .await;

    let out = fetch(&url).await;

    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("Title"), "{}", out.content);
    assert!(out.content.contains("First line."), "{}", out.content);
    assert!(out.content.contains("a b"), "a tag boundary is a word boundary: {}", out.content);
    assert!(!out.content.contains("alert"), "script is not prose: {}", out.content);
    assert!(!out.content.contains("color:red"), "nor is style: {}", out.content);
    assert!(out.content.contains(&url), "the answer says where the text came from: {}", out.content);
}

/// Somebody else's writing arriving in the model's context is not a fact and not
/// an instruction. The tool cannot decide that for the model, but it can refuse
/// to present it as anything but a quotation from a named place.
#[tokio::test]
async fn a_failing_page_is_reported_with_its_status_rather_than_as_content() {
    let url = serve("404 Not Found", "text/plain", "no such page").await;

    let out = fetch(&url).await;

    assert!(out.is_error, "{}", out.content);
    assert!(out.content.starts_with("404"), "{}", out.content);
    assert_eq!(out.meta["status"], 404);
}

#[tokio::test]
async fn plain_text_is_left_alone_and_a_non_http_address_is_refused() {
    let url = serve("200 OK", "text/plain", "<not markup> just text").await;
    let out = fetch(&url).await;
    assert!(out.content.contains("<not markup> just text"), "{}", out.content);

    let refused = fetch("file:///etc/passwd").await;
    assert!(refused.is_error, "{}", refused.content);
    assert!(refused.content.contains("http"), "the refusal says what is accepted: {}", refused.content);
}

/// What a rule wants to match when a request leaves the machine is where it is
/// going, so an allow rule can name a host and mean it.
#[test]
fn the_risk_a_fetch_reports_is_the_address() {
    let fetch = Fetch::new(std::time::Duration::from_secs(5)).unwrap();
    let risk = fetch.risk(&serde_json::json!({ "url": "https://docs.rs/serde" }));

    assert_eq!(risk, Risk::Network("https://docs.rs/serde".into()));
    let (policy, _) = rook_tools::policy::Policy::compile(
        rook_tools::policy::Mode::Ask,
        &["https://docs.rs/".into()],
        &[],
        &[],
    );
    assert_eq!(policy.decide(&risk), rook_tools::policy::Decision::Allow, "a host may be allowed");
    let elsewhere = fetch.risk(&serde_json::json!({ "url": "https://elsewhere.example/x" }));
    assert_eq!(policy.decide(&elsewhere), rook_tools::policy::Decision::Ask, "and only that host");
}
