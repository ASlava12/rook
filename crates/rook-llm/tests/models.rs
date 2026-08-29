//! Listing what an endpoint serves.
//!
//! The OpenAI shape is fixed but compatible servers add their own field for
//! context length, and there is no agreement on its name.

use std::time::Duration;

use rook_llm::openai::{Config, OpenAiCompatible};
use rook_llm::{LlmError, Provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve(status: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = [0u8; 8192];
        let _ = socket.read(&mut scratch).await;
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    format!("http://{addr}/v1")
}

fn provider(url: String) -> OpenAiCompatible {
    OpenAiCompatible::new("test/model", "model", Config::new(url, None, 8192)).unwrap()
}

#[tokio::test]
async fn the_openai_shape_is_parsed() {
    let url =
        serve("200 OK", r#"{"object":"list","data":[{"id":"gpt-x","owned_by":"openai"},{"id":"other"}]}"#)
            .await;
    let models = provider(url).models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-x");
    assert_eq!(models[0].owned_by.as_deref(), Some("openai"));
    assert_eq!(models[1].context_window, None, "absent is not zero");
}

#[tokio::test]
async fn the_several_names_for_context_length_are_all_understood() {
    for field in ["context_window", "context_length", "max_model_len"] {
        let body: &'static str =
            Box::leak(format!(r#"{{"data":[{{"id":"m","{field}":262144}}]}}"#).into_boxed_str());
        let models = provider(serve("200 OK", body).await).models().await.unwrap();
        assert_eq!(models[0].context_window, Some(262144), "{field} was not read");
    }
}

#[tokio::test]
async fn an_endpoint_that_lists_nothing_is_not_an_error() {
    let url = serve("200 OK", r#"{"object":"list"}"#).await;
    assert!(provider(url).models().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_http_error_carries_the_status_and_the_body() {
    let url = serve("401 Unauthorized", r#"{"error":"bad key"}"#).await;
    let Err(err) = provider(url).models().await else { panic!("a 401 must not look like no models") };
    match err {
        LlmError::Status { status, body } => {
            assert_eq!(status, 401);
            assert!(body.contains("bad key"), "{body}");
        }
        other => panic!("expected a status error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_body_that_is_not_the_expected_shape_is_a_decode_error() {
    let url = serve("200 OK", "<html>proxy error</html>").await;
    let Err(err) = provider(url).models().await else { panic!("html is not a model list") };
    assert!(matches!(err, LlmError::Decode(_)), "{err}");
}

#[tokio::test]
async fn an_unreachable_endpoint_reports_transport_rather_than_hanging() {
    let provider = provider("http://127.0.0.1:1/v1".into());
    let started = std::time::Instant::now();
    let Err(err) = provider.models().await else { panic!("nothing is listening there") };
    assert!(matches!(err, LlmError::Unreachable { .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn the_configured_context_window_overrides_the_provider_default() {
    let default = rook_llm::from_spec_with("ollama/x", Duration::from_secs(1), None).unwrap();
    let overridden = rook_llm::from_spec_with("ollama/x", Duration::from_secs(1), Some(262_144)).unwrap();
    assert_ne!(default.context_window(), 262_144);
    assert_eq!(overridden.context_window(), 262_144);
}
