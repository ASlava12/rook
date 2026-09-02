//! The HTTP transport, against a server that answers both ways.
//!
//! The protocol allows a POST to be answered with a single JSON object or with
//! an event stream, and a client that only handles one of them fails against
//! half the servers it meets.

use std::time::Duration;

use rook_mcp::{McpError, Server, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// `mode` is "json" or "sse"; the server answers initialize, tools/list and
/// tools/call, and requires the session id it hands out on initialize.
async fn spawn(mode: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut scratch = [0u8; 8192];
                // Read until the body is in hand: headers, then Content-Length.
                loop {
                    let n = match socket.read(&mut scratch).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    raw.extend_from_slice(&scratch[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(split) = text.find("\r\n\r\n") else { continue };
                    let length: usize = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if raw.len() < split + 4 + length {
                        continue;
                    }
                    let body = &text[split + 4..];
                    let request: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                    let method = request["method"].as_str().unwrap_or("");
                    let id = request["id"].clone();
                    let has_session = text.to_lowercase().contains("mcp-session-id: s-1");

                    if id.is_null() {
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    }
                    if method != "initialize" && !has_session {
                        let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 15\r\nConnection: close\r\n\r\nno session id\r\n").await;
                        return;
                    }

                    let result = match method {
                        "initialize" => serde_json::json!({
                            "protocolVersion": "2025-06-18",
                            "serverInfo": { "name": "over-http", "version": "2.0.0" },
                            "capabilities": { "tools": {} }
                        }),
                        "tools/list" => serde_json::json!({ "tools": [
                            { "name": "ping", "description": "Ping.", "inputSchema": { "type": "object" } }
                        ]}),
                        _ => serde_json::json!({ "content": [{ "type": "text", "text": "pong" }] }),
                    };
                    let payload = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });

                    let session = if method == "initialize" { "mcp-session-id: s-1\r\n" } else { "" };
                    let response = if mode == "sse" {
                        // Split across two events, with an unrelated notification
                        // first, so the client must skip what is not its answer.
                        let noise = r#"data: {"jsonrpc":"2.0","method":"notifications/progress"}"#;
                        let body = format!("{noise}\n\ndata: {payload}\n\n");
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{session}Connection: close\r\n\r\n{body}"
                        )
                    } else {
                        let body = payload.to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{session}Connection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                    return;
                }
            });
        }
    });
    format!("http://{addr}/mcp")
}

fn config(url: String) -> ServerConfig {
    ServerConfig {
        name: "remote".into(),
        url: Some(url),
        startup_timeout_secs: 5,
        call_timeout_secs: 5,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_json_answer_round_trips() {
    let server = Server::connect(&config(spawn("json").await)).await.unwrap();
    assert_eq!(server.info().server.name, "over-http");
    assert_eq!(server.list_tools().await.unwrap()[0].name, "ping");
    let result = server.call_tool("ping", &serde_json::json!({})).await.unwrap();
    assert_eq!(result.to_text(), "pong");
}

#[tokio::test]
async fn an_event_stream_answer_round_trips_and_skips_what_is_not_the_reply() {
    let server = Server::connect(&config(spawn("sse").await)).await.unwrap();
    assert_eq!(server.info().server.version, "2.0.0");
    let result = server.call_tool("ping", &serde_json::json!({})).await.unwrap();
    assert_eq!(result.to_text(), "pong", "the notification before it must not be mistaken for the answer");
}

#[tokio::test]
async fn the_session_id_from_initialize_is_carried_on_later_requests() {
    // The mock rejects anything after initialize that arrives without it, so a
    // successful tools/list is the assertion.
    let server = Server::connect(&config(spawn("json").await)).await.unwrap();
    assert!(server.list_tools().await.is_ok(), "the session header was not echoed back");
}

#[tokio::test]
async fn a_url_that_refuses_connections_fails_with_the_server_named() {
    let config = ServerConfig {
        name: "dead".into(),
        url: Some("http://127.0.0.1:1/mcp".into()),
        startup_timeout_secs: 3,
        ..Default::default()
    };
    let Err(err) = Server::connect(&config).await else { panic!("a dead endpoint must not connect") };
    assert!(matches!(err, McpError::Transport { .. }), "{err}");
    assert!(err.to_string().contains("dead"), "{err}");
}

#[tokio::test]
async fn a_server_with_neither_command_nor_url_says_so() {
    let config = ServerConfig { name: "empty".into(), ..Default::default() };
    let Err(err) = Server::connect(&config).await else { panic!("nothing to connect to") };
    assert!(matches!(err, McpError::NotConfigured { .. }), "{err}");
}

#[tokio::test]
async fn a_slow_endpoint_times_out_rather_than_hanging() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut scratch = [0u8; 8192];
        let _ = socket.read(&mut scratch).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let mut config = config(format!("http://{addr}/mcp"));
    config.startup_timeout_secs = 1;
    let Err(err) = Server::connect(&config).await else { panic!("it should not have connected") };
    assert!(matches!(err, McpError::Timeout { .. }), "{err}");
}

/// A url is configuration, so how much comes back is decided by whatever
/// answers it. Reading the body and then measuring it is a cap already paid.
#[tokio::test]
async fn a_server_that_never_stops_answering_is_refused_rather_than_held() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut scratch = [0u8; 8192];
                let _ = socket.read(&mut scratch).await;
                let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                            Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let chunk = format!("{:x}\r\n{}\r\n", 64 * 1024, "x".repeat(64 * 1024));
                while socket.write_all(chunk.as_bytes()).await.is_ok() {}
            });
        }
    });

    let config = ServerConfig {
        name: "endless".into(),
        url: Some(format!("http://{addr}/mcp")),
        startup_timeout_secs: 30,
        ..Default::default()
    };
    let answered = tokio::time::timeout(Duration::from_secs(120), Server::connect(&config))
        .await
        .expect("a body with no end must not be read to the end");
    let Err(refused) = answered else { panic!("a server sending for ever must not connect") };
    let refused = refused.to_string();

    assert!(refused.contains("still sending"), "{refused}");
    assert!(refused.contains("8388608"), "and says the bound it passed: {refused}");
}
