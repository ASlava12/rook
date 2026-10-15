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
    if mode.starts_with("elicitation") {
        if let Err(error) = elicitation_fixture(&mode) {
            eprintln!("elicitation fixture: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode.starts_with("acp-") {
        acp_fixture(&mode);
        return;
    }
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines().map_while(std::result::Result::ok) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(method) = message.get("method").and_then(|m| m.as_str()) else { continue };
        let Some(id) = message.get("id").and_then(|i| i.as_u64()) else { continue };

        if mode.starts_with("resources") {
            let response = match resource_fixture(&mode, &message) {
                Ok(result) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => {
                    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":error.to_string()}})
                }
            };
            let _ = writeln!(out, "{response}");
            let _ = out.flush();
            continue;
        }

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

        if mode == "workspace-probe"
            && method == "initialize"
            && let Ok(path) = std::env::var("MCP_MOCK_RECEIPT")
        {
            let value = serde_json::json!({"pid":std::process::id(),"cwd":std::env::current_dir().ok()});
            let _ = std::fs::write(path, value.to_string());
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
                if mode == "workspace-probe" {
                    let value = serde_json::json!({"pid":std::process::id(),"cwd":std::env::current_dir().ok(),"token":std::env::var("MCP_MOCK_TOKEN").ok()});
                    let reply = serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}]}});
                    let _ = writeln!(out, "{reply}");
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

fn elicitation_fixture(mode: &str) -> std::io::Result<()> {
    use serde_json::{Value, json};
    let mut out = std::io::stdout();
    let mut pending: Option<(Value, Value)> = None;
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line?).map_err(std::io::Error::other)?;
        let id = message["id"].clone();
        let result = match message["method"].as_str() {
            Some("initialize") => {
                json!({"protocolVersion":"2025-11-25","serverInfo":{"name":"forms","version":"1"},"capabilities":{"tools":{}}})
            }
            Some("notifications/initialized") => continue,
            Some("tools/list") => json!({"tools":[{"name":"collect","inputSchema":{"type":"object"}}]}),
            Some("tools/call") => {
                let request =
                    if mode.ends_with("string") { json!(format!("server-{id}")) } else { id.clone() };
                let count = if mode.contains("flood") {
                    5
                } else if mode.contains("duplicate") {
                    2
                } else {
                    1
                };
                for n in 0..count {
                    let nested = if mode.contains("flood") {
                        json!(format!("nested-{n}-{id}"))
                    } else {
                        request.clone()
                    };
                    let params = if mode.contains("url") {
                        json!({"mode":"url","message":"Authorize the requested action","url":"https://example.org/authorize?state=owned","elicitationId":"flow-1"})
                    } else {
                        json!({"message":"Which display name?","requestedSchema":{"type":"object","properties":{"name":{"type":"string","maxLength":64}},"required":["name"]}})
                    };
                    writeln!(
                        out,
                        "{}",
                        json!({"jsonrpc":"2.0","id":nested,"method":"elicitation/create","params":params})
                    )?;
                }
                out.flush()?;
                pending = Some((id, request));
                continue;
            }
            None => {
                let Some((original, expected)) = pending.take() else { continue };
                if id != expected {
                    return Err(std::io::Error::other("response belongs to another elicitation"));
                }
                if let Ok(path) = std::env::var("MCP_MOCK_RECEIPT") {
                    std::fs::write(path, serde_json::to_vec(&message).map_err(std::io::Error::other)?)?;
                }
                let action = message["result"]["action"].as_str().unwrap_or("error");
                writeln!(
                    out,
                    "{}",
                    json!({"jsonrpc":"2.0","id":original,"result":{"content":[{"type":"text","text":action}]}})
                )?;
                out.flush()?;
                continue;
            }
            _ => return Err(std::io::Error::other("unsupported elicitation fixture request")),
        };
        writeln!(out, "{}", json!({"jsonrpc":"2.0","id":id,"result":result}))?;
        out.flush()?;
    }
    Ok(())
}

fn resource_fixture(mode: &str, request: &serde_json::Value) -> std::io::Result<serde_json::Value> {
    use serde_json::json;
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => {
            json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"documents","version":"1"},"capabilities":{"resources":{}}})
        }
        "resources/list" => {
            if mode == "resources-duplicate" {
                return Ok(json!({"resources":[{"uri":"doc:a","name":"a"},{"uri":"doc:a","name":"b"}]}));
            }
            if mode == "resources-count" {
                return Ok(
                    json!({"resources":(0..3).map(|n| json!({"uri":format!("doc:{n}"),"name":"x"})).collect::<Vec<_>>()}),
                );
            }
            if mode == "resources-bytes" {
                return Ok(json!({"resources":[{"uri":"doc:a","name":"a","description":"x".repeat(3000)}]}));
            }
            if request["params"]["cursor"] == "page /+?" {
                return Ok(json!({"resources":[]}));
            }
            json!({"resources":[{"uri":"doc:русский","name":"source","mimeType":"text/plain"}],"nextCursor":"page /+?"})
        }
        "resources/templates/list" => {
            json!({"resourceTemplates":[{"uriTemplate":"doc:{id}","name":"document"}]})
        }
        "resources/read" => {
            if let Ok(path) = std::env::var("MCP_MOCK_RECEIPT") {
                std::fs::write(path, "read started")?;
            }
            if mode == "resources-crash" {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new().create(true).append(true).open(died_marker())?;
                writeln!(file, "read")?;
                std::process::exit(1);
            }
            if mode == "resources-hang" {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            if mode == "resources-bad-content" {
                return Ok(json!({"contents":[{"uri":"doc:a","text":"one","blob":"two"}]}));
            }
            if mode == "resources-many-contents" {
                return Ok(
                    json!({"contents":(0..65).map(|n| json!({"uri":format!("doc:{n}"),"text":"bounded"})).collect::<Vec<_>>()}),
                );
            }
            if mode == "resources-oversized-read" {
                return Ok(json!({"contents":[{"uri":"doc:a","text":"x".repeat(9*1024*1024)}]}));
            }
            json!({"contents":[{"uri":request["params"]["uri"],"mimeType":"text/plain","text":"Данные 📚 из ресурса"}]})
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "resources-only fixture received an unsupported method",
            ));
        }
    };
    Ok(result)
}

/// The owned external executor fixture exercises the real stdio boundary; it
/// never invokes a model or an installed messaging account.
fn acp_fixture(mode: &str) {
    use serde_json::{Value, json};
    let send = |value: Value| {
        println!("{value}");
        std::io::stdout().flush().ok();
    };
    if mode == "acp-terminal-command" {
        let argument = std::env::args().nth(2).unwrap_or_default();
        let value = std::env::var("MOCK_ACP_VALUE").unwrap_or_default();
        let token = std::env::var("MOCK_ACP_TOKEN").unwrap_or_default();
        std::fs::write("terminal.txt", format!("{argument}\n{value}\n{token}\n")).ok();
        println!("{argument}\n{value}\n{token}\ndone");
        return;
    }
    if mode == "acp-terminal-block-command" {
        let path = std::env::args().nth(2).unwrap_or_else(|| "terminal.pid".into());
        std::fs::write(path, std::process::id().to_string()).ok();
        println!("owned terminal started");
        std::io::stdout().flush().ok();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    if let Some(path) = std::env::args().nth(2) {
        std::fs::write(
            path,
            json!({"pid":std::process::id(),"cwd":std::env::current_dir().ok()}).to_string(),
        )
        .ok();
    }
    let mut terminal_id = String::new();
    let mut prompt_id = Value::Null;
    for line in std::io::stdin().lock().lines().map_while(std::result::Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else { return };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str) {
            Some("initialize") => {
                send(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentCapabilities":{}}}))
            }
            Some("session/new") => {
                send(json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"owned-external"}}))
            }
            Some("session/prompt") => {
                prompt_id = id;
                if let Some(path) = std::env::args().nth(2) {
                    let path = std::path::Path::new(&path).with_extension("prompt.json");
                    std::fs::write(path, message.get("params").unwrap_or(&Value::Null).to_string()).ok();
                }
                if mode == "acp-hang" {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
                if mode == "acp-large" {
                    send(
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x".repeat(300_000)}}}}),
                    );
                    continue;
                }
                if mode.starts_with("acp-terminal") {
                    let blocking = matches!(mode, "acp-terminal-hang" | "acp-terminal-loss");
                    let command = std::env::current_exe().ok();
                    let args = if blocking {
                        vec!["acp-terminal-block-command", "terminal.pid"]
                    } else {
                        vec!["acp-terminal-command", "literal $(touch injected.txt) `echo altered` ' quoted"]
                    };
                    send(
                        json!({"jsonrpc":"2.0","id":"create","method":"terminal/create","params":{"sessionId":"owned-external","command":command,"args":args,
                        "env":[{"name":"MOCK_ACP_VALUE","value":"literal $HOME & quote ' value"},{"name":"MOCK_ACP_TOKEN","value":"private-terminal-credential"}],"outputByteLimit":if mode=="acp-terminal-truncate" {8}else if mode=="acp-terminal-bad-limit" {-1}else {4096}}}),
                    );
                    continue;
                }
                if mode.starts_with("acp-read") {
                    let line = if mode == "acp-read-invalid" { 0 } else { 2 };
                    let limit = if mode == "acp-read-secret" { 2 } else { 1 };
                    send(
                        json!({"jsonrpc":"2.0","id":"read","method":"fs/read_text_file","params":{"sessionId":"owned-external","path":"notes.txt","line":line,"limit":limit}}),
                    );
                    continue;
                }
                if mode == "acp-result" || mode == "acp-messages" {
                    let count = if mode == "acp-result" { 3 } else { 32 };
                    let text = if mode == "acp-result" { "x".repeat(600) } else { "x".into() };
                    for _ in 0..count {
                        send(
                            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}}),
                        );
                    }
                    continue;
                }
                if mode == "acp-wrong-session" {
                    send(
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"foreign","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"foreign"}}}}),
                    );
                    continue;
                }
                send(
                    json!({"jsonrpc":"2.0","id":"permission","method":"session/request_permission","params":{"sessionId":"owned-external","toolCall":{"toolCallId":"write","title":"Write fixture.txt","kind":"edit"},"options":[{"optionId":"once","name":"Allow once","kind":"allow_once"},{"optionId":"deny","name":"Reject","kind":"reject_once"}]}}),
                );
            }
            None if id == "create" => {
                let Some(terminal) = message.pointer("/result/terminalId").and_then(Value::as_str) else {
                    send(json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"cancelled"}}));
                    continue;
                };
                terminal_id = terminal.into();
                if matches!(mode, "acp-terminal-hang" | "acp-terminal-loss") {
                    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    while !std::path::Path::new("terminal.pid").exists() && std::time::Instant::now() < until
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    if mode == "acp-terminal-loss" {
                        std::process::exit(1);
                    }
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
                send(
                    json!({"jsonrpc":"2.0","id":"wait","method":"terminal/wait_for_exit","params":{"sessionId":"owned-external","terminalId":terminal_id}}),
                );
            }
            None if id == "wait" => send(
                json!({"jsonrpc":"2.0","id":"output","method":"terminal/output","params":{"sessionId":"owned-external","terminalId":terminal_id}}),
            ),
            None if id == "output" => {
                if let Some(path) = std::env::args().nth(2) {
                    std::fs::write(
                        std::path::Path::new(&path).with_extension("terminal.json"),
                        message.to_string(),
                    )
                    .ok();
                }
                let text = message
                    .pointer("/result/output")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid terminal output");
                send(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}}),
                );
                send(
                    json!({"jsonrpc":"2.0","id":"release","method":"terminal/release","params":{"sessionId":"owned-external","terminalId":terminal_id}}),
                );
            }
            None if id == "release" => send(
                json!({"jsonrpc":"2.0","id":"stale-terminal","method":"terminal/output","params":{"sessionId":"owned-external","terminalId":terminal_id}}),
            ),
            None if id == "stale-terminal" => {
                let text = if message.get("error").is_some() {
                    "released terminal refused"
                } else {
                    "released terminal incorrectly accepted"
                };
                send(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}}),
                );
                send(json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"end_turn"}}));
            }
            None if id == "read" => {
                let text = message
                    .pointer("/result/content")
                    .and_then(Value::as_str)
                    .or_else(|| message.pointer("/error/message").and_then(Value::as_str))
                    .unwrap_or("invalid read reply");
                send(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}}),
                );
                send(json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"end_turn"}}));
            }
            None if id == "permission" => {
                if message.pointer("/result/outcome/optionId").and_then(Value::as_str) == Some("once") {
                    send(
                        json!({"jsonrpc":"2.0","id":"write","method":"fs/write_text_file","params":{"sessionId":"owned-external","path":"fixture.txt","content":"owned ACP change\n"}}),
                    );
                } else {
                    send(json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"cancelled"}}));
                }
            }
            None if id == "write" => {
                if mode == "acp-duplicate" {
                    send(
                        json!({"jsonrpc":"2.0","id":"write","method":"fs/write_text_file","params":{"sessionId":"owned-external","path":"fixture.txt","content":"duplicate must never replace the first write"}}),
                    );
                    continue;
                }
                if mode == "acp-loss" {
                    eprintln!("owned fixture lost its completion; private-fixture-credential");
                    std::process::exit(1);
                }
                let text = if message.get("error").is_some() { "write refused" } else { "wrote fixture.txt" };
                send(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"tool_call","toolCallId":"write","title":"External observed operation","status":"completed"}}}),
                );
                send(
                    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"owned-external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}}),
                );
                send(json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"end_turn"}}));
            }
            _ => return,
        }
    }
}
