//! Owned native CLI/HTTP/daemon probes. No user installation or provider.
use super::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct Server {
    endpoint: String,
    seen: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
#[derive(Clone, Copy)]
enum Behavior {
    Complete,
    RetryOnce,
    Quota,
    Truncated,
    Repetition,
    MaxTokens,
    Block,
}
impl Server {
    fn new(retry_first: bool) -> Self {
        Self::with_behavior(if retry_first { Behavior::RetryOnce } else { Behavior::Complete })
    }
    fn with_behavior(behavior: Behavior) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let output = seen.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut retry = matches!(behavior, Behavior::RetryOnce);
            while !stopping.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => panic!("owned server accept: {e}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let (head, request) = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() + n < 1024 * 1024, "bounded fixture HTTP request");
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(boundary) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..boundary]).into_owned();
                        let length: usize = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= boundary + 4 + length {
                            break (
                                head,
                                if length == 0 {
                                    Value::Null
                                } else {
                                    serde_json::from_slice(&bytes[boundary + 4..boundary + 4 + length])
                                        .unwrap()
                                },
                            );
                        }
                    }
                };
                let (status, mime, body) = if head.starts_with("GET ") {
                    ("200 OK", "application/json", json!({"data":[]}).to_string())
                } else {
                    let classifier =
                        request.to_string().contains("Classify whether an assistant's proposed last reply");
                    let streamed = request["stream"] == true;
                    output.lock().unwrap().push(request);
                    if matches!(behavior, Behavior::Quota) {
                        let body =
                            json!({"error":{"code":"insufficient_quota","message":"insufficient quota"}})
                                .to_string();
                        socket.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
                        continue;
                    }
                    if head.starts_with("POST /v1/chat/completions ") {
                        let (status, mime, body) = if retry {
                            retry = false;
                            (
                                "429 Too Many Requests",
                                "application/json",
                                json!({"error":{"message":"owned retry"}}).to_string(),
                            )
                        } else {
                            let text = if matches!(behavior, Behavior::Repetition) {
                                "OWNED_REPEAT no new observation or progress.\n".repeat(1500)
                            } else if classifier {
                                r#"{"action":"finish"}"#.into()
                            } else {
                                "Owned HTTP inspection is complete.".into()
                            };
                            let finish = if matches!(behavior, Behavior::MaxTokens) {
                                "length"
                            } else if matches!(behavior, Behavior::Block) {
                                "tool_calls"
                            } else {
                                "stop"
                            };
                            let mut message = json!({"role":"assistant","content":text});
                            if matches!(behavior, Behavior::Block) {
                                message = json!({"role":"assistant","content":"","tool_calls":[{"id":"owned-block-call", "type":"function",
                                    "function":{"name":"block_work","arguments":json!({"reason":"Owned prerequisite requires explicit resume."}).to_string()}}]});
                            }
                            if streamed {
                                let mut delta = message;
                                if let Some(calls) = delta["tool_calls"].as_array_mut() {
                                    calls[0]["index"] = json!(0);
                                }
                                let first = json!({"id":"owned-chat","model":"fixture-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                                let mut body = format!("data: {first}\n\n");
                                if !matches!(behavior, Behavior::Truncated | Behavior::Repetition) {
                                    let last = json!({"id":"owned-chat","model":"fixture-model","choices":[{"index":0,"delta":{},"finish_reason":finish}],
                                        "usage":{"prompt_tokens":10,"completion_tokens":8}});
                                    body.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
                                }
                                ("200 OK", "text/event-stream", body)
                            } else {
                                ("200 OK","application/json",json!({"model":"fixture-model","choices":[{"index":0,"message":message,"finish_reason":finish}],
                                    "usage":{"prompt_tokens":10,"completion_tokens":8}}).to_string())
                            }
                        };
                        let _ = socket.write_all(format!("HTTP/1.1 {status}\r\nRetry-After: 1\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes());
                        continue;
                    }
                    if streamed && matches!(behavior, Behavior::Truncated | Behavior::Repetition) {
                        let text = if matches!(behavior, Behavior::Repetition) {
                            "OWNED_REPEAT no new observation or progress.\n".repeat(1500)
                        } else {
                            "A partial visible prefix.".into()
                        };
                        let event = json!({"type":"response.output_text.delta","delta":text});
                        let body = format!("data: {event}\n\n");
                        let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                        continue;
                    }
                    let event = if retry {
                        retry = false;
                        json!({"type":"response.failed","response":{"status":"failed",
                            "error":{"code":"slow_down","headers":{"Retry-After":"1"}}}})
                    } else {
                        if matches!(behavior, Behavior::Block) {
                            let event = json!({"type":"response.completed","response":{"id":"owned-block", "status":"completed","model":"fixture-model",
                                "output":[{"type":"function_call","call_id":"owned-block-call","name":"block_work",
                                    "arguments":json!({"reason":"Owned prerequisite requires explicit resume."}).to_string()}],
                                "usage":{"input_tokens":10,"output_tokens":8}}});
                            let body = format!("data: {event}\n\n");
                            let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                            continue;
                        }
                        let text = if classifier {
                            r#"{"action":"finish"}"#
                        } else {
                            "Owned HTTP inspection is complete."
                        };
                        let limited = matches!(behavior, Behavior::MaxTokens) && !classifier;
                        json!({"type":if limited {"response.incomplete"} else {"response.completed"},"response":{"id":"owned-call", "status":if limited {"incomplete"} else {"completed"},"model":"fixture-model",
                            "incomplete_details":if limited {json!({"reason":"max_output_tokens"})} else {Value::Null},
                            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
                            "usage":{"input_tokens":10,"output_tokens":8}}})
                    };
                    if streamed {
                        ("200 OK", "text/event-stream", format!("data: {event}\n\n"))
                    } else {
                        ("200 OK", "application/json", event["response"].to_string())
                    }
                };
                let _ = socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        Self { endpoint, seen, stop, worker: Some(worker) }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn session(rook: &Rook) -> String {
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let id = rook_store::new_session_id();
    store
        .create_session(&rook_store::SessionMeta::new(
            id,
            "owned recording",
            rook.workspace.path().display().to_string(),
            1,
        ))
        .unwrap();
    rook_store::format_session_id(id)
}
fn config(rook: &Rook, server: &Server) {
    rook.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\n[models.local]\nmodel='fixture-model'\napi='openai'\nurl='{}'\ncontext_window=128000\n", server.endpoint));
}
fn export(rook: &Rook, session: &str, filename: &str, format: &str) -> Value {
    let path = rook.home.path().join(filename);
    let out = rook.run(&[
        "session",
        "model-calls",
        session,
        "--format",
        format,
        "--output",
        path.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    let bytes = std::fs::read(&path).unwrap();
    let second = rook.run(&[
        "session",
        "model-calls",
        session,
        "--format",
        format,
        "--output",
        path.to_str().unwrap(),
    ]);
    assert!(!second.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn model_call_native_flag_retry_export_and_daemon_reader_preserve_exact_pairs() {
    let one = one_at_a_time();
    let rook = Rook::new();
    let server = Server::new(true);
    config(&rook, &server);
    let saved = session(&rook);
    let out = rook.run(&[
        "run",
        "Inspect the owned example and finish.",
        "--session",
        &saved,
        "--record-model-calls",
    ]);
    assert!(out.status.success(), "{out:?}");
    let local = rook.json(&["session", "model-calls", &saved]);
    let requests = server.seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 3, "one failed physical retry, main reply, completion check");
    assert_eq!(local["calls"].as_array().unwrap().len(), 3);
    assert_eq!(local["eligible"], 2, "{local}");
    assert_eq!(local["calls"][0]["state"], "failed");
    assert_eq!(local["calls"][1]["purpose"], "main");
    assert_eq!(local["calls"][2]["purpose"], "completion_check");
    let records = export(&rook, &saved, "records-local.json", "records");
    assert_eq!(records["calls"][0]["request"], records["calls"][1]["request"]);
    let pairs = export(&rook, &saved, "pairs-local.json", "dataset");
    assert_eq!(pairs["examples"].as_array().unwrap().len(), 2);
    assert_eq!(pairs["examples"][0]["response"]["message"]["content"], "Owned HTTP inspection is complete.");
    assert_eq!(pairs["examples"][0]["request"], records["calls"][1]["request"]);
    assert_eq!(pairs["examples"][0]["dispatch"]["model"], "fixture-model");
    assert_eq!(pairs["examples"][0]["target"], "response_only");
    assert!(!local.to_string().contains("Inspect the owned example"));
    drop(one);
    let daemon = Daemon::start(&rook);
    assert_eq!(rook.json(&["session", "model-calls", &saved]), local);
    assert_eq!(export(&rook, &saved, "records-remote.json", "records"), records);
    assert_eq!(export(&rook, &saved, "pairs-remote.json", "dataset"), pairs);
    drop(daemon);
    assert_eq!(server.seen.lock().unwrap().len(), 3, "reader/export adds no HTTP generation");
}

#[test]
fn model_call_native_daemon_turn_flag_and_default_off_are_distinct() {
    let one = one_at_a_time();
    let rook = Rook::new();
    let server = Server::new(false);
    config(&rook, &server);
    let off = session(&rook);
    let enabled = session(&rook);
    drop(one);
    let daemon = Daemon::start(&rook);
    for (id, enabled) in [(&off, false), (&enabled, true)] {
        let mut args = vec!["run", "Inspect the owned example and finish.", "--session", id];
        if enabled {
            args.push("--record-model-calls=true");
        }
        let out = rook.run(&args);
        assert!(out.status.success(), "{out:?}");
        let report = rook.json(&["session", "model-calls", id]);
        assert_eq!(report["eligible"], if enabled { 2 } else { 0 }, "{report}");
        if !enabled {
            assert!(
                report["calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| c["input"].is_null() && c["output"].is_null()),
                "{report}"
            );
            let path = rook.home.path().join("empty-dataset.json");
            assert!(
                !rook
                    .run(&[
                        "session",
                        "model-calls",
                        id,
                        "--format",
                        "dataset",
                        "--output",
                        path.to_str().unwrap()
                    ])
                    .status
                    .success()
            );
            assert!(!path.exists());
        }
    }
    assert_eq!(server.seen.lock().unwrap().len(), 4, "recording adds no HTTP generation");
    drop(daemon);
}

#[test]
fn model_call_native_failover_keeps_distinct_physical_model_identities() {
    let one = one_at_a_time();
    let rook = Rook::new();
    let unavailable = Server::with_behavior(Behavior::Quota);
    let backup = Server::new(false);
    rook.write_config(&format!("[agent]\nmodel='primary'\ninstall_servers=false\none_script=false\n[agent.fallbacks]\nmain=['backup']\n[models.primary]\nmodel='unavailable-model'\napi='responses'\nurl='{}'\ncontext_window=128000\n[models.backup]\nmodel='fixture-model'\napi='responses'\nurl='{}'\ncontext_window=128000\n",unavailable.endpoint,backup.endpoint));
    let saved = session(&rook);
    let out = rook.run(&[
        "run",
        "Inspect the owned example and finish.",
        "--session",
        &saved,
        "--record-model-calls",
    ]);
    assert!(out.status.success(), "{out:?}");
    let report = export(&rook, &saved, "failover.json", "records");
    let calls = report["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0]["state"], "failed");
    assert_eq!(calls[0]["dispatch"]["model"], "unavailable-model");
    assert_eq!(calls[1]["dispatch"]["model"], "fixture-model");
    assert_ne!(calls[0]["attempt"], calls[1]["attempt"]);
    assert_eq!(unavailable.seen.lock().unwrap().len(), 1);
    assert_eq!(backup.seen.lock().unwrap().len(), 2);
    drop(one);
    let daemon = Daemon::start(&rook);
    assert_eq!(export(&rook, &saved, "failover-remote.json", "records"), report);
    drop(daemon);
}

#[test]
fn model_call_native_truncation_and_repetition_cancellation_never_export_complete_pairs() {
    for behavior in [Behavior::Truncated, Behavior::Repetition] {
        let _one = one_at_a_time();
        let rook = Rook::new();
        let server = Server::with_behavior(behavior);
        config(&rook, &server);
        let saved = session(&rook);
        let out =
            rook.run(&["run", "Inspect the owned example.", "--session", &saved, "--record-model-calls"]);
        assert!(!out.status.success(), "{out:?}");
        let report = export(&rook, &saved, "partial.json", "records");
        assert_eq!(report["eligible"], 0);
        let calls = report["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["completion_confirmed"], false);
        if matches!(behavior, Behavior::Repetition) {
            assert_eq!(calls[0]["state"], "interrupted");
        } else {
            assert!(matches!(calls[0]["state"].as_str(), Some("failed" | "incomplete")), "{report}");
        }
        assert_eq!(server.seen.lock().unwrap().len(), 1, "partial content is never retried as success");
        let path = rook.home.path().join("ineligible.json");
        assert!(
            !rook
                .run(&[
                    "session",
                    "model-calls",
                    &saved,
                    "--format",
                    "dataset",
                    "--output",
                    path.to_str().unwrap()
                ])
                .status
                .success()
        );
        assert!(!path.exists());
    }
}

#[test]
fn model_call_native_max_token_reply_has_explicit_dataset_exclusion() {
    let _one = one_at_a_time();
    let rook = Rook::new();
    let server = Server::with_behavior(Behavior::MaxTokens);
    config(&rook, &server);
    let saved = session(&rook);
    let _out = rook.run(&["run", "Inspect the owned example.", "--session", &saved, "--record-model-calls"]);
    let report = rook.json(&["session", "model-calls", &saved]);
    assert!(report["exclusions"]["max_token_reply"].as_u64().is_some_and(|n| n > 0), "{report}");
    assert!(
        report["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["purpose"] == "main")
            .all(|c| c["eligible"] == false)
    );
}

#[test]
fn model_call_native_responses_replay_state_is_an_explicit_context_omission() {
    let _one = one_at_a_time();
    let rook = Rook::new();
    let server = Server::new(false);
    config(&rook, &server);
    let text = std::fs::read_to_string(rook.home.path().join("config.toml"))
        .unwrap()
        .replace("api='openai'", "api='responses'");
    rook.write_config(&text);
    let saved = session(&rook);
    let out = rook.run(&[
        "run",
        "Inspect the owned example and finish.",
        "--session",
        &saved,
        "--record-model-calls",
    ]);
    assert!(out.status.success(), "{out:?}");
    let records = export(&rook, &saved, "responses-opaque.json", "records");
    assert_eq!(records["eligible"], 0);
    assert_eq!(records["exclusions"]["output_context_omitted"], 2);
    assert_eq!(records["calls"][0]["output"]["omissions"]["provider_state"], 1);
    assert_eq!(server.seen.lock().unwrap().len(), 2);
}

#[test]
fn model_call_native_saved_turn_choice_survives_daemon_restart_and_explicit_resume() {
    rook_llm::init_tls();
    for enabled in [false, true] {
        let one = one_at_a_time();
        let rook = Rook::new();
        let server = Server::with_behavior(Behavior::Block);
        config(&rook, &server);
        let saved = session(&rook);
        // Opposite configuration proves that the saved explicit choice wins.
        let mut text = std::fs::read_to_string(rook.home.path().join("config.toml")).unwrap();
        text.push_str(&format!("\n[telemetry.model_calls]\nenabled={}\n", !enabled));
        rook.write_config(&text);
        drop(one);
        let daemon = Daemon::start(&rook);
        let run: Value = tokio::runtime::Runtime::new().unwrap().block_on(async {
            reqwest::Client::new().post(format!("{}/api/work",daemon.address)).json(&json!({
                "goal":"Inspect the owned prerequisite and block until explicit resume.","autonomous":true,
                "conversation":{"session":saved,"model":null,"effort":"low","stance":"autonomous", "options":{"record_model_calls":enabled}},
                "max_iterations":0,"max_tokens":0,"max_seconds":0
            })).send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
        });
        let id = run["id"].as_str().unwrap();
        let wait = |iterations: u64| {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let run = rook.json(&["task", "show", id]);
                if run["status"] == "blocked" && run["iterations"] == iterations {
                    return run;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "managed work did not reach owned blocker: {run}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let first = wait(1);
        assert_eq!(first["conversation"]["options"]["record_model_calls"], enabled);
        let before = rook.json(&["session", "model-calls", &saved]);
        assert_eq!(before["eligible"], if enabled { 1 } else { 0 });
        drop(daemon);
        let daemon = Daemon::start(&rook);
        let parked = rook.json(&["task", "show", id]);
        assert_eq!(parked["status"], "blocked");
        assert_eq!(server.seen.lock().unwrap().len(), 1, "restart cannot run a blocked task");
        let resumed = rook.run(&["task", "resume", id]);
        assert!(resumed.status.success(), "{resumed:?}");
        let second = wait(2);
        assert_eq!(second["conversation"]["options"]["record_model_calls"], enabled);
        let after = rook.json(&["session", "model-calls", &saved]);
        assert_eq!(after["eligible"], if enabled { 2 } else { 0 });
        assert_eq!(server.seen.lock().unwrap().len(), 2, "only explicit resume performs the second call");
        drop(daemon);
    }
}

#[test]
fn model_call_native_compaction_adds_its_own_purpose_without_rewriting_earlier_snapshots() {
    let _one = one_at_a_time();
    let rook = Rook::new();
    let server = Server::new(false);
    config(&rook, &server);
    let mut text = std::fs::read_to_string(rook.home.path().join("config.toml")).unwrap();
    text.push_str("\n[models.local.compaction]\ncompact_at=0.1\nkeep_recent_tokens=256\n");
    rook.write_config(&text);
    let saved = session(&rook);
    let out = rook.run(&[
        "run",
        "Inspect the owned example and finish.",
        "--session",
        &saved,
        "--record-model-calls",
    ]);
    assert!(out.status.success(), "{out:?}");
    let before = export(&rook, &saved, "before-compaction.json", "records");
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        for index in 0..24 {
            let kind = if index % 2 == 0 {
                rook_store::EventKind::UserMessage
            } else {
                rook_store::EventKind::AssistantMessage
            };
            store
                .append_event(
                    rook_store::parse_session_id(&saved).unwrap(),
                    rook_store::NewEvent::new(
                        kind,
                        rook_store::Kind::Message,
                        format!("owned history {index}: {}", "retained fact. ".repeat(350)).as_bytes(),
                    ),
                )
                .unwrap();
        }
    }
    let out = rook.run(&[
        "run",
        "Inspect the next owned example and finish.",
        "--session",
        &saved,
        "--record-model-calls",
    ]);
    assert!(out.status.success(), "{out:?}");
    let after = export(&rook, &saved, "after-compaction.json", "records");
    assert!(after["calls"].as_array().unwrap().iter().any(|c| c["purpose"] == "compaction"), "{after}");
    for old in before["calls"].as_array().unwrap() {
        let same =
            after["calls"].as_array().unwrap().iter().find(|c| c["attempt"] == old["attempt"]).unwrap();
        assert_eq!(same, old);
    }
    let prefix = rook.json(&["session", "model-calls", &saved, "--through", &before["through"].to_string()]);
    assert_eq!(prefix["calls"].as_array().unwrap().len(), before["calls"].as_array().unwrap().len());
    assert!(!prefix["calls"].as_array().unwrap().iter().any(|c| c["purpose"] == "compaction"));
}
