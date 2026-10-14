//! Ready batches through both native entry points, with exact reopened receipts.
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

type Counts = ([usize; 2], [usize; 4], [usize; 2]);

struct Model {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Counts>>,
}

impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let joined = self.thread.take().unwrap().join();
        eprintln!("native delegation fixture stages and child requests: {joined:?}");
        if !std::thread::panicking() {
            let (stages, children, requests) = joined.unwrap();
            assert_eq!(
                (stages, children),
                ([4, 4], [2, 2, 2, 2]),
                "both parents reached their terminal stage and all four children actually ran"
            );
            assert!(
                requests.iter().all(|n| (5..=8).contains(n)),
                "both parents actually retried an unfinished wait: {requests:?}"
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
            let mut parents = [0; 2];
            let mut parent_requests = [0; 2];
            let mut children = [0; 4];
            let nursery_release = [0; 2].map(|_| Arc::new(AtomicBool::new(false)));
            let mut held = Vec::new();
            while !halt.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                socket.set_write_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                let mut bytes = Vec::new();
                let request: Value = loop {
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
                let mut defer = None;
                let (mime, body) = if request["stream"] == true {
                    // OpenAI accepts string/structured content and can fold
                    // consecutive user messages. Match the task line, not a
                    // marker quoted inside the source-wrapped nursery report.
                    let task = request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|message| message["role"] == "user")
                        .find_map(|message| {
                            let content = &message["content"];
                            content
                                .as_str()
                                .and_then(|text| text.lines().find(|line| line.starts_with("AUDIT CHILD: ")))
                                .or_else(|| {
                                    content
                                        .as_array()?
                                        .iter()
                                        .filter_map(|part| part["text"].as_str())
                                        .flat_map(str::lines)
                                        .find(|line| line.starts_with("AUDIT CHILD: "))
                                })
                        });
                    let mut body = String::new();
                    let checker = request["messages"]
                        .to_string()
                        .contains("You are checking a claim somebody else made.");
                    let (delta, finish) = if checker {
                        (
                            json!({"content":"The child records show completed read operations.\nVERDICT: proven"}),
                            "stop",
                        )
                    } else if let Some(task) = task {
                        let group = usize::from(task.contains("daemon"));
                        let at = group * 2 + usize::from(task.contains("nursery"));
                        let step = children[at];
                        children[at] += 1;
                        assert!(step < 2, "no extra child attempt: {task}");
                        if step == 0 {
                            for _ in 0..1024 {
                                body.push_str(&format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":"a"},"finish_reason":null}]})));
                            }
                            let calls: Vec<_> = (0..256).map(|i| json!({"index":i,"id":format!("read-{i}"),"type":"function","function":{"name":"read_file","arguments":r#"{"path":"notes.txt"}"#}})).collect();
                            (json!({"tool_calls":calls}), "tool_calls")
                        } else {
                            if task.contains("nursery") {
                                // This child cannot finish before its parent's
                                // first zero-wait poll returned still-running.
                                defer = Some(nursery_release[group].clone());
                            }
                            (json!({"content":"child report"}), "stop")
                        }
                    } else {
                        let group =
                            usize::from(request["messages"].to_string().contains("audit-native-daemon"));
                        let step = parents[group];
                        parent_requests[group] += 1;
                        assert!(
                            parent_requests[group] <= 8,
                            "parent never collected its children: {request}"
                        );
                        let scope = if group == 0 { "local" } else { "daemon" };
                        let latest_tool = request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .rev()
                            .find(|m| m["role"] == "tool")
                            .map(|m| m["content"].to_string())
                            .unwrap_or_default();
                        // A bounded wait may return while the child is running
                        // on a loaded host. Only its actual report closes this
                        // stage; another poll must not claim the work finished.
                        let waiting = step == 3 && latest_tool.contains("still running");
                        if waiting {
                            nursery_release[group].store(true, Ordering::SeqCst);
                        }
                        if !waiting {
                            parents[group] += 1;
                        }
                        let (tool, args) = match step {
                            0 => (
                                "delegate",
                                json!({"task":format!("AUDIT CHILD: {scope} blocking"),"wait":true}),
                            ),
                            1 => (
                                "delegate",
                                json!({"task":format!("AUDIT CHILD: {scope} nursery"),"wait":false}),
                            ),
                            2 => ("subagents", json!({"wait_secs":0})),
                            3 if waiting => ("subagents", json!({"wait_secs":60})),
                            3 => {
                                assert!(
                                    latest_tool.contains("child report"),
                                    "no completed child source: {latest_tool}"
                                );
                                ("", Value::Null)
                            }
                            _ => panic!("unexpected parent attempt {step}"),
                        };
                        if tool.is_empty() {
                            (json!({"content":"Collected both reports."}), "stop")
                        } else {
                            (
                                json!({"tool_calls":[{"index":0,"id":format!("parent-{step}-{}",parent_requests[group]),"type":"function","function":{"name":tool,"arguments":args.to_string()}}]}),
                                "tool_calls",
                            )
                        }
                    };
                    body.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5}})));
                    ("text/event-stream", body)
                } else if request["messages"].is_array() {
                    ("application/json", json!({"choices":[{"message":{"role":"assistant","content":r#"{"action":"finish"}"#},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string())
                } else {
                    ("application/json", r#"{"data":[]}"#.into())
                };
                let mut send = move || {
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                };
                if let Some(released) = defer {
                    assert!(held.len() < 2, "only one held nursery reply per parent");
                    let halt = halt.clone();
                    held.push(std::thread::spawn(move || {
                        while !released.load(Ordering::SeqCst) && !halt.load(Ordering::SeqCst) {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        send();
                    }));
                } else {
                    send();
                }
            }
            for thread in held {
                assert!(thread.join().is_ok());
            }
            (parents, children, parent_requests)
        });
        Self { url, stop, thread: Some(thread) }
    }
}

#[test]
fn coalesced_delegation_keeps_blocking_and_nursery_receipts_locally_and_through_daemon() {
    rook_llm::init_tls();
    let model = Model::new();
    for shared in [false, true] {
        let _one = (!shared).then(one_at_a_time);
        let rook = Rook::new();
        std::fs::write(rook.workspace.path().join("notes.txt"), "native actual file\n").unwrap();
        rook.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\ntool_cycle_guard=false\ndelegation_progress_entries=1\n[models.local]\nmodel='test'\napi='openai'\nurl='{}'\ncontext_window=200000\n", model.url));
        let daemon = shared.then(|| Daemon::start(&rook));
        let reply =
            rook.json(&["run", if shared { "audit-native-daemon" } else { "audit-native-local" }, "--yes"]);
        let parent = rook_store::parse_session_id(reply["session"].as_str().unwrap()).unwrap();
        assert_eq!(reply.get("outcome").unwrap_or(&reply)["reply"], "Collected both reports.", "{reply}");
        drop(daemon);
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let children: Vec<_> = store
            .list_sessions()
            .unwrap()
            .into_iter()
            .filter(|meta| {
                meta.parent == Some(parent)
                    && meta.title.starts_with("AUDIT CHILD:")
                    && meta.tags.iter().any(|tag| tag == "subtask")
            })
            .collect();
        assert_eq!(children.len(), 2, "both actual modes must produce child sessions");
        for child in children {
            let events = store.events(child.id, 0, 2048).unwrap();
            assert_eq!(
                events.iter().filter(|e| e.record.kind == rook_store::EventKind::ToolCall).count(),
                256
            );
            assert_eq!(
                events.iter().filter(|e| e.record.kind == rook_store::EventKind::ToolResult).count(),
                256
            );
            let receipt: Value = serde_json::from_slice(
                &store.kv_get(&format!("execution/{:032x}", child.id)).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(receipt["completed_operations"], 256);
            assert_eq!(receipt["status"], "end_turn");
            assert!(receipt["pending"].is_null());
            assert!(receipt["unknown"].as_array().unwrap().is_empty());
            let last = receipt["last_result_seq"].as_u64().unwrap();
            assert_eq!(
                store.events(child.id, last, 1).unwrap()[0].record.kind,
                rook_store::EventKind::ToolResult
            );
            let outcome: Value = serde_json::from_slice(
                &store.kv_get(&format!("execution-outcome/{:032x}", child.id)).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(outcome[1]["reply"], "child report");
            assert_eq!(outcome[1]["tools_called"].as_array().unwrap().len(), 256);
        }
    }
}
