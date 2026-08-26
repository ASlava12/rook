//! A minimal language server for the client's tests.
//!
//! A real one is a large download the test environment may not have. This is a
//! binary in the same crate, so `CARGO_BIN_EXE_lsp-mock` always exists on every
//! platform we build for.

use std::io::{BufRead, Read, Write};

fn main() {
    let mut stdin = std::io::BufReader::new(std::io::stdin());
    let mut out = std::io::stdout();

    while let Some(length) = read_content_length(&mut stdin) {
        let mut body = vec![0u8; length];
        if stdin.read_exact(&mut body).is_err() {
            break;
        }
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(&body) else { continue };
        let method = message["method"].as_str().unwrap_or("");

        if method == "textDocument/didOpen" {
            let uri = message.pointer("/params/textDocument/uri").cloned().unwrap_or_default();
            send(
                &mut out,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": {
                        "uri": uri,
                        "diagnostics": [{
                            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 9 } },
                            "severity": 1,
                            "source": "mock",
                            "message": "cannot find value `oops` in this scope"
                        }]
                    }
                }),
            );
            continue;
        }

        let Some(id) = message["id"].as_u64() else { continue };
        let uri = message
            .pointer("/params/textDocument/uri")
            .and_then(|u| u.as_str())
            .unwrap_or("file:///unknown")
            .to_string();

        let result = match method {
            "initialize" => serde_json::json!({
                "capabilities": { "definitionProvider": true, "referencesProvider": true },
                "serverInfo": { "name": "lsp-mock", "version": "1.0.0" }
            }),
            // A single object rather than a list, which the spec allows and a
            // client that assumes an array gets wrong.
            "textDocument/definition" => serde_json::json!({
                "uri": uri,
                "range": { "start": { "line": 10, "character": 3 }, "end": { "line": 10, "character": 8 } }
            }),
            "textDocument/references" => serde_json::json!([
                { "uri": uri, "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } } },
                { "uri": uri, "range": { "start": { "line": 20, "character": 8 }, "end": { "line": 20, "character": 13 } } }
            ]),
            "workspace/symbol" => {
                let query = message.pointer("/params/query").and_then(|q| q.as_str()).unwrap_or("");
                serde_json::json!([{
                    "name": query,
                    "kind": 12,
                    "containerName": "app",
                    "location": { "uri": "file:///tmp/x/src/lib.rs",
                                  "range": { "start": { "line": 41, "character": 0 }, "end": { "line": 41, "character": 6 } } }
                }])
            }
            "textDocument/hover" => serde_json::json!(null),
            _ => serde_json::json!(null),
        };

        send(&mut out, &serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }
}

fn send(out: &mut impl Write, value: &serde_json::Value) {
    let body = value.to_string();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn read_content_length(reader: &mut impl BufRead) -> Option<usize> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            return length;
        }
        if let Some(value) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().ok();
        }
    }
}
