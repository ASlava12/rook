//! MCP image data through a real transport, the loop, the store and replay.
use async_trait::async_trait;
use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
use rook_tools::Tool;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

struct Model {
    script: Mutex<Vec<Message>>,
    seen: Mutex<Vec<Request>>,
}
impl Model {
    fn new(script: Vec<Message>) -> Arc<Self> {
        Arc::new(Self { script: Mutex::new(script), seen: Mutex::new(Vec::new()) })
    }
}
#[async_trait]
impl Provider for Model {
    fn id(&self) -> &str {
        "scripted/images"
    }
    fn context_window(&self) -> usize {
        32000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let checker = request
            .messages
            .first()
            .is_some_and(|m| m.content.starts_with("Classify whether an assistant's proposed last reply"));
        let message = if checker {
            Message::assistant(r#"{"action":"finish"}"#)
        } else {
            let mut script = self.script.lock().unwrap();
            assert!(
                !script.is_empty(),
                "unexpected request: system={:?}; last={:?}",
                request.messages.first().map(|m| m.content.chars().take(300).collect::<String>()),
                request.messages.last().map(|m| m.content.chars().take(700).collect::<String>())
            );
            self.seen.lock().unwrap().push(request);
            script.remove(0)
        };
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Usage::default(), model: "scripted/images".into() })
    }
}
fn rook_at(root: &std::path::Path) -> Rook {
    let mut config = Config::default();
    config.agent.context_window = Some(32000);
    config.agent.compact_at = 0.4;
    config.sandbox.allow = vec!["camera__shot".into()];
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        root.into(),
    )
}
async fn mcp_server() -> (Arc<rook_mcp::Server>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let request = loop {
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break serde_json::Value::Null;
                }
                raw.extend_from_slice(&buffer[..n]);
                assert!(raw.len() <= 65536);
                let text = String::from_utf8_lossy(&raw);
                let Some((head, body)) = text.split_once("\r\n\r\n") else { continue };
                let size: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() < size {
                    continue;
                }
                break serde_json::from_str(body).unwrap_or_default();
            };
            if request["id"].is_null() {
                socket
                    .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .ok();
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion":"2025-06-18", "serverInfo":{"name":"camera","version":"1"},"capabilities":{"tools":{}}})
                }
                "tools/list" => {
                    json!({"tools":[{"name":"shot","description":"Take a screenshot.","inputSchema":{"type":"object"}}]})
                }
                _ => {
                    let mime = if request["params"]["arguments"]["invalid"] == true {
                        "image/jpeg"
                    } else {
                        "image/png"
                    };
                    json!({"content":[{"type":"image","mimeType":mime,"data":PNG}]})
                }
            };
            let body = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.ok();
        }
    });
    let config = rook_mcp::ServerConfig {
        name: "camera".into(),
        url: Some(format!("http://{address}/mcp")),
        ..Default::default()
    };
    let server = rook_mcp::Server::connect(&config, &Default::default()).await.unwrap();
    (Arc::new(server), task)
}

#[tokio::test]
async fn mcp_images_survive_a_batch_store_reopen_fork_and_explicit_retrieval() {
    // The only test in this process; keep vault and prompt state in its fixture.
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("ROOK_HOME", home.path());
    }
    let root = tempfile::tempdir().unwrap();
    let rook = rook_at(root.path());
    let (server, task) = mcp_server().await;
    let descriptors = server.list_tools().await.unwrap();
    let tool = rook_tools::mcp::McpTool::new(server.clone(), descriptors[0].clone());
    let ctx = rook_core::agent::tool_context(&rook.config, root.path(), &rook.output_dir);
    let refused = tool.call(&ctx, &json!({"invalid":true})).await.unwrap();
    assert!(refused.is_error && refused.images.is_empty());
    assert!(refused.content.contains("does not match"));
    let direct = tool.call(&ctx, &json!({})).await.unwrap();
    assert_eq!(direct.images.len(), 1, "image-only MCP result is still useful");
    assert!(!direct.content.contains(PNG), "base64 must not become text");

    let mut calls = Message::assistant("");
    calls.tool_calls = (0..5)
        .map(|n| ToolCall {
            id: format!("shot-{n}"),
            name: "camera__shot".into(),
            arguments: json!({"number":n}),
        })
        .collect();
    let model = Model::new(vec![calls, Message::assistant("Available screenshots inspected.")]);
    let session = rook.start_session("images").unwrap();
    let mut agent = AgentLoop::new(&rook, model.clone(), session);
    agent.tools.register_mcp_catalog([(server.clone(), descriptors.clone())], Default::default());
    agent.set_window_for_test(12000);
    let outcome = agent.run("take five screenshots").await.unwrap();
    assert!(outcome.compactions > 0, "the fixture must hit compaction before the screenshots are seen");
    {
        let requests = model.seen.lock().unwrap();
        let results: Vec<_> =
            requests[1].messages.iter().filter(|m| m.role == rook_llm::Role::Tool).collect();
        assert_eq!(results.len(), 5);
        assert!(results[0].images.is_empty(), "the oldest image must yield to the request bound");
        assert!(results[0].content.contains("include_images=true"));
        assert!(results[1..].iter().all(|m| m.images.len() == 1 && m.images[0].data == PNG));
        let calls: Vec<_> = requests[1].messages.iter().flat_map(|message| &message.tool_calls).collect();
        assert_eq!(calls.len(), results.len());
        for (call, result) in calls.iter().zip(&results) {
            assert_eq!(result.tool_call_id.as_deref(), Some(call.id.as_str()));
        }
    }
    let entries = rook.transcript(session, 0, 100, 10000).unwrap();
    assert!(entries.iter().all(|entry| !entry.body.contains(PNG)), "transcripts must not print base64");
    let result_id = entries.iter().find(|entry| entry.kind == "tool-result").unwrap().seq;
    let end = rook.store.get_session(session).unwrap().unwrap().next_seq;
    let fork = rook.fork_session(session, end).unwrap().id;
    let context = rook.context_usage(session, Some(32000)).unwrap();
    assert!(context.live_tokens >= 4 * 1536);
    assert!(context.live_tokens < 5 * 1536, "omitted pixels must not inflate live context: {context:?}");
    drop(agent);
    drop(rook);
    let rook = rook_at(root.path());
    for id in [session, fork] {
        let model = Model::new(vec![Message::assistant("I can still see the recent images.")]);
        AgentLoop::new(&rook, model.clone(), id).run("describe the screenshots").await.unwrap();
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen[0].messages.iter().map(|m| m.images.len()).sum::<usize>(), 4);
    }
    let summarizer = Model::new(vec![Message::assistant("Screenshots were inspected.")]);
    let mut agent = AgentLoop::new(&rook, summarizer, fork);
    agent.set_window_for_test(4000);
    agent.compact_now().await;
    assert!(rook.last_compaction(fork).unwrap().1.is_some(), "fixture must actually compact");
    let mut retrieve = Message::assistant("");
    retrieve.tool_calls.push(ToolCall {
        id: "restore".into(),
        name: "read_result".into(),
        arguments: json!({"result_id":result_id,"include_images":true}),
    });
    let model = Model::new(vec![retrieve, Message::assistant("The original image is restored.")]);
    AgentLoop::new(&rook, model.clone(), fork).run("retrieve the first screenshot").await.unwrap();
    {
        let seen = model.seen.lock().unwrap();
        assert_eq!(
            seen[0].messages.iter().map(|m| m.images.len()).sum::<usize>(),
            0,
            "compaction removes pixels, not originals"
        );
        assert!(seen[1].messages.iter().any(|m| m.images.first().is_some_and(|image| image.data == PNG)));
    }
    // Overflow takes the same approval/image/history path as direct MCP tools.
    let call = |name: &str, arguments: serde_json::Value| {
        let mut message = Message::assistant("");
        message.tool_calls.push(ToolCall { id: format!("deferred-{name}"), name: name.into(), arguments });
        message
    };
    let script = vec![
        call("mcp_tools", json!({"query":"screenshot"})),
        call("mcp_tools", json!({"name":"camera__shot"})),
        call("mcp_call", json!({"name":"camera__shot","arguments":{}})),
        Message::assistant("The deferred screenshot is visible."),
    ];
    let model = Model::new(script);
    let session = rook.start_session("deferred MCP").unwrap();
    let mut deferred = AgentLoop::new(&rook, model.clone(), session);
    deferred.tools.register_mcp_catalog(
        [(server.clone(), descriptors.clone())],
        rook_tools::mcp::CatalogLimits { max_server_tools: 0, ..Default::default() },
    );
    let result = deferred.run("Find and call the screenshot tool").await.unwrap();
    assert!(result.tools_called.contains(&"mcp_call".into()));
    {
        let requests = model.seen.lock().unwrap();
        assert!(requests.iter().all(|r| !r.tools.iter().any(|t| t.name == "camera__shot")));
        assert!(requests.last().unwrap().messages.iter().any(|m| !m.images.is_empty()));
    }
    let model = Model::new(vec![
        call("mcp_call", json!({"name":"camera__shot","arguments":{}})),
        Message::assistant("The tool was refused."),
    ]);
    let denied_session = rook.start_session("denied deferred MCP").unwrap();
    let mut denied = AgentLoop::new(&rook, model.clone(), denied_session);
    denied.tools.register_mcp_catalog(
        [(server.clone(), descriptors.clone())],
        rook_tools::mcp::CatalogLimits { max_server_tools: 0, ..Default::default() },
    );
    let mut deny_config = rook.config.clone();
    deny_config.sandbox.deny = vec!["camera__shot".into()];
    denied.policy = rook_core::agent::policy_for(&deny_config);
    denied.run("Call the screenshot tool").await.unwrap();
    {
        let requests = model.seen.lock().unwrap();
        assert!(requests[1].messages.iter().any(|m| m.content.contains("refused:")));
        assert!(
            requests.iter().flat_map(|r| &r.messages).all(|m| m.images.is_empty()),
            "deferred calls cannot bypass target deny rules"
        );
    }
    let model = Model::new(vec![
        call("mcp_call", json!({"name":"camera__shot","arguments":{}})),
        Message::assistant("The hook refused the call."),
    ]);
    let hook_session = rook.start_session("hooked deferred MCP").unwrap();
    let mut hooked = AgentLoop::new(&rook, model.clone(), hook_session);
    hooked.tools.register_mcp_catalog(
        [(server, descriptors)],
        rook_tools::mcp::CatalogLimits { max_server_tools: 0, ..Default::default() },
    );
    let reply = r#"{"decision":"deny","reason":"target hook reached"}"#;
    let command = if cfg!(windows) { format!("echo {reply}") } else { format!("printf '%s' '{reply}'") };
    let (hooks, errors) = rook_core::hooks::Hooks::compile(&[rook_core::hooks::HookConfig {
        event: rook_core::hooks::Event::PreTool,
        matches: Some("/^camera__shot$/".into()),
        command,
        ..Default::default()
    }]);
    assert!(errors.is_empty());
    hooked.hooks = Arc::new(hooks);
    hooked.run("Call the screenshot tool").await.unwrap();
    let requests = model.seen.lock().unwrap();
    assert!(
        requests[1].messages.iter().any(|m| m.content.contains("target hook reached")),
        "the target hook must apply to forwarded calls"
    );
    assert!(requests.iter().flat_map(|r| &r.messages).all(|m| m.images.is_empty()));
    task.abort();
}
