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

/// The window LM Studio keeps on its own endpoint is asked for only where that
/// endpoint is, so nothing else pays a 404 for it.
fn lm_studio(url: String) -> OpenAiCompatible {
    OpenAiCompatible::new("lmstudio/model", "qwen/qwen3.8-27b", Config::new(url, None, 8192)).unwrap()
}

/// Two requests, in order, so a test can answer `/v1/models` one way and the
/// server's own listing another.
async fn serving(answers: Vec<(&'static str, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for (status, body) in answers {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut scratch = [0u8; 8192];
            let _ = socket.read(&mut scratch).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}/v1")
}

/// LM Studio's compatible listing carries no context length, and its own does.
/// The number is worth asking for: a model serving 262144 was budgeted at the
/// 32768 this crate assumes for anything self-hosted, and nothing said so.
#[tokio::test]
async fn a_window_the_compatible_listing_omits_is_asked_for_where_it_is_kept() {
    let url = serving(vec![
        ("200 OK", r#"{"object":"list","data":[{"id":"qwen/qwen3.8-27b"}]}"#),
        (
            "200 OK",
            r#"{"data":[{"id":"qwen/qwen3.8-27b","max_context_length":262144,"loaded_context_length":32768}]}"#,
        ),
    ])
    .await;

    let models = lm_studio(url).models().await.unwrap();

    assert_eq!(models.len(), 1);
    assert_eq!(
        models[0].context_window,
        Some(32768),
        "what it is loaded with, not what it could hold: that is what it will serve"
    );
}

/// The same answer carries what decides whether a local model is usable, and
/// neither half is visible in its name. Three were tried here in one evening —
/// a 27B that ran from system memory at three tokens a second, a 9B whose file
/// would not load at all, a 35B MoE that served fifty — and each wall was found
/// only after switching to it.
#[tokio::test]
async fn a_local_listing_says_what_is_resident_and_how_it_is_quantised() {
    let url = serving(vec![
        ("200 OK", r#"{"object":"list","data":[{"id":"big"},{"id":"small"}]}"#),
        (
            "200 OK",
            r#"{"data":[
                {"id":"big","max_context_length":262144,"state":"loaded","quantization":"Q4_K_S"},
                {"id":"small","max_context_length":131072,"state":"not-loaded","quantization":"Q4_K_M"}
            ]}"#,
        ),
    ])
    .await;

    let models = OpenAiCompatible::new("lmstudio/model", "big", Config::new(url, None, 8192))
        .unwrap()
        .models()
        .await
        .unwrap();

    assert_eq!(models[0].loaded, Some(true), "the one in memory answers in a second");
    assert_eq!(models[1].loaded, Some(false), "and the one that is not takes a minute to start");
    assert_eq!(models[0].quantization.as_deref(), Some("Q4_K_S"));
    assert_eq!(models[1].quantization.as_deref(), Some("Q4_K_M"));
}

/// And said by nobody else, rather than guessed at. A hosted model is neither
/// resident nor quantised from here, and a blank column is the honest answer.
#[tokio::test]
async fn a_listing_that_says_neither_claims_neither() {
    let url = serve("200 OK", r#"{"data":[{"id":"m","context_length":128000}]}"#).await;
    let models = provider(url).models().await.unwrap();
    assert_eq!(models[0].loaded, None, "not `false`, which would be a claim");
    assert_eq!(models[0].quantization, None);
}

/// Not asked when the compatible listing already answered: a server that says
/// it properly pays no second round trip.
#[tokio::test]
async fn a_listing_that_carries_the_window_is_not_asked_twice() {
    let url = serving(vec![
        ("200 OK", r#"{"object":"list","data":[{"id":"m","context_length":128000}]}"#),
        ("500 Server Error", r#"{"error":"this must not be reached"}"#),
    ])
    .await;

    let models = provider(url).models().await.unwrap();

    assert_eq!(models[0].context_window, Some(128000));
}

/// And a server that is not LM Studio answers 404 there, which is silence: the
/// listing is what it was, and the assumed window stands.
#[tokio::test]
async fn a_server_without_that_endpoint_is_no_worse_off() {
    let url =
        serving(vec![("200 OK", r#"{"object":"list","data":[{"id":"m"}]}"#), ("404 Not Found", "{}")]).await;

    let models = provider(url).models().await.unwrap();

    assert_eq!(models[0].context_window, None);
    assert_eq!(models[0].id, "m", "and the listing itself is untouched");
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
        LlmError::Status { status, body, .. } => {
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
