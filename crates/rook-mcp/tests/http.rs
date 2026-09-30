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
    spawn_version(mode, "2025-06-18").await
}

async fn spawn_version(mode: &'static str, version: &'static str) -> String {
    spawn_reply(mode, version, false).await
}

async fn spawn_reply(mode: &'static str, version: &'static str, echo: bool) -> String {
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

                    let versions: Vec<_> = text[..split]
                        .lines()
                        .filter_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("mcp-protocol-version").then(|| value.trim())
                        })
                        .collect();
                    if method != "initialize" && versions != [version] {
                        let _ = socket
                            .write_all(
                                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    }
                    if id.is_null() && echo && mode.starts_with("notify-") {
                        let authorization = text[..split]
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("authorization").then_some(value.trim())
                            })
                            .expect("notification must be authenticated");
                        let (status, headers, body) = if mode == "notify-error" {
                            (500, String::new(), format!("rejected {authorization}"))
                        } else {
                            (401, format!("WWW-Authenticate: {authorization}\r\n"), String::new())
                        };
                        let reply = format!(
                            "HTTP/1.1 {status} Reply\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        socket.write_all(reply.as_bytes()).await.unwrap();
                        return;
                    }
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

                    if echo && method == "tools/call" {
                        let token = text[..split]
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("authorization")
                                    .then(|| value.trim().strip_prefix("Bearer ").unwrap().to_owned())
                            })
                            .expect("the fixture must receive the credential it reflects");
                        let payload = match mode {
                            "rpc" => {
                                serde_json::json!({"id": id, "error":{"code": -32000, "message": format!("request rejected: {token}")}})
                            }
                            "rpc-data" => {
                                serde_json::json!({"id":id,"error":{"code":-32000,"message":"refused","data":{"credential":token}}})
                            }
                            "resource" => {
                                serde_json::json!({"id":id,"result":{"content":[{"type":"resource","resource":{token.clone(): "sensitive key"}}]}})
                            }
                            _ => {
                                serde_json::json!({"id": id, "result":{"content":[{"type":"text","text":format!("authorization was {token}")}]}})
                            }
                        };
                        let (status, extra, body) = match mode {
                            "sse" => (
                                200,
                                "Content-Type: text/event-stream\r\n".to_owned(),
                                format!("data: {payload}\n\n"),
                            ),
                            "http-error" => (500, String::new(), format!("request rejected: {token}")),
                            "unauthorized" => (
                                401,
                                format!("WWW-Authenticate: Bearer error_description=\"{token}\"\r\n"),
                                String::new(),
                            ),
                            "bad-json" => (200, String::new(), format!("invalid response: {token}")),
                            _ => (200, "Content-Type: application/json\r\n".to_owned(), payload.to_string()),
                        };
                        let reply = format!(
                            "HTTP/1.1 {status} Reply\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        socket.write_all(reply.as_bytes()).await.unwrap();
                        return;
                    }

                    let result = match method {
                        "initialize" => serde_json::json!({
                            "protocolVersion": version,
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
    let server = Server::connect(&config(spawn("json").await), &Default::default()).await.unwrap();
    assert_eq!(server.info().server.name, "over-http");
    assert_eq!(server.list_tools().await.unwrap()[0].name, "ping");
    let result = server.call_tool("ping", &serde_json::json!({})).await.unwrap();
    assert_eq!(result.to_text(), "pong");
}

#[tokio::test]
async fn an_event_stream_answer_round_trips_and_skips_what_is_not_the_reply() {
    let server = Server::connect(&config(spawn("sse").await), &Default::default()).await.unwrap();
    assert_eq!(server.info().server.version, "2.0.0");
    let result = server.call_tool("ping", &serde_json::json!({})).await.unwrap();
    assert_eq!(result.to_text(), "pong", "the notification before it must not be mistaken for the answer");
}

#[tokio::test]
async fn the_session_id_from_initialize_is_carried_on_later_requests() {
    // The mock rejects anything after initialize that arrives without it, so a
    // successful tools/list is the assertion.
    let server = Server::connect(&config(spawn("json").await), &Default::default()).await.unwrap();
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
    let Err(err) = Server::connect(&config, &Default::default()).await else {
        panic!("a dead endpoint must not connect")
    };
    assert!(matches!(err, McpError::Transport { .. }), "{err}");
    assert!(err.to_string().contains("dead"), "{err}");
}

#[tokio::test]
async fn a_server_with_neither_command_nor_url_says_so() {
    let config = ServerConfig { name: "empty".into(), ..Default::default() };
    let Err(err) = Server::connect(&config, &Default::default()).await else {
        panic!("nothing to connect to")
    };
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
    let Err(err) = Server::connect(&config, &Default::default()).await else {
        panic!("it should not have connected")
    };
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
    let answered =
        tokio::time::timeout(Duration::from_secs(120), Server::connect(&config, &Default::default()))
            .await
            .expect("a body with no end must not be read to the end");
    let Err(refused) = answered else { panic!("a server sending for ever must not connect") };
    let refused = refused.to_string();

    assert!(refused.contains("still sending"), "{refused}");
    assert!(refused.contains("8388608"), "and says the bound it passed: {refused}");
}

/// A server that answers 401 and says what it would accept. Two challenges,
/// because more than one is allowed and the one that names the authorisation
/// server is not always the first.
async fn refusing() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 8192];
            let _ = socket.read(&mut scratch).await;
            let body = r#"{"error":"unauthorized"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\
                 WWW-Authenticate: Bearer resource_metadata=\"https://auth.example/.well-known\"\r\n\
                 WWW-Authenticate: Basic realm=\"mcp\"\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}/mcp")
}

/// A 401 is the one failure here a person can act on, and the header is where
/// the authorisation server is named. Reported as a status alone it is a dead
/// end: the challenge is gone and nothing says credentials are the answer.
#[tokio::test]
async fn a_server_that_wants_authentication_says_so_and_keeps_every_challenge() {
    let Err(refused) = Server::connect(&config(refusing().await), &Default::default()).await else {
        panic!("a server that answers 401 cannot have connected")
    };

    let said = refused.to_string();
    assert!(matches!(refused, McpError::Unauthorized { .. }), "{said}");
    assert!(said.contains("wants authentication"), "{said}");
    assert!(said.contains("https://auth.example/.well-known"), "the challenge is the way out: {said}");
    assert!(said.contains("Basic realm=\"mcp\""), "and every challenge is kept: {said}");
    assert!(said.contains("[[mcp]]"), "and it says where credentials go: {said}");
}

#[tokio::test]
async fn every_post_after_initialize_uses_the_negotiated_version_once() {
    let mut config = config(spawn_version("json", "2025-03-26").await);
    assert_ne!(rook_mcp::PROTOCOL_VERSION, "2025-03-26");
    config.headers.insert("MCP-Protocol-Version".into(), "incorrect-override".into());
    // The fixture rejects initialized notifications, tools/list and tools/call
    // unless each has exactly the server's version, including on a downgrade.
    let server = Server::connect(&config, &Default::default()).await.unwrap();
    assert_eq!(server.info().protocol_version, "2025-03-26");
    assert_eq!(server.list_tools().await.unwrap().len(), 1);
    assert_eq!(server.call_tool("ping", &serde_json::json!({})).await.unwrap().to_text(), "pong");
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_negotiated_versions_fail_before_initialized_notification() {
    for version in ["", "2025-06-18\r\nx-injected: bad", "not a version"] {
        let result =
            Server::connect(&config(spawn_version("json", version).await), &Default::default()).await;
        assert!(matches!(result, Err(McpError::Decode { .. })), "version {version:?} was accepted");
    }
}

#[tokio::test]
async fn an_oauth_credential_reflected_by_the_server_never_leaves_the_transport() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Rotating(AtomicUsize);
    #[async_trait::async_trait]
    impl rook_mcp::oauth::TokenSource for Rotating {
        async fn token(&self) -> Result<String, &'static str> {
            Ok(format!("private-access-token-{}", self.0.fetch_add(1, Ordering::SeqCst)))
        }
    }
    for mode in ["notify-error", "notify-unauthorized"] {
        let outcome = Server::connect_with_token(
            &config(spawn_reply(mode, "2025-06-18", true).await),
            &Default::default(),
            Some(Arc::new(Rotating(AtomicUsize::new(0)))),
        )
        .await;
        let error = match outcome {
            Err(error) => error,
            Ok(_) => panic!("rejected initialization cannot connect"),
        };
        let output = format!("{error:?}: {error}");
        assert!(
            !output.contains("private-access-token"),
            "{mode} exposed notification credentials: {output}"
        );
    }
    for mode in ["json", "sse", "rpc", "rpc-data", "resource", "http-error", "unauthorized", "bad-json"] {
        let tokens = Arc::new(Rotating(AtomicUsize::new(0)));
        let server = Server::connect_with_token(
            &config(spawn_reply(mode, "2025-06-18", true).await),
            &Default::default(),
            Some(tokens.clone()),
        )
        .await
        .unwrap();
        let args = serde_json::json!({});
        let (first, second) = tokio::join!(server.call_tool("ping", &args), server.call_tool("ping", &args));
        for outcome in [first, second] {
            assert!(outcome.is_err(), "{mode}: a credential-bearing reply must be refused whole");
            let output = match outcome {
                Ok(reply) => reply.to_text(),
                Err(error) => format!("{error:?}: {error}"),
            };
            assert!(
                !output.contains("private-access-token"),
                "{mode} leaked a reflected OAuth credential: {output}"
            );
        }
        assert!(tokens.0.load(Ordering::SeqCst) >= 4, "credentials must be acquired per request");
    }
}
