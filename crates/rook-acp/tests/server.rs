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
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Editor {
    fn start() -> Self {
        Self::start_with(Config::default(), |_| {})
    }

    fn start_with(config: Config, seed: impl FnOnce(&std::path::Path)) -> Self {
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

        let (client_in, server_in) = tokio::io::duplex(64 * 1024);
        let (server_out, client_out) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = rook_acp::serve(rook, BufReader::new(server_in), server_out).await;
        });

        Self {
            stdin: client_in,
            lines: BufReader::new(client_out).lines(),
            workspace: workspace.path().to_path_buf(),
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
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.lines.next_line())
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
    assert!(result["authMethods"].is_array());
}

#[tokio::test]
async fn a_new_session_comes_back_as_an_id_the_other_methods_accept() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": "/tmp", "mcpServers": [] })).await;
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
async fn an_unknown_method_gets_method_not_found_rather_than_silence() {
    let mut editor = Editor::start();
    // Deliberately not a method anyone might implement later: this test named
    // `session/set_mode` until that became one.
    let reply = editor.call(1, "session/no_such_method", serde_json::json!({})).await;
    assert_eq!(reply["error"]["code"], -32601, "{reply}");
    assert!(reply["error"]["message"].as_str().unwrap().contains("session/no_such_method"));
}

#[tokio::test]
async fn a_prompt_with_no_text_is_rejected_before_a_model_is_contacted() {
    let mut editor = Editor::start();
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": "/tmp", "mcpServers": [] })).await;
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
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": "/tmp", "mcpServers": [] })).await;
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
    let created = editor.call(1, "session/new", serde_json::json!({ "cwd": "/tmp", "mcpServers": [] })).await;
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
    config.sandbox.mode = rook_tools::policy::Mode::Auto;
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
    config.sandbox.mode = rook_tools::policy::Mode::Auto;
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

    assert_eq!(modes["currentModeId"], "ask", "the configured default");
    let ids: Vec<&str> =
        modes["availableModes"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["auto", "ask", "readonly"], "{modes}");
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
        .call(3, "session/set_mode", serde_json::json!({ "sessionId": id, "modeId": "readonly" }))
        .await;
    assert!(set["error"].is_null(), "{set}");

    let modes =
        editor.call(4, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["modes"].clone();
    assert_eq!(modes["currentModeId"], "readonly", "the change must outlive the request");

    let bad =
        editor.call(5, "session/set_mode", serde_json::json!({ "sessionId": id, "modeId": "yolo" })).await;
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
    config.sandbox.mode = rook_tools::policy::Mode::Auto;
    let mut editor = Editor::start_with(config, |_| {});
    let written = editor.workspace.join("new.txt");

    editor.call(1, "initialize", serde_json::json!({ "protocolVersion": 1 })).await;
    let id = editor.call(2, "session/new", serde_json::json!({ "cwd": "." })).await["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    editor.call(3, "session/set_mode", serde_json::json!({ "sessionId": id, "modeId": "readonly" })).await;

    let prompt = serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "session/prompt",
        "params": { "sessionId": id, "prompt": [{ "type": "text", "text": "write a file" }] }
    });
    editor.stdin.write_all(format!("{prompt}\n").as_bytes()).await.unwrap();
    let _ = editor.drain(Duration::from_millis(2500)).await;

    assert!(!written.exists(), "the mode the editor set must reach the turn that follows it");
}
