//! The protocol surface, driven the way an editor drives it.
//!
//! No model: these cover the methods an editor calls before and around a turn,
//! which is where a wrong field name silently produces an editor that renders
//! nothing.

use std::path::PathBuf;

use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Editor {
    stdin: tokio::io::DuplexStream,
    lines: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Editor {
    fn start() -> Self {
        let store_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            Store::open(store_dir.path()).unwrap(),
            Config::default(),
            Environment::bare("linux", "x86_64", "0.1.0"),
            SkillIndex::default(),
            PathBuf::from(workspace.path()),
        );

        let (client_in, server_in) = tokio::io::duplex(64 * 1024);
        let (server_out, client_out) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = rook_acp::serve(rook, BufReader::new(server_in), server_out).await;
        });

        Self { stdin: client_in, lines: BufReader::new(client_out).lines(), _dirs: (store_dir, workspace) }
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
    let reply = editor.call(1, "session/set_mode", serde_json::json!({})).await;
    assert_eq!(reply["error"]["code"], -32601, "{reply}");
    assert!(reply["error"]["message"].as_str().unwrap().contains("session/set_mode"));
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
