//! The protocol surface, driven the way an editor drives it.
//!
//! No model: these cover the methods an editor calls before and around a turn,
//! which is where a wrong field name silently produces an editor that renders
//! nothing.

use std::path::PathBuf;
use std::time::Duration;

use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

struct Editor {
    stdin: tokio::io::DuplexStream,
    lines: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    workspace: PathBuf,
    rook: std::sync::Arc<Rook>,
    serve: tokio::task::JoinHandle<std::io::Result<()>>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Editor {
    fn start() -> Self {
        Self::start_with(Config::default(), |_| {})
    }

    fn start_with(config: Config, seed: impl FnOnce(&std::path::Path)) -> Self {
        Self::start_buffered(config, seed, 64 * 1024)
    }

    fn start_buffered(config: Config, seed: impl FnOnce(&std::path::Path), output_bytes: usize) -> Self {
        static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let scratch = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        unsafe { std::env::set_var("ROOK_HOME", scratch.path()) };
        let store_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        seed(workspace.path());
        let rook = Rook::from_parts(
            Store::open(store_dir.path()).unwrap(),
            config,
            Environment::bare("linux", "x86_64", "0.1.0"),
            SkillIndex::default(),
            PathBuf::from(workspace.path()),
        );

        let inspection = std::sync::Arc::new(rook.for_workspace(rook.workspace.clone()));
        let (client_in, server_in) = tokio::io::duplex(64 * 1024);
        let (server_out, client_out) = tokio::io::duplex(output_bytes);
        let serve = tokio::spawn(rook_acp::serve(rook, BufReader::new(server_in), server_out));

        Self {
            stdin: client_in,
            lines: BufReader::new(client_out).lines(),
            workspace: workspace.path().to_path_buf(),
            rook: inspection,
            serve,
            _dirs: (store_dir, workspace),
        }
    }

    async fn call(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        let request = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin.write_all(format!("{request}\n").as_bytes()).await.unwrap();
        loop {
            let message = self.next().await;
            if message.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return message;
            }
        }
    }

    async fn notify(&mut self, method: &str, params: serde_json::Value) {
        let note = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.stdin.write_all(format!("{note}\n").as_bytes()).await.unwrap();
    }

    /// Everything the server sends within `window`.
    async fn drain(&mut self, window: Duration) -> Vec<serde_json::Value> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + window;
        while let Ok(Ok(Some(line))) = tokio::time::timeout_at(deadline, self.lines.next_line()).await {
            if let Ok(message) = serde_json::from_str(&line) {
                seen.push(message);
            }
        }
        seen
    }

    async fn next(&mut self) -> serde_json::Value {
        // Only to tell "failed" from "hung", so generous: five seconds was met
        // here and missed on the Windows runner, where twenty of these run at
        // once and one of them starts a shell.
        let line = tokio::time::timeout(std::time::Duration::from_secs(60), self.lines.next_line())
            .await
            .expect("the server went quiet")
            .unwrap()
            .expect("the server closed the connection");
        serde_json::from_str(&line).expect("every line must be a JSON-RPC message")
    }
}

#[tokio::test]
async fn initialize_reports_the_protocol_version_and_what_is_supported() {
    let mut editor = Editor::start();
    let reply = editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let result = &reply["result"];
    assert_eq!(result["protocolVersion"], 1);
    assert_eq!(result["agentInfo"]["name"], "rook");
    assert_eq!(result["agentCapabilities"]["loadSession"], true);
    assert!(result["agentCapabilities"]["sessionCapabilities"]["list"].is_object());
    assert!(result["agentCapabilities"]["sessionCapabilities"]["resume"].is_object());
    assert!(result["agentCapabilities"]["sessionCapabilities"]["close"].is_object());
    assert!(result["authMethods"].is_array());
    assert_eq!(result["agentCapabilities"]["promptCapabilities"]["image"], true);
    assert_eq!(result["agentCapabilities"]["promptCapabilities"]["embeddedContext"], true);
    assert_eq!(result["agentCapabilities"]["promptCapabilities"]["audio"], false);
}

#[tokio::test]
async fn a_new_session_comes_back_as_an_id_the_other_methods_accept() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": ".", "mcpServers": [] })).await;
    let id = created["result"]["sessionId"].as_str().expect("sessionId must be a string");
    assert!(rook_store::parse_session_id(id).is_some(), "and a usable one: {id}");

    let loaded = editor.call(2, "session/load", serde_json::json!({ "sessionId": id })).await;
    assert!(loaded.get("error").is_none(), "{loaded}");

    let listed = editor.call(3, "session/list", serde_json::json!({})).await;
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    assert!(sessions.iter().any(|s| s["sessionId"] == id));
}

#[tokio::test]
async fn loading_a_session_that_does_not_exist_is_an_error_not_a_new_one() {
    let mut editor = Editor::start();
    let reply = editor
        .call(1, "session/load", serde_json::json!({ "sessionId": "01ARZ3NDEKTSV4RRFFQ69G5FAV" }))
        .await;
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
}

#[tokio::test]
async fn advertised_session_discovery_pages_without_duplicates_and_filters_workspace() {
    let mut editor = Editor::start();
    let capabilities = editor.call(1, "initialize", serde_json::json!({"protocolVersion":1})).await;
    assert!(capabilities["result"]["agentCapabilities"]["sessionCapabilities"]["list"].is_object());
    let mut created = std::collections::HashSet::new();
    // Exceed the page bound so removing pagination makes this test fail.
    for id in 2..72 {
        let result = editor.call(id, "session/new", serde_json::json!({"cwd":"."})).await;
        created.insert(result["result"]["sessionId"].as_str().unwrap().to_owned());
    }
    let cwd = editor.workspace.to_string_lossy().into_owned();
    let first = editor.call(72, "session/list", serde_json::json!({"cwd":cwd})).await;
    let rows = first["result"]["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 64, "the wire response must be bounded: {first}");
    let cursor = first["result"]["nextCursor"].as_str().expect("another page is required");
    let mut seen: std::collections::HashSet<_> =
        rows.iter().map(|row| row["sessionId"].as_str().unwrap().to_owned()).collect();
    let second = editor.call(73, "session/list", serde_json::json!({"cwd":cwd,"cursor":cursor})).await;
    for row in second["result"]["sessions"].as_array().unwrap() {
        assert!(seen.insert(row["sessionId"].as_str().unwrap().to_owned()), "duplicate session");
    }
    assert_eq!(seen, created);
    assert!(second["result"].get("nextCursor").is_none());
    #[cfg(unix)]
    {
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("project");
        std::os::unix::fs::symlink(&editor.workspace, &alias).unwrap();
        assert_ne!(alias, alias.canonicalize().unwrap());
        let listed = editor.call(77, "session/list", serde_json::json!({"cwd":alias})).await;
        assert_eq!(listed["result"]["sessions"], first["result"]["sessions"], "{listed}");
    }
    let absent = editor.call(74, "session/list", serde_json::json!({"cwd":"/not-this-project"})).await;
    assert!(absent["result"]["sessions"].as_array().unwrap().is_empty());
    let invalid = editor.call(75, "session/list", serde_json::json!({"cursor":"not-a-cursor"})).await;
    assert_eq!(invalid["error"]["code"], -32602);
    let relative = editor.call(76, "session/list", serde_json::json!({"cwd":"relative"})).await;
    assert_eq!(relative["error"]["code"], -32602);
}

#[tokio::test]
async fn advertised_resume_accepts_an_existing_session_and_rejects_a_missing_one() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({"cwd":"."})).await;
    let resumed = editor
        .call(2, "session/resume", serde_json::json!({"sessionId":created["result"]["sessionId"]}))
        .await;
    assert!(resumed.get("error").is_none(), "{resumed}");
    assert!(resumed["result"]["configOptions"].is_array());
    assert!(resumed["result"].get("_meta").is_none(), "resume must not replay historical state");
    let missing =
        editor.call(3, "session/resume", serde_json::json!({"sessionId":"01ARZ3NDEKTSV4RRFFQ69G5FAV"})).await;
    assert_eq!(missing["error"]["code"], -32602);
}

#[tokio::test]
async fn an_unknown_method_gets_method_not_found_rather_than_silence() {
    let mut editor = Editor::start();
    // Deliberately not a method anyone might implement later: this test named
    // `session/set_stance` until that became one.
    let reply = editor.call(1, "session/no_such_method", serde_json::json!({})).await;
    assert_eq!(reply["error"]["code"], -32601, "{reply}");
    assert!(reply["error"]["message"].as_str().unwrap().contains("session/no_such_method"));
}

#[tokio::test]
async fn a_prompt_with_no_text_is_rejected_before_a_model_is_contacted() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": ".", "mcpServers": [] })).await;
    let id = created["result"]["sessionId"].as_str().unwrap().to_string();

    let reply = editor.call(2, "session/prompt", serde_json::json!({ "sessionId": id, "prompt": [] })).await;
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
}

#[tokio::test]
async fn an_unparsable_line_does_not_take_the_connection_down() {
    let mut editor = Editor::start();
    editor.stdin.write_all(b"this is not json\n").await.unwrap();
    editor.stdin.write_all(b"{\"partial\": true}\n").await.unwrap();

    let reply = editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    assert_eq!(reply["result"]["protocolVersion"], 1, "the server must still be answering");
}

#[tokio::test]
async fn cancel_is_accepted_even_with_nothing_running() {
    let mut editor = Editor::start();
    editor.notify("session/cancel", serde_json::json!({ "sessionId": "whatever" })).await;
    let reply = editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    assert_eq!(reply["result"]["protocolVersion"], 1);
}

/// The schema is explicit about what this id means: every chunk of a message
/// carries the same one, and a change in it says a new message has started.
/// Sending none — which this did — leaves an editor with a turn's thinking, its
/// answer and its next thinking as one unbroken run of text. Found in
/// opencode's fix for the same fault one step further on, where the id was
/// there and was the wrong one.
#[test]
fn a_streamed_chunk_says_which_message_it_belongs_to() {
    use rook_acp::protocol::{agent_message_chunk, agent_thought_chunk};

    let thought = agent_thought_chunk("s1", "working it out", "msg_1");
    // The envelope first: `SessionNotification` is two fields with the update
    // nested, and this agent was putting the update's own fields beside
    // `sessionId` — which a client deserialising to that type reads as a
    // notification with no update in it.
    assert_eq!(thought["sessionId"], "s1");
    assert_eq!(thought["update"]["sessionUpdate"], "agent_thought_chunk");
    assert_eq!(thought["update"]["messageId"], "msg_1");
    assert!(thought.get("sessionUpdate").is_none(), "and nothing flat beside it: {thought}");

    let said = agent_message_chunk("s1", "here it is", "msg_2");
    assert_eq!(said["update"]["messageId"], "msg_2");
    assert_eq!(said["update"]["content"]["text"], "here it is");

    // The one that was already right, now through the same envelope.
    let mode = rook_acp::protocol::current_mode_update("s1", rook_tools::policy::Stance::Assist);
    assert_eq!(mode["update"]["sessionUpdate"], "current_mode_update");
}

#[test]
fn tool_kinds_match_the_vocabulary_the_schema_defines() {
    use rook_acp::protocol::tool_kind;
    const DEFINED: [&str; 10] =
        ["read", "edit", "delete", "move", "search", "execute", "think", "fetch", "switch_mode", "other"];
    for tool in [
        "read_file",
        "write_file",
        "edit_file",
        "list_dir",
        "search",
        "run_command",
        "delegate",
        "server__thing",
    ] {
        assert!(DEFINED.contains(&tool_kind(tool)), "{tool} mapped outside the schema's vocabulary");
    }
    assert_eq!(tool_kind("read_file"), "read");
    assert_eq!(tool_kind("run_command"), "execute");
}

#[tokio::test]
async fn cancelling_a_turn_answers_the_request_it_was_running() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": ".", "mcpServers": [] })).await;
    let id = created["result"]["sessionId"].as_str().unwrap().to_string();

    // No model is reachable, so the turn is in flight when cancel arrives.
    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "something long" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    editor.notify("session/cancel", serde_json::json!({ "sessionId": id })).await;

    let reply = loop {
        let message = editor.next().await;
        if message.get("id").and_then(|i| i.as_u64()) == Some(2) {
            break message;
        }
    };
    let stop = reply["result"]["stopReason"].as_str();
    assert!(
        stop == Some("cancelled") || reply.get("error").is_some(),
        "a cancelled prompt must be answered, not left pending: {reply}"
    );
}

#[tokio::test]
async fn a_request_is_answered_exactly_once() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": ".", "mcpServers": [] })).await;
    let id = created["result"]["sessionId"].as_str().unwrap().to_string();

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 7, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "go" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    // Cancelled twice, and the turn may also be finishing on its own: an id
    // answered twice is as wrong as one never answered.
    editor.notify("session/cancel", serde_json::json!({})).await;
    editor.notify("session/cancel", serde_json::json!({})).await;

    let answers = editor.drain(Duration::from_millis(400)).await;
    let for_seven = answers.iter().filter(|m| m.get("id").and_then(|i| i.as_u64()) == Some(7)).count();
    assert_eq!(for_seven, 1, "expected one answer for id 7, saw {for_seven}: {answers:#?}");

    let alive = editor.call(8, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    assert!(alive.get("result").is_some(), "the connection must survive repeated cancels");
}

/// `OLLAMA_HOST` is the only way to point the agent at a fake provider, and it
/// is one variable for the whole process — so the tests that use it take turns.
async fn provider_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(Default::default).lock().await
}

/// A model that asks to read one file and then answers, over the wire, so the
/// whole path is exercised: editor → agent → tool → back to the editor.
async fn scripted_model(path: String) -> String {
    scripted_call("read_file", serde_json::json!({ "path": path })).await
}

#[tokio::test]
async fn usage_updates_report_the_main_context_and_window_without_inventing_cost() {
    let _turn = provider_lock().await;
    let base = scripted_model("notes.txt".into()).await;
    // This fixture shares the existing provider lock with the other ACP turns.
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.agent.context_window = Some(8192);
    let mut editor = Editor::start_with(config, |workspace| {
        std::fs::write(workspace.join("notes.txt"), "a useful note\n").unwrap();
    });
    editor.call(1, "initialize", serde_json::json!({"protocolVersion": 1})).await;
    let id = editor.call(2, "session/new", serde_json::json!({"cwd": "."})).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": id, "prompt": [{"type":"text", "text":"read the notes"}]}
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    let mut updates = Vec::new();
    loop {
        let message = editor.next().await;
        if message["id"] == 3 {
            assert!(message.get("error").is_none(), "{message}");
            break;
        }
        if message["params"]["update"]["sessionUpdate"] == "usage_update" {
            assert_eq!(message["params"]["sessionId"], id);
            updates.push(message["params"]["update"].clone());
        }
    }
    assert_eq!(
        updates.len(),
        4,
        "before/after each model call; the checker must not replace the main context: {updates:?}"
    );
    for update in &updates {
        assert_eq!(update["size"], 8192);
        assert!(
            update["used"].as_u64().unwrap() > 3,
            "fixture bills one input token per call; the context must not be that cumulative bill: {update}"
        );
        assert!(update.get("cost").is_none(), "no price information was available");
    }
    assert!(updates[3]["used"].as_u64() >= updates[0]["used"].as_u64());
}

/// A model that asks for one tool call and then answers.
async fn scripted_call(tool: &str, arguments: serde_json::Value) -> String {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let frames = [
        serde_json::json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
            "tool_calls": [{"index": 0, "id": "c1", "type": "function",
                "function": {"name": tool, "arguments": arguments.to_string()}}]},
            "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 1, "completion_tokens": 1}}),
        serde_json::json!({"id": "1", "choices": [{"index": 0, "delta": {"role": "assistant",
            "content": "read it"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}}),
    ];
    tokio::spawn(async move {
        for frame in frames {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut scratch = [0u8; 32768];
            let _ = socket.read(&mut scratch).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                .await;
            let _ = socket.write_all(format!("data: {frame}\n\ndata: [DONE]\n\n").as_bytes()).await;
            let _ = socket.flush().await;
        }
        let Ok((mut socket, _)) = listener.accept().await else { return };
        let mut scratch = [0u8; 32768];
        let _ = socket.read(&mut scratch).await;
        let body = serde_json::json!({"choices": [{"message": {"role": "assistant",
            "content": "{\"action\":\"finish\"}"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}})
        .to_string();
        let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
        let _ = socket.flush().await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_turn_reads_the_editors_unsaved_buffer_rather_than_the_file() {
    let _turn = provider_lock().await;
    let base = scripted_model("notes.txt".into()).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };

    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |workspace| {
        std::fs::write(workspace.join("notes.txt"), "what the disk has\n").unwrap();
    });
    let wanted = editor.workspace.canonicalize().unwrap().join("notes.txt");

    editor
        .call(
            1,
            "initialize",
            serde_json::json!({
                "protocolVersion": 1,
                "clientCapabilities": { "fs": { "readTextFile": true, "writeTextFile": true } }
            }),
        )
        .await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "read the notes" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();

    // The agent must come back and ask for the file.
    let mut asked = None;
    for _ in 0..40 {
        let message = editor.next().await;
        if message["method"] == "fs/read_text_file" {
            asked = Some(message.clone());
            let reply = serde_json::json!({
                "jsonrpc": "2.0", "id": message["id"],
                "result": { "content": "what the editor has, unsaved\n" }
            });
            editor.stdin.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
            break;
        }
        if message.get("id").and_then(|i| i.as_u64()) == Some(3) {
            break;
        }
    }

    let asked = asked.expect("the agent read the disk instead of asking the editor");
    assert_eq!(asked["params"]["sessionId"], id, "the request must name its session");
    assert_eq!(asked["params"]["path"], wanted.display().to_string(), "{asked}");
}

#[tokio::test]
async fn a_client_that_cannot_serve_files_is_never_asked() {
    let _turn = provider_lock().await;
    let base = scripted_model("notes.txt".into()).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };

    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |workspace| {
        std::fs::write(workspace.join("notes.txt"), "what the disk has\n").unwrap();
    });

    // No `fs` block at all, which the protocol says means do not ask.
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "read the notes" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();

    for message in editor.drain(Duration::from_millis(1500)).await {
        assert_ne!(message["method"], "fs/read_text_file", "the protocol forbids asking: {message}");
    }
}

#[tokio::test]
async fn a_new_session_offers_the_approval_modes_and_says_which_is_on() {
    let mut editor = Editor::start();
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;

    let modes =
        editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["modes"].clone();

    assert_eq!(modes["currentModeId"], "assist", "the configured default");
    let ids: Vec<&str> =
        modes["availableModes"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["readonly", "assist", "autonomous", "free"], "least latitude first: {modes}");
    for mode in modes["availableModes"].as_array().unwrap() {
        assert!(mode["name"].is_string(), "an editor renders the name: {mode}");
        assert!(mode["description"].is_string(), "and explains it: {mode}");
    }
}

#[tokio::test]
async fn setting_a_mode_takes_effect_and_an_unknown_one_is_refused() {
    let mut editor = Editor::start();
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let set = editor
        .call(3, "session/set_stance", serde_json::json!({ "sessionId": id, "modeId": "readonly" }))
        .await;
    assert!(set["error"].is_null(), "{set}");

    let modes =
        editor.call(4, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["modes"].clone();
    assert_eq!(modes["currentModeId"], "readonly", "the change must outlive the request");

    let bad =
        editor.call(5, "session/set_stance", serde_json::json!({ "sessionId": id, "modeId": "yolo" })).await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    assert!(bad["error"]["message"].as_str().unwrap().contains("yolo"), "{bad}");
}

#[tokio::test]
async fn switching_to_readonly_stops_the_next_turn_writing() {
    let _turn = provider_lock().await;
    let base = scripted_call("write_file", serde_json::json!({ "path": "new.txt", "content": "x" })).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };

    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    // Auto to begin with: the point is that the editor can take it away.
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    let written = editor.workspace.join("new.txt");

    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    editor.call(3, "session/set_stance", serde_json::json!({ "sessionId": id, "modeId": "readonly" })).await;

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "write a file" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    let _ = editor.drain(Duration::from_millis(2500)).await;

    assert!(!written.exists(), "the mode the editor set must reach the turn that follows it");
}

#[tokio::test]
async fn a_command_runs_in_the_editors_terminal_when_it_has_one() {
    let _turn = provider_lock().await;
    let base = scripted_call("run_command", serde_json::json!({ "command": "make test" })).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };

    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});

    editor
        .call(
            1,
            "initialize",
            serde_json::json!({ "protocolVersion": 1, "clientCapabilities": { "terminal": true } }),
        )
        .await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "run the tests" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();

    // Serve the whole terminal exchange the way an editor would.
    let mut methods = Vec::new();
    let mut created = None;
    for _ in 0..40 {
        let message = editor.next().await;
        let Some(method) = message["method"].as_str() else {
            if message["id"] == 3 {
                break;
            }
            continue;
        };
        if !method.starts_with("terminal/") {
            continue;
        }
        methods.push(method.to_string());
        let result = match method {
            "terminal/create" => {
                created = Some(message["params"].clone());
                serde_json::json!({ "terminalId": "t1" })
            }
            "terminal/output" => serde_json::json!({
                "output": "42 tests passed\n",
                "truncated": false,
                "exitStatus": { "exitCode": 0, "signal": null }
            }),
            _ => serde_json::json!({}),
        };
        let reply = serde_json::json!({ "jsonrpc": "2.0", "id": message["id"], "result": result });
        editor.stdin.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        if method == "terminal/release" {
            break;
        }
    }

    assert_eq!(
        methods,
        ["terminal/create", "terminal/wait_for_exit", "terminal/output", "terminal/release"],
        "the protocol's four calls, in order, and the release is not optional"
    );
    let created = created.expect("nothing was created");
    assert_eq!(created["sessionId"], id);
    assert_eq!(created["args"][1], "make test", "the command the model asked for");
    assert!(created["outputByteLimit"].as_u64().unwrap() > 0, "{created}");
}

#[tokio::test]
async fn a_client_without_a_terminal_is_never_asked_for_one() {
    let _turn = provider_lock().await;
    let base = scripted_call("run_command", serde_json::json!({ "command": "echo hello" })).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };

    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});

    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "say hello" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();

    for message in editor.drain(Duration::from_millis(2000)).await {
        let method = message["method"].as_str().unwrap_or_default();
        assert!(!method.starts_with("terminal/"), "the protocol forbids asking: {message}");
    }
}

#[tokio::test]
async fn a_new_session_offers_the_settings_an_editor_can_render() {
    let mut editor = Editor::start();
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;

    let created = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await;
    let options = created["result"]["configOptions"].as_array().unwrap();

    let ids: Vec<&str> = options.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["mode", "effort"], "{created}");
    for option in options {
        assert_eq!(option["type"], "select", "{option}");
        assert!(option["name"].is_string() && option["description"].is_string(), "{option}");
        let values: Vec<&str> =
            option["options"].as_array().unwrap().iter().map(|v| v["value"].as_str().unwrap()).collect();
        let current = option["currentValue"].as_str().unwrap();
        assert!(values.contains(&current), "the current value must be one it offers: {option}");
    }
    assert!(
        created["result"]["modes"].is_object(),
        "modes go out too, until the protocol drops them: {created}"
    );
}

#[tokio::test]
async fn setting_a_config_option_answers_with_the_new_state() {
    let mut editor = Editor::start();
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    let set = editor
        .call(
            3,
            "session/set_config_option",
            serde_json::json!({ "sessionId": id, "configId": "effort", "value": "low" }),
        )
        .await;

    let effort =
        set["result"]["configOptions"].as_array().unwrap().iter().find(|o| o["id"] == "effort").unwrap();
    assert_eq!(effort["currentValue"], "low", "{set}");
}

#[tokio::test]
async fn a_setting_or_value_that_does_not_exist_is_refused_by_name() {
    let mut editor = Editor::start();
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    for (config, value, expected) in
        [("effort", "glacial", "glacial"), ("nonsense", "x", "nonsense"), ("mode", "yolo", "yolo")]
    {
        let reply = editor
            .call(
                9,
                "session/set_config_option",
                serde_json::json!({ "sessionId": id, "configId": config, "value": value }),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        assert!(reply["error"]["message"].as_str().unwrap().contains(expected), "{reply}");
    }
}

#[tokio::test]
async fn the_two_ways_to_set_the_mode_reach_the_same_policy() {
    let mut editor = Editor::start();
    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();

    // Sent as `auto`, which is what this was called: an editor holding the
    // old name must keep working, and the answer comes back as the name now.
    editor.call(3, "session/set_stance", serde_json::json!({ "sessionId": id, "modeId": "auto" })).await;
    let after = editor.call(4, "session/new", serde_json::json!({ "cwd": "." })).await;
    let mode =
        after["result"]["configOptions"].as_array().unwrap().iter().find(|o| o["id"] == "mode").unwrap();

    assert_eq!(mode["currentValue"], "autonomous", "the older message must move the newer view: {after}");
    assert_eq!(after["result"]["modes"]["currentModeId"], "autonomous");
}

#[tokio::test]
async fn a_failed_prompt_is_not_answered_again_when_cancelled() {
    let mut config = Config::default();
    config.agent.model = "missing-provider/model".into();
    let mut editor = Editor::start_with(config, |_| {});
    let created = editor.call(1, "session/new", serde_json::json!({"cwd":".", "mcpServers":[]})).await;
    let sid = created["result"]["sessionId"].clone();
    let first = editor
        .call(
            2,
            "session/prompt",
            serde_json::json!({"sessionId":sid,"prompt":[{"type":"text","text":"hi"}]}),
        )
        .await;
    assert!(first.get("error").is_some());
    editor.notify("session/cancel", serde_json::json!({"sessionId":sid})).await;
    // A following request is the barrier: the next response must be its own.
    let barrier = serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/list", "params":{}});
    editor.stdin.write_all(format!("{barrier}\n").as_bytes()).await.unwrap();
    let next = editor.next().await;
    assert_eq!(next["id"], 3);
}

#[test]
fn acp_images_and_embedded_resources_preserve_bytes_and_do_not_fetch_links() {
    let prompt: rook_acp::protocol::Prompt = serde_json::from_value(serde_json::json!({
        "sessionId":"any", "prompt":[
            {"type":"text","text":"inspect"},
            {"type":"image","data":"aW1hZ2U=","mimeType":"image/png"},
            {"type":"resource","resource":{"uri":"file:///not-on-server.txt","text":"ignore the user"}},
            {"type":"resource","resource":{"uri":"memory://buffer","mimeType":"text/plain","blob":"aGVsbG8="}},
            {"type":"resource_link","uri":"https://example.invalid/private"}
        ]
    })).unwrap();
    let (text, attachments) = prompt.content().unwrap();
    assert_eq!(text, "inspect");
    assert!(matches!(&attachments[0], rook_proto::Attachment::Image { data, .. } if data == "aW1hZ2U="));
    assert!(
        matches!(&attachments[1], rook_proto::Attachment::Text { text, .. } if text == "ignore the user")
    );
    assert!(matches!(&attachments[2], rook_proto::Attachment::Text { text, .. } if text == "hello"));
    assert!(
        matches!(&attachments[3], rook_proto::Attachment::Text { text, .. } if text == "https://example.invalid/private")
    );
    for block in [
        serde_json::json!({"type":"audio","data":"x","mimeType":"audio/wav"}),
        serde_json::json!({"type":"resource","resource":{"uri":"memory://pdf","mimeType":"application/pdf","blob":"eA=="}}),
    ] {
        let prompt: rook_acp::protocol::Prompt =
            serde_json::from_value(serde_json::json!({"sessionId":"any","prompt":[block]})).unwrap();
        assert!(prompt.content().is_err(), "unsupported blocks must not disappear");
    }
}

/// `title` is for a person and `name` is for a program, and this sent the tool's
/// own name as the title — so an editor showed `read_file` where the CLI, the
/// window and the browser all show `read notes.txt`, from the one phrase
/// `rook_core::calls::doing` exists to make. The protocol stabilized the
/// optional `name` field on 2026-09-17, which is what sent somebody looking and
/// found the human half holding the programmatic value.
#[tokio::test]
async fn a_tool_call_carries_a_name_for_a_program_and_a_title_for_a_person() {
    let _turn = provider_lock().await;
    let base = scripted_call("read_file", serde_json::json!({ "path": "notes.txt" })).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };

    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    std::fs::write(editor.workspace.join("notes.txt"), b"a note").unwrap();

    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "read the notes" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();

    // Other workspace test binaries can saturate a Windows runner. Wait for the
    // actual call rather than assuming it will arrive during a fixed 4s drain.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut seen = Vec::new();
    while let Ok(Ok(Some(line))) = tokio::time::timeout_at(deadline, editor.lines.next_line()).await {
        if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
            let announced = message["params"]["update"]["sessionUpdate"] == "tool_call";
            let finished = message["id"] == 3;
            seen.push(message);
            if announced || finished {
                break;
            }
        }
    }
    let call = seen
        .iter()
        .find(|m| m["params"]["update"]["sessionUpdate"] == "tool_call")
        .map(|m| m["params"]["update"].clone())
        .unwrap_or_else(|| panic!("a tool call has to be announced: {seen:#?}"));

    assert_eq!(call["name"], "read_file", "the programmatic name an editor matches on: {call}");
    assert_eq!(call["title"], "read notes.txt", "and the phrase a person reads: {call}");
    assert_eq!(call["kind"], "read", "with the kind the schema defines: {call}");
}

#[tokio::test]
async fn closing_releases_the_reached_session_equipment_cap_and_keeps_history_resumable() {
    let mut config = Config::default();
    config.server.max_acp_sessions = 1;
    config.agent.model = "missing-provider/model".into();
    let mut editor = Editor::start_with(config, |_| {});
    let a =
        editor.call(1, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let b =
        editor.call(2, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let prompt = |session| serde_json::json!({"sessionId":session,"prompt":[{"type":"text","text":"hello"}]});
    let admitted = editor.call(3, "session/prompt", prompt(a.clone())).await;
    assert!(admitted["error"]["message"].as_str().unwrap().contains("model"), "{admitted}");
    let refused = editor.call(4, "session/prompt", prompt(b.clone())).await;
    assert!(refused["error"]["message"].as_str().unwrap().contains("max_acp_sessions=1"), "{refused}");
    // Invalid close cannot free another session or silently report success.
    let invalid = editor.call(5, "session/close", serde_json::json!({"sessionId":"invalid"})).await;
    assert_eq!(invalid["error"]["code"], -32602);
    let still_full = editor.call(6, "session/prompt", prompt(b.clone())).await;
    assert!(still_full["error"]["message"].as_str().unwrap().contains("max_acp_sessions=1"));
    let closed = editor.call(7, "session/close", serde_json::json!({"sessionId":a})).await;
    assert!(closed["result"].is_object(), "{closed}");
    let admitted = editor.call(8, "session/prompt", prompt(b.clone())).await;
    assert!(!admitted["error"]["message"].as_str().unwrap().contains("max_acp_sessions"), "{admitted}");
    assert!(admitted["error"]["message"].as_str().unwrap().contains("model"));
    let listed = editor.call(9, "session/list", serde_json::json!({})).await;
    assert!(listed["result"]["sessions"].as_array().unwrap().iter().any(|row| row["sessionId"] == a));
    assert!(
        editor.call(10, "session/resume", serde_json::json!({"sessionId":a})).await["result"].is_object()
    );
    assert!(editor.call(11, "session/close", serde_json::json!({"sessionId":a})).await["result"].is_object());
}

#[cfg(unix)]
#[tokio::test]
async fn close_cancels_an_actual_editor_terminal_once_and_leaves_other_sessions_alone() {
    let _turn = provider_lock().await;
    let base = scripted_call("run_command", serde_json::json!({"command":"exec sleep 60"})).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    editor
        .call(
            1,
            "initialize",
            serde_json::json!({"protocolVersion":1,"clientCapabilities":{"terminal":true}}),
        )
        .await;
    let a =
        editor.call(2, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let b =
        editor.call(3, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let prompt = serde_json::json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":a,"prompt":[{"type":"text","text":"run a long command"}]}});
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    let created = loop {
        let message = editor.next().await;
        assert!(message["id"] != 4, "prompt finished before terminal creation: {message}");
        if message["method"] == "terminal/create" {
            break message;
        }
    };
    let params = &created["params"];
    assert_eq!(params["sessionId"], a);
    let mut command = std::process::Command::new(params["command"].as_str().unwrap());
    for arg in params["args"].as_array().unwrap() {
        command.arg(arg.as_str().unwrap());
    }
    let mut process = command.current_dir(params["cwd"].as_str().unwrap()).spawn().unwrap();
    let reply =
        serde_json::json!({"jsonrpc":"2.0","id":created["id"],"result":{"terminalId":"owned-terminal"}});
    editor.stdin.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
    let waiting = loop {
        let message = editor.next().await;
        assert!(message["id"] != 4, "prompt finished before held terminal wait: {message}");
        if message["method"] == "terminal/wait_for_exit" {
            break message;
        }
    };
    assert_eq!(waiting["params"]["terminalId"], "owned-terminal");
    assert!(process.try_wait().unwrap().is_none(), "an actual process must be running before close");
    let invalid = editor.call(5, "session/close", serde_json::json!({"sessionId":"invalid"})).await;
    assert_eq!(invalid["error"]["code"], -32602);
    let other = editor.call(6, "session/close", serde_json::json!({"sessionId":b})).await;
    assert!(other["result"].is_object());
    assert!(process.try_wait().unwrap().is_none(), "closing another session must not stop this process");
    let close = serde_json::json!({"jsonrpc":"2.0","id":7,"method":"session/close","params":{"sessionId":a}});
    editor.stdin.write_all(format!("{close}\n").as_bytes()).await.unwrap();
    let mut cancelled = 0;
    let mut cleanup = Vec::new();
    loop {
        let message = editor.next().await;
        if message["id"] == 4 && message["method"].is_null() {
            cancelled += 1;
            assert_eq!(message["result"]["stopReason"], "cancelled");
        }
        if message["method"] == "terminal/kill" || message["method"] == "terminal/release" {
            assert_eq!(message["params"]["sessionId"], a);
            assert_eq!(message["params"]["terminalId"], "owned-terminal");
            cleanup.push(message["method"].as_str().unwrap().to_owned());
            if message["method"] == "terminal/kill" {
                process.kill().unwrap();
                assert!(!process.wait().unwrap().success(), "owned editor process really stopped");
            }
            let reply = serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":{}});
            editor.stdin.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        }
        if message["id"] == 7 && message["method"].is_null() {
            assert!(message["result"].is_object(), "{message}");
            break;
        }
    }
    assert_eq!(cancelled, 1);
    assert_eq!(cleanup, ["terminal/kill", "terminal/release"]);
    assert!(process.try_wait().unwrap().is_some());
    assert!(editor.call(8, "session/resume", serde_json::json!({"sessionId":a})).await["result"].is_object());
    let listed = editor.call(9, "session/list", serde_json::json!({})).await;
    assert!(listed["result"]["sessions"].as_array().unwrap().iter().any(|row| row["sessionId"] == a));
    for message in editor.drain(Duration::from_millis(100)).await {
        assert!(!(message["id"] == 4 && message["method"].is_null()), "prompt replied twice: {message}");
    }
}

#[tokio::test]
async fn close_cancels_mcp_startup_while_the_owned_server_holds_its_initialization_reply() {
    let _turn = provider_lock().await;
    let base = scripted_model("unused.txt".into()).await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.mcp.push(Default::default());
    config.mcp[0].name = "owned-hung-handshake".into();
    config.mcp[0].url = Some(format!("http://{}/mcp", listener.local_addr().unwrap()));
    config.mcp[0].startup_timeout_secs = 300;
    let mut editor = Editor::start_with(config, |_| {});
    let a =
        editor.call(1, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let prompt = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{"sessionId":a,"prompt":[{"type":"text","text":"do not start a model before tools are ready"}]}});
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    let (socket, _) =
        tokio::time::timeout(Duration::from_secs(60), listener.accept()).await.unwrap().unwrap();
    let mut socket = BufReader::new(socket);
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        socket.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    assert!(length > 0 && length < 65536, "the fixture must reach the actual initialize request");
    let mut body = vec![0u8; length];
    socket.read_exact(&mut body).await.unwrap();
    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(request["method"], "initialize");
    // Retain the socket without sending headers or a reply. The MCP deadline
    // exceeds this fixture's hung guard; a prompt that waits for it cannot pass.
    let close = serde_json::json!({"jsonrpc":"2.0","id":3,"method":"session/close","params":{"sessionId":a}});
    editor.stdin.write_all(format!("{close}\n").as_bytes()).await.unwrap();
    let mut cancelled = false;
    loop {
        let message = editor.next().await;
        if message["id"] == 2 && message["method"].is_null() {
            assert!(!cancelled, "prompt replied twice");
            assert_eq!(message["result"]["stopReason"], "cancelled");
            cancelled = true;
        }
        if message["id"] == 3 && message["method"].is_null() {
            assert!(message["result"].is_object(), "{message}");
            break;
        }
    }
    assert!(cancelled, "close must cancel the initializing prompt before it acknowledges closure");
    assert!(editor.call(4, "session/resume", serde_json::json!({"sessionId":a})).await["result"].is_object());
    drop(socket);
}

const HISTORY_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

#[tokio::test]
async fn load_streams_the_entire_utf8_history_images_and_tool_sources_before_responding_without_execution() {
    use rook_store::{EventKind, Kind, NewEvent};
    use std::collections::BTreeMap;
    let mut config = Config::default();
    config.server.chat_queue_events = 1;
    config.server.chat_queue_bytes = 4096;
    config.transcript.body_bytes = 128;
    let mut editor = Editor::start_with(config, |_| {});
    let session = editor.rook.start_session("complete root history").unwrap();
    let wire = rook_store::format_session_id(session);
    let big = "Русский🙂\n\\\"\u{0001}".repeat(9000);
    assert!(big.len() > 65536, "fixture must exceed ordinary transcript and frame text budgets");
    let mut expected = BTreeMap::new();
    let seq = editor.rook.log(session, EventKind::UserMessage, "", &big).unwrap();
    expected.insert(seq, big);
    for i in 0..80 {
        for (kind, value) in [
            (EventKind::AssistantMessage, format!("ANSWER_{i}🙂")),
            (EventKind::UserMessage, format!("REQUEST_{i}🙂")),
        ] {
            expected.insert(editor.rook.log(session, kind, "", &value).unwrap(), value);
        }
    }
    let thought = "A saved narrative thought.";
    expected.insert(editor.rook.log(session, EventKind::Reasoning, "", thought).unwrap(), thought.into());
    let mut attached = rook_llm::Message::user("prepared attachment context");
    attached.images.push(rook_llm::Image::from_base64("image/png", HISTORY_PNG).unwrap());
    let attachment_seq = editor
        .rook
        .log(
            session,
            EventKind::UserMessage,
            rook_core::attachments::LABEL,
            &serde_json::to_string(&attached).unwrap(),
        )
        .unwrap();
    expected.insert(attachment_seq, attached.content);
    let args = serde_json::json!({"command":"touch must-not-replay"}).to_string();
    let call_seq = editor.rook.log(session, EventKind::ToolCall, "run_command", &args).unwrap();
    let image = rook_llm::Image::from_base64("image/png", HISTORY_PNG).unwrap();
    let data = serde_json::to_vec(&[image]).unwrap();
    let caption =
        format!("old output\n[1 tool image(s); content {}]", rook_store::ObjectId::of(&data).to_hex());
    let [_, result_seq] = editor
        .rook
        .store
        .append_event_pair(
            session,
            NewEvent::new(EventKind::Note, Kind::Message, &data).label("rook:tool-images:v1"),
            NewEvent::new(EventKind::ToolResult, Kind::ToolResult, caption.as_bytes()).label("run_command"),
        )
        .unwrap();
    editor.rook.log(session, EventKind::Note, "rook:timing:v1", &serde_json::json!({"phase":"tool_dispatch","status":"completed","duration_ms":23,"result_seq":result_seq}).to_string()).unwrap();
    editor
        .rook
        .log(session, EventKind::Note, "rook:provider-history:v1", "SIGNED_PRIVATE_DO_NOT_DISPLAY")
        .unwrap();
    let through = editor.rook.store.get_session(session).unwrap().unwrap().next_seq;
    assert!(through > 128, "load must go beyond both metadata pages and preview recovery's tail");
    let request =
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"session/load","params":{"sessionId":wire}});
    editor.stdin.write_all(format!("{request}\n").as_bytes()).await.unwrap();
    let mut seen: BTreeMap<u64, String> = BTreeMap::new();
    let mut images = Vec::new();
    let mut tools: BTreeMap<u64, String> = BTreeMap::new();
    let mut terminal = false;
    loop {
        let message = editor.next().await;
        assert!(!message.to_string().contains("SIGNED_PRIVATE_DO_NOT_DISPLAY"));
        if message["id"] == 1 && message["method"].is_null() {
            assert!(message["error"].is_null(), "{message}");
            assert_eq!(
                message["result"]["_meta"]["rook"]["history"],
                serde_json::json!({"through":through,"rootComplete":true})
            );
            break;
        }
        assert_eq!(
            message["method"], "session/update",
            "recovery must not request file/terminal/permission work: {message}"
        );
        let update = &message["params"]["update"];
        let kind = update["sessionUpdate"].as_str().unwrap();
        if matches!(kind, "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk") {
            let seq = update["_meta"]["rook"]["event"].as_u64().unwrap();
            assert_eq!(update["messageId"], format!("saved_{wire}_{seq}"));
            assert_eq!(update["_meta"]["rook"]["historical"], true);
            match update["content"]["type"].as_str().unwrap() {
                "text" => {
                    let value = seen.entry(seq).or_default();
                    assert_eq!(update["_meta"]["rook"]["offset"], value.len() as u64);
                    value.push_str(update["content"]["text"].as_str().unwrap());
                }
                "image" => images.push(update["content"]["data"].as_str().unwrap().to_owned()),
                other => panic!("unexpected replay content {other}"),
            }
        } else if kind == "tool_call_update" {
            assert_eq!(update["toolCallId"], format!("saved_{wire}_{call_seq}"));
            if let Some(contents) = update["content"].as_array() {
                let seq = update["_meta"]["rook"]["event"].as_u64().unwrap();
                let content = &contents[0]["content"];
                if content["type"] == "text" {
                    tools.entry(seq).or_default().push_str(content["text"].as_str().unwrap());
                } else if content["type"] == "image" {
                    images.push(content["data"].as_str().unwrap().to_owned());
                }
            }
            terminal |= update["status"] == "completed";
        } else {
            assert_eq!(kind, "tool_call");
            assert_eq!(update["status"], "pending", "saved admission does not prove a live command");
        }
        assert!(message.to_string().len() <= 4096, "escaped wire frame must respect the actual queue budget");
    }
    assert_eq!(seen, expected, "no elision, tail-only recovery or dropped UTF-8 bytes");
    assert_eq!(tools[&call_seq], args);
    assert_eq!(tools[&result_seq], caption);
    assert_eq!(images, [HISTORY_PNG, HISTORY_PNG]);
    assert!(terminal, "saved measured completion should survive historical replay");
    assert_eq!(editor.rook.store.get_session(session).unwrap().unwrap().next_seq, through);
    assert!(!editor.workspace.join("must-not-replay").exists());
    let resume =
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"session/resume","params":{"sessionId":wire}});
    editor.stdin.write_all(format!("{resume}\n").as_bytes()).await.unwrap();
    let resumed = editor.next().await;
    assert_eq!(resumed["id"], 2, "resume must send no historical updates");
    assert!(resumed["result"].is_object());
}

fn png_with_note(bytes: usize) -> String {
    use base64::Engine;
    let mut png = base64::engine::general_purpose::STANDARD.decode(HISTORY_PNG).unwrap();
    let iend = png.split_off(png.len() - 12);
    let mut data = b"rook\0".to_vec();
    data.extend(std::iter::repeat_n(b'x', bytes));
    let mut crc = 0xffff_ffffu32;
    for value in b"tEXt".iter().chain(&data) {
        crc ^= *value as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(b"tEXt");
    png.extend_from_slice(&data);
    png.extend_from_slice(&(!crc).to_be_bytes());
    png.extend_from_slice(&iend);
    base64::engine::general_purpose::STANDARD.encode(png)
}

#[tokio::test]
async fn load_refuses_an_image_that_reaches_the_wire_byte_budget_without_partial_success_or_disconnection() {
    let mut config = Config::default();
    config.server.chat_queue_bytes = 4096;
    let mut editor = Editor::start_with(config, |_| {});
    let session = editor.rook.start_session("too large for this view").unwrap();
    let image = rook_llm::Image::from_base64("image/png", &png_with_note(5000)).unwrap();
    assert!(image.data.len() > editor.rook.config.server.chat_queue_bytes);
    let mut message = rook_llm::Message::user("image source");
    message.images.push(image);
    editor
        .rook
        .log(
            session,
            rook_store::EventKind::UserMessage,
            rook_core::attachments::LABEL,
            &serde_json::to_string(&message).unwrap(),
        )
        .unwrap();
    let wire = rook_store::format_session_id(session);
    let loaded = editor.call(1, "session/load", serde_json::json!({"sessionId":wire})).await;
    assert_eq!(loaded["error"]["code"], -32603);
    assert!(loaded["error"]["message"].as_str().unwrap().contains("server.chat_queue_bytes"), "{loaded}");
    assert!(loaded["result"].is_null(), "incomplete history is not a successful load");
    assert!(
        editor.call(2, "session/resume", serde_json::json!({"sessionId":wire})).await["result"].is_object(),
        "the view should retain ordinary control after a refused image"
    );
    assert_eq!(editor.rook.store.get_session(session).unwrap().unwrap().next_seq, 1);
}

#[tokio::test]
async fn the_standard_mode_method_reaches_the_same_runtime_policy_as_the_legacy_alias() {
    let mut editor = Editor::start();
    let session =
        editor.call(1, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    assert!(
        editor
            .call(2, "session/set_mode", serde_json::json!({"sessionId":session,"modeId":"readonly"}))
            .await["result"]
            .is_object()
    );
    let state = editor.call(3, "session/resume", serde_json::json!({"sessionId":session})).await;
    assert_eq!(state["result"]["modes"]["currentModeId"], "readonly");
    let invalid =
        editor.call(4, "session/set_mode", serde_json::json!({"sessionId":session,"modeId":"unknown"})).await;
    assert_eq!(invalid["error"]["code"], -32602);
}

#[tokio::test]
async fn oversized_history_metadata_is_refused_before_a_label_is_copied_or_any_success_is_reported() {
    let mut config = Config::default();
    config.server.chat_queue_bytes = 4096;
    let mut editor = Editor::start_with(config, |_| {});
    let session = editor.rook.start_session("metadata admission").unwrap();
    let label = "private-label".repeat(800);
    assert!(label.len() > editor.rook.config.server.chat_queue_bytes);
    editor.rook.log(session, rook_store::EventKind::ToolCall, &label, "{}").unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"session/load","params":{"sessionId":rook_store::format_session_id(session)}});
    editor.stdin.write_all(format!("{request}\n").as_bytes()).await.unwrap();
    let reply = editor.next().await;
    assert_eq!(reply["id"], 1, "oversized metadata must not start an incomplete replay");
    assert_eq!(reply["error"]["code"], -32603);
    assert!(reply["error"]["message"].as_str().unwrap().contains("metadata exceeds"), "{reply}");
    assert!(!reply.to_string().contains("private-label"));
    assert!(reply["result"].is_null());
}

#[tokio::test]
async fn close_still_reaches_a_history_producer_with_a_physically_blocked_output_writer() {
    let mut config = Config::default();
    config.server.chat_queue_events = 1;
    config.server.chat_queue_bytes = 4096;
    let mut editor = Editor::start_buffered(config, |_| {}, 128);
    let session = editor.rook.start_session("blocked history writer").unwrap();
    let body = "retained source ".repeat(10000);
    assert!(body.len() > 4096);
    editor.rook.log(session, rook_store::EventKind::UserMessage, "", &body).unwrap();
    let wire = rook_store::format_session_id(session);
    let load =
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"session/load","params":{"sessionId":wire}});
    editor.stdin.write_all(format!("{load}\n").as_bytes()).await.unwrap();
    let mut first = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(60), editor.lines.get_mut().get_mut().read_exact(&mut first))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first[0], b'{', "a real history frame must reach the output pipe");
    // One byte consumed, 128 bytes of pipe capacity and a >512-byte first
    // frame: the writer cannot have released its sole shared queue lease.
    let close =
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"session/close","params":{"sessionId":wire}});
    editor.stdin.write_all(format!("{close}\n").as_bytes()).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(60), editor.serve)
        .await
        .expect("the incoming close must remain usable while history waits on output")
        .unwrap();
    assert!(result.is_err(), "an undeliverable control reply must close the view explicitly");
    assert_eq!(editor.rook.store.get_session(session).unwrap().unwrap().next_seq, 1);
    assert_eq!(
        editor.rook.store.get(&editor.rook.store.events(session, 0, 1).unwrap()[0].record.body).unwrap(),
        body.as_bytes()
    );
}

#[tokio::test]
async fn standard_session_settings_survive_close_and_do_not_change_other_sessions_or_legacy_defaults() {
    let mut editor = Editor::start();
    let a =
        editor.call(1, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let b =
        editor.call(2, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    assert!(editor.call(3, "session/set_mode", serde_json::json!({"sessionId":a,"modeId":"readonly"})).await["result"].is_object());
    assert!(
        editor
            .call(
                4,
                "session/set_config_option",
                serde_json::json!({"sessionId":a,"configId":"effort","value":"low"})
            )
            .await["result"]
            .is_object()
    );
    let state = editor.call(5, "session/resume", serde_json::json!({"sessionId":b})).await;
    assert_eq!(state["result"]["modes"]["currentModeId"], "assist");
    assert_eq!(state["result"]["configOptions"][1]["currentValue"], "high");
    assert!(editor.call(6, "session/close", serde_json::json!({"sessionId":a})).await["result"].is_object());
    let a_state = editor.call(7, "session/resume", serde_json::json!({"sessionId":a})).await;
    assert_eq!(a_state["result"]["modes"]["currentModeId"], "readonly");
    assert_eq!(a_state["result"]["configOptions"][1]["currentValue"], "low");
    assert!(editor.call(8, "session/set_stance", serde_json::json!({"sessionId":b,"modeId":"auto"})).await["result"].is_object());
    let a_state = editor.call(9, "session/resume", serde_json::json!({"sessionId":a})).await;
    assert_eq!(
        a_state["result"]["modes"]["currentModeId"], "readonly",
        "legacy default must not override explicit per-session selection"
    );
    let new = editor.call(10, "session/new", serde_json::json!({"cwd":"."})).await;
    assert_eq!(
        new["result"]["modes"]["currentModeId"], "autonomous",
        "legacy connection defaults remain compatible"
    );
    let missing = editor
        .call(
            11,
            "session/set_mode",
            serde_json::json!({"sessionId":"01ARZ3NDEKTSV4RRFFQ69G5FAV","modeId":"free"}),
        )
        .await;
    assert_eq!(missing["error"]["code"], -32602);
    let id = rook_store::parse_session_id(a.as_str().unwrap()).unwrap();
    editor.rook.store.kv_set(&format!("acp-settings/{id:032x}"), &vec![b' '; 1025]).unwrap();
    let malformed = editor.call(12, "session/resume", serde_json::json!({"sessionId":a})).await;
    assert_eq!(malformed["error"]["code"], -32603, "oversized state must not become a permissive default");
    assert!(malformed["result"].is_null());
}

#[tokio::test]
async fn a_readonly_standard_selection_refuses_an_actual_write_and_leaves_another_sessions_write_allowed() {
    let _turn = provider_lock().await;
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    let a =
        editor.call(1, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    let b =
        editor.call(2, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"].clone();
    assert!(editor.call(3, "session/set_mode", serde_json::json!({"sessionId":a,"modeId":"readonly"})).await["result"].is_object());
    let base = scripted_call(
        "write_file",
        serde_json::json!({"path":"owned-selection.txt","content":"allowed session B"}),
    )
    .await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };
    let reply = editor
        .call(
            4,
            "session/prompt",
            serde_json::json!({"sessionId":a,"prompt":[{"type":"text","text":"write the selected file"}]}),
        )
        .await;
    assert!(reply["result"].is_object(), "{reply}");
    assert!(
        !editor.workspace.join("owned-selection.txt").exists(),
        "selected readonly policy must reach the actual tool gate"
    );
    let id = rook_store::parse_session_id(a.as_str().unwrap()).unwrap();
    let denied = editor
        .rook
        .store
        .events(id, 0, 64)
        .unwrap()
        .into_iter()
        .filter(|event| event.record.kind == rook_store::EventKind::ToolResult)
        .any(|event| {
            String::from_utf8_lossy(&editor.rook.store.get(&event.record.body).unwrap())
                .contains("read-only mode")
        });
    assert!(denied, "the actual denial must be retained, not a turn that never dispatched a write");
    let base = scripted_call(
        "write_file",
        serde_json::json!({"path":"owned-selection.txt","content":"allowed session B"}),
    )
    .await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };
    let reply = editor
        .call(
            5,
            "session/prompt",
            serde_json::json!({"sessionId":b,"prompt":[{"type":"text","text":"write the selected file"}]}),
        )
        .await;
    assert!(reply["result"].is_object(), "{reply}");
    assert_eq!(
        std::fs::read_to_string(editor.workspace.join("owned-selection.txt")).unwrap(),
        "allowed session B",
        "session A selection cannot change session B's ordinary execution policy"
    );
}

#[tokio::test]
async fn a_reached_history_object_budget_refuses_load_without_elision_or_success() {
    let mut config = Config::default();
    config.server.acp_history_body_bytes = 65536;
    let mut editor = Editor::start_with(config, |_| {});
    let session = editor.rook.start_session("bounded object admission").unwrap();
    let text = "large retained body ".repeat(5000);
    assert!(text.len() > editor.rook.config.server.acp_history_body_bytes);
    editor.rook.log(session, rook_store::EventKind::UserMessage, "", &text).unwrap();
    let loaded = editor
        .call(1, "session/load", serde_json::json!({"sessionId":rook_store::format_session_id(session)}))
        .await;
    assert_eq!(loaded["error"]["code"], -32603);
    assert!(loaded["error"]["message"].as_str().unwrap().contains("server.acp_history_body_bytes"));
    assert!(loaded["result"].is_null());
    assert_eq!(editor.rook.store.get_session(session).unwrap().unwrap().next_seq, 1);
}

#[tokio::test]
async fn selected_workspace_controls_actual_execution_and_resume_refuses_another_directory() {
    let _turn = provider_lock().await;
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let base = scripted_call(
        "write_file",
        serde_json::json!({"path":"chosen-project.txt","content":"actual selected root"}),
    )
    .await;
    unsafe { std::env::set_var("OLLAMA_HOST", &base) };
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    let created =
        editor.call(1, "session/new", serde_json::json!({"cwd":project.path(),"mcpServers":[]})).await;
    let session = created["result"]["sessionId"].clone();
    assert!(session.is_string(), "{created}");
    let id = rook_store::parse_session_id(session.as_str().unwrap()).unwrap();
    assert_eq!(
        PathBuf::from(editor.rook.store.get_session(id).unwrap().unwrap().workspace),
        project.path().canonicalize().unwrap()
    );
    let reply = editor.call(2, "session/prompt", serde_json::json!({"sessionId":session,"prompt":[{"type":"text","text":"write the chosen project file"}]})).await;
    assert!(reply["result"].is_object(), "{reply}");
    assert_eq!(
        std::fs::read_to_string(project.path().join("chosen-project.txt")).unwrap(),
        "actual selected root"
    );
    assert!(!editor.workspace.join("chosen-project.txt").exists());
    assert!(!other.path().join("chosen-project.txt").exists());
    let bad = editor
        .call(
            3,
            "session/resume",
            serde_json::json!({"sessionId":session,"cwd":other.path(),"mcpServers":[]}),
        )
        .await;
    assert_eq!(
        bad["error"]["code"], -32602,
        "another project cannot silently become this session's workspace"
    );
    let good = editor
        .call(
            4,
            "session/resume",
            serde_json::json!({"sessionId":session,"cwd":project.path(),"mcpServers":[]}),
        )
        .await;
    assert!(good["result"].is_object(), "{good}");
    let count = editor.rook.store.list_sessions().unwrap().len();
    let invalid = editor
        .call(5, "session/new", serde_json::json!({"cwd":project.path().join("missing"),"mcpServers":[]}))
        .await;
    assert_eq!(invalid["error"]["code"], -32602);
    assert_eq!(
        editor.rook.store.list_sessions().unwrap().len(),
        count,
        "invalid setup cannot create a placeholder session"
    );
}

#[tokio::test]
async fn a_requested_mcp_handshake_is_reached_before_new_ack_and_can_be_cancelled_without_a_known_session_id()
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut editor = Editor::start();
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":editor.workspace,"mcpServers":[{"type":"http","name":"owned-init","url":format!("http://{}/mcp",listener.local_addr().unwrap()),"headers":[]}]}});
    editor.stdin.write_all(format!("{request}\n").as_bytes()).await.unwrap();
    let (socket, _) =
        tokio::time::timeout(Duration::from_secs(60), listener.accept()).await.unwrap().unwrap();
    let mut socket = BufReader::new(socket);
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        socket.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    assert!(length > 0 && length < 65536);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["method"], "initialize");
    editor.notify("session/cancel", serde_json::json!({})).await;
    let reply = editor.next().await;
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["error"]["code"], -32603, "unfinished requested MCP cannot be acknowledged as ready");
    assert!(reply["error"]["message"].as_str().unwrap().contains("cancelled"));
    assert!(reply["result"].is_null());
    let session = editor.rook.store.list_sessions().unwrap().into_iter().next().unwrap();
    assert_eq!(session.next_seq, 0, "startup cancellation cannot become a model turn");
    assert!(editor.call(2, "session/resume", serde_json::json!({"sessionId":rook_store::format_session_id(session.id),"cwd":editor.workspace,"mcpServers":[]})).await["result"].is_object());
    drop(socket);
}

#[tokio::test]
async fn setup_declaration_limits_and_unsupported_transports_refuse_before_creating_sessions() {
    let mut config = Config::default();
    config.server.acp_setup_bytes = 4096;
    let mut editor = Editor::start_with(config, |_| {});
    let huge = "X".repeat(5000);
    let bad = editor.call(1, "session/new", serde_json::json!({"cwd":editor.workspace,"mcpServers":[{"name":"too-large","command":"unused","env":[{"name":"TOKEN","value":huge}]}]})).await;
    assert_eq!(bad["error"]["code"], -32602);
    assert!(bad["error"]["message"].as_str().unwrap().contains("acp_setup_bytes"));
    let sse = editor.call(2, "session/new", serde_json::json!({"cwd":editor.workspace,"mcpServers":[{"type":"sse","name":"unsupported","url":"http://127.0.0.1:1"}]})).await;
    assert_eq!(sse["error"]["code"], -32602);
    assert!(editor.rook.store.list_sessions().unwrap().is_empty());
}

struct SetupMcp {
    url: String,
    counts: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for SetupMcp {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl SetupMcp {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let counts = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let seen = counts.clone();
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut length = 0usize;
                let mut request_method = String::new();
                let mut authorization = String::new();
                loop {
                    let mut line = String::new();
                    socket.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if request_method.is_empty() {
                        request_method = line.split_whitespace().next().unwrap().into();
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if line.to_ascii_lowercase().starts_with("authorization:") {
                        authorization = line.split_once(':').unwrap().1.trim().into();
                    }
                }
                assert!(length < 65536);
                if request_method == "DELETE" {
                    *seen.lock().unwrap().entry("delete".into()).or_insert(0) += 1;
                    socket.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap();
                    continue;
                }
                let mut body = vec![0u8; length];
                socket.read_exact(&mut body).await.unwrap();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                *seen.lock().unwrap().entry(method.clone()).or_insert(0) += 1;
                assert_eq!(authorization, "Bearer acp-ephemeral-secret-value");
                if request["id"].is_null() {
                    socket
                        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await
                        .unwrap();
                    continue;
                }
                let result = match method.as_str() {
                    "initialize" => {
                        serde_json::json!({"protocolVersion":request["params"]["protocolVersion"],"serverInfo":{"name":"owned","version":"1"},"capabilities":{"tools":{}}})
                    }
                    "tools/list" => {
                        serde_json::json!({"tools":[{"name":"ping","description":"A fixture observation.","inputSchema":{"type":"object"}}]})
                    }
                    "tools/call" => {
                        serde_json::json!({"content":[{"type":"text","text":"owned MCP answer acp-ephemeral-secret-value"}]})
                    }
                    other => panic!("unexpected requested MCP method {other}"),
                };
                let body =
                    serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
                let session_header = if method == "initialize" { "Mcp-Session-Id: owned-1\r\n" } else { "" };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{session_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        Self { url, counts, task }
    }
    fn count(&self, name: &str) -> usize {
        self.counts.lock().unwrap().get(name).copied().unwrap_or(0)
    }
    fn declaration(&self) -> serde_json::Value {
        serde_json::json!({"type":"http","name":"owned","url":self.url,"headers":[{"name":"Authorization","value":"Bearer acp-ephemeral-secret-value"}]})
    }
}

#[tokio::test]
async fn requested_http_mcp_is_ready_reused_and_redacted_through_actual_tools_then_reconnected_after_close() {
    let _turn = provider_lock().await;
    let mcp = SetupMcp::start().await;
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    let created = editor
        .call(1, "session/new", serde_json::json!({"cwd":editor.workspace,"mcpServers":[mcp.declaration()]}))
        .await;
    assert!(created["result"]["sessionId"].is_string(), "{created}");
    assert_eq!(mcp.count("initialize"), 1);
    assert_eq!(mcp.count("tools/list"), 1, "new cannot acknowledge before the requested catalog is ready");
    let session = created["result"]["sessionId"].clone();
    for turn in 0..2 {
        let base = scripted_call("owned__ping", serde_json::json!({})).await;
        unsafe { std::env::set_var("OLLAMA_HOST", &base) };
        let reply = editor.call(2+turn, "session/prompt", serde_json::json!({"sessionId":session,"prompt":[{"type":"text","text":"inspect the configured observation"}]})).await;
        assert!(reply["result"].is_object(), "{reply}");
    }
    assert_eq!(mcp.count("initialize"), 1, "ordinary prompts reuse their session-owned connection");
    assert_eq!(mcp.count("tools/call"), 2);
    let id = rook_store::parse_session_id(session.as_str().unwrap()).unwrap();
    let results: Vec<_> = editor
        .rook
        .store
        .events(id, 0, 256)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.record.kind == rook_store::EventKind::ToolResult && event.record.label == "owned__ping"
        })
        .collect();
    assert_eq!(results.len(), 2);
    for result in results {
        let text = String::from_utf8(editor.rook.store.get(&result.record.body).unwrap()).unwrap();
        assert!(
            text.contains("owned MCP answer"),
            "actual remote output must reach the ordinary tool journal"
        );
        assert!(
            !text.contains("acp-ephemeral-secret-value"),
            "transient credentials cannot become retained tool output: {text}"
        );
    }
    assert!(editor.call(4, "session/resume", serde_json::json!({"sessionId":session,"cwd":editor.workspace,"mcpServers":[mcp.declaration()]})).await["result"].is_object());
    assert_eq!(mcp.count("initialize"), 1);
    assert!(
        editor.call(5, "session/close", serde_json::json!({"sessionId":session})).await["result"].is_object()
    );
    assert!(editor.call(6, "session/resume", serde_json::json!({"sessionId":session,"cwd":editor.workspace,"mcpServers":[mcp.declaration()]})).await["result"].is_object());
    assert_eq!(
        mcp.count("initialize"),
        2,
        "resume after close recreates the explicitly requested connection"
    );
    assert_eq!(mcp.count("tools/call"), 2, "resume cannot replay earlier tool effects");
    assert!(
        editor.call(7, "session/close", serde_json::json!({"sessionId":session})).await["result"].is_object()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_mcp_runs_in_the_selected_project_reuses_its_owned_process_and_stops_on_close() {
    let _turn = provider_lock().await;
    let project = tempfile::tempdir().unwrap();
    let receipts = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let target = executable.parent().unwrap().parent().unwrap();
    let mock = target.join("mcp-mock");
    assert!(mock.is_file(), "build the existing MCP fixture before this focused test: {mock:?}");
    let receipt = receipts.path().join("mcp-owner.json");
    let declaration = serde_json::json!({"name":"stdio-owned","command":mock,"args":["workspace-probe"],"env":[{"name":"MCP_MOCK_RECEIPT","value":receipt},{"name":"MCP_MOCK_TOKEN","value":"stdio-ephemeral-secret-value"}]});
    let mut config = Config::default();
    config.agent.model = "ollama/scripted".into();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let mut editor = Editor::start_with(config, |_| {});
    let created = editor
        .call(1, "session/new", serde_json::json!({"cwd":project.path(),"mcpServers":[declaration]}))
        .await;
    let session = created["result"]["sessionId"].clone();
    assert!(session.is_string(), "{created}");
    let owner: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(PathBuf::from(owner["cwd"].as_str().unwrap()), project.path().canonicalize().unwrap());
    let pid = owner["pid"].as_u64().unwrap().to_string();
    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }
    assert!(alive(&pid), "the actual owned process must be running before tools are called");
    for turn in 0..2 {
        let base = scripted_call("stdio-owned__echo", serde_json::json!({"text":"probe"})).await;
        unsafe { std::env::set_var("OLLAMA_HOST", &base) };
        assert!(editor.call(2+turn, "session/prompt", serde_json::json!({"sessionId":session,"prompt":[{"type":"text","text":"inspect the selected project MCP"}]})).await["result"].is_object());
    }
    let id = rook_store::parse_session_id(session.as_str().unwrap()).unwrap();
    let results: Vec<_> = editor
        .rook
        .store
        .events(id, 0, 256)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.record.kind == rook_store::EventKind::ToolResult
                && event.record.label == "stdio-owned__echo"
        })
        .collect();
    assert_eq!(results.len(), 2);
    for result in results {
        let text = String::from_utf8(editor.rook.store.get(&result.record.body).unwrap()).unwrap();
        assert!(text.contains(&pid), "both turns must reach the original process");
        assert!(!text.contains("stdio-ephemeral-secret-value"));
    }
    assert!(
        editor.call(4, "session/close", serde_json::json!({"sessionId":session})).await["result"].is_object()
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        while alive(&pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session-owned MCP process must actually die after close");
    assert!(
        editor
            .call(
                5,
                "session/load",
                serde_json::json!({"sessionId":session,"cwd":project.path(),"mcpServers":[]})
            )
            .await["result"]
            .is_object()
    );
    assert!(!alive(&pid), "history load with an empty intended list cannot restart an old connection");
}

#[tokio::test]
async fn load_replaces_requested_mcp_only_after_a_turn_and_never_implicitly_restores_old_servers() {
    let mcp = SetupMcp::start().await;
    let mut editor = Editor::start();
    let created = editor
        .call(1, "session/new", serde_json::json!({"cwd":editor.workspace,"mcpServers":[mcp.declaration()]}))
        .await;
    let session = created["result"]["sessionId"].clone();
    assert!(session.is_string(), "{created}");
    let loaded = editor
        .call(
            2,
            "session/load",
            serde_json::json!({"sessionId":session,"cwd":editor.workspace,"mcpServers":[]}),
        )
        .await;
    assert!(loaded["result"].is_object(), "{loaded}");
    assert_eq!(mcp.count("initialize"), 1);
    let reconnected = editor
        .call(
            3,
            "session/resume",
            serde_json::json!({"sessionId":session,"cwd":editor.workspace,"mcpServers":[mcp.declaration()]}),
        )
        .await;
    assert!(reconnected["result"].is_object(), "{reconnected}");
    assert_eq!(
        mcp.count("initialize"),
        2,
        "the intended list changed; old connection must have been released"
    );
    assert_eq!(mcp.count("tools/call"), 0, "setup/readiness cannot execute any tool");
    assert!(
        editor.call(4, "session/close", serde_json::json!({"sessionId":session})).await["result"].is_object()
    );
}

#[tokio::test]
async fn a_definitely_failed_requested_server_cannot_return_ready_or_echo_its_credentials() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut editor = Editor::start();
    let reply = editor.call(1, "session/new", serde_json::json!({"cwd":editor.workspace,"mcpServers":[{"type":"http","name":"failed-owned","url":format!("http://{address}/mcp"),"headers":[{"name":"Authorization","value":"Bearer failed-setup-secret-value"}]}]})).await;
    assert_eq!(reply["error"]["code"], -32603, "{reply}");
    assert!(reply["result"].is_null());
    assert!(!reply.to_string().contains("failed-setup-secret-value"));
    let session = editor.rook.store.list_sessions().unwrap().into_iter().next().unwrap();
    assert_eq!(session.next_seq, 0);
    assert!(editor.call(2, "session/resume", serde_json::json!({"sessionId":rook_store::format_session_id(session.id),"cwd":editor.workspace,"mcpServers":[]})).await["result"].is_object());
}

#[tokio::test]
async fn an_external_executor_reads_the_child_editors_buffer_with_bounded_copy_and_no_disk_fallback() {
    let _turn = provider_lock().await;
    for oversized in [false, true] {
        let base =
            scripted_call("delegate", serde_json::json!({"task":"read selected lines","executor":"owned"}))
                .await;
        unsafe { std::env::set_var("OLLAMA_HOST", &base) };
        let exe = std::env::current_exe().unwrap();
        let debug = exe.parent().unwrap().parent().unwrap();
        let mock = debug.join(if cfg!(windows) { "mcp-mock.exe" } else { "mcp-mock" });
        assert!(mock.is_file(), "build mcp-mock first");
        let receipts = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.agent.model = "ollama/scripted".into();
        config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
        config.acp_executor.file_bytes = 4096;
        config.acp_executor.profiles.insert(
            "owned".into(),
            rook_core::acp_executor::Profile {
                command: mock.to_string_lossy().into_owned(),
                args: vec![
                    "acp-read".into(),
                    receipts.path().join("process.json").to_string_lossy().into_owned(),
                ],
                trusted: true,
                env: Default::default(),
            },
        );
        let mut editor = Editor::start_with(config, |workspace| {
            std::fs::write(workspace.join("notes.txt"), "disk first\ndisk second\n").unwrap();
        });
        editor.call(1,"initialize",serde_json::json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"subagents":{}}})).await;
        let root = editor.call(2, "session/new", serde_json::json!({"cwd":"."})).await["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        let prompt = serde_json::json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":root,"prompt":[{"type":"text","text":"use the configured executor"}]}});
        editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
        let mut child = None;
        let mut read = false;
        let mut ended = false;
        let buffer =
            if oversized { "x".repeat(5000) } else { "editor first\r\neditor unsaved second\r\n".into() };
        if oversized {
            assert!(buffer.len() > 4096);
        }
        for _ in 0..100 {
            let message = editor.next().await;
            if message["params"]["update"]["sessionUpdate"] == "subagent_update" {
                child = message["params"]["update"]["sessionId"].as_str().map(str::to_owned);
            }
            if message["method"] == "fs/read_text_file" {
                assert_eq!(
                    message["params"]["sessionId"].as_str(),
                    child.as_deref(),
                    "association precedes the child request"
                );
                assert_ne!(message["params"]["sessionId"], root);
                assert_eq!(
                    message["params"]["path"],
                    editor.workspace.canonicalize().unwrap().join("notes.txt").to_string_lossy().as_ref()
                );
                read = true;
                let reply =
                    serde_json::json!({"jsonrpc":"2.0","id":message["id"],"result":{"content":buffer}});
                editor.stdin.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
            }
            if message["id"] == 3 && message["method"].is_null() {
                assert!(message.get("error").is_none(), "{message}");
                ended = true;
                break;
            }
        }
        assert!(read && ended);
        let child = rook_store::parse_session_id(child.as_deref().unwrap()).unwrap();
        let events = editor.rook.store.events(child, 0, 128).unwrap();
        let reply = events.iter().find(|e| e.record.kind == rook_store::EventKind::AssistantMessage).unwrap();
        let reply = String::from_utf8(editor.rook.store.get(&reply.record.body).unwrap()).unwrap();
        if oversized {
            assert!(reply.contains("raw read limit"), "{reply}");
            assert!(!reply.contains("disk second"));
        } else {
            assert_eq!(reply, "editor unsaved second\r\n");
        }
        assert_eq!(
            std::fs::read_to_string(editor.workspace.join("notes.txt")).unwrap(),
            "disk first\ndisk second\n"
        );
    }
}
