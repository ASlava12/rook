//! Metadata is untrusted HTTP input too. These tests use real sockets to
//! exercise byte/count limits, total deadlines, authentication and decoding.
use std::time::Duration;

use rook_llm::{CatalogLimits, Effort, ModelCapabilities, Provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve(replies: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while request.len() < 16384 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..n]);
            }
            requests.push(String::from_utf8(request).unwrap());
            let _ = socket.write_all(&reply).await;
        }
        requests
    });
    (address, task)
}

fn response(body: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

fn openai(url: String) -> rook_llm::openai::OpenAiCompatible {
    rook_llm::openai::OpenAiCompatible::new(
        "local/test",
        "test",
        rook_llm::openai::Config::new(url, None, 8192),
    )
    .unwrap()
}

#[tokio::test]
async fn compatible_metadata_distinguishes_no_from_unknown() {
    let (url, task) = serve(vec![response(r#"{"data":[
        {"id":"known","context_length":65536,"supported_parameters":["tools","reasoning"],"architecture":{"input_modalities":["text","image"]}},
        {"id":"text","supported_parameters":[],"architecture":{"input_modalities":["text"]}},
        {"id":"unknown"}
    ]}"#)]).await;
    let models = openai(url).models().await.unwrap();
    assert_eq!(models[0].context_window, Some(65536));
    assert_eq!(models[0].capabilities.tools, Some(true));
    assert_eq!(models[0].capabilities.image_input, Some(true));
    assert_eq!(models[0].capabilities.reasoning, Some(true));
    assert_eq!(models[1].capabilities.tools, Some(false));
    assert_eq!(models[1].capabilities.image_input, Some(false));
    assert_eq!(
        models[1].capabilities.reasoning, None,
        "absence of an effort parameter is not absence of reasoning"
    );
    assert_eq!(models[2].capabilities, ModelCapabilities::default());
    task.await.unwrap();
}

#[tokio::test]
async fn anthropic_reports_capabilities_and_only_advertised_effort_levels() {
    let (url, task) = serve(vec![response(r#"{"data":[
        {"id":"claude-current","max_input_tokens":200000,"capabilities":{"image_input":{"supported":true},"thinking":{"supported":true,"types":{"adaptive":{"supported":true}}},"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},"high":{"supported":true},"max":{"supported":true},"xhigh":{"supported":false}}}},
        {"id":"no-effort","capabilities":{"effort":{"supported":false},"image_input":{"supported":false}}},
        {"id":"unknown","capabilities":null}
    ]}"#)]).await;
    let provider = rook_llm::anthropic::Anthropic::new(
        "test",
        "claude-current",
        rook_llm::anthropic::Config::new(url, "test-key".into(), "claude-current"),
    )
    .unwrap();
    let models = provider.models().await.unwrap();
    assert_eq!(models[0].context_window, Some(200000));
    assert_eq!(models[0].capabilities.adaptive_thinking, Some(true));
    assert_eq!(
        models[0].capabilities.effort_levels,
        Some(vec![Effort::Low, Effort::Medium, Effort::High, Effort::Max])
    );
    assert_eq!(models[1].capabilities.effort_levels, Some(vec![]));
    assert_eq!(models[1].capabilities.image_input, Some(false));
    assert_eq!(models[2].capabilities, ModelCapabilities::default());
    assert!(task.await.unwrap()[0].to_ascii_lowercase().contains("x-api-key: test-key"));
}

#[tokio::test]
async fn google_reports_explicit_thinking_support_without_guessing_for_older_servers() {
    let (url, task) = serve(vec![response(
        r#"{"models":[
        {"name":"models/gemini-known","inputTokenLimit":1000000,"thinking":true},
        {"name":"models/plain","thinking":false},
        {"name":"models/unknown"}
    ]}"#,
    )])
    .await;
    let provider = rook_llm::google::Google::new(
        "test",
        "gemini-known",
        rook_llm::google::Config::new(url, "test-key".into(), "gemini-known"),
    )
    .unwrap();
    let models = provider.models().await.unwrap();
    assert_eq!(models[0].id, "gemini-known");
    assert_eq!(models[0].context_window, Some(1000000));
    assert_eq!(models[0].capabilities.reasoning, Some(true));
    assert_eq!(models[1].capabilities.reasoning, Some(false));
    assert_eq!(models[2].capabilities.reasoning, None);
    task.await.unwrap();
}

#[tokio::test]
async fn metadata_byte_cap_covers_announced_chunked_and_error_bodies() {
    let limits = CatalogLimits { max_bytes: 1024, ..Default::default() };
    let oversized = "x".repeat(1025);
    for reply in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\nConnection: close\r\n\r\n".to_vec(),
        format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{oversized}\r\n0\r\n\r\n", oversized.len()).into_bytes(),
        format!("HTTP/1.1 503 Unavailable\r\nConnection: close\r\n\r\n{oversized}").into_bytes(),
    ] {
        let (url, task) = serve(vec![reply]).await;
        let error = openai(url).models_with(limits).await.unwrap_err();
        assert!(error.to_string().contains("exceeds 1024 bytes"), "{error}");
        task.await.unwrap();
    }
}

#[tokio::test]
async fn count_cap_and_bad_envelopes_fail_instead_of_pretending_to_be_an_empty_catalog() {
    for body in [
        r#"{"data":[{"id":"one"},{"id":"two"}]}"#,
        r#"{"data":[],"data":[]}"#,
        r#"{"error":"not a listing"}"#,
        r#"{"data":[]} {}"#,
    ] {
        let (url, task) = serve(vec![response(body)]).await;
        let error =
            openai(url).models_with(CatalogLimits { max_models: 1, ..Default::default() }).await.unwrap_err();
        assert!(error.to_string().contains("invalid model metadata"), "{error}");
        task.await.unwrap();
    }
}

#[tokio::test]
async fn a_trickling_response_cannot_extend_the_metadata_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        loop {
            if socket.write_all(b"1\r\n \r\n").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        openai(url).models_with(CatalogLimits { timeout_secs: 1, ..Default::default() }),
    )
    .await;
    let error = result.expect("continued chunks must not reset a total deadline").unwrap_err();
    assert!(error.to_string().contains("timed out after 1s"), "{error}");
    task.abort();
}

#[tokio::test]
async fn lm_studio_enrichment_authenticates_and_cannot_bypass_the_byte_cap() {
    for (accepted, native) in [
        (
            true,
            response(
                r#"{"data":[{"id":"test","loaded_context_length":16384,"max_context_length":65536,"state":"loaded","quantization":"Q4_K_M"}]}"#,
            ),
        ),
        (false, b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\nConnection: close\r\n\r\n".to_vec()),
    ] {
        let (url, task) = serve(vec![
            response(r#"{"data":[{"id":"test"}]}"#),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec(),
            native,
        ])
        .await;
        let provider = rook_llm::openai::OpenAiCompatible::new(
            "lmstudio/test",
            "test",
            rook_llm::openai::Config::new(format!("{url}/v1"), Some("private-key".into()), 8192),
        )
        .unwrap();
        let models =
            provider.models_with(CatalogLimits { max_bytes: 1024, ..Default::default() }).await.unwrap();
        assert_eq!(models[0].context_window, accepted.then_some(16384));
        assert_eq!(models[0].loaded, accepted.then_some(true));
        let requests = task.await.unwrap();
        assert!(requests[1].starts_with("GET /api/v1/models "));
        assert!(requests[2].starts_with("GET /api/v0/models "));
        assert!(
            requests.iter().all(|r| r.to_ascii_lowercase().contains("authorization: bearer private-key"))
        );
    }
}

#[tokio::test]
async fn metadata_limits_survive_the_production_retry_and_queue_wrappers() {
    let (url, task) = serve(vec![response(r#"{"data":[{"id":"one"},{"id":"two"}]}"#)]).await;
    let endpoint = rook_llm::Endpoint {
        name: "catalog-wrapper-test".into(),
        api: rook_llm::Api::OpenAi,
        metadata_api: rook_llm::MetadataApi::None,
        assumed_context_window: None,
        url,
        key: None,
        model: "one".into(),
        context_window: None,
        parallel: Some(1),
        key_in_the_clear: false,
        proxy: rook_llm::Proxy::Direct,
        queue: None,
    };
    let provider =
        rook_llm::from_endpoints_with(vec![endpoint], Duration::from_secs(5), rook_llm::Prefer::AsConfigured)
            .unwrap();
    let error =
        provider.models_with(CatalogLimits { max_models: 1, ..Default::default() }).await.unwrap_err();
    assert!(error.to_string().contains("exceeds 1 models"), "{error}");
    assert_eq!(task.await.unwrap().len(), 1, "malformed metadata is not a retryable generation request");
}

#[tokio::test]
async fn status_errors_remain_status_errors_and_broken_utf8_is_rejected() {
    let (url, task) = serve(vec![
        b"HTTP/1.1 429 Limited\r\nRetry-After: 12\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow"
            .to_vec(),
    ])
    .await;
    let error = openai(url).models().await.unwrap_err();
    assert!(
        matches!(error, rook_llm::LlmError::Status {status: 429, retry_after: Some(seconds), body} if seconds == Duration::from_secs(12) && body == "slow")
    );
    task.await.unwrap();
    let (url, task) =
        serve(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff".to_vec()]).await;
    let error = openai(url).models().await.unwrap_err();
    assert!(error.to_string().contains("not UTF-8"));
    task.await.unwrap();
}
