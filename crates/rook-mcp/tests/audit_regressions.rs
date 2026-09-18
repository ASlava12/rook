use rook_mcp::{Server, ServerConfig};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
async fn request(socket: &mut TcpStream) -> serde_json::Value {
    let mut raw = Vec::new();
    loop {
        let mut chunk = [0u8; 8192];
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&raw);
        if let Some(split) = text.find("\r\n\r\n") {
            let length: usize = text[..split]
                .lines()
                .find_map(|l| {
                    l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if raw.len() >= split + 4 + length {
                return serde_json::from_slice(&raw[split + 4..split + 4 + length]).unwrap();
            }
        }
    }
}
async fn json(socket: &mut TcpStream, value: serde_json::Value) {
    let body = value.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
}
fn config(url: String) -> ServerConfig {
    ServerConfig {
        name: "audit".into(),
        url: Some(url),
        startup_timeout_secs: 1,
        call_timeout_secs: 1,
        ..Default::default()
    }
}
#[tokio::test]
async fn json_body_is_covered_by_the_request_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        Server::connect(&config(url), &rook_llm::Proxy::default()),
    )
    .await;
    handle.abort();
    assert!(result.unwrap().is_err(), "the MCP deadline must expire before the outer watchdog");
}
#[tokio::test]
async fn a_committed_tool_call_is_not_repeated_when_its_reply_is_lost() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let req = request(&mut socket).await;
            match req["method"].as_str().unwrap() {
                "initialize" => {
                    json(
                        &mut socket,
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": req["id"],
                            "result": {
                                "protocolVersion": "2025-06-18",
                                "serverInfo": {"name": "audit", "version": "1"},
                                "capabilities": {}
                            }
                        }),
                    )
                    .await;
                }
                "notifications/initialized" => {
                    socket
                        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                }
                "tools/call" => {
                    if count.fetch_add(1, Ordering::SeqCst) > 0 {
                        json(
                            &mut socket,
                            serde_json::json!({
                                "jsonrpc": "2.0", "id": req["id"],
                                "result": {"content": [{"type": "text", "text": "committed"}]}
                            }),
                        )
                        .await;
                    }
                }
                _ => panic!("unexpected request: {req}"),
            }
        }
    });
    let server = Server::connect(&config(url), &rook_llm::Proxy::default()).await.unwrap();
    let result = server.call_tool("charge", &serde_json::json!({"amount":1})).await;
    handle.abort();
    assert!(result.is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}
