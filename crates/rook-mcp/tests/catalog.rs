use rook_mcp::{McpError, Server, ServerConfig};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Fixture {
    config: ServerConfig,
    cursors: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture(pages: Vec<Value>, delay: Duration) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let cursors = Arc::new(Mutex::new(Vec::new()));
    let log = cursors.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let request = loop {
                let mut scratch = [0u8; 4096];
                let n = socket.read(&mut scratch).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&scratch[..n]);
                assert!(raw.len() < 65536);
                let header = String::from_utf8_lossy(&raw);
                let Some(at) = header.find("\r\n\r\n") else { continue };
                let length: usize = header[..at]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if raw.len() >= at + 4 + length {
                    break serde_json::from_slice::<Value>(&raw[at + 4..at + 4 + length]).unwrap();
                }
            };
            if request["id"].is_null() {
                let _ = socket
                    .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"pages","version":"1"},"capabilities":{"tools":{}}})
                }
                "tools/list" => {
                    let i = {
                        let mut log = log.lock().unwrap();
                        let i = log.len();
                        log.push(request["params"]["cursor"].clone());
                        i
                    };
                    tokio::time::sleep(delay).await;
                    pages[i.min(pages.len() - 1)].clone()
                }
                other => panic!("unexpected method {other}"),
            };
            let body = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    Fixture {
        config: ServerConfig {
            name: "paged".into(),
            url: Some(url),
            startup_timeout_secs: 30,
            call_timeout_secs: 10,
            ..Default::default()
        },
        cursors,
        task,
    }
}
fn tool(name: &str) -> Value {
    json!({"name":name,"description":"original description","inputSchema":{"type":"object","properties":{"inner":{"oneOf":[{"type":"null"},{"type":"array","items":{"type":"string"}}]}}}})
}

#[tokio::test]
async fn all_pages_including_empty_ones_preserve_cursors_order_and_nested_schemas() {
    let first = tool("first");
    let last = tool("last");
    let f = fixture(
        vec![
            json!({"tools":[first],"nextCursor":"opaque /+ ?"}),
            json!({"tools":[],"nextCursor":"final"}),
            json!({"tools":[last]}),
        ],
        Duration::ZERO,
    )
    .await;
    let server = Server::connect(&f.config, &Default::default()).await.unwrap();
    let tools = server.list_tools().await.unwrap();
    assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["first", "last"]);
    assert_eq!(tools[1].input_schema, last["inputSchema"]);
    assert_eq!(*f.cursors.lock().unwrap(), [Value::Null, json!("opaque /+ ?"), json!("final")]);
}

#[tokio::test]
async fn cycles_duplicates_bad_envelopes_and_incomplete_catalogs_are_errors_not_prefixes() {
    for (pages, field, maximum, error, requests) in [
        (
            vec![json!({"tools":[tool("a")],"nextCursor":"cycle"}), json!({"tools":[],"nextCursor":"cycle"})],
            "pages",
            64,
            "cycle",
            2,
        ),
        (
            vec![json!({"tools":[tool("a")],"nextCursor":"next"}), json!({"tools":[tool("a")]})],
            "pages",
            64,
            "duplicate",
            2,
        ),
        (vec![json!({"tools":[],"nextCursor":false})], "pages", 64, "nextCursor", 1),
        (vec![json!({"tools":[],"nextCursor":""})], "pages", 64, "nextCursor", 1),
        (vec![json!({"unrelated":[]})], "pages", 64, "tools array", 1),
        (
            vec![
                json!({"tools":[tool("a")],"nextCursor":"one"}),
                json!({"tools":[tool("b")],"nextCursor":"two"}),
                json!({"tools":[tool("c")]}),
            ],
            "tools",
            2,
            "catalog_max_tools",
            3,
        ),
        (
            vec![
                json!({"tools":[],"nextCursor":"one"}),
                json!({"tools":[],"nextCursor":"two"}),
                json!({"tools":[]}),
            ],
            "pages",
            2,
            "catalog_max_pages",
            2,
        ),
    ] {
        let mut f = fixture(pages, Duration::ZERO).await;
        if field == "pages" {
            f.config.catalog_max_pages = maximum;
        } else {
            f.config.catalog_max_tools = maximum;
        }
        let server = Server::connect(&f.config, &Default::default()).await.unwrap();
        let message = server.list_tools().await.unwrap_err().to_string();
        assert!(message.contains(error), "{message}");
        assert_eq!(f.cursors.lock().unwrap().len(), requests);
    }
}

#[tokio::test]
async fn catalog_bytes_and_time_are_shared_by_all_pages() {
    let mut a = tool("a");
    a["description"] = json!("x".repeat(500));
    let mut b = a.clone();
    b["name"] = json!("b");
    let pages = vec![json!({"tools":[a],"nextCursor":"second"}), json!({"tools":[b]})];
    assert!(pages.iter().all(|p| p.to_string().len() < 1024));
    assert!(pages.iter().map(|p| p.to_string().len()).sum::<usize>() > 1024);
    let mut f = fixture(pages, Duration::ZERO).await;
    f.config.catalog_max_bytes = 1024;
    let server = Server::connect(&f.config, &Default::default()).await.unwrap();
    assert!(server.list_tools().await.unwrap_err().to_string().contains("catalog_max_bytes"));
    assert_eq!(f.cursors.lock().unwrap().len(), 2);
    let mut f = fixture(
        vec![json!({"tools":[],"nextCursor":"next"}), json!({"tools":[]})],
        Duration::from_millis(650),
    )
    .await;
    f.config.catalog_timeout_secs = 1;
    let server = Server::connect(&f.config, &Default::default()).await.unwrap();
    let error =
        tokio::time::timeout(Duration::from_secs(15), server.list_tools()).await.unwrap().unwrap_err();
    assert!(matches!(error,McpError::Timeout{method,..} if method=="tools/list catalog"));
    assert_eq!(
        f.cursors.lock().unwrap().len(),
        2,
        "each page fits the per-call timeout; only the shared catalog deadline expires"
    );
}
