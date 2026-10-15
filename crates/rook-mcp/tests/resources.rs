use rook_mcp::{Server, ServerConfig};
use std::time::Duration;

fn config(mode: &str) -> ServerConfig {
    ServerConfig {
        name: "documents".into(),
        command: env!("CARGO_BIN_EXE_mcp-mock").into(),
        args: vec![mode.into()],
        startup_timeout_secs: 30,
        call_timeout_secs: 2,
        ..Default::default()
    }
}

#[tokio::test]
async fn resource_only_server_preserves_opaque_cursors_templates_and_unicode_contents() {
    let server = Server::connect(&config("resources"), &Default::default()).await.unwrap();
    assert!(server.has_resources());
    assert!(server.info().capabilities.get("tools").is_none(), "fixture must not implement tools");
    let first = server.resource_page(false, None).await.unwrap();
    assert_eq!(first.entries[0]["uri"], "doc:русский");
    assert_eq!(first.next_cursor.as_deref(), Some("page /+?"));
    let last = server.resource_page(false, first.next_cursor.as_deref()).await.unwrap();
    assert!(last.entries.is_empty());
    assert!(last.next_cursor.is_none());
    let templates = server.resource_page(true, None).await.unwrap();
    assert_eq!(templates.entries[0]["uriTemplate"], "doc:{id}");
    let read = server.read_resource("doc:русский").await.unwrap();
    assert_eq!(read.contents[0].uri, "doc:русский");
    assert_eq!(read.contents[0].text.as_deref(), Some("Данные 📚 из ресурса"));
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_pages_and_reached_limits_never_return_a_partial_catalog() {
    for (mode, message) in [
        ("resources-duplicate", "repeated"),
        ("resources-count", "catalog_max_tools"),
        ("resources-bytes", "byte limit"),
    ] {
        let mut config = config(mode);
        config.catalog_max_tools = 2;
        config.catalog_max_bytes = 1024;
        let server = Server::connect(&config, &Default::default()).await.unwrap();
        assert!(server.resource_page(false, None).await.unwrap_err().to_string().contains(message));
        server.shutdown().await;
    }
    let server = Server::connect(&config("resources-bad-content"), &Default::default()).await.unwrap();
    assert!(server.read_resource("doc:a").await.unwrap_err().to_string().contains("exactly one"));
    server.shutdown().await;
}

#[tokio::test]
async fn resource_read_is_not_replayed_after_an_ambiguous_disconnect() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("reads");
    let mut config = config("resources-crash");
    config.env.insert("MCP_MOCK_MARKER".into(), marker.to_string_lossy().into_owned());
    let server = Server::connect(&config, &Default::default()).await.unwrap();
    let error = server.read_resource("doc:a").await.unwrap_err().to_string();
    assert!(error.contains("not retried"), "{error}");
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "read\n");
    assert_eq!(server.active_requests(), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn resource_body_limits_and_missing_capability_are_enforced_before_returning_content() {
    for (mode, needle) in
        [("resources-many-contents", "64 content blocks"), ("resources-oversized-read", "not retried")]
    {
        let server = Server::connect(&config(mode), &Default::default()).await.unwrap();
        let error = server.read_resource("doc:a").await.unwrap_err().to_string();
        assert!(error.contains(needle), "{error}");
        server.shutdown().await;
    }
    let server = Server::connect(&config("ok"), &Default::default()).await.unwrap();
    assert!(!server.has_resources());
    assert!(server.resource_page(false, None).await.unwrap_err().to_string().contains("did not advertise"));
    assert!(server.read_resource("doc:a").await.unwrap_err().to_string().contains("did not advertise"));
    assert_eq!(
        server.call_tool("echo", &serde_json::json!({"text":"still usable"})).await.unwrap().to_text(),
        "echo: still usable"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn cancellation_releases_resource_request_ownership_and_shutdown_stops_the_child() {
    let directory = tempfile::tempdir().unwrap();
    let receipt = directory.path().join("started");
    let mut config = config("resources-hang");
    config.env.insert("MCP_MOCK_RECEIPT".into(), receipt.to_string_lossy().into_owned());
    config.call_timeout_secs = 30;
    let server = std::sync::Arc::new(Server::connect(&config, &Default::default()).await.unwrap());
    assert!(server.child_pid().await.is_some());
    let reading = server.clone();
    let request = tokio::spawn(async move { reading.read_resource("doc:a").await });
    tokio::time::timeout(Duration::from_secs(15), async {
        while !receipt.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the actual server must start reading before cancellation");
    assert_eq!(server.active_requests(), 1);
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert_eq!(server.active_requests(), 0);
    server.shutdown().await;
    assert!(server.child_pid().await.is_none());
}
