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

#[test]
fn phase_routing_changes_the_physical_model_after_a_write_locally_and_through_the_daemon() {
    rook_llm::init_tls();
    for shared in [false, true] {
        let rook = Rook::new();
        std::fs::write(rook.workspace.path().join("evidence.txt"), "before\n").unwrap();
        let tool = |id: &str, name: &str, args: Value| json!({"role":"assistant","content":"","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]});
        let model = Model::with_messages(vec![
            tool("read-before", "read_file", json!({"path":"evidence.txt"})),
            tool(
                "failed-edit",
                "edit_file",
                json!({"path":"evidence.txt","edits":[{"old":"absent text","new":"not written"}]}),
            ),
            tool("write-once", "write_file", json!({"path":"evidence.txt","content":"after\n"})),
            json!({"role":"assistant","content":"IMPLEMENTATION_REPLY"}),
            json!({"role":"assistant","content":"RESUMED_IMPLEMENTATION_REPLY"}),
        ]);
        model.release.store(16, Ordering::SeqCst);
        let settings = config(&model.url, "initial", "ask", rook.workspace.path())
            .replace("mode='ask'", "mode='ask'\nallow=['evidence.txt']")
            .replace(
                "[models.initial]",
                "[models.initial]\nimplementation_model='followup'\ncontext_window=65536",
            )
            .replace("[models.followup]", "[models.followup]\ncontext_window=32768");
        rook.write_config(&settings);
        let mut daemon = shared.then(|| Daemon::start(&rook));
        let output = rook.run(&["--json", "run", "Read evidence, write it once, then answer."]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let outcome: Value = serde_json::from_slice(&output.stdout).unwrap();
        let session = outcome["session"].as_str().unwrap();
        assert_eq!(model.next()["model"], "initial-model");
        assert_eq!(model.next()["model"], "initial-model", "read-only exploration stays on analysis");
        assert_eq!(model.next()["model"], "initial-model", "a failed edit does not activate implementation");
        let implementation = model.next();
        assert_eq!(implementation["model"], "followup-model");
        let messages = implementation["messages"].as_array().unwrap();
        let write = messages
            .iter()
            .filter_map(|m| m["tool_calls"].as_array())
            .flatten()
            .find(|c| c["function"]["name"] == "write_file")
            .unwrap();
        let arguments: Value =
            serde_json::from_str(write["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(arguments["content"], "after\n", "the new model retains the actual write arguments");
        assert!(messages.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == write["id"]));
        assert!(
            implementation["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "write_file")
        );
        assert!(!implementation["messages"].to_string().contains("rook:model-phase:v1"));
        assert_eq!(std::fs::read_to_string(rook.workspace.path().join("evidence.txt")).unwrap(), "after\n");
        if shared {
            drop(daemon.take());
            daemon = Some(Daemon::start(&rook));
        }
        let resumed = rook.run(&["--json", "run", "Continue without another edit.", "--session", session]);
        assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
        assert_eq!(
            model.next()["model"],
            "followup-model",
            "saved branch phase survives a new process/daemon"
        );
        assert!(model.requests.try_recv().is_err(), "no classifier or extra work request");
        drop(daemon);
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        let id = rook_store::parse_session_id(session).unwrap();
        let events = store.events(id, 0, 256).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(
                    |e| e.record.kind == rook_store::EventKind::ToolResult && e.record.label == "write_file"
                )
                .count(),
            1
        );
        assert!(store.kv_get_limited(&format!("model-phase/{id:032x}"), 16384).unwrap().is_some());
    }
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
fn live_tool_completion_points_to_a_readable_measured_result_before_the_next_model_reply() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::with_messages(vec![json!({
        "role":"assistant", "content":"", "tool_calls":[{
            "index":0,"id":"write-once","type":"function","function":{
                "name":"write_file","arguments":"{\"path\":\"evidence.txt\",\"content\":\"after\\n\"}"
            }
        }]
    })]);
    std::fs::write(rook.workspace.path().join("evidence.txt"), "before\n").unwrap();
    rook.write_config(
        &config(&model.url, "initial", "ask", rook.workspace.path())
            .replace("mode='ask'", "mode='ask'\nallow=['evidence.txt']"),
    );
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
                json!({"type":"prompt","text":"write evidence"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    let started = runtime.block_on(socket_event(&mut socket, "started"));
    let session = started["session"].as_str().unwrap();
    model.next();
    model.release.store(1, Ordering::SeqCst);
    let completed = runtime.block_on(socket_event(&mut socket, "tool_done"));
    assert_eq!(completed["name"], "write_file");
    assert_eq!(completed["failed"], false);
    let seq = completed["result_seq"].as_u64().expect("exact saved result reference");
    let base = format!("{}/api/sessions/{session}/history", daemon.address);
    let page = runtime.block_on(get(&client, &format!("{base}/{seq}?offset=0")));
    assert_eq!(page["entry"]["kind"], "tool-result");
    assert_eq!(page["entry"]["label"], "write_file");
    assert_eq!(page["entry"]["tool_measurement"]["failed"], false);
    assert!(page["entry"]["tool_measurement"]["duration_ms"].is_u64());
    let note = page["entry"]["change_note"].as_u64().unwrap();
    let changes = runtime.block_on(get(&client, &format!("{base}/{note}?offset=0")));
    let text = changes["entry"]["body"].as_str().unwrap();
    assert!(text.contains("-before") && text.contains("+after"), "{text}");
    assert_eq!(std::fs::read_to_string(rook.workspace.path().join("evidence.txt")).unwrap(), "after\n");
    model.next(); // The next reply is still withheld while the result is browsed.
    model.release.store(2, Ordering::SeqCst);
    runtime.block_on(socket_event(&mut socket, "done"));
}

#[test]
fn live_command_and_search_details_are_readable_from_the_daemon_before_the_next_reply() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::with_messages(vec![json!({"role":"assistant","content":"","tool_calls":[
        {"index":0,"id":"command","type":"function","function":{"name":"run_command","arguments":"{\"command\":\"exit 7\"}"}},
        {"index":1,"id":"search","type":"function","function":{"name":"search","arguments":"{\"path\":\"haystack.txt\",\"pattern\":\"needle\",\"limit\":1}"}}
    ]})]);
    std::fs::write(rook.workspace.path().join("haystack.txt"), "needle needle\nneedle\n").unwrap();
    rook.write_config(
        &config(&model.url, "initial", "ask", rook.workspace.path())
            .replace("mode='ask'", "mode='ask'\nallow=['exit 7']"),
    );
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
                json!({"type":"prompt","text":"run command and search"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    let started = runtime.block_on(socket_event(&mut socket, "started"));
    let session = started["session"].as_str().unwrap();
    model.next();
    model.release.store(1, Ordering::SeqCst);
    let mut names = Vec::new();
    for _ in 0..2 {
        let completed = runtime.block_on(socket_event(&mut socket, "tool_done"));
        let name = completed["name"].as_str().unwrap();
        names.push(name.to_string());
        let seq = completed["result_seq"].as_u64().unwrap();
        let page = runtime.block_on(get(
            &client,
            &format!("{}/api/sessions/{session}/history/{seq}?offset=0", daemon.address),
        ));
        assert_eq!(page["entry"]["label"], name);
        let details = &page["entry"]["tool_details"];
        assert!(details["note_seq"].as_u64().unwrap() < seq);
        if name == "run_command" {
            assert_eq!(details["exit_code"], 7);
            assert_eq!(completed["failed"], true);
            assert_eq!(page["entry"]["tool_measurement"]["failed"], true);
        } else {
            assert_eq!(name, "search");
            assert_eq!(details["matches"], 2);
            assert_eq!(details["files_scanned"], 1);
            assert_eq!(details["complete"], true);
        }
    }
    names.sort();
    assert_eq!(names, ["run_command", "search"]);
    let next = model.next();
    assert!(
        next["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| !m["content"].as_str().unwrap_or("").contains("search_complete"))
    );
    model.release.store(2, Ordering::SeqCst);
    runtime.block_on(socket_event(&mut socket, "done"));
}

#[test]
fn daemon_skill_completion_links_success_repeated_success_and_failure_before_the_next_reply() {
    rook_llm::init_tls();
    let rook = Rook::new();
    rook.skill(
        "greeting",
        "---\nname: greeting\ndescription: Use when greeting.\nversion: 1.0.0\n---\nSKILL_LINK_MARKER\n",
    );
    let calls: Vec<_> = ["greeting", "greeting", "no-such-skill"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            json!({"index":index,"id":format!("skill-{index}"),"type":"function","function":{
                "name":"load_skill","arguments":json!({"name":name}).to_string()
            }})
        })
        .collect();
    let model = Model::with_messages(vec![json!({"role":"assistant","content":"","tool_calls":calls})]);
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
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
                json!({"type":"prompt","text":"load the greeting skill twice and try a missing skill"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        socket
    });
    let started = runtime.block_on(socket_event(&mut socket, "started"));
    let session = started["session"].as_str().unwrap();
    model.next();
    model.release.store(1, Ordering::SeqCst);
    let mut completed = Vec::new();
    for failed in [false, false, true] {
        let event = runtime.block_on(socket_event(&mut socket, "tool_done"));
        assert_eq!(event["name"], "load_skill");
        assert_eq!(event["failed"], failed);
        let seq = event["result_seq"].as_u64().expect("saved built-in result must be linked live");
        let page = runtime.block_on(get(
            &client,
            &format!("{}/api/sessions/{session}/history/{seq}?offset=0", daemon.address),
        ));
        assert_eq!(page["entry"]["kind"], "tool-result");
        assert_eq!(page["entry"]["label"], "load_skill");
        assert_eq!(page["entry"]["tool_measurement"]["failed"], failed);
        assert!(page["entry"]["tool_measurement"]["timing_seq"].as_u64().unwrap() > seq);
        let body = page["entry"]["body"].as_str().unwrap().to_string();
        assert!(body.contains(if failed { "no-such-skill" } else { "SKILL_LINK_MARKER" }));
        completed.push((seq, body));
    }
    assert!(completed.windows(2).all(|pair| pair[0].0 < pair[1].0));
    let next = model.next();
    let tools: Vec<_> = next["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(tools.len(), 3);
    for (tool, (_, body)) in tools[..2].iter().zip(&completed[..2]) {
        assert_eq!(tool["content"], *body, "navigation must preserve the loaded skill's source envelope");
        let envelope: Value = serde_json::from_str(body).unwrap();
        assert!(envelope.get("result_id").is_none());
        assert!(envelope["rook_source"]["origin"].as_str().unwrap().ends_with("SKILL.md"));
    }
    model.release.store(2, Ordering::SeqCst);
    runtime.block_on(socket_event(&mut socket, "done"));
}

#[test]
fn socket_stop_uses_observed_goal_generation_and_replays_once() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let session = rook_store::new_session_id();
    let id = rook_store::format_session_id(session);
    let ordinary = rook_store::new_session_id();
    let ordinary_id = rook_store::format_session_id(ordinary);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    store
        .create_session(&rook_store::SessionMeta::new(
            session,
            "stop identity",
            rook.workspace.path().display().to_string(),
            rook_store::now_unix(),
        ))
        .unwrap();
    store
        .create_session(&rook_store::SessionMeta::new(
            ordinary,
            "ordinary stop identity",
            rook.workspace.path().display().to_string(),
            rook_store::now_unix(),
        ))
        .unwrap();
    drop(store);
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let work = format!("{}/api/work/{id}", daemon.address);
    let mut socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":id,"text":"/goal inspect"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    });
    let announced = runtime.block_on(socket_event(&mut socket, "goal"));
    let generation = announced["generation"].as_str().unwrap().to_owned();
    assert_eq!(runtime.block_on(get(&client, &work))["generation"], generation);
    model.next(); // Keep the provider request in flight while Stop is retried.
    let started = runtime.block_on(socket_event(&mut socket, "agent"));
    assert!(started["text"].as_str().unwrap().contains("Goal started"), "{started}");
    runtime.block_on(async {
        socket.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"stop","id":"x".repeat(65),"generation":generation}).to_string().into(),
        )).await.unwrap();
        let error = socket_event(&mut socket, "error").await;
        assert!(error["message"].as_str().unwrap().contains("Stop ID must"), "{error}");
        assert_ne!(get(&client, &work).await["status"], "paused");
        let stop = json!({"type":"stop","id":"stop-once","generation":generation});
        socket.send(tokio_tungstenite::tungstenite::Message::Text(stop.to_string().into())).await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while get(&client, &work).await["status"] != "paused" {
            assert!(tokio::time::Instant::now() < deadline, "Stop did not pause the goal");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let receipt = socket_event(&mut socket, "stop_applied").await;
        assert_eq!(receipt["id"], "stop-once");
        assert_eq!(receipt["generation"], generation);
        assert_eq!(receipt["already_applied"], false);
        let first = socket_event(&mut socket, "agent").await;
        assert!(first["text"].as_str().unwrap().contains("Pausing goal"), "{first}");
        client.post(format!("{work}/control")).json(&json!("resume"))
            .send().await.unwrap().error_for_status().unwrap();
        assert_ne!(get(&client, &work).await["status"], "paused");
        socket.send(tokio_tungstenite::tungstenite::Message::Text(stop.to_string().into())).await.unwrap();
        let receipt = socket_event(&mut socket, "stop_applied").await;
        assert_eq!(receipt["id"], "stop-once");
        assert_eq!(receipt["already_applied"], true);
        let event = socket_event(&mut socket, "agent").await;
        assert!(event["text"].as_str().unwrap().contains("already applied"), "{event}");
        assert_ne!(get(&client, &work).await["status"], "paused", "a replay paused the resumed goal");
        socket.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"stop","id":"without-generation"}).to_string().into(),
        )).await.unwrap();
        let error = socket_event(&mut socket, "error").await;
        assert!(error["message"].as_str().unwrap().contains("identity is not known"), "{error}");
        assert_ne!(get(&client, &work).await["status"], "paused");
        socket.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"stop","id":"wrong-generation","generation":rook_store::format_session_id(rook_store::new_session_id())}).to_string().into(),
        )).await.unwrap();
        let error = socket_event(&mut socket, "error").await;
        assert!(error["message"].as_str().unwrap().contains("earlier run generation"), "{error}");
        assert_ne!(get(&client, &work).await["status"], "paused");
    });
    let mut ordinary_socket = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":ordinary_id,"text":"ordinary task"}).to_string().into(),
            ))
            .await
            .unwrap();
        let goal = socket_event(&mut socket, "goal").await;
        assert!(goal["generation"].is_null(), "{goal}");
        socket
    });
    runtime.block_on(async {
        ordinary_socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"stop","id":"ordinary-stop"}).to_string().into(),
            ))
            .await
            .unwrap();
        let receipt = socket_event(&mut ordinary_socket, "stop_applied").await;
        assert_eq!(receipt["id"], "ordinary-stop");
        assert!(receipt["generation"].is_null());
        assert_eq!(receipt["already_applied"], false);
        let _ = socket_event(&mut ordinary_socket, "cancelled").await;
    });
}

#[test]
fn socket_stop_rejects_an_earlier_ordinary_turn() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let session = rook_store::new_session_id();
    let session_id = rook_store::format_session_id(session);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                session,
                "ordinary Stop turn",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
    }
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut socket = runtime.block_on(async {
        let (socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket
    });
    runtime.block_on(async {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session_id,"text":"first turn"}).to_string().into(),
            ))
            .await
            .unwrap();
    });
    let first = runtime.block_on(socket_event(&mut socket, "turn"))["id"].as_str().unwrap().to_owned();
    runtime.block_on(async {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"stop","id":"first-stop","turn":first}).to_string().into(),
            ))
            .await
            .unwrap();
        assert_eq!(socket_event(&mut socket, "stop_applied").await["id"], "first-stop");
        socket_event(&mut socket, "cancelled").await;
        // The aborted task drops its execution guard before a successor starts.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":session_id,"text":"second turn"}).to_string().into(),
            ))
            .await
            .unwrap();
    });
    let second = runtime.block_on(socket_event(&mut socket, "turn"))["id"].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    runtime.block_on(async {
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"stop","id":"first-stop","session":session_id,"turn":first}).to_string().into(),
            ))
            .await
            .unwrap();
        let duplicate = socket_event(&mut socket, "stop_applied").await;
        assert_eq!(duplicate["id"], "first-stop");
        assert_eq!(duplicate["already_applied"], true);
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"stop","id":"first-stop","session":session_id,"turn":second})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let conflict = socket_event(&mut socket, "error").await;
        assert!(conflict["message"].as_str().unwrap().contains("another ordinary turn"), "{conflict}");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"stop","id":"late-first-stop","turn":first}).to_string().into(),
            ))
            .await
            .unwrap();
        let error = socket_event(&mut socket, "error").await;
        assert!(error["message"].as_str().unwrap().contains("earlier ordinary turn"), "{error}");
        socket.send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"stop","id":"wrong-session","session":rook_store::format_session_id(rook_store::new_session_id()),"turn":second}).to_string().into(),
        )).await.unwrap();
        let error = socket_event(&mut socket, "error").await;
        assert!(error["message"].as_str().unwrap().contains("differs from the attached session"), "{error}");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"stop","id":"second-stop","turn":second}).to_string().into(),
            ))
            .await
            .unwrap();
        let applied = socket_event(&mut socket, "stop_applied").await;
        assert_eq!(applied["id"], "second-stop");
        socket_event(&mut socket, "cancelled").await;
    });
    drop(socket);
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    let restarted = Daemon::start(&rook);
    runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            restarted.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        for (id, turn) in [("first-stop", &first), ("second-stop", &second)] {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"type":"stop","id":id,"session":session_id,"turn":turn}).to_string().into(),
                ))
                .await
                .unwrap();
            let receipt = socket_event(&mut socket, "stop_applied").await;
            assert_eq!(receipt["id"], id);
            assert_eq!(receipt["already_applied"], true);
        }
    });
}

#[test]
fn a_retried_continue_prompt_does_not_resume_a_later_paused_goal() {
    rook_llm::init_tls();
    let rook = Rook::new();
    rook.write_config("[agent]\nmodel='missing-model'\ninstall_servers=false\n");
    let session = rook_store::new_session_id();
    let id = rook_store::format_session_id(session);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                session,
                "resume identity",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
    }
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let work = format!("{}/api/work/{id}", daemon.address);
    runtime.block_on(async {
        client
            .post(format!("{}/api/work", daemon.address))
            .json(&json!({
                "goal":"Wait for explicit continuation", "autonomous":false,
                "conversation":{"session":id, "model":null, "effort":"high", "stance":"autonomous"},
                "max_iterations":0, "max_tokens":0, "max_seconds":0,
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    assert_eq!(rook.json(&["task", "pause", &id])["run"]["status"], "paused");
    let prompt = json!({"type":"prompt","session":id,"id":"continue-once","text":"/continue"});
    runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while get(&client, &work).await["status"] == "paused" {
            assert!(tokio::time::Instant::now() < deadline, "first continuation did not resume the goal");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    assert_eq!(rook.json(&["task", "pause", &id])["run"]["status"], "paused");
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();
    let daemon = Daemon::start(&rook);
    runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1)
        ))
        .await
        .unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        let admission = socket_event(&mut socket, "agent").await;
        assert_eq!(admission["admission"], json!({"id":"continue-once","session":id}));
        assert_eq!(socket_event(&mut socket, "done").await["stopped"], "already_admitted");
        assert_eq!(get(&client, &format!("{}/api/work/{id}", daemon.address)).await["status"], "paused");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({"type":"prompt","session":id,"text":"/continue"}).to_string().into(),
            ))
            .await
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while get(&client, &format!("{}/api/work/{id}", daemon.address)).await["status"] == "paused" {
            assert!(tokio::time::Instant::now() < deadline, "legacy continuation no longer resumes");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let before = get(&client, &format!("{}/api/work/{id}", daemon.address)).await;
        client
            .post(format!("{}/api/work/{id}/control", daemon.address))
            .json(&"cancel")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while get(&client, &format!("{}/api/health", daemon.address)).await["turns_running"] != 0 {
            assert!(tokio::time::Instant::now() < deadline, "cancelled continuation still owns its stage");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // A missing provider cannot retain a live execution. The new goal is
        // deliberately paused before retrying the original continuation.
        let replacement: Value = client
            .post(format!("{}/api/work", daemon.address))
            .json(&json!({
                "goal":"A different goal", "autonomous":false,
                "conversation":before["conversation"],
                "max_iterations":0, "max_tokens":0, "max_seconds":0,
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_ne!(replacement["generation"], before["generation"]);
        client
            .post(format!("{}/api/work/{id}/control", daemon.address))
            .json(&"pause")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let history = format!("{}/api/sessions/{id}/history?limit=100", daemon.address);
        let boundary = get(&client, &history).await["through"].clone();
        // A fresh caller does not consume completion frames left over from
        // the separate legacy continuation checked above.
        let (mut replay, _) = tokio_tungstenite::connect_async(format!(
            "{}/api/chat",
            daemon.address.replacen("http", "ws", 1),
        ))
        .await
        .unwrap();
        replay.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        assert_eq!(socket_event(&mut replay, "done").await["stopped"], "already_admitted");
        let after = get(&client, &format!("{}/api/work/{id}", daemon.address)).await;
        assert_eq!(after["status"], "paused");
        assert_eq!(after["generation"], replacement["generation"]);
        assert_eq!(get(&client, &history).await["through"], boundary);
    });
}

#[test]
fn a_socket_continuation_is_confirmed_before_its_held_reply_and_rejoins_on_retry() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let session = rook_store::new_session_id();
    let id = rook_store::format_session_id(session);
    {
        let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
        store
            .create_session(&rook_store::SessionMeta::new(
                session,
                "continuation acknowledgement",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
    }
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let work = format!("{}/api/work/{id}", daemon.address);
    let created: Value = runtime.block_on(async {
        client
            .post(format!("{}/api/work", daemon.address))
            .json(&json!({
                "goal":"CONTINUATION_TASK", "autonomous":false,
                "conversation":{"session":id, "model":null, "effort":"high", "stance":"readonly"},
                "max_iterations":0, "max_tokens":0, "max_seconds":0,
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    assert!(model.next()["messages"].to_string().contains("CONTINUATION_TASK"));
    runtime.block_on(async {
        client
            .post(format!("{work}/control"))
            .json(&"pause")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        model.release.store(1, Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while get(&client, &format!("{}/api/health", daemon.address)).await["turns_running"] != 0 {
            assert!(tokio::time::Instant::now() < deadline, "pause did not finish its operation");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    let prompt = json!({"type":"prompt","session":id,"id":"resume-stable","text":"/continue"});
    runtime.block_on(async {
        let address = format!("{}/api/chat", daemon.address.replacen("http", "ws", 1));
        let (mut first, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        first.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        assert_eq!(
            socket_event(&mut first, "agent").await["admission"],
            json!({"id":"resume-stable","session":id})
        );
        assert_eq!(model.release.load(Ordering::SeqCst), 1, "the resumed reply is still withheld");
        assert!(model.next()["messages"].to_string().contains("CONTINUATION_TASK"));
        let (mut retry, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        retry.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        assert_eq!(
            socket_event(&mut retry, "agent").await["admission"],
            json!({"id":"resume-stable","session":id})
        );
        assert_eq!(socket_event(&mut retry, "attached").await["session"], id);
        assert_eq!(get(&client, &work).await["generation"], created["generation"]);
        assert!(model.requests.try_recv().is_err(), "retry must not construct a second model request");
        let mut conflict = prompt.clone();
        conflict["options"] = json!({"schema_retries":1});
        retry.send(tokio_tungstenite::tungstenite::Message::Text(conflict.to_string().into())).await.unwrap();
        assert!(
            socket_event(&mut retry, "failed").await["message"]
                .as_str()
                .unwrap()
                .contains("different prompt")
        );
        client
            .post(format!("{work}/control"))
            .json(&"cancel")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        model.release.store(2, Ordering::SeqCst);
    });
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
fn goal_correction_retry_keeps_its_receipt_across_daemon_restart() {
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
                "restart correction",
                rook.workspace.path().display().to_string(),
                rook_store::now_unix(),
            ))
            .unwrap();
        rook_store::format_session_id(id)
    };
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let address = format!("{}/api/chat", daemon.address.replacen("http", "ws", 1));
    let mut first = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type":"prompt","session":session,"text":"/goal RESTART_GOAL"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        socket
    });
    assert!(model.next()["messages"].to_string().contains("RESTART_GOAL"));
    let work_url = format!("{}/api/work/{session}", daemon.address);
    let generation = runtime.block_on(get(&client, &work_url))["generation"].clone();
    let page = runtime.block_on(get(&client, &format!("{}/api/sessions/{session}/queue", daemon.address)));
    let target = page["submission_target"].as_str().unwrap().to_owned();
    let correction = json!({
        "type":"prompt","session":session,"text":"RESTART_CORRECTION",
        "id":"stable-correction","target":target
    });
    runtime.block_on(async {
        first
            .send(tokio_tungstenite::tungstenite::Message::Text(correction.to_string().into()))
            .await
            .unwrap();
    });
    let admitted = runtime.block_on(socket_event(&mut first, "interjected"))["receipt"].clone();
    let reference = admitted["reference"].as_str().unwrap().to_owned();
    drop(first);
    drop(daemon);
    std::fs::remove_file(rook.home.path().join("rookd.addr")).unwrap();

    // The direct CLI path reads the same receipt while the daemon is down.
    let local = rook.json(&["session", "queue", &session, "show", &reference]);
    assert_eq!(local["receipt"]["id"], "stable-correction");
    assert_eq!(local["reference"], reference);
    let daemon = Daemon::start(&rook);
    let resumed =
        model.requests.recv_timeout(std::time::Duration::from_secs(30)).expect("goal did not resume");
    assert_eq!(
        resumed["messages"].to_string().matches("RESTART_CORRECTION").count(),
        1,
        "saved correction must enter the resumed model request once: {resumed}"
    );
    let accepted = runtime
        .block_on(get(&client, &format!("{}/api/sessions/{session}/queue/{reference}", daemon.address)));
    assert!(accepted["receipt"]["applied_at"].is_number(), "{accepted}");
    let address = format!("{}/api/chat", daemon.address.replacen("http", "ws", 1));
    let mut retry = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(correction.to_string().into()))
            .await
            .unwrap();
        socket
    });
    let repeated = runtime.block_on(socket_event(&mut retry, "interjected"))["receipt"].clone();
    assert_eq!(repeated["reference"], admitted["reference"]);
    assert_eq!(repeated["id"], admitted["id"]);
    assert_eq!(repeated["submitted_at"], admitted["submitted_at"]);
    assert_eq!(
        runtime.block_on(get(&client, &format!("{}/api/work/{session}", daemon.address)))["generation"],
        generation
    );
    let page = runtime.block_on(get(
        &client,
        &format!("{}/api/sessions/{session}/queue?include_finished=true", daemon.address),
    ));
    assert_eq!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["receipt"]["id"] == "stable-correction")
            .count(),
        1,
        "{page}"
    );
    runtime.block_on(async {
        retry
            .send(tokio_tungstenite::tungstenite::Message::Text(
                json!({
                    "type":"prompt","session":session,"text":"CHANGED_CORRECTION",
                    "id":"stable-correction","target":target
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    });
    let failed = runtime.block_on(socket_event(&mut retry, "failed"));
    assert!(failed["message"].as_str().unwrap().contains("different text"), "{failed}");
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
    let admission = runtime.block_on(socket_event(&mut first, "agent"));
    assert_eq!(admission["admission"]["id"], "goal-stable", "{admission}");
    let started = runtime.block_on(socket_event(&mut first, "started"));
    let session = started["session"].as_str().unwrap().to_string();
    assert_eq!(admission["admission"]["session"], session);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let work_url = format!("{}/api/work/{session}", daemon.address);
    let first_run = runtime.block_on(get(&client, &work_url));
    let mut retry = runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into())).await.unwrap();
        socket
    });
    let admission = runtime.block_on(socket_event(&mut retry, "agent"));
    assert_eq!(admission["admission"], json!({"id":"goal-stable","session":session}));
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
    // Admission records the requested goal. Neither a stage nor a retry
    // should add a duplicate note or overwrite a later accepted correction.
    assert_eq!(
        store.events(id, 0, 100).unwrap().iter().filter(|event| event.record.label == "goal").count(),
        1
    );
}

#[test]
fn refused_goal_creation_never_acknowledges_or_duplicates_its_reserved_request() {
    rook_llm::init_tls();
    let rook = Rook::new();
    let model = Model::new();
    rook.write_config(&config(&model.url, "initial", "ask", rook.workspace.path()));
    let daemon = Daemon::start(&rook);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let limit = rook_core::Config::default().work.max_goal_bytes;
    let goal = "x".repeat(limit + 1);
    assert!(goal.len() > limit);
    let prompt = json!({"type":"prompt","session":null,"id":"rejected-goal", "text":format!("/goal {goal}")});
    let address = format!("{}/api/chat", daemon.address.replacen("http", "ws", 1));
    for _ in 0..2 {
        runtime.block_on(async {
            let (mut socket, _) = tokio_tungstenite::connect_async(&address).await.unwrap();
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(prompt.to_string().into()))
                .await
                .unwrap();
            for _ in 0..128 {
                let frame = tokio::time::timeout(std::time::Duration::from_secs(90), socket.next())
                    .await
                    .expect("goal refusal timed out")
                    .expect("socket closed")
                    .unwrap();
                let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                assert!(event["admission"].is_null(), "a refused goal cannot acknowledge admission: {event}");
                if event["type"] == "failed" {
                    assert!(event["message"].as_str().unwrap().contains("goal must contain text"), "{event}");
                    return;
                }
            }
            panic!("goal refusal was not delivered");
        });
    }
    drop(daemon);
    let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1, "retry reuses the reserved session even before goal admission");
    assert_eq!(sessions[0].next_seq, 0, "neither refusal writes goal or prompt events");
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
