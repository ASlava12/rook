use serde_json::Value;
use std::{
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub(super) struct Server {
    pub(super) url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub(super) fn new(
        mut respond: impl FnMut(&str, Value) -> Option<(&'static str, String)> + Send + 'static,
    ) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let thread = std::thread::spawn(move || {
            while !halt.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
                let mut bytes = Vec::new();
                let (path, request) = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() + n <= 2 * 1024 * 1024);
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&bytes[..at]);
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + len {
                        break (
                            head.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned(),
                            if len == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[at + 4..at + 4 + len]).unwrap()
                            },
                        );
                    }
                };
                if let Some((mime, body)) = respond(&path, request) {
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            }
        });
        Self { url, stop, thread: Some(thread) }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let done = thread.join();
            if !std::thread::panicking() {
                assert!(done.is_ok());
            }
        }
    }
}
