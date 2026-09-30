use rook_core::{McpSession, mcp_connections::Settings};
use rook_mcp::ServerConfig;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Semaphore,
};

struct Fixture {
    config: ServerConfig,
    calls: Arc<Semaphore>,
    release_calls: Arc<Semaphore>,
    listings: Arc<Semaphore>,
    release_lists: Arc<Semaphore>,
    refuse: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(label: &'static str, hold_lists: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = ServerConfig {
            name: "test".into(),
            url: Some(format!("http://{}/mcp", listener.local_addr().unwrap())),
            ..Default::default()
        };
        let calls = Arc::new(Semaphore::new(0));
        let release_calls = Arc::new(Semaphore::new(0));
        let listings = Arc::new(Semaphore::new(0));
        let release_lists = Arc::new(Semaphore::new(if hold_lists { 0 } else { 1024 }));
        let refuse = Arc::new(AtomicBool::new(false));
        let (seen, release, listed, list_release, refusal) =
            (calls.clone(), release_calls.clone(), listings.clone(), release_lists.clone(), refuse.clone());
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    socket = listener.accept() => {
                        let (mut socket, _) = socket.unwrap();
                        let (seen, release, listed, list_release, refusal) = (seen.clone(), release.clone(), listed.clone(), list_release.clone(), refusal.clone());
                        clients.spawn(async move {
                            let mut bytes = Vec::new();
                            let mut chunk = [0; 4096];
                            let (end, length) = loop {
                                let n = socket.read(&mut chunk).await.unwrap();
                                if n == 0 { return; }
                                assert!(bytes.len()+n <= 65536);
                                bytes.extend_from_slice(&chunk[..n]);
                                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let header = String::from_utf8_lossy(&bytes[..at]);
                                    let length = header.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|s| s.trim().parse::<usize>().unwrap())).unwrap_or(0);
                                    break (at+4, length);
                                }
                            };
                            while bytes.len() < end+length {
                                let n = socket.read(&mut chunk).await.unwrap();
                                if n == 0 { return; }
                                assert!(bytes.len()+n <= 65536);
                                bytes.extend_from_slice(&chunk[..n]);
                            }
                            if length == 0 { return; }
                            let request: Value = serde_json::from_slice(&bytes[end..end+length]).unwrap();
                            let method = request["method"].as_str().unwrap();
                            if request.get("id").is_none() {
                                let _ = socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                                return;
                            }
                            let result = match method {
                                "initialize" => json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":label,"version":"1"}}),
                                "tools/list" => {
                                    listed.add_permits(1);
                                    list_release.acquire().await.unwrap().forget();
                                    json!({"tools":[{"name":"echo","description":label,"inputSchema":{"type":"object","properties":{}}}]})
                                },
                                "tools/call" => {
                                    seen.add_permits(1);
                                    release.acquire().await.unwrap().forget();
                                    json!({"content":[{"type":"text","text":label}]})
                                },
                                _ => panic!("unexpected method {method}"),
                            };
                            let body = if refusal.load(Ordering::SeqCst) {
                                json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"SECRET_BEARER_TOKEN"}})
                            } else { json!({"jsonrpc":"2.0","id":request["id"],"result":result}) }.to_string();
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                            let _ = socket.write_all(response.as_bytes()).await;
                        });
                    },
                    Some(result) = clients.join_next() => { result.unwrap(); },
                }
            }
        });
        Self { config, calls, release_calls, listings, release_lists, refuse, task }
    }
}
async fn observed(semaphore: &Semaphore) {
    tokio::time::timeout(std::time::Duration::from_secs(60), semaphore.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}

#[tokio::test]
async fn replacement_preserves_in_flight_calls_and_changes_only_later_snapshots() {
    let old = Fixture::new("old", false).await;
    let new = Fixture::new("new", false).await;
    let session = McpSession::connect([&old.config], &rook_llm::Proxy::default(), Settings::default()).await;
    let snapshot = session.servers();
    let server = snapshot[0].0.clone();
    let call = tokio::spawn(async move { server.call_tool("echo", &json!({})).await });
    observed(&old.calls).await;
    assert_eq!(session.report().servers[0].active_requests, 1);
    let report = session.reconnect("test", [&new.config], &rook_llm::Proxy::default()).await.unwrap();
    assert_eq!(report.servers[0].generation, 2);
    assert_eq!(session.servers()[0].1[0].description, "new");
    assert_eq!(snapshot[0].1[0].description, "old");
    assert!(!call.is_finished(), "replacement must not terminate the old invocation");
    assert_eq!(new.calls.available_permits(), 0, "replacement must never replay the old invocation");
    old.release_calls.add_permits(1);
    assert_eq!(call.await.unwrap().unwrap().to_text(), "old");
    assert_eq!(snapshot[0].0.active_requests(), 0);
    let server = session.servers()[0].0.clone();
    new.release_calls.add_permits(1);
    assert_eq!(server.call_tool("echo", &json!({})).await.unwrap().to_text(), "new");
}

#[tokio::test]
async fn failed_candidate_keeps_the_working_catalog_and_never_exposes_server_secrets() {
    let old = Fixture::new("old", false).await;
    let bad = Fixture::new("bad", false).await;
    bad.refuse.store(true, Ordering::SeqCst);
    let session = McpSession::connect([&old.config], &rook_llm::Proxy::default(), Settings::default()).await;
    let error = session.reconnect("test", [&bad.config], &rook_llm::Proxy::default()).await.unwrap_err();
    assert!(!error.contains("SECRET"));
    let report = session.report();
    assert_eq!(report.servers[0].state, "connected");
    assert_eq!(report.servers[0].generation, 1);
    assert!(report.servers[0].error.is_some());
    assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
    assert_eq!(session.servers()[0].1[0].description, "old");
    old.release_calls.add_permits(1);
    assert_eq!(session.servers()[0].0.call_tool("echo", &json!({})).await.unwrap().to_text(), "old");
}

#[tokio::test]
async fn discovery_is_atomic_and_cancellation_clears_busy_without_losing_the_old_connection() {
    let old = Fixture::new("old", false).await;
    let new = Fixture::new("new", true).await;
    let session =
        Arc::new(McpSession::connect([&old.config], &rook_llm::Proxy::default(), Settings::default()).await);
    let working = session.clone();
    let config = new.config.clone();
    let replacing =
        tokio::spawn(async move { working.reconnect("test", [&config], &rook_llm::Proxy::default()).await });
    observed(&new.listings).await;
    assert!(session.report().servers[0].reconnecting);
    assert_eq!(session.servers()[0].1[0].description, "old");
    let error = session.reconnect("test", [&old.config], &rook_llm::Proxy::default()).await.unwrap_err();
    assert!(error.contains("already running"));
    replacing.abort();
    assert!(replacing.await.unwrap_err().is_cancelled());
    assert!(!session.report().servers[0].reconnecting);
    assert_eq!(session.servers()[0].1[0].description, "old");
    new.release_lists.add_permits(2);
    session.reconnect("test", [&new.config], &rook_llm::Proxy::default()).await.unwrap();
    assert_eq!(session.servers()[0].1[0].description, "new");
}

#[tokio::test]
async fn disabled_servers_are_visible_and_removal_from_future_turns_preserves_old_snapshots() {
    let old = Fixture::new("old", false).await;
    let mut disabled = old.config.clone();
    disabled.enabled = false;
    let session = McpSession::connect([&disabled], &rook_llm::Proxy::default(), Settings::default()).await;
    assert_eq!(session.report().servers[0].state, "disabled");
    assert!(session.servers().is_empty());
    assert_eq!(old.listings.available_permits(), 0);
    session.reconnect("test", [&old.config], &rook_llm::Proxy::default()).await.unwrap();
    let snapshot = session.servers();
    session.reconnect("test", [&disabled], &rook_llm::Proxy::default()).await.unwrap();
    assert!(session.servers().is_empty());
    old.release_calls.add_permits(1);
    assert_eq!(snapshot[0].0.call_tool("echo", &json!({})).await.unwrap().to_text(), "old");
}

#[tokio::test]
async fn admission_is_bounded_and_duplicates_fail_before_any_process_or_request() {
    let fixture = Fixture::new("server", false).await;
    let limits = Settings { max_servers: 1, parallel_connects: 1, ..Default::default() };
    let too_many = [&fixture.config, &fixture.config];
    assert!(too_many.len() > limits.max_servers);
    let session = McpSession::connect(too_many, &rook_llm::Proxy::default(), limits).await;
    assert!(session.report().issue.unwrap().contains("too many"));
    assert!(session.servers().is_empty());
    assert_eq!(fixture.listings.available_permits(), 0);
    let session = McpSession::connect(too_many, &rook_llm::Proxy::default(), Settings::default()).await;
    assert!(session.report().issue.unwrap().contains("duplicate"));
    assert!(session.servers().is_empty());
    assert_eq!(fixture.listings.available_permits(), 0);
    let session = McpSession::connect(
        [&fixture.config],
        &rook_llm::Proxy::default(),
        Settings { parallel_connects: 0, ..limits },
    )
    .await;
    assert!(session.report().issue.unwrap().contains("parallel_connects"));
}

#[tokio::test]
async fn startup_parallelism_is_bounded_while_retaining_declaration_order() {
    let first = Fixture::new("first", true).await;
    let mut second = Fixture::new("second", false).await;
    second.config.name = "second".into();
    let configs = [first.config.clone(), second.config.clone()];
    let connecting = tokio::spawn(async move {
        McpSession::connect(
            &configs,
            &rook_llm::Proxy::default(),
            Settings { parallel_connects: 1, ..Default::default() },
        )
        .await
    });
    observed(&first.listings).await;
    assert_eq!(second.listings.available_permits(), 0);
    first.release_lists.add_permits(1);
    let session = connecting.await.unwrap();
    assert_eq!(session.servers().len(), 2);
    assert_eq!(session.servers()[0].1[0].description, "first");
    assert_eq!(session.servers()[1].1[0].description, "second");
}

#[tokio::test]
async fn last_request_failure_is_reported_without_the_server_response_and_clears_on_success() {
    let fixture = Fixture::new("healthy", false).await;
    let session =
        McpSession::connect([&fixture.config], &rook_llm::Proxy::default(), Settings::default()).await;
    let server = session.servers()[0].0.clone();
    fixture.refuse.store(true, Ordering::SeqCst);
    fixture.release_calls.add_permits(1);
    assert!(server.call_tool("echo", &json!({})).await.is_err());
    let report = session.report();
    assert!(report.servers[0].last_request_error.as_deref().unwrap().contains("refused"));
    assert_eq!(report.servers[0].active_requests, 0);
    assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
    fixture.refuse.store(false, Ordering::SeqCst);
    fixture.release_calls.add_permits(1);
    server.call_tool("echo", &json!({})).await.unwrap();
    assert!(session.report().servers[0].last_request_error.is_none());
}
