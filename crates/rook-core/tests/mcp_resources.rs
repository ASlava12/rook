//! Resource-only servers through HTTP/SSE, shared equipment and durable turns.
use async_trait::async_trait;
use rook_core::{
    Config, McpSession, Rook,
    agent::{self, AgentLoop},
};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
use rook_tools::{ToolBox, ToolContext, policy::Risk};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
struct Fixture {
    config: rook_mcp::ServerConfig,
    changed: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Value>>>,
    text: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(sse: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = rook_mcp::ServerConfig {
            name: "documents".into(),
            url: Some(format!("http://{}/mcp", listener.local_addr().unwrap())),
            ..Default::default()
        };
        let changed = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (change, log) = (changed.clone(), requests.clone());
        let text = "Данные 📚 \"quoted\" \\ \u{00} \u{1f} \u{08} \u{0c} \r\n\t".repeat(400);
        let source = text.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut raw = Vec::new();
                let request: Value = loop {
                    let mut bytes = [0; 4096];
                    let count = socket.read(&mut bytes).await.unwrap();
                    if count == 0 {
                        break Value::Null;
                    }
                    assert!(raw.len() + count <= 65536);
                    raw.extend_from_slice(&bytes[..count]);
                    let text = String::from_utf8_lossy(&raw);
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let size: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|n| n.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() < size {
                        continue;
                    }
                    break serde_json::from_str(body).unwrap();
                };
                if request["id"].is_null() {
                    let _ = socket
                        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    continue;
                }
                log.lock().unwrap().push(request.clone());
                let changed = change.load(Ordering::SeqCst);
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => {
                        json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"docs","version":"1"},"capabilities":{"resources":{}}})
                    }
                    "resources/list" => {
                        if request["params"]["cursor"] == "opaque +/?" {
                            json!({"resources":[]})
                        } else {
                            json!({"resources":(0..40).map(|n|json!({"uri":format!("doc:{n}"),"name":format!("{changed}-{n}"),"description":"evidence ".repeat(100),"ignored":"not advertised"})).collect::<Vec<_>>(),"nextCursor":"opaque +/?"})
                        }
                    }
                    "resources/templates/list" => {
                        json!({"resourceTemplates":[{"uriTemplate":"doc:{id}","name":"document"}]})
                    }
                    "resources/read" => match request["params"]["uri"].as_str().unwrap() {
                        "doc:picture" => {
                            json!({"contents":[{"uri":"doc:picture","mimeType":"image/png","blob":PNG}]})
                        }
                        "doc:binary" => {
                            json!({"contents":[{"uri":"doc:binary","mimeType":"application/octet-stream","blob":"AAAA"}]})
                        }
                        "doc:multiple" => {
                            json!({"contents":[{"uri":"doc:1","text":"one"},{"uri":"doc:2","text":"two"}]})
                        }
                        uri => {
                            json!({"contents":[{"uri":uri,"mimeType":"text/plain","text":if changed { "changed" } else { &source }}]})
                        }
                    },
                    other => panic!("resource-only fixture must not receive {other}"),
                };
                let body = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
                let (mime, body) = if sse {
                    ("text/event-stream", format!("data: {body}\n\n"))
                } else {
                    ("application/json", body)
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        Self { config, changed, requests, text, task }
    }
    async fn session(&self) -> McpSession {
        let session = McpSession::connect([&self.config], &rook_llm::Proxy::Direct, Default::default()).await;
        assert!(session.failures().is_empty(), "{:?}", session.failures());
        assert_eq!(session.report().servers[0].tools, 0);
        assert!(session.report().servers[0].resources);
        assert_eq!(session.servers().len(), 1);
        session
    }
}

#[tokio::test]
async fn resource_discovery_is_paged_bounded_attributed_and_rejects_a_changed_continuation() {
    for sse in [false, true] {
        let fixture = Fixture::new(sse).await;
        let session = fixture.session().await;
        let mut tools = ToolBox::default();
        tools.register_mcp_catalog(session.servers(), Default::default());
        let mut context = ToolContext::new(std::env::temp_dir());
        context.max_output_bytes = 4096;
        let local = tools.call(&context, "list_mcp_resources", &json!({})).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&local.content).unwrap()["servers"], json!(["documents"]));
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            1,
            "local discovery must not read catalogs or bodies"
        );
        let list = tools.get("list_mcp_resources").unwrap();
        assert!(matches!(
            list.risk(&json!({"server":"documents"})),
            Risk::External { claims_read_only: true, .. }
        ));
        let (policy, problems) =
            rook_tools::policy::Policy::compile(rook_tools::policy::Stance::ReadOnly, &[], &[], &[]);
        assert!(problems.is_empty());
        assert!(
            matches!(
                policy.decide(&list.risk(&json!({"server":"documents"}))),
                rook_tools::policy::Decision::Deny(_)
            ),
            "a server's read-only claim cannot bypass the normal external-call policy"
        );
        assert_eq!(list.mcp_source(&json!({"server":"documents"})), Some(("documents", "resources/list")));
        let mut args = json!({"server":"documents"});
        let mut count = 0;
        let first = loop {
            let reply = tools.call(&context, "list_mcp_resources", &args).await.unwrap();
            assert!(!reply.is_error, "{}", reply.content);
            assert!(reply.content.len() <= context.max_output_bytes);
            assert!(!reply.content.contains("not advertised"));
            let page: Value = serde_json::from_str(&reply.content).unwrap();
            count += page["entries"].as_array().unwrap().len();
            if page["next_offset"].is_null() {
                break page;
            }
            assert!(page["next_cursor"].is_null());
            args["offset"] = page["next_offset"].clone();
            args["page_sha256"] = page["page_sha256"].clone();
        };
        assert_eq!(count, 40, "every entry must stay reachable despite the display byte limit");
        assert_eq!(first["next_cursor"], "opaque +/?");
        let empty = tools
            .call(
                &context,
                "list_mcp_resources",
                &json!({"server":"documents","cursor":first["next_cursor"]}),
            )
            .await
            .unwrap();
        let page: Value = serde_json::from_str(&empty.content).unwrap();
        assert!(page["entries"].as_array().unwrap().is_empty());
        assert!(page["next_cursor"].is_null());
        let templates = tools
            .call(&context, "list_mcp_resources", &json!({"server":"documents","templates":true}))
            .await
            .unwrap();
        assert!(templates.content.contains("uriTemplate"));
        fixture.changed.store(true, Ordering::SeqCst);
        let error = tools.call(&context, "list_mcp_resources", &args).await.unwrap_err();
        assert!(error.to_string().contains("source changed"));
        assert!(fixture.requests.lock().unwrap().iter().all(|r| r["method"] != "resources/read"));
    }
}

#[tokio::test]
async fn resource_text_continuations_preserve_unicode_and_binary_payloads_do_not_become_text() {
    let fixture = Fixture::new(true).await;
    let session = fixture.session().await;
    let mut tools = ToolBox::default();
    tools.register_mcp_catalog(session.servers(), Default::default());
    let mut context = ToolContext::new(std::env::temp_dir());
    context.max_output_bytes = 2048;
    assert!(fixture.text.len() > context.max_output_bytes);
    let mut args = json!({"server":"documents","uri":"doc:русский"});
    let mut collected = String::new();
    loop {
        let reply = tools.call(&context, "read_mcp_resource", &args).await.unwrap();
        assert!(!reply.is_error, "{}", reply.content);
        assert!(reply.content.len() <= context.max_output_bytes);
        let body: Value = serde_json::from_str(&reply.content).unwrap();
        assert_eq!(body["server"], "documents");
        assert_eq!(body["uri"], "doc:русский");
        collected.push_str(body["text"].as_str().unwrap());
        if body["next_offset"].is_null() {
            break;
        }
        args["offset"] = body["next_offset"].clone();
        args["sha256"] = body["sha256"].clone();
    }
    assert_eq!(collected, fixture.text);
    fixture.changed.store(true, Ordering::SeqCst);
    assert!(
        tools
            .call(&context, "read_mcp_resource", &args)
            .await
            .unwrap_err()
            .to_string()
            .contains("source changed")
    );
    let image = tools
        .call(&context, "read_mcp_resource", &json!({"server":"documents","uri":"doc:picture"}))
        .await
        .unwrap();
    assert_eq!(image.images.len(), 1);
    assert_eq!(image.images[0].width, 1);
    assert!(!image.content.contains(PNG));
    let binary = tools
        .call(&context, "read_mcp_resource", &json!({"server":"documents","uri":"doc:binary"}))
        .await
        .unwrap();
    assert!(binary.content.contains("unsupported binary"));
    assert!(!binary.content.contains("AAAA"));
    let second = tools
        .call(&context, "read_mcp_resource", &json!({"server":"documents","uri":"doc:multiple","content":1}))
        .await
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&second.content).unwrap()["text"], "two");
    for args in [
        json!({"server":"other","uri":"doc:x"}),
        json!({"server":"documents","uri":""}),
        json!({"server":"documents","uri":"doc:a","offset":1}),
    ] {
        let reply = tools.call(&context, "read_mcp_resource", &args).await;
        assert!(reply.as_ref().map_or(true, |r| r.is_error));
    }
}

struct Model {
    seen: Mutex<Vec<Request>>,
}
#[async_trait]
impl Provider for Model {
    fn id(&self) -> &str {
        "scripted/resources"
    }
    fn context_window(&self) -> usize {
        32000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let classify = request
            .messages
            .first()
            .is_some_and(|m| m.content.starts_with("Classify whether an assistant's proposed last reply"));
        let mut seen = self.seen.lock().unwrap();
        let message = if classify {
            Message::assistant(r#"{"action":"finish"}"#)
        } else if seen.is_empty() {
            assert!(request.tools.iter().any(|t| t.name == "read_mcp_resource"));
            Message {
                tool_calls: vec![ToolCall {
                    id: "read-doc".into(),
                    name: "read_mcp_resource".into(),
                    arguments: json!({"server":"documents","uri":"doc:multiple","content":1}),
                }],
                ..Message::assistant("")
            }
        } else {
            Message::assistant("The second document contains two.")
        };
        if !classify {
            seen.push(request);
        }
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Usage::default(), model: self.id().into() })
    }
}

#[tokio::test]
async fn shared_equipment_exposes_resource_only_servers_and_the_ordinary_journal_replays_source_results() {
    let fixture = Fixture::new(false).await;
    let mcp = fixture.session().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.agent.context_window = Some(32000);
    config.agent.install_servers = false;
    config.sandbox.allow = vec!["documents__resources_read".into()];
    let rook = Rook::from_parts(
        rook_store::Store::open(root.path().join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        root.path().into(),
    );
    let session = rook.start_session("read MCP document").unwrap();
    let model = Arc::new(Model { seen: Mutex::new(Vec::new()) });
    let mut agent = AgentLoop::new(&rook, model.clone(), session);
    agent::equip(
        &mut agent,
        agent::servers_for(&rook.config, root.path()),
        &mcp,
        agent::jobs_for(&rook.config),
    );
    agent.run("Read the second resource content block").await.unwrap();
    let seen = model.seen.lock().unwrap();
    assert!(seen.len() >= 2);
    assert!(
        seen[1].messages.iter().any(|m| m.role == rook_llm::Role::Tool
            && m.content.contains("doc:2")
            && m.content.contains("two"))
    );
    drop(seen);
    drop(agent);
    let history = rook.transcript_page(session, &Default::default()).unwrap();
    let saved = serde_json::to_string(&history).unwrap();
    assert!(saved.contains("read_mcp_resource"));
    assert!(saved.contains("doc:2"));
    rook.store.flush().unwrap();
    drop(rook);
    let reopened = rook_store::Store::open(root.path().join("store")).unwrap();
    let events = reopened.events(session, 0, 100).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.record.kind == rook_store::EventKind::ToolResult
                && e.record.label == "read_mcp_resource")
    );
    assert!(!fixture.requests.lock().unwrap().iter().any(|r| r["method"] == "tools/list"));
}
