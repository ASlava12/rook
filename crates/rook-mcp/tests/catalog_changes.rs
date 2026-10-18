#[path = "support/catalog_http.rs"]
mod catalog_http;
use catalog_http::Fixture;
use rook_mcp::{Server, ServerConfig};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn config(f: &Fixture) -> ServerConfig {
    let mut config = ServerConfig {
        name: "catalog-http".into(),
        url: Some(f.url.clone()),
        refresh_tools: true,
        ..Default::default()
    };
    config.headers.insert("Authorization".into(), "Bearer owned-header-fixture".into());
    config
}
async fn until(f: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn idle_http_notifications_use_the_negotiated_session_and_stop_with_the_transport() {
    let fixture = Fixture::new(true).await;
    let server = Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap();
    until(|| server.catalog_status().notifications == "HTTP notifications connected").await;
    server.list_tools().await.unwrap();
    let before = fixture.state.lists.load(Ordering::SeqCst);
    fixture.state.change();
    until(|| server.catalog_status().stale).await;
    server.refresh_tool_catalog().await;
    assert!(fixture.state.lists.load(Ordering::SeqCst) > before);
    assert!(server.cached_tools().unwrap().1.iter().any(|tool| tool.name == "new"));
    assert!(!server.catalog_status().stale);
    {
        let headers = fixture.state.headers.lock().unwrap();
        let get = headers.iter().find(|head| head.starts_with("GET ")).unwrap().to_ascii_lowercase();
        assert!(get.contains("mcp-session-id: owned-catalog-session"));
        assert!(get.contains("mcp-protocol-version: 2025-11-25"));
        assert!(get.contains("authorization: bearer owned-header-fixture"));
    }
    server.shutdown().await;
    until(|| fixture.state.get_active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_server_without_get_notifications_still_invalidates_from_its_post_stream() {
    let fixture = Fixture::new(false).await;
    let server = Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap();
    until(|| server.catalog_status().notifications.contains("unsupported")).await;
    server.list_tools().await.unwrap();
    server.call_tool("advance", &json!({})).await.unwrap();
    assert!(server.catalog_status().stale);
    server.refresh_tool_catalog().await;
    assert!(server.cached_tools().unwrap().1.iter().any(|tool| tool.name == "new"));
    assert_eq!(
        fixture.state.gets.load(Ordering::SeqCst),
        1,
        "405 is optional GET support, not a reconnect loop"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn http_catalog_pages_restart_after_invalidation_and_failed_refreshes_are_cooled_down() {
    let fixture = Fixture::new(false).await;
    let server = Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap();
    server.list_tools().await.unwrap();
    fixture.state.invalidate_page.store(true, Ordering::SeqCst);
    server.call_tool("advance", &json!({})).await.unwrap();
    server.refresh_tool_catalog().await;
    let (generation, tools) = server.cached_tools().unwrap();
    assert!(tools.iter().any(|tool| tool.name == "new"));
    assert!(tools.iter().all(|tool| !tool.name.starts_with("torn-")));
    fixture.state.fail.store(true, Ordering::SeqCst);
    server.call_tool("advance", &json!({})).await.unwrap();
    server.refresh_tool_catalog().await;
    assert_eq!(server.catalog_status().generation, generation);
    assert!(server.catalog_status().stale);
    assert!(!serde_json::to_string(&server.catalog_status()).unwrap().contains("fixture-secret"));
    let listed = fixture.state.lists.load(Ordering::SeqCst);
    server.refresh_tool_catalog().await;
    assert_eq!(fixture.state.lists.load(Ordering::SeqCst), listed);
    server.shutdown().await;
}

struct Elicitor(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl rook_mcp::elicitation::Elicitor for Elicitor {
    async fn elicit(
        &self,
        _: &rook_mcp::elicitation::ElicitationOrigin,
        _: &serde_json::Value,
    ) -> Result<rook_mcp::elicitation::ElicitationResponse, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(rook_mcp::elicitation::ElicitationResponse::cancel())
    }
}
#[tokio::test]
async fn refresh_can_finish_beside_a_live_call_and_get_requests_cannot_borrow_its_input_owner() {
    let fixture = Fixture::new(true).await;
    let server = Arc::new(Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap());
    until(|| server.catalog_status().notifications == "HTTP notifications connected").await;
    server.list_tools().await.unwrap();
    fixture.state.hold.store(true, Ordering::SeqCst);
    let prompts = Arc::new(AtomicUsize::new(0));
    let running = {
        let server = server.clone();
        let prompts = prompts.clone();
        tokio::spawn(async move {
            server
                .call_tool_interactive(
                    "hold",
                    &json!({}),
                    &rook_mcp::elicitation::Interaction {
                        elicitor: Arc::new(Elicitor(prompts)),
                        trace: None,
                    },
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(10), fixture.state.call_started.notified()).await.unwrap();
    fixture.state.change();
    until(|| server.catalog_status().stale).await;
    tokio::time::timeout(Duration::from_secs(10), server.refresh_tool_catalog()).await.unwrap();
    assert!(!running.is_finished(), "the admitted call has not been released");
    assert!(server.cached_tools().unwrap().1.iter().any(|tool| tool.name == "new"));
    fixture.state.events.send(json!({"jsonrpc":"2.0","id":77,"method":"elicitation/create","params":{"mode":"form","message":"unowned input","requestedSchema":{"type":"object","properties":{}}}})).unwrap();
    until(|| fixture.state.callback_errors.load(Ordering::SeqCst) == 1).await;
    assert_eq!(prompts.load(Ordering::SeqCst), 0, "a background GET has no invocation authority");
    fixture.state.release.notify_one();
    running.await.unwrap().unwrap();
    server.shutdown().await;
    until(|| fixture.state.get_active.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn unrequested_or_unnegotiated_http_push_never_opens_an_idle_stream() {
    for (configured, negotiated) in [(false, true), (true, false)] {
        let fixture = Fixture::new(true).await;
        fixture.state.support_changes.store(negotiated, Ordering::SeqCst);
        let mut source = config(&fixture);
        source.refresh_tools = configured;
        let server = Server::connect(&source, &rook_llm::Proxy::Direct).await.unwrap();
        server.list_tools().await.unwrap();
        server.call_tool("advance", &json!({})).await.unwrap();
        server.refresh_tool_catalog().await;
        assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 0);
        assert_eq!(server.catalog_status().generation, 1);
        assert!(server.cached_tools().unwrap().1.iter().any(|tool| tool.name == "old"));
        server.shutdown().await;
    }
}

#[tokio::test]
async fn dropping_the_last_transport_owner_closes_its_idle_http_stream() {
    let fixture = Fixture::new(true).await;
    let server = Arc::new(Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap());
    until(|| server.catalog_status().notifications == "HTTP notifications connected").await;
    let owner = server.clone();
    drop(server);
    assert_eq!(fixture.state.get_active.load(Ordering::SeqCst), 1);
    drop(owner);
    until(|| fixture.state.get_active.load(Ordering::SeqCst) == 0).await;
}

struct Token;
#[async_trait::async_trait]
impl rook_mcp::oauth::TokenSource for Token {
    async fn token(&self) -> Result<String, &'static str> {
        Ok("owned-notification-canary-credential".into())
    }
}
#[tokio::test]
async fn an_authenticated_get_cannot_turn_a_reflected_credential_into_catalog_data() {
    let fixture = Fixture::new(true).await;
    let server =
        Server::connect_with_token(&config(&fixture), &rook_llm::Proxy::Direct, Some(Arc::new(Token)))
            .await
            .unwrap();
    until(|| server.catalog_status().notifications == "HTTP notifications connected").await;
    server.list_tools().await.unwrap();
    fixture.state.events.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{"echo":"owned-notification-canary-credential"}})).unwrap();
    until(|| server.catalog_status().notifications.contains("disconnected")).await;
    assert!(
        !serde_json::to_string(&server.catalog_status())
            .unwrap()
            .contains("owned-notification-canary-credential")
    );
    assert_eq!(server.catalog_status().generation, 1);
    assert!(server.catalog_status().stale);
    server.shutdown().await;
    until(|| fixture.state.get_active.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn cancelled_refresh_publishes_cooldown_before_releasing_queued_readers() {
    let fixture = Fixture::new(false).await;
    let server = Arc::new(Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap());
    server.list_tools().await.unwrap();
    server.call_tool("advance", &json!({})).await.unwrap();
    fixture.state.slow_list.store(true, Ordering::SeqCst);
    let first = {
        let server = server.clone();
        tokio::spawn(async move { server.refresh_tool_catalog().await })
    };
    tokio::time::timeout(Duration::from_secs(10), fixture.state.list_started.notified()).await.unwrap();
    let second = {
        let server = server.clone();
        tokio::spawn(async move { server.refresh_tool_catalog().await })
    };
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(10), second).await.unwrap().unwrap();
    assert_eq!(
        fixture.state.lists.load(Ordering::SeqCst),
        2,
        "only startup and the cancelled owner reached the server"
    );
    let status = server.catalog_status();
    assert!(status.stale && status.retry_after_secs.is_some());
    assert_eq!(status.generation, 1);
    fixture.state.slow_list.store(false, Ordering::SeqCst);
    fixture.state.list_release.notify_one();
    assert_eq!(server.catalog_status().generation, 1, "a dropped future cannot publish a late catalog");
    server.shutdown().await;
}

#[tokio::test]
async fn notification_reconnect_respects_the_servers_sse_retry_delay() {
    let fixture = Fixture::new(true).await;
    fixture.state.get_retry_ms.store(1500, Ordering::SeqCst);
    fixture.state.close_get_once.store(true, Ordering::SeqCst);
    let server = Server::connect(&config(&fixture), &rook_llm::Proxy::Direct).await.unwrap();
    until(|| {
        fixture.state.gets.load(Ordering::SeqCst) >= 2
            && server.catalog_status().notifications == "HTTP notifications connected"
    })
    .await;
    {
        let times = fixture.state.get_times.lock().unwrap();
        assert_eq!(times.len(), 2);
        assert!(
            times[1].duration_since(times[0]) >= Duration::from_millis(1500),
            "the client cannot reconnect before retry: 1500 has elapsed"
        );
    }
    server.shutdown().await;
    until(|| fixture.state.get_active.load(Ordering::SeqCst) == 0).await;
}
