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
            // `Connection: close` on every answer: this serves one request per
            // connection and then drops it, and without saying so a client is
            // entitled to reuse the socket for the next request — which is what
            // a redirect does, and it found the connection already gone.
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
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
        rook_tools::policy::Stance::Assist,
        &["https://docs.rs/".into()],
        &[],
        &[],
    );
    assert_eq!(policy.decide(&risk), rook_tools::policy::Decision::Allow, "a host may be allowed");
    let elsewhere = fetch.risk(&serde_json::json!({ "url": "https://elsewhere.example/x" }));
    assert_eq!(policy.decide(&elsewhere), rook_tools::policy::Decision::Ask, "and only that host");
}

/// The approval named an address. A redirect to another host is how that
/// approval turns into a request somewhere nobody agreed to.
#[tokio::test]
async fn a_redirect_off_the_approved_host_is_reported_rather_than_followed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 4096];
            let _ = socket.read(&mut scratch).await;
            let head = String::from_utf8_lossy(&scratch).to_string();
            // `/here` stays on this host; `/away` does not.
            let response = if head.contains("GET /away") {
                "HTTP/1.1 302 Found\r\nLocation: http://elsewhere.example/taken\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                    .to_string()
            } else if head.contains("GET /here") {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{addr}/landed\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                )
            } else {
                let body = "the page it landed on";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
            };
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    let stayed = fetch(&format!("http://{addr}/here")).await;
    assert!(!stayed.is_error, "a redirect within the host is ordinary and is followed: {}", stayed.content);
    assert!(stayed.content.contains("the page it landed on"), "{}", stayed.content);

    let left = fetch(&format!("http://{addr}/away")).await;
    assert!(left.is_error, "{}", left.content);
    assert!(
        left.content.contains("elsewhere.example"),
        "and it says where it wanted to go, so the model can ask for that address: {}",
        left.content
    );
}

async fn searx(body: &'static str) -> String {
    serve("200 OK", "application/json", body).await
}

#[tokio::test]
async fn a_search_lists_what_the_engine_returned_with_its_summaries_marked_as_such() {
    let base = searx(
        r#"{"results":[
             {"title":"Serde","url":"https://serde.rs","content":"A <b>framework</b> for serializing"},
             {"title":"Docs","url":"https://docs.rs/serde","content":"API documentation"},
             {"title":"No address","content":"dropped, because there is nothing to read"}
           ]}"#,
    )
    .await;
    let base = base.trim_end_matches("/page").to_string();

    let search =
        rook_tools::web::Search::new(rook_tools::web::Engine::Searx(base), std::time::Duration::from_secs(5))
            .unwrap();
    let out = search.call(&ctx(), &serde_json::json!({ "query": "serde json" })).await.unwrap();

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.meta["results"], 2, "a result with no address is not one: {}", out.content);
    assert!(out.content.contains("https://serde.rs"), "{}", out.content);
    assert!(
        out.content.contains("A framework for serializing"),
        "the engine's own summary comes through, markup and all removed: {}",
        out.content
    );
}

/// A rule that allows a local instance must not also allow a hosted one: what
/// leaves the machine is a request to a host, and which host is the question.
#[test]
fn the_risk_a_search_reports_is_the_engine_not_the_query() {
    let local = rook_tools::web::Search::new(
        rook_tools::web::Engine::Searx("http://127.0.0.1:8888".into()),
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let hosted = rook_tools::web::Search::new(
        rook_tools::web::Engine::Brave("a-key".into()),
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let asking = serde_json::json!({ "query": "anything at all" });

    let (policy, _) = rook_tools::policy::Policy::compile(
        rook_tools::policy::Stance::Assist,
        &["http://127.0.0.1:8888".into()],
        &[],
        &[],
    );

    assert_eq!(policy.decide(&local.risk(&asking)), rook_tools::policy::Decision::Allow);
    assert_eq!(
        policy.decide(&hosted.risk(&asking)),
        rook_tools::policy::Decision::Ask,
        "allowing your own search engine must not allow somebody else's"
    );
}

/// Naming an engine whose key is not set is a reason to offer nothing: a tool
/// that fails on its first call teaches the model to stop asking.
#[test]
fn an_engine_that_cannot_work_is_not_offered() {
    unsafe { std::env::remove_var("BRAVE_API_KEY") };
    assert!(rook_tools::web::Engine::named("brave", "").is_none());
    assert!(rook_tools::web::Engine::named("", "http://x").is_none());
    assert!(rook_tools::web::Engine::named("nonsense", "http://x").is_none());
    assert_eq!(
        rook_tools::web::Engine::named("searxng", "http://127.0.0.1:8888/"),
        Some(rook_tools::web::Engine::Searx("http://127.0.0.1:8888".into())),
        "a trailing slash must not become a double one"
    );
}

/// A page is arbitrary text. Deciding whether a `<` opens a `<script>` by
/// slicing the six bytes after it splits a character the moment those bytes are
/// not ASCII — and a page with `a < b` in its prose and a word of Japanese after
/// it is an ordinary page, not a malformed one. In release the profile aborts on
/// panic, so this took the process with it.
#[tokio::test]
async fn a_page_that_puts_a_bare_angle_bracket_before_multibyte_text_is_read() {
    let url = serve(
        "200 OK",
        "text/html",
        // Two shapes a real page has and a naive stripper does not survive: a
        // bracket that opens nothing, and a malformed tag whose name runs
        // straight into multibyte text.
        "<p>if a < aa\u{65e5}\u{672c}\u{8a9e} then b</p><a\u{65e5}\u{672c}\u{8a9e}>\
         <p>and \u{4eca}\u{65e5}</p>",
    )
    .await;

    let out = fetch(&url).await;

    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("a < aa\u{65e5}\u{672c}\u{8a9e} then b"),
        "a bracket that opens no tag is the character itself, and the sentence around it \
         survives: {}",
        out.content
    );
    assert!(out.content.contains("\u{4eca}\u{65e5}"), "{}", out.content);
}

/// The cap was applied to a body already in memory, which is not a cap: a
/// server answering with far more than a page is not stopped by a check that
/// runs after it has all arrived.
#[tokio::test]
async fn a_page_far_larger_than_a_page_is_stopped_while_it_arrives() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else { return };
        let mut scratch = [0u8; 4096];
        let _ = socket.read(&mut scratch).await;
        // Chunked, and never ending: a `Content-Length` would let the client
        // refuse on the header alone, which is not what is being tested.
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n";
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let chunk = format!("{:x}\r\n{}\r\n", 64 * 1024, "x".repeat(64 * 1024));
        while socket.write_all(chunk.as_bytes()).await.is_ok() {}
    });

    let url = format!("http://{addr}/endless");
    let ctx = ToolContext::new(std::env::temp_dir());
    let fetch = Fetch::new(std::time::Duration::from_secs(20)).unwrap();
    let out = fetch.call(&ctx, &serde_json::json!({ "url": url })).await.unwrap();

    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("may bring back"), "and says which limit: {}", out.content);
}
