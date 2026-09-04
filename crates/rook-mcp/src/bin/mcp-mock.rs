//! A minimal MCP server used by the client's tests.
//!
//! A real server is a Node or Python process the test environment may not have.
//! This is a binary in the same crate, so `CARGO_BIN_EXE_mcp-mock` always points
//! at something that exists on every platform we build for.
//!
//! `mcp-mock [mode]` — `ok` (default), `slow`, `error`, `crash`, `noise`, `mute`.

use std::io::{BufRead, Write};

/// A file rather than process state: the point is to die, and what survives
/// dying is on disk. The path comes from the caller so two tests cannot collide.
fn died_marker() -> String {
    std::env::var("MCP_MOCK_MARKER").unwrap_or_else(|_| "/tmp/rook-mcp-mock-died".into())
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ok".into());
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines().map_while(std::result::Result::ok) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else { continue };
        let Some(id) = message.get("id").and_then(|i| i.as_u64()) else { continue };

        if mode == "crash" && method == "tools/call" {
            std::process::exit(1);
        }
        // The common real failure: the server starts, takes a call, and dies
        // saying why on the only channel it has left.
        if mode == "last-words" && method == "tools/call" {
            eprintln!("ANTHROPIC_API_KEY is not set");
            std::process::exit(1);
        }
        // Dies once and serves afterwards, which is what a server crashing on a
        // bad input and being restarted looks like.
        if mode == "crash-once" && method == "tools/call" && !std::path::Path::new(&died_marker()).exists() {
            std::fs::write(died_marker(), "1").ok();
            std::process::exit(1);
        }
        // Alive, reading, and never answering again: stdout is closed and the
        // process is not. A server killed by the OOM killer mid-write looks
        // like this from here, and it is the one shape where writing to it
        // still succeeds — so a caller waiting for the reply waits for the
        // whole timeout unless something else says the answer is not coming.
        if mode == "mute" && method == "tools/call" {
            #[cfg(unix)]
            {
                // Taking ownership of fd 1 and dropping it closes stdout. The
                // handle in `out` is dead after this and nothing writes to it.
                use std::os::fd::FromRawFd;
                drop(unsafe { std::fs::File::from_raw_fd(1) });
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            // Windows has no way to close a standard handle from the standard
            // library, so there the mode is an ordinary death: the test still
            // holds, by the path that was already fast.
            #[cfg(not(unix))]
            std::process::exit(1);
        }
        if mode == "slow" && method != "initialize" {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        if mode == "noise" {
            let _ = writeln!(out, r#"{{"jsonrpc":"2.0","method":"notifications/progress"}}"#);
            let _ = writeln!(out, "not json at all");
            let _ = writeln!(out, r#"{{"jsonrpc":"2.0","id":99999,"result":{{}}}}"#);
        }

        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": "2025-06-18",
                "serverInfo": { "name": "mock", "version": "1.2.3" },
                "capabilities": { "tools": {} },
                "instructions": "a mock server"
            }),
            "tools/list" => serde_json::json!({ "tools": [
                {
                    "name": "echo",
                    "description": "Echo a message back.",
                    "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] },
                    "annotations": { "readOnlyHint": true }
                },
                { "name": "picture", "description": "Return an image.", "inputSchema": { "type": "object" } }
            ]}),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or_default();
                let tool = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if mode == "error" {
                    let _ = writeln!(
                        out,
                        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32602,"message":"no such tool"}}}}"#
                    );
                    let _ = out.flush();
                    continue;
                }
                match tool {
                    "picture" => serde_json::json!({
                        "content": [{ "type": "image", "data": "AAAA", "mimeType": "image/png" }]
                    }),
                    _ => {
                        let text = params
                            .get("arguments")
                            .and_then(|a| a.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        serde_json::json!({ "content": [{ "type": "text", "text": format!("echo: {text}") }] })
                    }
                }
            }
            _ => serde_json::json!({}),
        };

        let _ = writeln!(out, "{}", serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        let _ = out.flush();
    }
}
