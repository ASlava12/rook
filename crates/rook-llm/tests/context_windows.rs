//! Context discovery uses the requested model and every fallback route.
use rook_llm::{Api, CatalogLimits, Endpoint, Prefer, Provider};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn catalog(api: Api, window: Option<usize>) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = match api {
        Api::OpenAi | Api::Responses => serde_json::json!({"data":[{"id":"decoy","context_length":999999},{"id":"actual","context_length":window}]}),
        Api::Anthropic => serde_json::json!({"data":[{"id":"decoy","max_input_tokens":999999},{"id":"actual","max_input_tokens":window}],"has_more":false}),
        Api::Google => serde_json::json!({"models":[{"name":"models/decoy","inputTokenLimit":999999},{"name":"models/actual","inputTokenLimit":window}]}),
    }.to_string();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        while bytes.len() < 16384 && !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
        }
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, task)
}
fn endpoint(api: Api, url: String, name: &str) -> Endpoint {
    Endpoint {
        name: name.into(),
        api,
        metadata_api: rook_llm::MetadataApi::None,
        assumed_context_window: None,
        url,
        key: Some("test-key".into()),
        model: "actual".into(),
        context_window: None,
        parallel: Some(1),
        key_in_the_clear: false,
        proxy: Default::default(),
        queue: None,
    }
}
fn build(endpoints: Vec<Endpoint>) -> Box<dyn Provider> {
    rook_llm::from_endpoints_with(endpoints, Duration::from_secs(30), Prefer::AsConfigured).unwrap()
}

#[tokio::test]
async fn named_providers_discover_the_requested_model_through_retry_and_queue_wrappers() {
    for api in [Api::OpenAi, Api::Responses, Api::Anthropic, Api::Google] {
        let (url, task) = catalog(api, Some(65536)).await;
        let provider = build(vec![endpoint(api, url, "alias-not-a-model")]);
        assert!(provider.context_key().is_some());
        assert!(!provider.context_is_explicit());
        assert_eq!(provider.discover_context_window(CatalogLimits::default()).await.unwrap(), Some(65536));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn configured_windows_are_preserved_without_contacting_the_catalog() {
    for api in [Api::OpenAi, Api::Responses, Api::Anthropic, Api::Google] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut endpoint = endpoint(api, format!("http://{}", listener.local_addr().unwrap()), "explicit");
        endpoint.context_window = Some(8192);
        let provider = build(vec![endpoint]);
        assert!(provider.context_is_explicit());
        assert_eq!(provider.discover_context_window(CatalogLimits::default()).await.unwrap(), Some(8192));
        assert!(tokio::time::timeout(Duration::from_millis(30), listener.accept()).await.is_err());
    }
}

#[tokio::test]
async fn fallback_discovery_fits_both_models_and_retains_unknown_routes_assumptions() {
    for (second, expected) in [(Some(8192), 8192), (Some(65536), 65536), (None, 32768)] {
        let (primary_url, primary) = catalog(Api::OpenAi, Some(262144)).await;
        let (fallback_url, fallback) = catalog(Api::OpenAi, second).await;
        let provider = build(vec![
            endpoint(Api::OpenAi, primary_url, "primary"),
            endpoint(Api::OpenAi, fallback_url, "fallback"),
        ]);
        assert_eq!(provider.context_window(), 32768);
        assert_eq!(provider.discover_context_window(CatalogLimits::default()).await.unwrap(), Some(expected));
        primary.await.unwrap();
        fallback.await.unwrap();
    }
}

#[tokio::test]
async fn an_unreachable_fallback_cannot_inherit_the_large_primary_window() {
    let (url, primary) = catalog(Api::OpenAi, Some(262144)).await;
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let absent = format!("http://{}", socket.local_addr().unwrap());
    drop(socket);
    let provider =
        build(vec![endpoint(Api::OpenAi, url, "primary"), endpoint(Api::OpenAi, absent, "absent")]);
    assert_eq!(provider.discover_context_window(CatalogLimits::default()).await.unwrap(), Some(32768));
    primary.await.unwrap();
}

#[test]
fn context_identity_tracks_connections_credentials_models_limits_and_the_whole_fallback_group() {
    for api in [Api::OpenAi, Api::Responses, Api::Anthropic, Api::Google] {
        let original = endpoint(api, "http://127.0.0.1:1".into(), "alias");
        let key = build(vec![original.clone()]).context_key().unwrap();
        let mut alias = original.clone();
        alias.name = "another-alias".into();
        assert_eq!(build(vec![alias]).context_key(), Some(key));
        for variant in 0..6 {
            let mut changed = original.clone();
            match variant {
                0 => changed.url = "http://127.0.0.1:2".into(),
                1 => changed.key = Some("rotated-key".into()),
                2 => changed.model = "another-model".into(),
                3 => changed.context_window = Some(8192),
                4 => changed.proxy = rook_llm::Proxy::Direct,
                _ => changed.context_window = Some(build(vec![original.clone()]).context_window()),
            }
            assert_ne!(build(vec![changed.clone()]).context_key(), Some(key));
            assert_ne!(build(vec![original.clone(), changed]).context_key(), Some(key));
        }
        let mut other = original.clone();
        other.url = "http://127.0.0.1:3".into();
        let group = build(vec![original.clone(), other.clone()]).context_key();
        other.key = Some("different-account".into());
        assert_ne!(build(vec![original, other]).context_key(), group);
    }
}

#[tokio::test]
async fn a_fallbacks_explicit_window_stays_in_the_discovered_minimum_without_a_probe() {
    let (url, primary) = catalog(Api::OpenAi, Some(262144)).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut fallback =
        endpoint(Api::OpenAi, format!("http://{}", listener.local_addr().unwrap()), "explicit-fallback");
    fallback.context_window = Some(8192);
    let provider = build(vec![endpoint(Api::OpenAi, url, "primary"), fallback]);
    assert!(!provider.context_is_explicit());
    assert_eq!(provider.discover_context_window(CatalogLimits::default()).await.unwrap(), Some(8192));
    primary.await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(30), listener.accept()).await.is_err());
}
