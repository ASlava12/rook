use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

struct Model {
    url: String,
    release: Arc<AtomicUsize>,
    requests: mpsc::Receiver<Value>,
    stop: Arc<AtomicBool>,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
impl Model {
    fn new() -> Self {
        Self::with_messages(Vec::new())
    }
    fn with_messages(messages: Vec<Value>) -> Self {
        assert!(messages.len() <= 16);
        let messages = Arc::new(messages);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicUsize::new(0));
        let count = Arc::new(AtomicUsize::new(0));
        let (send, requests) = mpsc::sync_channel(16);
        let (halt, gate) = (stop.clone(), release.clone());
        std::thread::spawn(move || {
            let mut connections = 0;
            while !halt.load(Ordering::SeqCst) && connections < 64 {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                connections += 1;
                let (halt, gate, count, send) = (halt.clone(), gate.clone(), count.clone(), send.clone());
                let messages = messages.clone();
                std::thread::spawn(move || {
                    socket.set_read_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 16384];
                    let request = loop {
                        let Ok(n) = socket.read(&mut chunk) else { return };
                        if n == 0 {
                            return;
                        }
                        assert!(bytes.len() + n <= 4 * 1024 * 1024, "mock request limit exceeded");
                        bytes.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&bytes);
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            if !head.contains("chat/completions") {
                                let _=socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":[]}");
                                return;
                            }
                            let length: usize = head
                                .lines()
                                .find_map(|l| {
                                    l.to_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse().ok())
                                })
                                .unwrap_or(0);
                            if body.len() >= length {
                                break serde_json::from_str::<Value>(body).unwrap();
                            }
                        }
                    };
                    let message = if request["stream"] == true {
                        let ordinal = count.fetch_add(1, Ordering::SeqCst) + 1;
                        send.try_send(request.clone()).unwrap();
                        while gate.load(Ordering::SeqCst) < ordinal {
                            if halt.load(Ordering::SeqCst) {
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        messages.get(ordinal - 1).cloned().unwrap_or_else(
                            || json!({"role":"assistant","content":format!("Completed answer {ordinal}")}),
                        )
                    } else {
                        json!({"role":"assistant","content":r#"{"action":"finish"}"#})
                    };
                    let reason = if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
                    let choice = if request["stream"] == true {
                        json!({"index":0,"delta":message,"finish_reason":reason})
                    } else {
                        json!({"index":0,"message":message,"finish_reason":reason})
                    };
                    let answer=json!({"id":"test","model":"test","choices":[choice],"usage":{"prompt_tokens":1,"completion_tokens":1}}).to_string();
                    let (mime, body) = if request["stream"] == true {
                        ("text/event-stream", format!("data: {answer}\n\ndata: [DONE]\n\n"))
                    } else {
                        ("application/json", answer)
                    };
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        Self { url, release, requests, stop }
    }
    fn next(&self) -> Value {
        self.requests
            .recv_timeout(std::time::Duration::from_secs(90))
            .expect("follow-up never requested the model")
    }
}

fn config(endpoint: &str, model: &str, mode: &str, workspace: &std::path::Path) -> String {
    let hooks = if cfg!(unix) {
        let path = workspace.join("hook-count").to_string_lossy().into_owned();
        let command = format!("printf x >> {}; echo PROMPT_HOOK_CONTEXT", shlex::try_quote(&path).unwrap());
        format!(
            "\n[[hooks]]\nevent='prompt'\ncommand={}\n[[hooks]]\nevent='session_start'\ncommand='echo SESSION_HOOK_CONTEXT'\n",
            serde_json::to_string(&command).unwrap()
        )
    } else {
        String::new()
    };
    format!(
        "[agent]\nmodel='{model}'\ninstall_servers=false\none_script=false\n[sandbox]\nmode='{mode}'\n[models.initial]\napi='openai'\nmodel='initial-model'\nurl='{endpoint}'\n[models.followup]\napi='openai'\nmodel='followup-model'\nurl='{endpoint}'\n{hooks}"
    )
}

async fn get(client: &reqwest::Client, url: &str) -> Value {
    client.get(url).send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
}

async fn socket_event(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    kind: &str,
) -> Value {
    for _ in 0..128 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(30), socket.next())
            .await
            .expect("socket response timed out")
            .expect("socket closed")
            .expect("socket read failed");
        let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        if event["type"] == kind {
            return event;
        }
    }
    panic!("socket never sent {kind}")
}
async fn enqueue(client: &reqwest::Client, url: &str, id: &str) -> Value {
    let page = get(client, url).await;
    client.post(url).json(&json!({"action":"follow_up","target":page["follow_up_target"],"id":id,"text":format!("TASK_{id}")})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
}

#[test]
fn socket_corrections_reuse_caller_receipts_and_old_frames_still_work() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let (session, goal_session) = {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let create = |title| {
            let id = rook_store::new_session_id();
            store
                .create_session(&rook_store::SessionMeta::new(
                    id,
                    title,
                    rook.workspace.path().display().to_string(),
                    rook_store::now_unix(),
                ))
                .unwrap();
            rook_store::format_session_id(id)
        };
        (create("socket correction identity"), create("socket goal correction identity"))
    };
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session,"text":"FIRST_TASK"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    model.next(); // The model is blocked; every subsequent prompt is a correction.
    runtime.block_on(async {
        for (id, text) in [
            (Some("caller-one"), "the correction"),
            (Some("caller-one"), "the correction"),
            (Some("caller-one"), "different text"),
            (None, "legacy correction"),
        ] {
            let mut prompt = json!({"type":"prompt","session":session,"text":text});
            if let Some(id) = id {
                prompt["id"] = id.into();
                prompt["target"] = "session".into();
            }
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into()))
                .await
                .unwrap();
        }
        let mut acknowledged = 0;
        let mut rejected = false;
        while acknowledged < 3 || !rejected {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(30), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            match event["type"].as_str() {
                Some("interjected") => acknowledged += 1,
                Some("failed") if event["message"].as_str().unwrap_or("").contains("different text") => {
                    rejected = true;
                }
                _ => {}
            }
        }
        let queue =
            get(&client, &format!("{}/api/sessions/{session}/queue?include_finished=true", daemon.address))
                .await;
        let items = queue["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "retry and rejected conflict must not create another receipt: {queue}");
        assert_eq!(items[0]["receipt"]["id"], "caller-one");
        assert_eq!(items[0]["receipt"]["text"], "the correction");
        assert_eq!(items[1]["receipt"]["text"], "legacy correction");
        assert_ne!(items[1]["receipt"]["id"], "caller-one");
    });
    let mut goal_socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":goal_session,"text":"/goal FIRST_GOAL"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    runtime.block_on(async {
        let work_url = format!("{}/api/work/{goal_session}", daemon.address);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let response = client.get(&work_url).send().await.unwrap();
            if response.status().is_success() {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "goal was not created");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let page = get(&client, &format!("{}/api/sessions/{goal_session}/queue", daemon.address)).await;
        let target = page["submission_target"].as_str().unwrap();
        assert!(target.starts_with("goal."));
        let correction = json!({"type":"prompt","session":goal_session,"text":"goal correction","id":"goal-caller","target":target});
        for _ in 0..2 {
            goal_socket
                .send(tokio_tungstenite::tungstenite::Message::Text(correction.to_string().into()))
                .await
                .unwrap();
        }
        goal_socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({
            "type":"prompt","session":goal_session,"text":"wrong goal","id":"wrong-goal","target":"goal.stale"
        }).to_string().into())).await.unwrap();
        let mut references = Vec::new();
        let mut rejected = false;
        while references.len() < 2 || !rejected {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(30), goal_socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            if event["type"] == "interjected" {
                references.push(event["receipt"]["reference"].as_str().unwrap().to_string());
            } else if event["type"] == "failed" && event["message"].as_str().unwrap_or("").contains("no longer current") {
                rejected = true;
            }
        }
        assert_eq!(references[0], references[1]);
        let work = get(&client, &work_url).await;
        let messages = work["instructions"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "goal retry must retain one receipt: {work}");
        assert_eq!(messages[0]["id"], "goal-caller");
    });
    model.release.store(1, Ordering::SeqCst);
    model.release.store(2, Ordering::SeqCst);
}

#[test]
fn first_socket_prompt_retries_join_then_acknowledge_without_another_session_or_turn() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let address = format!("{}/api/chat", daemon.address.replacen("http", "ws", 1));
    let prompt = json!({"type":"prompt","session":null,"text":"FIRST_SOCKET_TASK","id":"first-stable"});
    let mut first = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    let started = runtime.block_on(socket_event(&mut first, "started"));
    let session = started["session"].as_str().unwrap().to_string();
    assert!(rook_store::parse_session_id(&session).is_some());
    // Keep the original model response in flight across the retry. A failure
    // before that request is a socket error, not a model timeout.
    if model.requests.recv_timeout(std::time::Duration::from_secs(90)).is_err() {
        panic!("turn ended before model request: {}", runtime.block_on(socket_event(&mut first, "failed")));
    }

    let mut retry = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    let attached = runtime.block_on(socket_event(&mut retry, "attached"));
    assert_eq!(attached["session"], session);
    assert_eq!(attached["running"], true);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let queue = runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/queue", daemon.address)));
    assert_eq!(queue["items"].as_array().unwrap().len(), 0, "a retry is not a correction: {queue}");

    runtime.block_on(async {
        retry
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type":"prompt","session":null,"text":"DIFFERENT_TASK","id":"first-stable"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    });
    let conflict = runtime.block_on(socket_event(&mut retry, "failed"));
    assert!(conflict["message"].as_str().unwrap().contains("different prompt"), "{conflict}");
    model.release.store(1, Ordering::SeqCst);
    let done = runtime.block_on(socket_event(&mut first, "done"));
    assert_ne!(done["stopped"], "already_admitted");
    drop(first);
    drop(retry);

    let mut after = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    assert_eq!(runtime.block_on(socket_event(&mut after, "started"))["session"], session);
    assert_eq!(runtime.block_on(socket_event(&mut after, "done"))["stopped"], "already_admitted");

    let named = json!({"type":"prompt","session":session,"text":"NAMED_SOCKET_TASK","id":"named-stable"});
    runtime.block_on(async {
        after.send(tokio_tungstenite::tungstenite::Message::Text(named.to_string().into())).await.unwrap();
    });
    assert_eq!(runtime.block_on(socket_event(&mut after, "started"))["session"], session);
    model.requests.recv_timeout(std::time::Duration::from_secs(90)).unwrap();
    let mut named_retry = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(named.to_string().into())).await.unwrap();
        socket
    });
    assert_eq!(runtime.block_on(socket_event(&mut named_retry, "attached"))["running"], true);
    model.release.store(2, Ordering::SeqCst);
    runtime.block_on(socket_event(&mut after, "done"));
    runtime.block_on(async {
        after.send(tokio_tungstenite::tungstenite::Message::Text(named.to_string().into())).await.unwrap();
    });
    assert_eq!(runtime.block_on(socket_event(&mut after, "started"))["session"], session);
    assert_eq!(runtime.block_on(socket_event(&mut after, "done"))["stopped"], "already_admitted");
    drop(named_retry);
    drop(after);
    drop(daemon);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let id = rook_store::parse_session_id(&session).unwrap();
    assert_eq!(
        store
            .events(id, 0, 100)
            .unwrap()
            .iter()
            .filter(|event| event.record.kind == rook_store::EventKind::UserMessage)
            .count(),
        2,
    );
}

#[test]
fn first_socket_goal_retry_keeps_one_generation_and_no_extra_goal_event() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let address = format!("{}/api/chat", daemon.address.replacen("http", "ws", 1));
    let prompt = json!({"type":"prompt","session":null,"text":"/goal GOAL_RETRY_TASK","id":"goal-stable"});
    let mut first = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    assert!(model.next()["messages"].to_string().contains("GOAL_RETRY_TASK"));
    let started = runtime.block_on(socket_event(&mut first, "started"));
    let session = started["session"].as_str().unwrap().to_string();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let work_url = format!("{}/api/work/{session}", daemon.address);
    let first_run = runtime.block_on(get(&client, &work_url));
    let mut retry = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    assert_eq!(runtime.block_on(socket_event(&mut retry, "attached"))["session"], session);
    runtime.block_on(async {
        retry
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type":"prompt","session":null,"text":"/goal DIFFERENT_TASK","id":"goal-stable"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    });
    let conflict = runtime.block_on(socket_event(&mut retry, "failed"));
    assert!(conflict["message"].as_str().unwrap().contains("different prompt"), "{conflict}");
    runtime.block_on(async {
        client
            .post(format!("{work_url}/control"))
            .json(&"cancel")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    drop(first);
    drop(retry);
    let mut after = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    assert_eq!(runtime.block_on(socket_event(&mut after, "started"))["session"], session);
    assert_eq!(runtime.block_on(socket_event(&mut after, "done"))["stopped"], "already_admitted");
    assert_eq!(runtime.block_on(get(&client, &work_url))["generation"], first_run["generation"]);
    drop(after);
    drop(daemon);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let id = rook_store::parse_session_id(&session).unwrap();
    assert_eq!(store.list_sessions().unwrap().iter().filter(|entry| entry.id == id).count(), 1);
    // Admission records the requested goal; the work iteration records its
    // effective goal once more. A retry must not add a third note.
    assert_eq!(
        store.events(id, 0, 100).unwrap().iter().filter(|event| event.record.label == "goal").count(),
        2
    );
}

#[test]
fn killed_followups_resume_once_with_saved_settings_and_cancelled_ones_stay_stopped() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let session = {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let id = rook_store::new_session_id();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "follow-up recovery",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        rook_store::format_session_id(id)
    };
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client =
        reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(90)).build().unwrap();
    let mut socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"setting","name":"stance","value":"readonly"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session,"text":"FIRST_TASK"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    let first = model.next();
    assert_eq!(first["model"], "initial-model");
    let queue = format!("{}/api/sessions/{session}/queue", daemon.address);
    runtime.block_on(async {
        enqueue(&client, &queue, "one").await;
        enqueue(&client, &queue, "two").await;
        for (name, value) in [("model", "followup"), ("effort", "low")] {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"type":"setting","name":name,"value":value}).to_string().into(),
                ))
                .await
                .unwrap();
        }
        // Observe the acknowledgement before releasing the preceding turn.
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(90), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Ok(text) = frame.to_text() {
                let event: Value = serde_json::from_str(text).unwrap();
                if event["type"] == "settings" && event["model"] == "followup" && event["effort"] == "low" {
                    break;
                }
            }
        }
    });
    model.release.store(1, Ordering::SeqCst);
    let second = model.next();
    assert_eq!(second["model"], "followup-model");
    let before =
        runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)));
    assert!(before[0]["prompt"].is_object());
    let receipt = runtime.block_on(get(&client, &format!("{queue}/session.one")));
    assert!(receipt["receipt"]["applied_at"].is_number());
    if cfg!(unix) {
        assert_eq!(std::fs::read(rook.workspace.path().join("hook-count")).unwrap(), b"xx");
    }
    drop(daemon);
    drop(socket);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    // Defaults change during downtime; the session's saved readonly mode and
    // explicitly selected model must remain authoritative for recovery.
    rook.write_config(&config(&model.url, "initial", "auto", rook.workspace.path()));
    let daemon = Daemon::start(&rook);
    let resumed = model.next();
    assert_eq!(resumed["model"], "followup-model");
    let messages = resumed["messages"].to_string();
    assert!(messages.contains("Nothing you do may change this machine"));
    if cfg!(unix) {
        assert!(messages.contains("SESSION_HOOK_CONTEXT"));
        assert!(messages.contains("PROMPT_HOOK_CONTEXT"));
        assert_eq!(std::fs::read(rook.workspace.path().join("hook-count")).unwrap(), b"xx");
    }
    let after =
        runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)));
    assert_eq!(after[0]["turn"], before[0]["turn"]);
    assert_eq!(after[0]["prompt"], before[0]["prompt"]);
    let queue = format!("{}/api/sessions/{session}/queue", daemon.address);
    assert_eq!(
        runtime.block_on(get(&client, &format!("{queue}/session.one")))["receipt"]["applied_at"],
        receipt["receipt"]["applied_at"]
    );
    model.release.store(3, Ordering::SeqCst);
    let fourth = model.next();
    assert_eq!(fourth["model"], "followup-model");
    assert!(fourth["messages"].to_string().contains("TASK_two"));
    model.release.store(4, Ordering::SeqCst);
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let state = get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)).await;
            if state[0]["status"] == "end_turn" {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "{state}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        enqueue(&client, &queue, "idle").await;
    });
    let fifth = model.next();
    assert!(fifth["messages"].to_string().contains("TASK_idle"));
    let cancelled_turn = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"attach","session":session}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"cancel"}).to_string().into()))
            .await
            .unwrap();
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(90), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Ok(text) = frame.to_text() {
                let event: Value = serde_json::from_str(text).unwrap();
                if event["type"] == "cancelled" {
                    break;
                }
            }
        }
        get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)).await[0]["turn"].clone()
    });
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let id = rook_store::parse_session_id(&session).unwrap();
        let saved: Value =
            serde_json::from_slice(&store.kv_get(&format!("followup-driver/{id:032x}")).unwrap().unwrap())
                .unwrap();
        assert_eq!(saved["paused"], true);
        assert_eq!(saved["model"], "followup");
        assert_eq!(saved["stance"], "readonly");
        assert_eq!(saved["effort"], "low");
        let prompts = store
            .events(id, 0, 1000)
            .unwrap()
            .into_iter()
            .filter(|e| e.record.kind == rook_store::EventKind::UserMessage)
            .map(|e| String::from_utf8(store.get(&e.record.body).unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(prompts, ["FIRST_TASK", "TASK_one", "TASK_two", "TASK_idle"]);
    }
    let daemon = Daemon::start(&rook);
    assert!(
        model.requests.recv_timeout(std::time::Duration::from_secs(3)).is_err(),
        "cancelled follow-up was restarted"
    );
    assert_eq!(
        runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)))[0]["turn"],
        cancelled_turn
    );
}

#[test]
fn continuing_a_killed_predecessor_releases_its_followups_only_after_completion() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let session = {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let id = rook_store::new_session_id();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "continuation",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        rook_store::format_session_id(id)
    };
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client =
        reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(90)).build().unwrap();
    let socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session,"text":"ORIGINAL_TASK"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    model.next();
    let queue = format!("{}/api/sessions/{session}/queue", daemon.address);
    runtime.block_on(enqueue(&client, &queue, "one"));
    let before =
        runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)));
    let original = before[0]["turn"].as_str().unwrap().to_owned();
    drop(socket);
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    let daemon = Daemon::start(&rook);
    assert!(
        model.requests.recv_timeout(std::time::Duration::from_secs(3)).is_err(),
        "an unfinished ordinary predecessor is not an automatic follow-up"
    );
    let mut socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session,"text":"/continue"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    let resumed = model.next();
    let messages = resumed["messages"].to_string();
    assert!(messages.contains(rook_core::agent::CARRY_ON));
    assert!(!messages.contains("TASK_one"));
    let state =
        runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/recovery", daemon.address)));
    assert_ne!(state[0]["turn"], original);
    assert_eq!(state[0]["continuation"], original);
    let queue = format!("{}/api/sessions/{session}/queue", daemon.address);
    let page = runtime.block_on(get(&client, &queue));
    assert_eq!(page["follow_up_target"], format!("turn.{original}"));
    runtime.block_on(enqueue(&client, &queue, "late"));
    model.release.store(2, Ordering::SeqCst);
    let first = model.next()["messages"].to_string();
    assert!(first.contains("TASK_one"));
    assert!(!first.contains("TASK_late"));
    model.release.store(3, Ordering::SeqCst);
    assert!(model.next()["messages"].to_string().contains("TASK_late"));
    model.release.store(4, Ordering::SeqCst);
    let boundaries = runtime.block_on(async {
        let mut boundaries = Vec::new();
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(90), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Ok(text) = frame.to_text() {
                let event: Value = serde_json::from_str(text).unwrap();
                if event["type"] == "follow_up" {
                    assert!(boundaries.len() < 2);
                    boundaries.push(event["id"].as_str().unwrap().to_owned());
                }
                if event["type"] == "done" {
                    assert_eq!(event["stopped"], "end_turn");
                    break;
                }
            }
        }
        boundaries
    });
    assert_eq!(boundaries, ["one", "late"]);
    let report = runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/turns", daemon.address)));
    assert_eq!(report["totals"]["turns"], 3);
    assert_eq!(report["totals"]["completed"], 3);
    assert_eq!(report["items"][0]["summary"]["follow_up"], "late");
    assert_eq!(report["items"][1]["summary"]["follow_up"], "one");
    assert_eq!(report["items"][2]["summary"]["continuation"], original);
    let remote = rook.json(&["session", "turns", &session]);
    assert_eq!(remote, report);
    for name in ["one", "late"] {
        assert!(
            runtime.block_on(get(&client, &format!("{queue}/session.{name}")))["receipt"]["applied_at"]
                .is_number()
        );
    }
    drop(socket);
    drop(daemon);
    assert_eq!(rook.json(&["session", "turns", &session]), report);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let id = rook_store::parse_session_id(&session).unwrap();
    let prompts = store
        .events(id, 0, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e.record.kind == rook_store::EventKind::UserMessage)
        .map(|e| String::from_utf8(store.get(&e.record.body).unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["ORIGINAL_TASK", rook_core::agent::CARRY_ON, "TASK_one", "TASK_late"]);
}

#[test]
fn a_followup_promoted_to_a_goal_keeps_its_observer_and_uses_the_new_goals_options() {
    rook_llm::init_tls();
    let rook = Rook::new();
    std::fs::write(rook.workspace.path().join("evidence.txt"), "fixture evidence").unwrap();
    let reply = |text: &str| json!({"role":"assistant","content":text});
    let read = json!({"role":"assistant","content":"", "tool_calls":[{"index":0,"id":"evidence","type":"function","function":{"name":"read_file","arguments":r#"{"path":"evidence.txt"}"#}}]});
    let model = Model::with_messages(vec![
        reply("first goal finished"),
        read.clone(),
        reply("Read evidence.txt.\nVERDICT: holds"),
        reply("follow-up finished"),
        reply("promotion acknowledged"),
        reply("new goal finished"),
        read,
        reply("Read evidence.txt.\nVERDICT: holds"),
    ]);
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let session = {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let id = rook_store::new_session_id();
        store
            .create_session(&rook_store::SessionMeta::new(
                id,
                "goal handoff",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        rook_store::format_session_id(id)
    };
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client =
        reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(90)).build().unwrap();
    let mut socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session,"text":"/goal FIRST_GOAL"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    assert!(model.next()["messages"].to_string().contains("FIRST_GOAL"));
    let queue = format!("{}/api/sessions/{session}/queue", daemon.address);
    let work = format!("{}/api/work/{session}", daemon.address);
    let first_goal = runtime.block_on(get(&client, &work));
    let queued = runtime.block_on(enqueue(&client, &queue, "one"));
    runtime.block_on(enqueue(&client, &queue, "stale"));
    assert_eq!(
        queued["receipt"]["follow_up"]["after"],
        format!("goal.{}", first_goal["generation"].as_str().unwrap())
    );
    model.release.store(1, Ordering::SeqCst);
    assert!(model.next()["messages"].to_string().contains("The claim:"));
    assert!(
        runtime.block_on(get(&client, &format!("{queue}/session.one")))["receipt"]["applied_at"].is_null()
    );
    model.release.store(2, Ordering::SeqCst);
    assert!(model.next()["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool"));
    model.release.store(3, Ordering::SeqCst);
    assert!(model.next()["messages"].to_string().contains("TASK_one"));
    assert_eq!(runtime.block_on(get(&client, &work))["status"], "completed");
    runtime.block_on(async {
        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({
            "type":"prompt","session":session,"text":"/goal SECOND_GOAL",
            "options":{"attachments":[{"type":"text","name":"new-goal-note","text":"NEW_GOAL_ATTACHMENT"}]}
        }).to_string().into())).await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let current = get(&client, &work).await;
            if current["generation"] != first_goal["generation"] {
                assert_eq!(current["goal"], "SECOND_GOAL");
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "new goal was not admitted");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    model.release.store(4, Ordering::SeqCst);
    // Promotion steers the already-running turn at its next safe boundary.
    // Attachments belong to the new goal's first supervised stage.
    let promoted = model.next()["messages"].to_string();
    assert!(promoted.contains("[work instruction"), "{promoted}");
    assert!(promoted.contains("/goal SECOND_GOAL"), "{promoted}");
    model.release.store(5, Ordering::SeqCst);
    let new_work = model.next()["messages"].to_string();
    assert!(new_work.contains("SECOND_GOAL"), "{new_work}");
    assert!(
        new_work.contains("NEW_GOAL_ATTACHMENT"),
        "the new generation must use its own options: {new_work}"
    );
    model.release.store(6, Ordering::SeqCst);
    assert!(model.next()["messages"].to_string().contains("The claim:"));
    model.release.store(7, Ordering::SeqCst);
    let checked = model.next();
    assert!(
        checked["messages"].as_array().unwrap().iter().any(|m| m["role"] == "tool"),
        "the new goal's checker must receive its tool result: {checked}"
    );
    model.release.store(8, Ordering::SeqCst);
    runtime.block_on(async {
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(90), socket.next()).await.unwrap().unwrap().unwrap();
            if let Ok(text) = frame.to_text() {
                let event: Value = serde_json::from_str(text).unwrap();
                if event["type"] == "done" {
                    assert_eq!(event["reply"], "new goal finished", "the observer must not receive a terminal ending at the old goal or follow-up boundary: {event}");
                    assert_eq!(event["stopped"], "end_turn");
                    break;
                }
            }
        }
    });
    let final_goal = runtime.block_on(get(&client, &work));
    assert_ne!(final_goal["generation"], first_goal["generation"]);
    assert_eq!(final_goal["status"], "completed");
    assert_eq!(final_goal["iterations"], 1);
    let stale = runtime.block_on(get(&client, &format!("{queue}/session.stale")));
    assert!(stale["receipt"]["applied_at"].is_null());
    assert!(stale["receipt"]["follow_up"]["reserved"].is_null());
    assert_eq!(stale["receipt"]["follow_up"]["after"], queued["receipt"]["follow_up"]["after"]);
}
