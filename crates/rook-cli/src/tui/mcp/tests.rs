#![cfg(test)]

use super::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

struct Fixture {
    config: rook_mcp::ServerConfig,
    began: Receiver<()>,
    release: SyncSender<()>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let config = rook_mcp::ServerConfig {
            name: "owned-slow-server".into(),
            url: Some(format!("http://{}/mcp", listener.local_addr().unwrap())),
            startup_timeout_secs: 30,
            ..Default::default()
        };
        let (began_send, began) = sync_channel(1);
        let (release, resume) = sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut header = Vec::new();
                let mut byte = [0];
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 16384);
                    if socket.read_exact(&mut byte).is_err() {
                        break;
                    }
                    header.push(byte[0]);
                }
                if !header.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let header = String::from_utf8(header).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                assert!(length < 65536);
                if length == 0 {
                    let _ = socket.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                let mut bytes = vec![0; length];
                socket.read_exact(&mut bytes).unwrap();
                let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if request.get("id").is_none() {
                    let _ = socket.write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => serde_json::json!({
                        "protocolVersion":"2025-11-25", "capabilities":{"tools":{}},
                        "serverInfo":{"name":"owned","version":"1"}
                    }),
                    "tools/list" => {
                        began_send.try_send(()).unwrap();
                        resume.recv_timeout(Duration::from_secs(30)).unwrap();
                        serde_json::json!({"tools":[{"name":"echo", "description":"owned ready tool",
                            "inputSchema":{"type":"object","properties":{}}}]})
                    }
                    method => panic!("unexpected request {method}"),
                };
                let body =
                    serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes());
            }
        });
        Self { config, began, release, stop, thread: Some(thread) }
    }

    fn app(&self) -> (tempfile::TempDir, App) {
        let home = tempfile::tempdir().unwrap();
        let config = rook_core::Config { mcp: vec![self.config.clone()], ..Default::default() };
        let rook = rook_core::Rook::from_parts(
            rook_store::Store::open(home.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            std::path::PathBuf::from("."),
        );
        let runtime =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        (home, App::new(crate::source::Source::Local(Arc::new(rook)), runtime, true))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.release.try_send(());
        let result = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

#[test]
fn initial_mcp_discovery_leaves_the_window_and_draft_live_without_admitting_or_replaying_a_prompt() {
    let fixture = Fixture::new();
    let (_home, mut app) = fixture.app();
    fixture.began.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(app.shared.mcp.pending_connections(), 1);
    app.chat.input.set("  UNSENT_MCP_STARTUP_DRAFT  ");
    let history = app.chat.history.clone();
    app.on_key(crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.chat.input.as_str(), "  UNSENT_MCP_STARTUP_DRAFT  ");
    assert_eq!(app.chat.history, history);
    assert!(!app.chat.busy && app.chat.session.is_none());
    assert!(app.source.sessions().unwrap().is_empty());
    app.command("mcp");
    app.mcp.poll();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains("owned-slow-server") && screen.contains("connecting"));
    fixture.release.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let notice = loop {
        if let Some(notice) = app.mcp.poll() {
            break notice;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(notice.contains("1 connected, 0 failed"));
    assert_eq!(app.shared.mcp.pending_connections(), 0);
    assert_eq!(app.shared.mcp.servers()[0].1[0].description, "owned ready tool");
    assert_eq!(app.chat.input.as_str(), "  UNSENT_MCP_STARTUP_DRAFT  ");
    assert!(!app.chat.busy && app.chat.session.is_none());
    assert!(app.source.sessions().unwrap().is_empty(), "finishing startup must not send a prompt");
    assert!(app.mcp.poll().is_none(), "startup completion is announced once");
    let session = app.shared.mcp.clone();
    drop(app);
    assert!(session.servers().is_empty());
    assert_eq!(session.report().servers[0].state, "closed");
}

#[test]
fn closing_the_window_cancels_owned_initial_mcp_discovery_before_the_server_is_released() {
    let fixture = Fixture::new();
    let (_home, app) = fixture.app();
    fixture.began.recv_timeout(Duration::from_secs(10)).unwrap();
    let session = app.shared.mcp.clone();
    drop(app);
    assert_eq!(session.pending_connections(), 0);
    assert!(session.servers().is_empty());
    assert_eq!(session.report().servers[0].state, "closed");
    // The fixture's held response is released only by its later Drop.
}
