use rook_core::{
    Config, Vault,
    model_catalog::{Mode, Origin, discover},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn server(
    replies: Vec<(u16, String)>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in replies {
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
            seen.fetch_add(1, Ordering::SeqCst);
            requests.push(String::from_utf8(bytes).unwrap());
            let response = format!(
                "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (url, count, task)
}
fn config(url: &str) -> Config {
    toml::from_str(&format!("[agent]\nmodel='desk'\n[models.desk]\napi='openai'\nmodel='one'\nurl='{url}'\n"))
        .unwrap()
}
fn models(id: &str) -> String {
    serde_json::json!({"data":[{"id":id,"context_length":32768,"supported_parameters":["tools"]}]})
        .to_string()
}

#[tokio::test]
async fn cache_records_age_and_provenance_and_refresh_does_not_hide_failures() {
    let dir = tempfile::tempdir().unwrap();
    let (url, count, task) = server(vec![
        (200, models("one")),
        (503, "unavailable".into()),
        (401, "bad key".into()),
        (503, "unavailable".into()),
    ])
    .await;
    let mut config = config(&url);
    let vault = Vault::empty();
    let live = discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap();
    assert!(matches!(live.origin, Origin::Endpoint));
    assert_eq!(live.models[0].capabilities.tools, Some(true));
    let cached = discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap();
    assert!(matches!(cached.origin, Origin::Cache));
    assert!(cached.credentials_resolved);
    assert_eq!(cached.observed_at, live.observed_at);
    let offline = discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap();
    assert!(matches!(offline.origin, Origin::Cache));
    assert!(!offline.credentials_resolved);
    assert!(offline.notices[0].contains("not checked"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    config.model_catalog.cache_ttl_secs = 0;
    let stale = discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap();
    assert!(matches!(stale.origin, Origin::StaleCache));
    assert!(stale.notices.iter().any(|n| n.contains("endpoint unavailable")));
    assert!(matches!(
        discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await,
        Err(rook_llm::LlmError::Status { status: 401, .. })
    ));
    assert!(matches!(
        discover(&config, &vault, "desk", Mode::Refresh, dir.path()).await,
        Err(rook_llm::LlmError::Status { status: 503, .. })
    ));
    assert_eq!(task.await.unwrap().len(), 4);
}

#[tokio::test]
async fn offline_without_cache_is_configuration_only_and_never_needs_a_secret() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config(&format!("http://{}/v1", listener.local_addr().unwrap()));
    config.models.get_mut("desk").unwrap().key = "secret:absent".into();
    config.models.get_mut("desk").unwrap().context_window = Some(8192);
    let result = discover(&config, &Vault::empty(), "desk", Mode::Offline, dir.path()).await.unwrap();
    assert!(matches!(result.origin, Origin::Configuration));
    assert_eq!(result.models[0].id, "one");
    assert_eq!(result.models[0].context_window, Some(8192));
    assert_eq!(result.models[0].capabilities, rook_llm::ModelCapabilities::default());
    assert!(result.observed_at.is_none());
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "offline reads do not create a cache or lock"
    );
    assert!(tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept()).await.is_err());
}

#[tokio::test]
async fn changing_an_account_or_endpoint_cannot_reuse_another_live_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let (url, count, task) = server(vec![(200, models("account-one")), (200, models("account-two"))]).await;
    let mut config = config(&url);
    config.models.get_mut("desk").unwrap().key = "secret:account".into();
    let mut vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    vault.keep("account", "secret-one").unwrap();
    let first = discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap();
    assert_eq!(first.models[0].id, "account-one");
    vault.keep("account", "secret-two").unwrap();
    let second = discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap();
    assert_eq!(second.models[0].id, "account-two");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let requests = task.await.unwrap();
    assert!(requests[0].to_lowercase().contains("authorization: bearer secret-one"));
    assert!(requests[1].to_lowercase().contains("authorization: bearer secret-two"));
    let cache = std::fs::read_to_string(dir.path().join("models-v1.json")).unwrap();
    assert!(!cache.contains("secret-one") && !cache.contains("secret-two") && !cache.contains(&url));
    config.models.get_mut("desk").unwrap().url = "http://127.0.0.1:1/v1".into();
    assert!(matches!(
        discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap().origin,
        Origin::Configuration
    ));
}

#[tokio::test]
async fn cache_failure_is_not_a_failed_model_listing_and_cache_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("not-a-directory");
    std::fs::write(&cache, "keep").unwrap();
    let (url, _, task) = server(vec![(200, models("one")), (200, models("two"))]).await;
    let mut config = config(&url);
    let first = discover(&config, &Vault::empty(), "desk", Mode::PreferCache, &cache).await.unwrap();
    assert!(matches!(first.origin, Origin::Endpoint));
    assert!(first.notices[0].contains("could not be cached"));
    config.model_catalog.cache_enabled = false;
    let second = discover(&config, &Vault::empty(), "desk", Mode::PreferCache, &cache).await.unwrap();
    assert_eq!(second.models[0].id, "two");
    assert!(second.notices.is_empty());
    assert_eq!(std::fs::read_to_string(cache).unwrap(), "keep");
    task.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn offline_cache_lookup_does_not_execute_a_credential_helper() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("helper-ran");
    let mut vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    vault
        .refer(
            "account",
            &rook_core::secrets::Source::Command(format!("touch '{}'; printf token", marker.display())),
        )
        .unwrap();
    let mut config = config("http://127.0.0.1:1/v1");
    config.models.get_mut("desk").unwrap().key = "secret:account".into();
    let result = discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap();
    assert!(matches!(result.origin, Origin::Configuration));
    assert!(!marker.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn rotating_a_helpers_result_invalidates_a_live_cache_but_offline_does_not_run_it() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("helper-called");
    let token = dir.path().join("token");
    std::fs::write(&token, "account-one-token").unwrap();
    let mut vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    vault
        .refer(
            "account",
            &rook_core::secrets::Source::Command(format!(
                "touch '{}'; cat '{}'",
                marker.display(),
                token.display()
            )),
        )
        .unwrap();
    let (url, count, task) = server(vec![(200, models("first")), (200, models("second"))]).await;
    let mut config = config(&url);
    config.models.get_mut("desk").unwrap().key = "secret:account".into();
    assert_eq!(
        discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap().models[0].id,
        "first"
    );
    std::fs::remove_file(&marker).unwrap();
    std::fs::write(&token, "account-two-token").unwrap();
    let offline = discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap();
    assert_eq!(offline.models[0].id, "first", "offline is an observation, not a current account check");
    assert!(!offline.credentials_resolved);
    assert!(!marker.exists());
    assert_eq!(
        discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap().models[0].id,
        "second"
    );
    assert!(marker.exists());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let requests = task.await.unwrap();
    assert!(requests[1].to_lowercase().contains("authorization: bearer account-two-token"));
}

#[tokio::test]
async fn corrupt_oversize_or_future_dated_cache_is_not_treated_as_endpoint_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _, task) = server(vec![(200, models("one"))]).await;
    let config = config(&url);
    let vault = Vault::empty();
    discover(&config, &vault, "desk", Mode::Refresh, dir.path()).await.unwrap();
    task.await.unwrap();
    let file = dir.path().join("models-v1.json");
    let mut cached: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    cached["entries"][0]["observed_at"] = serde_json::json!(u64::MAX);
    std::fs::write(&file, serde_json::to_vec(&cached).unwrap()).unwrap();
    assert!(matches!(
        discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap().origin,
        Origin::Configuration
    ));
    for body in ["{broken".to_string(), " ".repeat(config.model_catalog.cache_max_bytes + 1)] {
        std::fs::write(&file, body).unwrap();
        assert!(matches!(
            discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap().origin,
            Origin::Configuration
        ));
    }
}

fn anthropic_page(ids: &[&str], cursor: Option<&str>) -> String {
    serde_json::json!({
        "data": ids.iter().map(|id| serde_json::json!({"id":id,"max_input_tokens":65536})).collect::<Vec<_>>(),
        "has_more":cursor.is_some(), "last_id":cursor
    }).to_string()
}

#[tokio::test]
async fn only_a_complete_paginated_catalog_replaces_the_offline_observation() {
    let dir = tempfile::tempdir().unwrap();
    let (url, count, task) = server(vec![
        (200, anthropic_page(&["one"], Some("one"))),
        (200, anthropic_page(&["two"], None)),
        (200, anthropic_page(&["replacement-prefix"], Some("replacement-prefix"))),
        (503, "page two unavailable".into()),
        (200, anthropic_page(&["another-prefix"], Some("another-prefix"))),
    ])
    .await;
    let mut config = config(url.trim_end_matches("/v1"));
    let endpoint = config.models.get_mut("desk").unwrap();
    endpoint.api = "anthropic".into();
    endpoint.key = "catalog-test-key".into();
    let vault = Vault::empty();
    let live = discover(&config, &vault, "desk", Mode::Refresh, dir.path()).await.unwrap();
    assert!(matches!(live.origin, Origin::Endpoint));
    assert_eq!(live.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    let offline = discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap();
    assert!(matches!(offline.origin, Origin::Cache));
    assert_eq!(offline.models[1].id, "two");
    assert_eq!(offline.models[1].context_window, Some(65536));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let file = dir.path().join("models-v1.json");
    let complete = std::fs::read(&file).unwrap();
    assert!(matches!(
        discover(&config, &vault, "desk", Mode::Refresh, dir.path()).await,
        Err(rook_llm::LlmError::Status { status: 503, .. })
    ));
    assert_eq!(std::fs::read(&file).unwrap(), complete, "failed page must not replace the cache");
    config.model_catalog.limits.max_pages = 1;
    let error = discover(&config, &vault, "desk", Mode::Refresh, dir.path()).await.unwrap_err();
    assert!(error.to_string().contains("exceeds 1 pages"), "{error}");
    assert_eq!(std::fs::read(&file).unwrap(), complete, "limit failure must not store its prefix");
    let offline = discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap();
    assert_eq!(offline.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    assert_eq!(task.await.unwrap().len(), 5);
}

#[tokio::test]
async fn a_cache_from_before_pagination_is_not_reused_as_a_complete_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let (url, count, task) = server(vec![(200, models("old-prefix")), (200, models("refreshed"))]).await;
    let config = config(&url);
    let vault = Vault::empty();
    discover(&config, &vault, "desk", Mode::Refresh, dir.path()).await.unwrap();
    let file = dir.path().join("models-v1.json");
    let mut cached: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(cached["entries"][0]["complete"], true);
    cached["entries"][0].as_object_mut().unwrap().remove("complete");
    std::fs::write(&file, serde_json::to_vec(&cached).unwrap()).unwrap();
    let offline = discover(&config, &vault, "desk", Mode::Offline, dir.path()).await.unwrap();
    assert!(matches!(offline.origin, Origin::Configuration));
    assert_eq!(offline.models[0].id, "one");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let refreshed = discover(&config, &vault, "desk", Mode::PreferCache, dir.path()).await.unwrap();
    assert!(matches!(refreshed.origin, Origin::Endpoint));
    assert_eq!(refreshed.models[0].id, "refreshed");
    assert_eq!(task.await.unwrap().len(), 2);
}

#[tokio::test]
async fn native_metadata_inherits_from_a_named_endpoint_and_survives_offline_cache() {
    let dir = tempfile::tempdir().unwrap();
    let (url,_,task)=server(vec![
        (200,serde_json::json!({"data":[{"id":"work-instance"}]}).to_string()),
        (200,serde_json::json!({"models":[{"key":"local-model","max_context_length":131072,"loaded_instances":[{"id":"work-instance","config":{"context_length":8192}}],"capabilities":{"vision":false,"trained_for_tool_use":true}}]}).to_string()),
    ]).await;
    let mut config:Config=toml::from_str(&format!("[agent]\nmodel='office'\n[endpoints.desktop]\napi='openai'\nmetadata_api='lmstudio'\nurl='{url}'\nkey='native-test-key'\n[models.office]\nendpoint='desktop'\nmodel='work-instance'\n")).unwrap();
    assert!(config.validation_errors().is_empty());
    let vault = Vault::empty();
    let live = discover(&config, &vault, "office", Mode::Refresh, dir.path()).await.unwrap();
    assert_eq!(live.models[0].context_window, Some(8192));
    assert_eq!(live.models[0].max_context_window, Some(131072));
    assert_eq!(live.models[0].capabilities.tools, Some(true));
    assert_eq!(live.models[0].capabilities.image_input, Some(false));
    let offline = discover(&config, &vault, "office", Mode::Offline, dir.path()).await.unwrap();
    assert!(matches!(offline.origin, Origin::Cache));
    assert_eq!(offline.models[0].context_window, Some(8192));
    assert_eq!(offline.models[0].max_context_window, Some(131072));
    let requests = task.await.unwrap();
    assert!(requests[1].starts_with("GET /api/v1/models "));
    assert!(
        requests.iter().all(|r| r.to_ascii_lowercase().contains("authorization: bearer native-test-key"))
    );
    config.endpoints.get_mut("desktop").unwrap().metadata_api = rook_llm::MetadataApi::None;
    assert!(matches!(
        discover(&config, &vault, "office", Mode::Offline, dir.path()).await.unwrap().origin,
        Origin::Configuration
    ));
}
