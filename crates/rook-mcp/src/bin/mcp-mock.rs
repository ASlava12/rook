//! A minimal MCP server used by the client's tests.
//!
//! A real server is a Node or Python process the test environment may not have.
//! This is a binary in the same crate, so `CARGO_BIN_EXE_mcp-mock` always points
//! at something that exists on every platform we build for.
//!
//! `mcp-mock [mode]` — `ok` (default), `slow`, `error`, `crash`, `noise`.

use std::io::{BufRead, Write};

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
