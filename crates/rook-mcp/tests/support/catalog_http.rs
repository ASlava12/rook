//! Owned streamable-HTTP fixture shared by client, tool-binding and engine tests.
#![allow(dead_code)] // Each test crate uses a different slice of the same fixture.
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct State {
    pub version: AtomicUsize,
    pub lists: AtomicUsize,
    pub gets: AtomicUsize,
    pub get_active: AtomicUsize,
    pub get_retry_ms: AtomicU64,
    pub close_get_once: AtomicBool,
    pub get_times: Mutex<Vec<std::time::Instant>>,
    pub fail: AtomicBool,
    pub empty: AtomicBool,
    pub support_changes: AtomicBool,
    pub resources_only: AtomicBool,
    pub slow_list: AtomicBool,
    pub list_started: tokio::sync::Notify,
    pub list_release: tokio::sync::Notify,
    pub invalidate_page: AtomicBool,
    pub calls: Mutex<Vec<String>>,
    pub headers: Mutex<Vec<String>>,
    pub callback_errors: AtomicUsize,
    pub events: tokio::sync::broadcast::Sender<Value>,
    pub hold: AtomicBool,
    pub call_started: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 0.into(),
            lists: 0.into(),
            gets: 0.into(),
            get_active: 0.into(),
            get_retry_ms: u64::MAX.into(),
            close_get_once: false.into(),
            get_times: Default::default(),
            fail: false.into(),
            empty: false.into(),
            support_changes: true.into(),
            resources_only: false.into(),
            slow_list: false.into(),
            list_started: Default::default(),
            list_release: Default::default(),
            invalidate_page: false.into(),
            calls: Default::default(),
            headers: Default::default(),
            callback_errors: 0.into(),
            events: tokio::sync::broadcast::channel(8).0,
            hold: false.into(),
            call_started: Default::default(),
            release: Default::default(),
        }
    }
}
impl State {
    pub fn change(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
        let _ = self.events.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}));
    }
}
pub struct Fixture {
    pub url: String,
    pub state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    pub async fn new(get_supported: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (events, _) = tokio::sync::broadcast::channel(8);
        let state = Arc::new(State { events, ..Default::default() });
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        assert!(requests.len() < 128);
                        requests.spawn(serve(socket, shared.clone(), get_supported));
                    },
                    Some(result) = requests.join_next(), if !requests.is_empty() => result.unwrap(),
                }
            }
        });
        Self { url, state, task }
    }
}

async fn serve(mut socket: tokio::net::TcpStream, state: Arc<State>, get_supported: bool) {
    let mut raw = Vec::new();
    let (head, body) = loop {
        let mut bytes = [0; 4096];
        let n = socket.read(&mut bytes).await.unwrap();
        if n == 0 {
            return;
        }
        assert!(raw.len() + n <= 1024 * 1024);
        raw.extend_from_slice(&bytes[..n]);
        let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let length: usize = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|s| s.trim().parse().ok())
            })
            .unwrap_or(0);
        if raw.len() < split + 4 + length {
            continue;
        }
        let body = if length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&raw[split + 4..split + 4 + length]).unwrap()
        };
        break (head, body);
    };
    {
        let mut headers = state.headers.lock().unwrap();
        assert!(headers.len() < 128);
        headers.push(head.clone());
    }
    if head.starts_with("GET ") {
        state.gets.fetch_add(1, Ordering::SeqCst);
        state.get_times.lock().unwrap().push(std::time::Instant::now());
        if !get_supported {
            reply(&mut socket, 405, "application/json", "").await;
            return;
        }
        let mut events = state.events.subscribe();
        if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.is_err() {return;}
        if !chunk(&mut socket, ": connected\n\n").await {
            return;
        }
        let retry = state.get_retry_ms.load(Ordering::SeqCst);
        if retry != u64::MAX && !chunk(&mut socket, &format!("retry: {retry}\n\n")).await {
            return;
        }
        if state.close_get_once.swap(false, Ordering::SeqCst) {
            return;
        }
        state.get_active.fetch_add(1, Ordering::SeqCst);
        struct Active(Arc<State>);
        impl Drop for Active {
            fn drop(&mut self) {
                self.0.get_active.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _active = Active(state.clone());
        let mut closed = [0; 1];
        loop {
            tokio::select! {
                _ = socket.read(&mut closed) => break,
                event = events.recv() => {
                    let Ok(event) = event else { break };
                    if !chunk(&mut socket, &format!("data: {event}\n\n")).await {break;}
                }
            }
        }
        return;
    }
    if body.get("id").is_none() {
        reply(&mut socket, 202, "application/json", "").await;
        return;
    }
    if body.get("method").is_none() {
        let head = head.to_ascii_lowercase();
        let accept = head
            .lines()
            .find(|line| line.starts_with("accept:"))
            .expect("callback response needs an Accept header");
        assert!(accept.contains("application/json") && accept.contains("text/event-stream"));
        assert_eq!(body["error"]["code"], -32601);
        state.callback_errors.fetch_add(1, Ordering::SeqCst);
        reply(&mut socket, 202, "application/json", "").await;
        return;
    }
    let mut notify = false;
    let result = match body["method"].as_str().unwrap() {
        "initialize" => {
            let capabilities = if state.resources_only.load(Ordering::SeqCst) {
                json!({"resources":{}})
            } else {
                json!({"tools":{"listChanged":state.support_changes.load(Ordering::SeqCst)},"resources":{}})
            };
            json!({"protocolVersion":"2025-11-25","capabilities":capabilities,"serverInfo":{"name":"owned-http-catalog"}})
        }
        "tools/list" => {
            state.lists.fetch_add(1, Ordering::SeqCst);
            if state.slow_list.load(Ordering::SeqCst) {
                state.list_started.notify_one();
                state.list_release.notified().await;
            }
            if state.fail.load(Ordering::SeqCst) {
                let response = json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32603,"message":"fixture-secret-not-for-status"}});
                reply(&mut socket, 200, "application/json", &response.to_string()).await;
                return;
            }
            if state.empty.load(Ordering::SeqCst) {
                json!({"tools":[]})
            } else if body["params"]["cursor"] == "tail" {
                json!({"tools":[{"name":"torn-tail","inputSchema":{"type":"object"}}]})
            } else if state.invalidate_page.swap(false, Ordering::SeqCst) {
                state.change();
                notify = true;
                json!({"tools":[{"name":"torn-prefix","inputSchema":{"type":"object"}}],"nextCursor":"tail"})
            } else {
                let name = if state.version.load(Ordering::SeqCst) == 0 { "old" } else { "new" };
                json!({"tools":[{"name":"advance","inputSchema":{"type":"object"}},{"name":name,"description":name,"inputSchema":{"type":"object"},"_meta":{"ui":{"resourceUri":"ui://catalog/view"}}}]})
            }
        }
        "tools/call" => {
            let name = body["params"]["name"].as_str().unwrap().to_string();
            state.calls.lock().unwrap().push(name.clone());
            if name == "hold" && state.hold.load(Ordering::SeqCst) {
                state.call_started.notify_one();
                state.release.notified().await;
            }
            if name == "advance" {
                state.change();
                notify = true;
            }
            json!({"content":[{"type":"text","text":name}]})
        }
        "resources/read" => {
            json!({"contents":[{"uri":"ui://catalog/view","mimeType":"text/html;profile=mcp-app","text":"<html><body><p>Owned catalog view</p></body></html>"}]})
        }
        _ => json!({}),
    };
    let response = json!({"jsonrpc":"2.0","id":body["id"],"result":result});
    if notify {
        let event = json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"});
        reply(&mut socket, 200, "text/event-stream", &format!("data: {event}\n\ndata: {response}\n\n")).await;
    } else {
        reply(&mut socket, 200, "application/json", &response.to_string()).await;
    }
}
async fn chunk(socket: &mut tokio::net::TcpStream, body: &str) -> bool {
    socket.write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes()).await.is_ok()
}
async fn reply(socket: &mut tokio::net::TcpStream, status: u16, mime: &str, body: &str) {
    let _ = socket.write_all(format!("HTTP/1.1 {status} Reply\r\nContent-Type: {mime}\r\nmcp-session-id: owned-catalog-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
}
