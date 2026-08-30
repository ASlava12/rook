//! What a failure tells the user to do. The first command a new user runs is
//! the one most likely to fail, and "transport error: error sending request for
//! url (…)" told them nothing they could act on.

use rook_llm::LlmError;

fn message(endpoint: &str) -> String {
    LlmError::unreachable(endpoint, std::io::Error::other("connection refused")).to_string()
}

/// What a client actually hands over: its own message names the url and never
/// the reason, and the reason is at the bottom of the chain.
#[derive(Debug)]
struct Wrapped(std::io::Error);

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error sending request for url (http://127.0.0.1:11434/v1/models)")
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[test]
fn the_reason_is_reported_rather_than_the_url_a_second_time() {
    let said = LlmError::unreachable(
        "http://127.0.0.1:11434/v1",
        Wrapped(std::io::Error::other("Connection refused (os error 61)")),
    )
    .to_string();

    assert!(said.contains("Connection refused"), "the cause is what distinguishes the fixes: {said}");
    assert!(
        !said.contains("/v1/models"),
        "the path is stripped and must not come back in the detail: {said}"
    );
    assert!(!said.contains("error sending request"), "and the client's own wrapper says nothing: {said}");
}

#[test]
fn an_endpoint_that_is_not_running_says_to_start_it() {
    let said = message("http://127.0.0.1:11434/v1/models");

    assert!(said.contains("http://127.0.0.1:11434"), "{said}");
    assert!(!said.contains("/v1/models"), "the path a request used is noise: {said}");
    assert!(said.contains("Start the server"), "{said}");
    assert!(said.contains("rook models"), "and name the command that lists them: {said}");
}

#[test]
fn a_local_server_moved_off_the_usual_address_still_reads_as_local() {
    for endpoint in ["http://127.0.0.2:11434", "http://localhost:1234/v1", "http://[::1]:8080"] {
        assert!(message(endpoint).contains("Start the server"), "{endpoint} read as remote");
    }
}

#[test]
fn a_hosted_endpoint_is_told_to_check_the_network_and_the_key() {
    let said = message("https://api.anthropic.com/v1/messages");

    assert!(said.contains("https://api.anthropic.com"), "{said}");
    assert!(said.contains("API key"), "{said}");
    assert!(!said.contains("Start the server"), "nobody can start api.anthropic.com: {said}");
}

#[test]
fn an_empty_api_key_is_reported_as_unset_rather_than_sent() {
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "  ") };
    let Err(err) =
        rook_llm::from_spec_with("anthropic/claude-opus-5", std::time::Duration::from_secs(1), None)
    else {
        panic!("a blank key must not build a provider")
    };
    let err = err.to_string();
    unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

    assert!(err.contains("ANTHROPIC_API_KEY is not set"), "an exported blank is the usual slip: {err}");
    assert!(err.contains("ollama"), "and it should name a way out: {err}");
}

/// A server that answers by path and keeps answering, because finding out what
/// a refusal meant takes a second request to the same server.
async fn serve(routes: &'static [(&'static str, &'static str, &'static str)]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 8192];
            let read = socket.read(&mut scratch).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&scratch[..read]).to_string();
            let (status, body) = routes
                .iter()
                .find(|(path, _, _)| head.contains(path))
                .map(|(_, status, body)| (*status, *body))
                .unwrap_or(("404 Not Found", "{}"));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}/v1")
}

async fn ask(url: String, model: &str) -> LlmError {
    use rook_llm::openai::{Config, OpenAiCompatible};
    use rook_llm::{Provider, Request};

    let provider = OpenAiCompatible::new("test/model", model, Config::new(url, None, 8192)).unwrap();
    provider.complete(Request::new(Vec::new())).await.unwrap_err()
}

/// The default spec names a model that has to be pulled first, so this is the
/// failure a new user with Ollama running actually hits.
#[tokio::test]
async fn a_model_the_server_does_not_have_is_told_what_it_does_have() {
    let url = serve(&[
        ("/chat/completions", "404 Not Found", r#"{"error":"model not found"}"#),
        ("/models", "200 OK", r#"{"data":[{"id":"llama3.2"},{"id":"qwen2.5-coder:7b"}]}"#),
    ])
    .await;

    let said = ask(url, "qwen3-coder:30b").await.to_string();
    assert!(said.contains("qwen3-coder:30b"), "names the one that is missing: {said}");
    assert!(said.contains("llama3.2") && said.contains("qwen2.5-coder:7b"), "and the ones present: {said}");
    assert!(said.contains("`[agent] model`"), "and what to change: {said}");
}

#[tokio::test]
async fn a_server_with_nothing_pulled_says_so_rather_than_listing_nothing() {
    let url = serve(&[
        ("/chat/completions", "404 Not Found", r#"{"error":"model not found"}"#),
        ("/models", "200 OK", r#"{"data":[]}"#),
    ])
    .await;

    let said = ask(url, "qwen3-coder:30b").await.to_string();
    assert!(said.contains("it has none"), "{said}");
}

#[tokio::test]
async fn a_base_url_that_serves_nothing_is_not_reported_as_a_missing_model() {
    // Both requests 404, which is what a wrong base URL looks like — the model
    // may be perfectly fine and guessing otherwise sends the user after it.
    let url = serve(&[]).await;

    let said = ask(url, "qwen3-coder:30b").await.to_string();
    assert!(!said.contains("no model"), "a guess dressed as a diagnosis: {said}");
    assert!(said.contains("404"), "{said}");
}

#[test]
fn an_unknown_provider_names_the_ones_that_exist() {
    let Err(err) = rook_llm::from_spec_with("googl/gemini-2.5-pro", std::time::Duration::from_secs(1), None)
    else {
        panic!("a typo must not resolve")
    };
    let said = err.to_string();
    assert!(said.contains("googl"), "{said}");
    for known in rook_llm::PROVIDERS {
        assert!(said.contains(known), "{known} is missing from the list: {said}");
    }
}

/// The list is prose about a `match`, so it drifts unless something checks.
#[test]
fn every_listed_provider_is_one_the_code_actually_dispatches() {
    for name in rook_llm::PROVIDERS {
        let spec = format!("{name}/some-model");
        // A missing key or endpoint is the environment's business, not the
        // question being asked here.
        let built = rook_llm::from_spec_with(&spec, std::time::Duration::from_secs(1), None);
        assert!(!matches!(built, Err(LlmError::UnknownProvider { .. })), "{name} is listed and not handled");
    }
}
