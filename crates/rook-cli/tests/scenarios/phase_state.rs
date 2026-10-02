//! Native state continuity through real local and daemon turns, reopen and forks.
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct Native {
    url: String,
    requests: mpsc::Receiver<Value>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Native {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn opaque(anthropic: bool) -> Value {
    if anthropic {
        json!([
            {"type":"thinking","thinking":"inspect then write","signature":"signed-original"},
            {"type":"redacted_thinking","data":"redacted-original"},
            {"type":"tool_use","id":"original-write","name":"write_file","input":{"path":"evidence.txt","content":"after\n"}}
        ])
    } else {
        json!([
            {"type":"reasoning","id":"rs-original","encrypted_content":"encrypted-original","summary":[],"future_field":{"keep":true}},
            {"type":"function_call","id":"fc-original","call_id":"original-write","name":"write_file","arguments":"{\"path\":\"evidence.txt\",\"content\":\"after\\n\"}"}
        ])
    }
}
impl Native {
    fn new(anthropic: bool) -> Self {
        Self::with_quota(anthropic, false)
    }
    fn with_quota(anthropic: bool, refuse_first: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let (send, requests) = mpsc::sync_channel(32);
        let thread = std::thread::spawn(move || {
            let mut ordinal = 0;
            let mut connections = 0;
            while !halt.load(Ordering::SeqCst) && connections < 64 {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                connections += 1;
                // Accepted sockets can inherit the listener's nonblocking mode
                // on Windows; a first read must wait for the request to arrive.
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                let mut bytes = Vec::new();
                let request: Value = loop {
                    let mut chunk = [0; 8192];
                    let n = match socket.read(&mut chunk) {
                        Ok(n) => n,
                        Err(error) => {
                            eprintln!("native read failed after {} bytes: {error}", bytes.len());
                            break Value::Null;
                        }
                    };
                    if n == 0 {
                        break Value::Null;
                    }
                    assert!(bytes.len() + n <= 4 * 1024 * 1024);
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(split) = bytes.windows(4).position(|s| s == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&bytes[..split]);
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() < split + 4 + length {
                        continue;
                    }
                    break serde_json::from_slice(&bytes[split + 4..split + 4 + length]).unwrap();
                };
                if request.is_null() {
                    continue;
                }
                let streamed = request["stream"] == true;
                let first = streamed && ordinal == 0;
                if streamed {
                    ordinal += 1;
                    send.try_send(request.clone()).unwrap();
                }
                if streamed && refuse_first && ordinal == 1 {
                    let body = r#"{"error":{"code":"insufficient_quota"}}"#;
                    let _ = write!(
                        socket,
                        "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    continue;
                }
                let text =
                    if streamed { "Finished from existing results." } else { r#"{"action":"finish"}"# };
                let output = if first {
                    opaque(anthropic)
                } else if anthropic {
                    json!([{"type":"text","text":text}])
                } else {
                    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}])
                };
                let (mime, body) = if anthropic {
                    let response = json!({"id":"msg-native","model":request["model"],"content":output,"stop_reason":if first {"tool_use"} else {"end_turn"},"usage":{"input_tokens":1,"output_tokens":1}});
                    if streamed {
                        let mut body = format!(
                            "data: {}\n\n",
                            json!({"type":"message_start","message":{"model":request["model"],"usage":{"input_tokens":1,"output_tokens":0}}})
                        );
                        for (index, block) in output.as_array().unwrap().iter().enumerate() {
                            let start = match block["type"].as_str().unwrap() {
                                "thinking" => json!({"type":"thinking","thinking":""}),
                                "tool_use" => {
                                    json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}})
                                }
                                "text" => json!({"type":"text","text":""}),
                                "redacted_thinking" => block.clone(),
                                other => panic!("unexpected fixture block: {other}"),
                            };
                            body.push_str(&format!(
                                "data: {}\n\n",
                                json!({"type":"content_block_start","index":index,"content_block":start})
                            ));
                            let deltas = match block["type"].as_str().unwrap() {
                                "thinking" => vec![
                                    json!({"type":"thinking_delta","thinking":block["thinking"]}),
                                    json!({"type":"signature_delta","signature":block["signature"]}),
                                ],
                                "tool_use" => vec![
                                    json!({"type":"input_json_delta","partial_json":block["input"].to_string()}),
                                ],
                                "text" => vec![json!({"type":"text_delta","text":block["text"]})],
                                "redacted_thinking" => Vec::new(),
                                other => panic!("unexpected fixture block: {other}"),
                            };
                            for delta in deltas {
                                body.push_str(&format!(
                                    "data: {}\n\n",
                                    json!({"type":"content_block_delta","index":index,"delta":delta})
                                ));
                            }
                            body.push_str(&format!(
                                "data: {}\n\n",
                                json!({"type":"content_block_stop","index":index})
                            ));
                        }
                        body.push_str(&format!("data: {}\n\ndata: {{\"type\":\"message_stop\"}}\n\n", json!({"type":"message_delta","delta":{"stop_reason":response["stop_reason"]},"usage":{"output_tokens":1}})));
                        ("text/event-stream", body)
                    } else {
                        ("application/json", response.to_string())
                    }
                } else {
                    let response = json!({"status":"completed","model":request["model"],"output":output,"usage":{"input_tokens":1,"output_tokens":1}});
                    if streamed {
                        (
                            "text/event-stream",
                            format!("data: {}\n\n", json!({"type":"response.completed","response":response})),
                        )
                    } else {
                        ("application/json", response.to_string())
                    }
                };
                assert!(body.len() <= 8192);
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            assert!(connections < 64, "native fixture connection budget exhausted: ordinal={ordinal}");
        });
        Self { url, requests, stop, thread: Some(thread) }
    }
    fn next(&self) -> Value {
        // Each CLI invocation has finished after the server captured its request.
        self.requests
            .try_recv()
            .unwrap_or_else(|why| panic!("missing completed request at {}: {why}", self.url))
    }
}

fn assert_replay(request: &Value, anthropic: bool, incomplete: bool) {
    let original = opaque(anthropic);
    if anthropic {
        let messages = request["messages"].as_array().unwrap();
        let blocks = messages
            .iter()
            .filter_map(|m| m["content"].as_array())
            .find(|blocks| blocks.iter().any(|b| b["type"] == "thinking"))
            .unwrap();
        assert_eq!(&blocks[..2], &original.as_array().unwrap()[..2]);
        let mut call = blocks[2].clone();
        if let Some(cache) = call.as_object_mut().unwrap().remove("cache_control") {
            assert_eq!(cache["type"], "ephemeral");
        }
        assert_eq!(call, original[2]);
        let result = messages
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == "original-write")
            .unwrap();
        assert_eq!(result["content"].as_str().unwrap().contains("no result was recorded"), incomplete);
        assert!(!request.to_string().contains("rook_anthropic_scope"));
    } else {
        let items = request["input"].as_array().unwrap();
        let at = items.iter().position(|item| item["id"] == "rs-original").unwrap();
        assert_eq!(&items[at..at + 2], original.as_array().unwrap());
        let result = items[at + 2..]
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == "original-write")
            .unwrap();
        assert_eq!(result["output"].as_str().unwrap().contains("no result was recorded"), incomplete);
    }
}

fn run(rook: &Rook, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = rook.run(&args);
    assert!(
        output.status.success(),
        "native turn failed: {}\ndaemon stderr: {}",
        String::from_utf8_lossy(&output.stderr),
        std::fs::read_to_string(rook.home.path().join("rookd.err")).unwrap_or_default()
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn native_phase_state_survives_continuation_reopen_and_both_fork_boundaries_locally_and_through_daemon() {
    rook_llm::init_tls();
    for shared in [false, true] {
        for anthropic in [false, true] {
            for compatible in [false, true] {
                eprintln!("phase shared={shared} anthropic={anthropic} compatible={compatible}");
                let rook = Rook::new();
                let model = Native::new(anthropic);
                let api = if anthropic { "anthropic" } else { "responses" };
                let physical = if anthropic { "claude-opus-5" } else { "gpt-6-astra" };
                let target = if compatible { physical } else { "different-physical-model" };
                rook.write_config(&format!("[agent]\nmodel='analysis'\ninstall_servers=false\none_script=false\nplan_first=false\n[sandbox]\nmode='ask'\nallow=['evidence.txt']\n[models.analysis]\napi='{api}'\nmodel='{physical}'\nurl='{}'\nkey='fixture-key'\ncontext_window=128000\nimplementation_model='implementation'\n[models.implementation]\napi='{api}'\nmodel='{target}'\nurl='{}'\nkey='fixture-key'\ncontext_window=128000\n", model.url, model.url));
                let mut daemon = shared.then(|| Daemon::start(&rook));
                let first = run(&rook, &["run", "Write evidence once, then answer."]);
                let session = first["session"].as_str().unwrap();
                let source = model.next();
                let continuation = model.next();
                assert_replay(&continuation, anthropic, false);
                assert_eq!(continuation["tools"], source["tools"]);
                drop(daemon.take());
                let (interrupted, before, after) = {
                    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
                    let id = rook_store::parse_session_id(session).unwrap();
                    let events = store.events(id, 0, 256).unwrap();
                    let before = events.iter().find(|e| e.record.label == "rook:model-phase:v1").unwrap().seq;
                    let after = store.get_session(id).unwrap().unwrap().next_seq;
                    let interrupted = events
                        .iter()
                        .find(|e| {
                            e.record.kind == rook_store::EventKind::ToolResult
                                && e.record.label == "write_file"
                        })
                        .unwrap()
                        .seq;
                    assert!(interrupted < before && before < after);
                    (interrupted, before, after)
                };
                daemon = shared.then(|| Daemon::start(&rook));
                run(&rook, &["run", "Report existing results without another write.", "--session", session]);
                let resumed = model.next();
                assert_replay(&resumed, anthropic, false);
                assert_eq!(resumed["tools"], source["tools"]);
                let context = rook.json(&["session", "context", session]);
                assert_eq!(
                    context["last_response"]["receipt"]["phase"],
                    if compatible { "implementation" } else { "implementation_held" }
                );
                for (cut, incomplete, activated) in
                    [(interrupted, true, false), (before, false, false), (after, false, true)]
                {
                    eprintln!("fork cut={cut} incomplete={incomplete} activated={activated}");
                    let fork = rook.ok(&["session", "fork", session, "--at", &cut.to_string()]);
                    let child = fork.split_whitespace().last().unwrap();
                    run(
                        &rook,
                        &["run", "Report historical results without another write.", "--session", child],
                    );
                    let request = model.next();
                    assert_replay(&request, anthropic, incomplete);
                    assert_eq!(request["tools"], source["tools"]);
                    let context = rook.json(&["session", "context", child]);
                    assert_eq!(
                        context["last_response"]["receipt"]["phase"],
                        if !activated {
                            "analysis"
                        } else if compatible {
                            "implementation"
                        } else {
                            "implementation_held"
                        }
                    );
                }
                assert_eq!(
                    std::fs::read_to_string(rook.workspace.path().join("evidence.txt")).unwrap(),
                    "after\n"
                );
                drop(daemon);
                let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
                let id = rook_store::parse_session_id(session).unwrap();
                assert_eq!(
                    store
                        .events(id, 0, 256)
                        .unwrap()
                        .iter()
                        .filter(|e| e.record.kind == rook_store::EventKind::ToolResult
                            && e.record.label == "write_file")
                        .count(),
                    1
                );
            }
        }
    }
}

#[test]
fn native_fallback_keeps_its_actual_origin_after_reopen_and_fork_locally_and_through_daemon() {
    rook_llm::init_tls();
    for shared in [false, true] {
        for anthropic in [false, true] {
            let rook = Rook::new();
            let primary = Native::with_quota(anthropic, true);
            let backup = Native::new(anthropic);
            let api = if anthropic { "anthropic" } else { "responses" };
            let physical = if anthropic { "claude-opus-5" } else { "gpt-6-astra" };
            rook.write_config(&format!("[agent]\nmodel='selected'\ninstall_servers=false\none_script=false\nplan_first=false\n[sandbox]\nmode='ask'\nallow=['evidence.txt']\n[models.selected]\napi='{api}'\nmodel='{physical}'\nurl='{}'\nkey='fixture-key'\ncontext_window=128000\n[models.backup]\napi='{api}'\nmodel='{physical}'\nurl='{}'\nkey='fixture-key'\ncontext_window=128000\npriority=1\n", primary.url, backup.url));
            let mut daemon = shared.then(|| Daemon::start(&rook));
            let first = run(&rook, &["run", "Write evidence once, then answer."]);
            let session = first["session"].as_str().unwrap();
            primary.next();
            let source = backup.next();
            let continuation = backup.next();
            assert_replay(&continuation, anthropic, false);
            assert_eq!(continuation["tools"], source["tools"]);
            drop(daemon.take());
            let (interrupted, end) = {
                let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
                let id = rook_store::parse_session_id(session).unwrap();
                let events = store.events(id, 0, 256).unwrap();
                let interrupted = events
                    .iter()
                    .find(|e| {
                        e.record.kind == rook_store::EventKind::ToolResult && e.record.label == "write_file"
                    })
                    .unwrap()
                    .seq;
                (interrupted, store.get_session(id).unwrap().unwrap().next_seq)
            };
            // A new process has no remembered cooldown; origin comes from the
            // saved provider state, not the old process's missing-endpoint map.
            daemon = shared.then(|| Daemon::start(&rook));
            run(&rook, &["run", "Report existing results without another write.", "--session", session]);
            assert_replay(&backup.next(), anthropic, false);
            for (cut, incomplete) in [(interrupted, true), (end, false)] {
                let fork = rook.ok(&["session", "fork", session, "--at", &cut.to_string()]);
                let child = fork.split_whitespace().last().unwrap();
                run(&rook, &["run", "Report historical results without another write.", "--session", child]);
                let request = backup.next();
                assert_replay(&request, anthropic, incomplete);
                assert_eq!(request["tools"], source["tools"]);
                let context = rook.json(&["session", "context", child]);
                assert_eq!(context["last_response"]["receipt"]["dispatch"]["provider"], "backup");
            }
            assert!(
                primary.requests.try_recv().is_err(),
                "a recovered primary was sent foreign opaque history"
            );
            assert_eq!(
                std::fs::read_to_string(rook.workspace.path().join("evidence.txt")).unwrap(),
                "after\n"
            );
            drop(daemon);
            let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
            let id = rook_store::parse_session_id(session).unwrap();
            assert_eq!(
                store
                    .events(id, 0, 256)
                    .unwrap()
                    .iter()
                    .filter(|e| e.record.kind == rook_store::EventKind::ToolResult
                        && e.record.label == "write_file")
                    .count(),
                1
            );
        }
    }
}
