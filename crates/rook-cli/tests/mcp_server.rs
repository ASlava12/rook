//! Rook offered as an MCP server.
//!
//! Driven the way a client drives it — lines of JSON-RPC in, lines out — rather
//! than by calling the functions underneath, because the shape on the wire is
//! the whole contract.

use std::io::Write;
use std::process::{Command, Stdio};

fn talk(workspace: &std::path::Path, home: &std::path::Path, lines: &[&str]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rook"))
        .env("ROOK_HOME", home)
        .args(["--workspace", workspace.to_str().unwrap(), "mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for line in lines {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "what the disk has\n").unwrap();
    (workspace, tempfile::tempdir().unwrap())
}

#[test]
fn a_client_can_list_the_tools_and_read_a_file_through_them() {
    let (workspace, home) = fixture();
    let said = talk(
        workspace.path(),
        home.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"note.txt"}}}"#,
        ],
    );

    assert_eq!(said.len(), 3, "a notification must not be answered: {said:?}");
    assert_eq!(said[0]["result"]["serverInfo"]["name"], "rook");
    let tools: Vec<&str> =
        said[1]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tools.contains(&"read_file"), "{tools:?}");
    assert!(tools.contains(&"run_command"), "{tools:?}");
    // Served for as long as stdin is open, so a command left running is one this
    // can keep — and a tool offered here and not there is the asymmetry the
    // three front ends exist to avoid.
    assert!(tools.contains(&"job"), "{tools:?}");

    let call = &said[2]["result"];
    assert_eq!(call["isError"], false, "{call}");
    assert!(
        call["content"][0]["text"].as_str().unwrap().contains("what the disk has"),
        "the arguments have to arrive in `params`, which is where a client puts them: {call}"
    );
}

/// A client reaching in from outside is not more trusted than the model inside,
/// and with nobody at this end to ask, the answer is no.
#[test]
fn a_write_with_nobody_to_approve_it_is_refused_and_says_why() {
    let (workspace, home) = fixture();
    let said = talk(
        workspace.path(),
        home.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"planted.txt","content":"x"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"nonsense/method"}"#,
        ],
    );

    assert_eq!(said[0]["result"]["isError"], true, "{:?}", said[0]);
    let why = said[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(why.contains("--yes"), "the refusal has to say what would make it possible: {why}");
    assert!(!workspace.path().join("planted.txt").exists(), "and nothing may have been written");

    assert_eq!(said[1]["error"]["code"], -32601, "an unknown method is an error, not a result");
}

/// The client and the server are both here, so they can be pointed at each
/// other. Nothing else checks both halves of the wire at once — a hand-written
/// exchange only ever proves the side it was written against.
///
/// It also pins the thing that made this fail the first time it was tried:
/// serving must not open the store, or it cannot run beside anything that
/// already has — which is the arrangement somebody wants when they run the web
/// UI and point an editor at the tools.
#[test]
fn the_client_and_the_server_understand_each_other() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "loopback works\n").unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[[mcp]]\nname = \"self\"\ncommand = {:?}\nargs = [\"--workspace\", {:?}, \"mcp\", \"serve\"]\n",
            env!("CARGO_BIN_EXE_rook"),
            workspace.path().to_str().unwrap()
        ),
    )
    .unwrap();

    let rook = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rook"))
            .env("ROOK_HOME", home.path())
            .args(["--workspace", workspace.path().to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // The client opens the store; the server it spawns must not need it.
    let listed = rook(&["mcp", "ls"]);
    assert!(listed.contains("self"), "the server has to have answered its handshake: {listed}");
    assert!(listed.contains("2025-06-18"), "{listed}");

    let tools = rook(&["mcp", "tools", "self"]);
    assert!(tools.contains("read_file"), "{tools}");
    assert!(tools.contains("run_command"), "{tools}");

    let read = rook(&["mcp", "call", "self", "read_file", r#"{"path":"note.txt"}"#]);
    assert!(read.contains("loopback works"), "a call has to round-trip through both halves: {read}");
}
