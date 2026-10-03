//! Owned HTTP providers and real ACP connections; no installed daemon or model.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rook_core::{Config, ModelSource, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::{EventKind, Store};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

struct Editor {
    input: tokio::io::DuplexStream,
    output: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    serve: tokio::task::JoinHandle<std::io::Result<()>>,
    rook: Arc<Rook>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}
impl Editor {
    fn start(config: Config, seed: impl FnOnce(&Rook)) -> Self {
        static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        // This test binary sets one scratch home once, before starting agents.
        unsafe { std::env::set_var("ROOK_HOME", home.path()) };
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            Store::open(store.path()).unwrap(),
            config,
            Environment::bare("windows", "x86_64", "0.1.0"),
            SkillIndex::default(),
            workspace.path().to_path_buf(),
        );
        seed(&rook);
        let inspection = Arc::new(rook.for_workspace(rook.workspace.clone()));
        let (input, server_input) = tokio::io::duplex(64 * 1024);
        let (server_output, output) = tokio::io::duplex(64 * 1024);
        let serve = tokio::spawn(rook_acp::serve(rook, BufReader::new(server_input), server_output));
        Self {
            input,
            output: BufReader::new(output).lines(),
            serve,
            rook: inspection,
            _dirs: (store, workspace),
        }
    }
    async fn send(&mut self, message: Value) {
        self.input.write_all(format!("{message}\n").as_bytes()).await.unwrap();
    }
    async fn request(&mut self, id: u64, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await;
    }
    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(60), self.output.next_line())
            .await
            .expect("ACP went quiet")
            .unwrap()
            .expect("ACP closed");
        serde_json::from_str(&line).unwrap()
    }
    async fn until(&mut self, id: u64) -> Vec<Value> {
        let mut messages = Vec::new();
        loop {
            let message = self.next().await;
            let done = message["id"] == id && message["method"].is_null();
            messages.push(message);
            if done {
                return messages;
            }
        }
    }
    async fn initialize(&mut self, caps: Value) {
        self.request(1, "initialize", json!({"protocolVersion":1,"clientCapabilities":caps})).await;
        assert!(self.until(1).await.last().unwrap()["error"].is_null());
    }
    async fn close(mut self) {
        self.input.shutdown().await.unwrap();
        // Read any pending output while the writer releases its leases.
        while tokio::time::timeout(Duration::from_secs(60), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .is_some()
        {}
        tokio::time::timeout(Duration::from_secs(60), self.serve).await.unwrap().unwrap().unwrap();
    }

    async fn reopen(mut self) -> Self {
        self.input.shutdown().await.unwrap();
        while self.output.next_line().await.unwrap().is_some() {}
        tokio::time::timeout(Duration::from_secs(60), self.serve).await.unwrap().unwrap().unwrap();
        let config = self.rook.config.clone();
        let workspace = self.rook.workspace.clone();
        drop(self.rook);
        let rook = Rook::from_parts(
            Store::open(self._dirs.0.path()).unwrap(),
            config,
            Environment::bare("windows", "x86_64", "0.1.0"),
            SkillIndex::default(),
            workspace,
        );
        let inspection = Arc::new(rook.for_workspace(rook.workspace.clone()));
        let (input, server_input) = tokio::io::duplex(64 * 1024);
        let (server_output, output) = tokio::io::duplex(64 * 1024);
        let serve = tokio::spawn(rook_acp::serve(rook, BufReader::new(server_input), server_output));
        Self { input, output: BufReader::new(output).lines(), serve, rook: inspection, _dirs: self._dirs }
    }
}

struct Model {
    base: String,
    seen: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Model {
    async fn start(fail_child: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let mut counts: HashMap<String, usize> = HashMap::new();
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    socket.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(length > 0 && length < 2 * 1024 * 1024);
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                record.lock().unwrap().push(request.clone());
                let summary = request["messages"][0]["content"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("You are compacting"));
                let users: String = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m["role"] == "user")
                    .filter_map(|m| m["content"].as_str())
                    .collect();
                let key = ["GRANDCHILD_ALPHA", "CHILD_ALPHA", "CHILD_BETA", "ROOT_TASK"]
                    .into_iter()
                    .find(|key| users.contains(key))
                    .unwrap_or("COMPACTION_TASK");
                let count = counts.entry(key.into()).or_default();
                let index = *count;
                if request["stream"] == true && !summary {
                    *count += 1;
                }
                if fail_child && key == "CHILD_BETA" {
                    let body = json!({"error":{"message":"controlled terminal child failure","type":"invalid_request_error"}}).to_string();
                    socket.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    continue;
                }
                let call = match (key, index) {
                    ("ROOT_TASK", 0) => {
                        Some(("delegate", json!({"tasks":["CHILD_ALPHA", "CHILD_BETA"],"context":"none"})))
                    }
                    ("CHILD_ALPHA", 0) => {
                        Some(("delegate", json!({"tasks":["GRANDCHILD_ALPHA"],"context":"none"})))
                    }
                    ("CHILD_BETA", 0) => {
                        Some(("write_file", json!({"path":"notes.txt","content":"editor-owned change"})))
                    }
                    ("CHILD_BETA", 1) => Some(("run_command", json!({"command":"echo fixture"}))),
                    ("GRANDCHILD_ALPHA", 0) => Some(("read_file", json!({"path":"notes.txt"}))),
                    _ => None,
                };
                let delta = if summary {
                    json!({"content":"## Goal\nContinue the explicitly accepted task.\n## Open\nInspect the saved source."})
                } else if let Some((name, arguments)) = call {
                    json!({"tool_calls":[{"index":0,"id":format!("{key}_{index}"),"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]})
                } else {
                    json!({"content":format!("{key}_DONE")})
                };
                let streamed = request["stream"] == true;
                let body = if streamed {
                    let finish = if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
                    let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":128,"completion_tokens":8}});
                    format!("data: {frame}\n\ndata: [DONE]\n\n")
                } else {
                    json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}).to_string()
                };
                let content_type = if streamed { "text/event-stream" } else { "application/json" };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        Self { base, seen, task }
    }
    fn config(&self) -> Config {
        let mut config = Config::default();
        config.agent.model = "fixture".into();
        config.agent.install_servers = false;
        config.agent.max_steps = 8;
        config.models.insert(
            "fixture".into(),
            ModelSource {
                model: "fixture".into(),
                api: "openai".into(),
                url: self.base.clone(),
                context_window: Some(8192),
                parallel: Some(0),
                ..Default::default()
            },
        );
        config
    }
}

#[tokio::test]
async fn live_children_announce_before_owned_events_and_editor_requests_with_restricted_controls() {
    let model = Model::start(false).await;
    let mut editor = Editor::start(model.config(), |rook| {
        std::fs::write(rook.workspace.join("notes.txt"), "disk remains unchanged").unwrap();
    });
    editor
        .initialize(json!({"subagents":{},"terminal":true,"fs":{"readTextFile":true,"writeTextFile":true}}))
        .await;
    editor.request(2, "session/new", json!({"cwd":"."})).await;
    let root = editor.until(2).await.last().unwrap()["result"]["sessionId"].as_str().unwrap().to_owned();
    editor
        .request(3, "session/prompt", json!({"sessionId":root,"prompt":[{"type":"text","text":"ROOT_TASK"}]}))
        .await;
    let mut parents = HashMap::new();
    let mut titles = HashMap::new();
    let mut calls = HashMap::new();
    let mut messages = Vec::new();
    let mut checked_controls = false;
    let mut fs_requests = 0;
    let mut terminal_methods = Vec::new();
    loop {
        let message = editor.next().await;
        if message["method"] == "session/update" {
            let session = message["params"]["sessionId"].as_str().unwrap();
            let update = &message["params"]["update"];
            if update["sessionUpdate"] == "subagent_update" {
                let child = update["sessionId"].as_str().unwrap();
                assert!(
                    session == root || parents.contains_key(session),
                    "parent must be announced first: {message}"
                );
                if let Some(old) = parents.insert(child.to_owned(), session.to_owned()) {
                    assert_eq!(old, session);
                }
                if let Some(title) = update["title"].as_str() {
                    titles.insert(child.to_owned(), title.to_owned());
                    assert_eq!(update["capabilities"], json!({}));
                }
            } else if session != root {
                assert!(parents.contains_key(session), "child event preceded its association: {message}");
                if update["sessionUpdate"] == "session_message_chunk" {
                    assert_eq!(update["senderSessionId"], session);
                    assert_eq!(update["recipientSessionId"], parents[session]);
                }
                assert_ne!(
                    update["sessionUpdate"], "agent_message_chunk",
                    "child replies are directed to their parent"
                );
                if update["sessionUpdate"] == "tool_call" {
                    calls.insert(session.to_owned(), update["toolCallId"].clone());
                }
            }
            if update["sessionUpdate"] == "session_message" {
                let child = update["recipientSessionId"].as_str().unwrap();
                assert!(parents.contains_key(child), "directed task preceded its child association");
                assert_eq!(update["senderSessionId"], parents[child]);
            }
        }
        if message["method"].as_str().is_some_and(|method| method.starts_with("terminal/")) {
            let child = message["params"]["sessionId"].as_str().unwrap();
            assert_ne!(child, root);
            assert!(parents.contains_key(child), "terminal request must follow its child association");
            let method = message["method"].as_str().unwrap();
            terminal_methods.push(method.to_owned());
            let result = match method {
                "terminal/create" => json!({"terminalId":"child-terminal"}),
                "terminal/output" => {
                    json!({"output":"fixture\n","truncated":false,"exitStatus":{"exitCode":0,"signal":null}})
                }
                _ => json!({}),
            };
            editor.send(json!({"jsonrpc":"2.0","id":message["id"],"result":result})).await;
        }
        if matches!(
            message["method"].as_str(),
            Some("session/request_permission" | "fs/read_text_file" | "fs/write_text_file")
        ) {
            let session = message["params"]["sessionId"].as_str().unwrap().to_owned();
            assert_ne!(session, root, "child requests must not borrow parent identity");
            assert!(parents.contains_key(&session), "request preceded association: {message}");
            let result = if message["method"] == "session/request_permission" {
                assert_eq!(message["params"]["toolCall"]["toolCallId"], calls[&session]);
                assert!(messages.iter().any(|m: &Value| m["params"]["update"]["sessionId"] == session
                    && m["params"]["update"]["state"]["state"] == "requires_action"));
                if !checked_controls {
                    checked_controls = true;
                    editor
                        .send(
                            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}),
                        )
                        .await;
                    for (id, method, params) in [
                        (
                            90,
                            "session/prompt",
                            json!({"sessionId":session,"prompt":[{"type":"text","text":"inject"}]}),
                        ),
                        (91, "session/load", json!({"sessionId":session})),
                        (92, "session/set_mode", json!({"sessionId":session,"modeId":"autonomous"})),
                        (93, "session/prompt", json!({"sessionId":"bad","prompt":[]})),
                    ] {
                        editor.request(id, method, params).await;
                    }
                    // Loading the live parent must not publish stale unknown states.
                    editor.request(94, "session/load", json!({"sessionId":root})).await;
                }
                json!({"outcome":{"outcome":"selected","optionId":"once"}})
            } else {
                fs_requests += 1;
                if message["method"] == "fs/read_text_file" {
                    json!({"content":"unsaved editor buffer"})
                } else {
                    json!({})
                }
            };
            editor.send(json!({"jsonrpc":"2.0","id":message["id"],"result":result})).await;
        }
        let done = message["id"] == 3 && message["method"].is_null();
        messages.push(message);
        if done {
            break;
        }
    }
    assert_eq!(messages.last().unwrap()["result"]["stopReason"], "end_turn", "{messages:#?}");
    assert!(checked_controls && fs_requests >= 2);
    assert_eq!(
        terminal_methods,
        ["terminal/create", "terminal/wait_for_exit", "terminal/output", "terminal/release"]
    );
    assert_eq!(parents.len(), 3, "two siblings and a nested child must actually run");
    let alpha = titles.iter().find(|(_, title)| *title == "CHILD_ALPHA").unwrap().0;
    let grandchild = titles.iter().find(|(_, title)| *title == "GRANDCHILD_ALPHA").unwrap().0;
    assert_eq!(&parents[grandchild], alpha);
    for id in 90..94 {
        assert!(
            messages.iter().any(|m| m["id"] == id && m["error"]["code"] == -32602),
            "rejected request {id}: {messages:#?}"
        );
    }
    assert!(messages.iter().any(|m| m["id"] == 94 && m["result"].is_object()));
    for (child, title) in &titles {
        assert!(
            messages.iter().any(|m| m["params"]["sessionId"] == *child
                && m["params"]["update"]["content"]["text"] == format!("{title}_DONE")),
            "missing own child answer {title}"
        );
        assert!(messages.iter().any(|m| m["params"]["update"]["sessionId"] == *child
            && m["params"]["update"]["state"]["state"] == "idle"));
    }
    assert_eq!(
        std::fs::read_to_string(editor.rook.workspace.join("notes.txt")).unwrap(),
        "disk remains unchanged"
    );
    assert!(
        model
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "user"
                    && m["content"].as_str().is_some_and(|s| s.contains("CHILD_BETA")))
                && r["tools"].is_array())
            .all(|r| r["tools"].as_array().unwrap().iter().all(|tool| tool["function"]["name"] != "ask")),
        "children must not borrow the parent's interactive question tool"
    );
    assert!(model.seen.lock().unwrap().iter().any(|r| {
        r["messages"].as_array().unwrap().iter().any(|m| {
            m["role"] == "tool" && m["content"].as_str().is_some_and(|s| s.contains("unsaved editor buffer"))
        })
    }));
    editor.request(95, "session/load", json!({"sessionId":root})).await;
    let recovered = editor.until(95).await;
    assert!(recovered.iter().any(|m| m["params"]["update"]["sessionId"] == *grandchild));
    assert_eq!(
        recovered.last().unwrap()["result"]["_meta"]["rook"]["recovery"]["childHistoryReplayed"],
        false
    );
    editor.close().await;
}

#[tokio::test]
async fn failed_child_reports_error_on_its_parent_association_while_v1_root_still_finishes() {
    let model = Model::start(true).await;
    let mut config = model.config();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start(config, |_| {});
    editor.initialize(json!({"subagents":{}})).await;
    editor.request(2, "session/new", json!({"cwd":"."})).await;
    let root = editor.until(2).await.last().unwrap()["result"]["sessionId"].as_str().unwrap().to_owned();
    editor
        .request(3, "session/prompt", json!({"sessionId":root,"prompt":[{"type":"text","text":"ROOT_TASK"}]}))
        .await;
    let messages = editor.until(3).await;
    let failed = messages
        .iter()
        .find(|m| m["params"]["update"]["state"]["stopReason"] == "error")
        .expect("actual failed child must report failure");
    assert_eq!(failed["params"]["sessionId"], root);
    assert_eq!(failed["params"]["update"]["state"]["state"], "idle");
    assert_eq!(failed["params"]["update"]["state"]["error"]["code"], -32603);
    assert_eq!(messages.last().unwrap()["result"]["stopReason"], "end_turn");
    editor
        .request(
            4,
            "session/prompt",
            json!({"sessionId":root,"prompt":[{"type":"text","text":"CHILD_BETA"}]}),
        )
        .await;
    let root_failure = editor.until(4).await;
    assert!(
        root_failure.last().unwrap()["error"].is_object(),
        "stable v1 root failure remains a JSON-RPC error response"
    );
    assert!(root_failure.last().unwrap()["result"].is_null());
    editor.close().await;
}

#[tokio::test]
async fn only_object_preview_capabilities_replay_bounded_saved_compactions_and_child_associations() {
    for caps in [
        json!({}),
        json!({"subagents":null,"session":{"compaction":null}}),
        json!({"subagents":true,"session":{"compaction":true}}),
        json!({"subagents":{},"session":{"compaction":{}}}),
    ] {
        let mut root = 0;
        let mut child = 0;
        let compaction = rook_store::format_session_id(rook_store::new_session_id());
        let mut editor = Editor::start(Config::default(), |rook| {
            root = rook.start_session("saved parent").unwrap();
            child = rook.fork_for_subtask(root, "saved child").unwrap();
            rook.log(
                root,
                EventKind::Compaction,
                "auto",
                &json!({"through_seq":0,"summary":"saved ordinary summary","compaction_id":compaction})
                    .to_string(),
            )
            .unwrap();
            // Old records still recover with a deterministic source ID.
            rook.log(
                child,
                EventKind::Compaction,
                "auto",
                &json!({"through_seq":0,"summary":"old child summary"}).to_string(),
            )
            .unwrap();
        });
        let enabled = caps["subagents"].is_object();
        editor.initialize(caps).await;
        editor.request(2, "session/load", json!({"sessionId":rook_store::format_session_id(root)})).await;
        let first = editor.until(2).await;
        assert_eq!(
            first.iter().any(|m| m["params"]["update"]["sessionUpdate"] == "subagent_update"),
            enabled
        );
        assert_eq!(
            first.iter().any(|m| m["params"]["update"]["sessionUpdate"] == "compaction_update"),
            enabled
        );
        if enabled {
            let child_id = rook_store::format_session_id(child);
            let association =
                first.iter().position(|m| m["params"]["update"]["sessionId"] == child_id).unwrap();
            let child_compaction = first
                .iter()
                .position(|m| {
                    m["params"]["sessionId"] == child_id
                        && m["params"]["update"]["sessionUpdate"] == "compaction_update"
                })
                .unwrap();
            assert!(association < child_compaction);
            assert_eq!(
                first[association]["params"]["update"]["state"]["state"], "unknown",
                "an unsaved finish must not be fabricated"
            );
            assert!(first.iter().any(|m| m["params"]["update"]["compactionId"] == compaction));
            editor.request(3, "session/load", json!({"sessionId":rook_store::format_session_id(root)})).await;
            let second = editor.until(3).await;
            let ids = |messages: &[Value]| {
                messages
                    .iter()
                    .filter_map(|m| m["params"]["update"]["compactionId"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            };
            assert_eq!(ids(&first), ids(&second));
        }
        editor.close().await;
    }
}

#[tokio::test]
async fn actual_compaction_sends_start_and_saved_completion_with_one_recoverable_id() {
    let model = Model::start(false).await;
    let mut config = model.config();
    config.models.get_mut("fixture").unwrap().context_window = Some(4000);
    let mut root = 0;
    let mut editor = Editor::start(config, |rook| {
        root = rook.start_session("long transcript").unwrap();
        for i in 0..12 {
            let kind = if i % 2 == 0 { EventKind::UserMessage } else { EventKind::AssistantMessage };
            rook.log(root, kind, "turn", &format!("distinct evidence {i}: ").repeat(140)).unwrap();
        }
        assert!(
            rook.context_usage(root, Some(4000)).unwrap().needs_compaction,
            "fixture must cross the actual threshold"
        );
    });
    editor.initialize(json!({"session":{"compaction":{}}})).await;
    editor.request(2,"session/prompt",json!({"sessionId":rook_store::format_session_id(root),"prompt":[{"type":"text","text":"COMPACTION_TASK"}]})).await;
    let messages = editor.until(2).await;
    let events: Vec<_> =
        messages.iter().filter(|m| m["params"]["update"]["sessionUpdate"] == "compaction_update").collect();
    assert_eq!(events.len(), 2, "actual lifecycle: {messages:#?}");
    assert_eq!(events[0]["params"]["update"]["status"], "in_progress");
    assert_eq!(events[1]["params"]["update"]["status"], "completed");
    let id = events[0]["params"]["update"]["compactionId"].as_str().unwrap().to_owned();
    assert_eq!(events[1]["params"]["update"]["compactionId"], id);
    let saved = editor
        .rook
        .store
        .events(root, 0, 1000)
        .unwrap()
        .into_iter()
        .find(|e| e.record.kind == EventKind::Compaction)
        .unwrap();
    let note: Value = serde_json::from_slice(&editor.rook.store.get(&saved.record.body).unwrap()).unwrap();
    assert_eq!(note["compaction_id"], id);
    assert_eq!(events[1]["params"]["update"]["summary"][0]["text"], note["summary"]);
    let mut editor = editor.reopen().await;
    editor.initialize(json!({"session":{"compaction":{}}})).await;
    editor.request(3, "session/load", json!({"sessionId":rook_store::format_session_id(root)})).await;
    assert!(editor.until(3).await.iter().any(|m| m["params"]["update"]["compactionId"] == id));
    let summary = model
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|r| r["messages"][0]["content"].as_str().is_some_and(|s| s.starts_with("You are compacting")))
        .unwrap()
        .clone();
    assert_eq!(summary["max_tokens"], 2048);
    editor.close().await;
}

#[tokio::test]
async fn cancelling_the_parent_loses_child_observation_without_fabricating_child_completion() {
    let model = Model::start(false).await;
    let mut editor = Editor::start(model.config(), |_| {});
    editor.initialize(json!({"subagents":{}})).await;
    editor.request(2, "session/new", json!({"cwd":"."})).await;
    let root = editor.until(2).await.last().unwrap()["result"]["sessionId"].as_str().unwrap().to_owned();
    editor
        .request(3, "session/prompt", json!({"sessionId":root,"prompt":[{"type":"text","text":"ROOT_TASK"}]}))
        .await;
    let waiting_child = loop {
        let message = editor.next().await;
        if message["method"] == "session/request_permission" {
            break message["params"]["sessionId"].as_str().unwrap().to_owned();
        }
        assert!(message["method"].is_string(), "root finished before cancellation precondition: {message}");
    };
    editor.request(4, "session/cancel", json!({"sessionId":waiting_child})).await;
    assert_eq!(editor.until(4).await.last().unwrap()["error"]["code"], -32602);
    editor.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":root}})).await;
    let mut cancelled = false;
    let mut unknown = false;
    while !(cancelled && unknown) {
        let message = editor.next().await;
        if message["id"] == 3 && message["method"].is_null() {
            assert!(!cancelled, "prompt answered twice");
            assert_eq!(message["result"]["stopReason"], "cancelled");
            cancelled = true;
        }
        if message["params"]["update"]["sessionId"] == waiting_child {
            let state = &message["params"]["update"]["state"];
            assert_ne!(state["state"], "idle", "dropping observation is not a completed child");
            unknown |= state["state"] == "unknown";
        }
    }
    editor.close().await;
}

#[tokio::test]
async fn recovery_bounds_are_reached_before_child_and_summary_copying_and_report_incomplete_replay() {
    let mut root = 0;
    let summary = "атрибутированный источник 🙂\n".repeat(4000);
    assert!(summary.len() > 64 * 1024);
    let mut editor = Editor::start(Config::default(), |rook| {
        root = rook.start_session("bounded recovery").unwrap();
        for i in 0..70 {
            rook.fork_for_subtask(root, &format!("unfinished child {i}")).unwrap();
        }
        rook.log(
            root,
            EventKind::Compaction,
            "auto",
            &json!({"through_seq":0,"summary":summary}).to_string(),
        )
        .unwrap();
    });
    editor.initialize(json!({"subagents":{},"session":{"compaction":{}}})).await;
    editor.request(2, "session/load", json!({"sessionId":rook_store::format_session_id(root)})).await;
    let messages = editor.until(2).await;
    let announcements = messages.iter().filter(|m| m["params"]["update"]["capabilities"].is_object()).count();
    assert_eq!(announcements, 63, "the recovery boundary must be reached");
    assert_eq!(messages.last().unwrap()["result"]["_meta"]["rook"]["recovery"]["truncated"], true);
    let update = &messages
        .iter()
        .find(|m| m["params"]["update"]["sessionUpdate"] == "compaction_update")
        .unwrap()["params"]["update"];
    let shown = update["summary"][0]["text"].as_str().unwrap();
    assert!(shown.len() <= 64 * 1024 && summary.starts_with(shown));
    assert_eq!(update["_meta"]["rook"]["summaryTruncated"], true);
    assert_eq!(update["_meta"]["rook"]["savedSession"], rook_store::format_session_id(root));
    editor.close().await;
}

#[tokio::test]
async fn compaction_preview_does_not_imply_child_preview_and_child_preview_does_not_imply_compaction() {
    for (caps, children, compaction) in [
        (json!({"subagents":{},"session":{"compaction":null}}), true, false),
        (json!({"subagents":null,"session":{"compaction":{}}}), false, true),
    ] {
        let mut root = 0;
        let mut child = 0;
        let mut editor = Editor::start(Config::default(), |rook| {
            root = rook.start_session("independent previews").unwrap();
            child = rook.fork_for_subtask(root, "child").unwrap();
            for session in [root, child] {
                rook.log(
                    session,
                    EventKind::Compaction,
                    "auto",
                    &json!({"through_seq":0,"summary":"saved"}).to_string(),
                )
                .unwrap();
            }
        });
        editor.initialize(caps).await;
        editor.request(2, "session/load", json!({"sessionId":rook_store::format_session_id(root)})).await;
        let messages = editor.until(2).await;
        assert_eq!(
            messages.iter().any(|m| m["params"]["update"]["sessionUpdate"] == "subagent_update"),
            children
        );
        assert_eq!(
            messages.iter().any(|m| m["params"]["update"]["sessionUpdate"] == "compaction_update"),
            compaction
        );
        if !children {
            assert!(
                !messages.iter().any(|m| m["params"]["sessionId"] == rook_store::format_session_id(child))
            );
        }
        editor.close().await;
    }
}
