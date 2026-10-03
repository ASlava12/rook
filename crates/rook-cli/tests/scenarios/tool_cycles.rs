//! The same cycle and continuation receipts through native CLI and daemon.
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Model {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<[usize; 2]>>,
}

impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let joined = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            let counts = joined.unwrap();
            assert_eq!(
                counts,
                [9, 9],
                "five loop requests, final answer, continued refusal, new call and new answer"
            );
        }
    }
}

impl Model {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut counts = [0; 2];
            while !halt.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                socket.set_read_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                socket.set_write_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                let mut bytes = Vec::new();
                let request = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() + n <= 2 * 1024 * 1024, "bounded fixture input");
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(at) = bytes.windows(4).position(|s| s == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&bytes[..at]);
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + length {
                        break if length == 0 {
                            Value::Null
                        } else {
                            serde_json::from_slice(&bytes[at + 4..at + 4 + length]).unwrap()
                        };
                    }
                };
                let checker = request["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| {
                        message["role"] == "user"
                            && message["content"]
                                .as_str()
                                .is_some_and(|text| text.contains("the agent has just finished a turn"))
                    })
                });
                let (mime, body) = if request["stream"] == true && checker {
                    let answer = json!({"choices":[{"index":0,"delta":{"content":"No independent evidence obtained.\nVERDICT: unproven"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":3}});
                    ("text/event-stream", format!("data: {answer}\n\ndata: [DONE]\n\n"))
                } else if request["stream"] == true {
                    let group = usize::from(request["messages"].to_string().contains("guard-daemon"));
                    let at = counts[group];
                    counts[group] += 1;
                    assert!(at < 9, "a stopped cycle must not consume another physical attempt");
                    let (delta, finish) = if matches!(at, 5 | 8) {
                        (json!({"content":"The task remains open."}), "stop")
                    } else {
                        (
                            json!({"tool_calls":[{"index":0,"id":format!("call_{at}"),"type":"function","function":{"name":"run_command","arguments":json!({"command":"echo stable", "title":format!("checking {at}")}).to_string()}}]}),
                            "tool_calls",
                        )
                    };
                    let answer = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
                    ("text/event-stream", format!("data: {answer}\n\ndata: [DONE]\n\n"))
                } else if request["messages"].is_array() {
                    ("application/json", json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string())
                } else {
                    ("application/json", r#"{"data":[]}"#.into())
                };
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            counts
        });
        Self { url, stop, thread: Some(thread) }
    }
}

#[test]
fn tool_cycles_stop_native_commands_and_survive_continuation_locally_and_through_daemon() {
    rook_llm::init_tls();
    let model = Model::new();
    let rook = Rook::new();
    rook.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\n[models.local]\nmodel='test'\napi='openai'\nurl='{}'\ncontext_window=128000\n", model.url));
    for shared in [false, true] {
        let daemon = shared.then(|| Daemon::start(&rook));
        let first =
            rook.run(&["run", if shared { "guard-daemon" } else { "guard-local" }, "--yes", "--json"]);
        assert!(!first.status.success(), "a stopped cycle is unfinished: {first:?}");
        let output: Value = serde_json::from_slice(&first.stdout).unwrap();
        let outcome = output.get("outcome").unwrap_or(&output);
        let session = output["session"].as_str().unwrap();
        assert_eq!(outcome["stopped"], "looping", "{output}");
        if !shared {
            assert_eq!(outcome["tools_called"].as_array().unwrap().len(), 2);
        }
        let continued = rook.run(&["run", "/continue", "--session", session, "--yes", "--json"]);
        assert!(!continued.status.success(), "continuation does not erase the cycle receipt");
        let continued: Value = serde_json::from_slice(&continued.stdout).unwrap();
        let outcome = continued.get("outcome").unwrap_or(&continued);
        assert_eq!(outcome["stopped"], "looping", "{continued}");
        if !shared {
            assert!(outcome["tools_called"].as_array().unwrap().is_empty(), "no repeated side effect");
        }
        let fresh = rook.json(&["run", "a new authorized inspection", "--session", session, "--yes"]);
        let outcome = fresh.get("outcome").unwrap_or(&fresh);
        if !shared {
            assert_eq!(outcome["tools_called"].as_array().unwrap().len(), 1);
        }
        let coverage = &rook.json(&["session", "context", session])["cost_coverage"];
        assert_eq!(coverage["attempts_pending"], 0);
        assert_eq!(coverage["attempts_started"], coverage["attempts_completed"]);
        drop(daemon);
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let id = rook_store::parse_session_id(session).unwrap();
        let called: Vec<usize> = store
            .events(id, 0, 1000)
            .unwrap()
            .iter()
            .filter(|event| event.record.label == "turn-result")
            .map(|event| {
                let outcome: Value = serde_json::from_slice(&store.get(&event.record.body).unwrap()).unwrap();
                outcome[1]["tools_called"].as_array().unwrap().len()
            })
            .collect();
        assert_eq!(called, [2, 0, 1], "both frontends retain actual execution counts through reopen");
        let state: Value = serde_json::from_slice(
            &store
                .kv_get_limited(&format!("rook:tool-cycles:session:{id}:false"), 128 * 1024)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state["version"], 1);
        assert_eq!(state["history"].as_array().unwrap().len(), 1);
        assert!(state["stopped"].is_null());
        assert!(!state.to_string().contains("stable"), "only fingerprints are retained");
    }
}
