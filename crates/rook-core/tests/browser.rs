use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rook_core::{Config, Rook, agent::AgentLoop};
use rook_llm::{Message, Provider, Request, Response, Role, StopReason, ToolCall};
use serde_json::{Value, json};

struct Scripted {
    actions: Mutex<VecDeque<Value>>,
    children: Mutex<VecDeque<Value>>,
    observed: Mutex<Vec<String>>,
    image_requests: Mutex<Vec<usize>>,
}
fn body(content: &str) -> String {
    let value: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    value["rook_source"]["content"].as_str().unwrap_or(content).to_owned()
}
#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &str {
        "fixture/browser"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.image_requests.lock().unwrap().push(request.messages.iter().map(|m| m.images.len()).sum());
        if request.messages.first().is_some_and(|m| m.content.starts_with("Classify whether an assistant")) {
            return Ok(Response {
                message: Message::assistant(r#"{"action":"finish"}"#),
                stop_reason: StopReason::EndTurn,
                usage: Default::default(),
                model: self.id().into(),
            });
        }
        let latest = request.messages.iter().rev().find(|m| m.role == Role::Tool).map(|m| body(&m.content));
        if let Some(latest) = &latest {
            eprintln!("browser result: {latest}");
            self.observed.lock().unwrap().push(latest.clone());
        }
        let child = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .is_some_and(|m| m.content.contains("CHILD_BROWSER"));
        let action = if child {
            self.children.lock().unwrap().pop_front()
        } else {
            self.actions.lock().unwrap().pop_front()
        };
        let message = match action {
            Some(mut action) => {
                let name = action
                    .as_object_mut()
                    .unwrap()
                    .remove("$tool")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "browser".into());
                if let Some(label) = action.get("ref_label").and_then(Value::as_str).map(str::to_owned) {
                    let line = request
                        .messages
                        .iter()
                        .rev()
                        .filter(|m| m.role == Role::Tool)
                        .flat_map(|m| body(&m.content).lines().map(str::to_owned).collect::<Vec<_>>())
                        .find(|line| line.contains(&label) && line.contains("[ref="))
                        .unwrap_or_else(|| panic!("no {label} in the recorded snapshots: {latest:?}"));
                    let reference = line.split("[ref=").nth(1).unwrap().split(']').next().unwrap();
                    action.as_object_mut().unwrap().remove("ref_label");
                    action["ref"] = json!(reference);
                }
                if action["action"] == "dialog" && action.get("context").is_none() {
                    action["context"] = request
                        .messages
                        .iter()
                        .rev()
                        .filter(|m| m.role == Role::Tool)
                        .find_map(|m| {
                            serde_json::from_str::<Value>(&body(&m.content))
                                .ok()
                                .and_then(|status| status["context"].as_str().map(str::to_owned))
                        })
                        .expect("the agent must read current browser status before responding to a dialog")
                        .into();
                }
                Message {
                    tool_calls: vec![ToolCall {
                        id: format!("action-{}", self.actions.lock().unwrap().len()),
                        name,
                        arguments: action,
                    }],
                    ..Message::assistant("")
                }
            }
            None => Message::assistant("Browser observations are recorded in the tool results."),
        };
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Default::default(), model: self.id().into() })
    }
}
fn open(root: &std::path::Path, config: Config) -> Rook {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        rook_skills::SkillIndex::default(),
        workspace,
    )
}
fn provider(actions: Vec<Value>) -> Arc<Scripted> {
    Arc::new(Scripted {
        actions: Mutex::new(actions.into()),
        children: Mutex::new(VecDeque::new()),
        observed: Mutex::new(Vec::new()),
        image_requests: Mutex::new(Vec::new()),
    })
}

#[cfg(unix)]
#[tokio::test]
async fn delegated_browser_actions_use_a_child_context_and_preserve_the_parents_page() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config {
        browser: rook_core::browser::Settings {
            enabled: true,
            command: "python3".into(),
            args: vec![
                format!("{}/tests/fixtures/browser_mcp.py", env!("CARGO_MANIFEST_DIR")),
                root.path().join("calls.jsonl").display().to_string(),
            ],
            startup_timeout_secs: 120,
            ..Default::default()
        },
        ..Default::default()
    };
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    config.agent.lazy_tools = false;
    let rook = open(root.path(), config);
    let session = rook.start_session("parent browser").unwrap();
    let provider = provider(vec![
        json!({"action":"navigate","url":"http://localhost/parent"}),
        json!({"$tool":"delegate","task":"CHILD_BROWSER: open your own local page and close your browser.","context":"none"}),
        json!({"action":"snapshot"}),
        json!({"action":"close"}),
    ]);
    provider
        .children
        .lock()
        .unwrap()
        .extend([json!({"action":"navigate","url":"http://localhost/child"}), json!({"action":"close"})]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    let result = agent
        .run("Inspect a local page, delegate an independent inspection, then inspect your page again.")
        .await
        .unwrap();
    assert!(!result.delegated.is_empty(), "{result:?}");
    let page = rook.transcript(session, 0, 128, 16384).unwrap();
    let replies: Vec<_> = page.iter().filter(|e| e.kind == "tool-result" && e.label == "browser").collect();
    assert!(replies[0].body.contains("/parent"));
    assert!(replies[1].body.contains("/parent"));
    assert!(!replies[1].body.contains("/child"));
    let child = rook_store::parse_session_id(&result.delegated[0]).unwrap();
    let child_page = rook.transcript(child, 0, 64, 16384).unwrap();
    let observation = child_page.iter().find(|e| e.kind == "tool-result" && e.label == "browser").unwrap();
    assert!(observation.body.contains("/child"));
    assert!(observation.body.contains(&rook_store::format_session_id(child)));
    assert!(!observation.body.contains(&rook_store::format_session_id(session)));
}

#[tokio::test]
async fn browser_denial_precedes_startup_and_is_saved_as_an_ordinary_tool_result() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.browser.enabled = true;
    config.browser.command = "rook-browser-that-is-not-installed".into();
    config.agent.lazy_tools = false;
    let rook = open(root.path(), config);
    let session = rook.start_session("browser approval").unwrap();
    let provider = provider(vec![
        json!({"action":"navigate","url":"http://localhost/approval-test"}),
        json!({"action":"dialog","context":"unadmitted:context","accept":true}),
    ]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    let cost = agent
        .tool_specs()
        .iter()
        .map(|t| (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4))
        .sum::<usize>();
    // Resource paging and browser diagnostics add bounded schema fields, not another tool catalog.
    assert!(cost < 3000, "enabled browser catalog costs {cost} tokens");
    let result = agent.run("Inspect the local page using browser navigation").await.unwrap();
    assert!(result.tools_called.contains(&"browser".into()));
    let observed = provider.observed.lock().unwrap().join("\n");
    assert!(observed.contains("not run") || observed.contains("refused"), "{observed}");
    assert!(!observed.contains("browser setup failed"), "denied calls cannot spawn an executable");
    let events = rook.transcript(session, 0, 100, 16384).unwrap();
    assert!(
        events.iter().any(|e| e.kind == "tool-result" && e.label == "browser" && e.body.contains("not run"))
    );
    let receipts = rook.execution(session).unwrap();
    assert!(!receipts.is_empty());
    assert!(receipts.iter().all(|r| r.pending.is_none()));
}

#[tokio::test]
async fn browser_observations_recheck_external_state_but_stable_answers_and_mutations_still_stop() {
    struct StableBrowser(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait]
    impl rook_tools::Tool for StableBrowser {
        fn name(&self) -> &str {
            "browser"
        }
        fn spec(&self) -> rook_llm::ToolSpec {
            rook_llm::ToolSpec {
                name: "browser".into(),
                description: "Owned stable browser fixture".into(),
                parameters: json!({"type":"object"}),
            }
        }
        fn risk(&self, _: &Value) -> rook_tools::policy::Risk {
            rook_tools::policy::Risk::ReadOnly
        }
        async fn call(
            &self,
            _: &rook_tools::ToolContext,
            _: &Value,
        ) -> rook_tools::Result<rook_tools::ToolOutcome> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(rook_tools::ToolOutcome::ok("unchanged page"))
        }
    }
    for (action, attempts, expected, dispatched) in [
        (json!({"action":"snapshot"}), 5, "end_turn", 5),
        (json!({"action":"snapshot"}), 24, "looping", 20),
        (json!({"action":"click","ref":"fixture"}), 5, "looping", 2),
        (json!({"action":"dialog","context":"fixture:context","accept":true}), 5, "looping", 2),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.agent.lazy_tools = false;
        config.agent.max_steps = 32;
        let rook = open(root.path(), config);
        let session = rook.start_session("browser input proof").unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut agent = AgentLoop::new(&rook, provider(vec![action; attempts]), session);
        agent.tools.register(Arc::new(StableBrowser(count.clone())));
        let outcome = agent
            .run("Observe the owned browser fixture; do not assume workspace files prove its page state.")
            .await
            .unwrap();
        assert_eq!(outcome.stopped, expected, "{outcome:?}");
        assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), dispatched);
    }
}

#[tokio::test]
#[ignore = "requires pinned Playwright MCP and installed Chromium; run explicitly as documented"]
async fn native_dialogs_resume_the_page_through_the_ordinary_agent_loop() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await;
            let html = r#"<!doctype html><title>Owned Rook dialogs</title><button id="alert">Alert</button><button id="confirm">Confirm</button><button id="prompt">Prompt</button><p id="result">Waiting</p><script>
document.querySelector('#alert').onclick=()=>{alert('Owned alert');document.querySelector('#result').textContent='alert:closed';};
document.querySelector('#confirm').onclick=()=>{const result=confirm('Owned confirm');document.querySelector('#result').textContent='confirm:'+result;};
document.querySelector('#prompt').onclick=()=>{const result=prompt('Owned prompt','default');document.querySelector('#result').textContent='prompt:'+JSON.stringify(result);};
</script>"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                html.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.browser.enabled = true;
    config.browser.startup_timeout_secs = 120;
    if let Ok(path) = std::env::var("ROOK_BROWSER_MCP_CLI") {
        config.browser.command = "node".into();
        config.browser.args = vec![path];
    }
    config.agent.lazy_tools = false;
    config.agent.max_steps = 48;
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let rook = open(root.path(), config);
    let session = rook.start_session("native dialogs").unwrap();
    let mut actions = vec![json!({"action":"navigate","url":format!("http://{address}/")})];
    let cases = [
        ("Alert", true, None, "alert:closed"),
        ("Confirm", false, None, "confirm:false"),
        ("Confirm", true, None, "confirm:true"),
        ("Prompt", true, Some("Rook answer"), "prompt:\"Rook answer\""),
        ("Prompt", false, None, "prompt:null"),
        ("Prompt", true, Some(""), "prompt:\"\""),
    ];
    for (label, accept, text, expected) in cases {
        actions.push(json!({"action":"snapshot"}));
        actions.push(json!({"action":"click","ref_label":format!("button \"{label}\"")}));
        actions.push(json!({"action":"status"}));
        let mut dialog = json!({"action":"dialog","accept":accept});
        if let Some(text) = text {
            dialog["text"] = json!(text);
        }
        actions.push(dialog);
        actions.push(json!({"action":"wait","text":expected}));
        actions.push(json!({"action":"snapshot"}));
    }
    actions.push(json!({"action":"close"}));
    let provider = provider(actions);
    let result = tokio::time::timeout(Duration::from_secs(240),AgentLoop::new(&rook,provider.clone(),session).run("Respond to the owned alert, accept and dismiss confirm, and answer, dismiss and clear the prompt. Observe each result and close.")).await.unwrap().unwrap();
    app.abort();
    let _ = app.await;
    assert_eq!(result.stopped, "end_turn", "{result:?}");
    let observed = provider.observed.lock().unwrap().join("\n");
    for (_, _, _, expected) in cases {
        assert!(observed.contains(expected), "missing {expected}: {observed}");
    }
    assert!(!observed.contains("outcome is unknown"), "{observed}");
    assert!(!observed.contains("No dialog visible"), "{observed}");
    assert!(rook.execution(session).unwrap().iter().all(|r| r.pending.is_none()));
    assert!(std::fs::read_dir(&rook.workspace).unwrap().next().is_none());
}

#[tokio::test]
#[ignore = "requires the pinned Playwright MCP package and installed Chromium; run explicitly as documented"]
async fn real_chromium_form_and_screenshot_pass_through_the_ordinary_agent_loop() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let submissions = Arc::new(Mutex::new(Vec::<String>::new()));
    let saved = submissions.clone();
    let app = tokio::spawn(async move {
        let mut workers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted=listener.accept()=>{
                    let (mut socket,_)=accepted.unwrap();let saved=saved.clone();
                    workers.spawn(async move {
                        let mut raw=Vec::new();let (start,length)=loop {
                            assert!(raw.len()<65536);let mut part=[0u8;2048];let n=socket.read(&mut part).await.unwrap();if n==0{return;}raw.extend_from_slice(&part[..n]);
                            if let Some(end)=raw.windows(4).position(|w|w==b"\r\n\r\n") {
                                let header=String::from_utf8_lossy(&raw[..end]);let length=header.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).map(|n|n.parse::<usize>().unwrap()).unwrap_or(0);
                                assert!(length<=16384);break(end+4,length);
                            }
                        };
                        while raw.len()<start+length {let mut part=[0u8;2048];let n=socket.read(&mut part).await.unwrap();if n==0{return;}raw.extend_from_slice(&part[..n]);}
                        let posting=raw.starts_with(b"POST /save ");
                        let content=if posting {
                            let text=String::from_utf8(raw[start..start+length].to_vec()).unwrap();saved.lock().unwrap().push(text.clone());format!("Saved: {text}")
                        }else {r#"<!doctype html><title>Rook browser fixture</title><label>Message<input id="message"></label><button id="save">Save</button><p id="result">Waiting</p><script>document.querySelector('#save').onclick=async()=>{const response=await fetch('/save',{method:'POST',body:document.querySelector('#message').value});document.querySelector('#result').textContent=await response.text();};</script>"#.into()};
                        let header=format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",if posting{"text/plain; charset=utf-8"}else{"text/html; charset=utf-8"},content.len());
                        let _=socket.write_all(header.as_bytes()).await;let _=socket.write_all(content.as_bytes()).await;
                    });
                },
                _=workers.join_next(),if !workers.is_empty()=>{}
            }
        }
    });
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.browser.enabled = true;
    config.browser.startup_timeout_secs = 120;
    config.browser.include_images =
        std::env::var("ROOK_BROWSER_TEST_INCLUDE_IMAGES").as_deref() == Ok("true");
    // Optional direct local launcher avoids the npm installation wrapper in this
    // runtime check; ordinary product defaults use the pinned npx package.
    if let Ok(path) = std::env::var("ROOK_BROWSER_MCP_CLI") {
        config.browser.command = "node".into();
        config.browser.args = vec![path];
    }
    config.agent.lazy_tools = false;
    config.agent.max_steps = 12;
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let expect_pixels = config.browser.include_images;
    let rook = open(root.path(), config);
    let session = rook.start_session("real browser").unwrap();
    let provider = provider(vec![
        json!({"action":"navigate","url":format!("http://{address}/")}),
        json!({"action":"snapshot"}),
        json!({"action":"type","ref_label":"textbox","text":"rook-browser-check"}),
        json!({"action":"click","ref_label":"button"}),
        json!({"action":"wait","text":"Saved: rook-browser-check"}),
        json!({"action":"snapshot"}),
        json!({"action":"screenshot"}),
        json!({"action":"close"}),
    ]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    let result=tokio::time::timeout(Duration::from_secs(240),agent.run("Use browser to fill Message with rook-browser-check, save, observe the marker, take a screenshot and close.")).await.unwrap().unwrap();
    app.abort();
    let _ = app.await;
    assert_eq!(result.stopped, "end_turn", "{result:?}");
    assert_eq!(submissions.lock().unwrap().as_slice(), ["rook-browser-check"]);
    let observed = provider.observed.lock().unwrap().join("\n");
    assert!(observed.contains("Saved: rook-browser-check"), "{observed}");
    assert!(!observed.contains("outcome is unknown"), "{observed}");
    let mut entries = Vec::new();
    let mut from = Some(0);
    for _ in 0..16 {
        let page = rook
            .transcript_page(session, &rook_core::transcript::PageRequest { from, ..Default::default() })
            .unwrap();
        entries.extend(page.items);
        match page.next {
            Some(next) => {
                assert!(next > from.unwrap());
                from = Some(next);
            }
            None => {
                from = None;
                break;
            }
        }
    }
    assert!(from.is_none(), "the fixture must inspect the complete retained turn");
    let mut actions = Vec::new();
    let mut screenshots = 0;
    for entry in entries {
        if entry.kind == "tool-call" && entry.label == "browser" {
            actions.push(
                serde_json::from_str::<Value>(&entry.body).unwrap()["action"].as_str().unwrap().to_owned(),
            );
        }
        if let Some(details) = entry.tool_details
            && let rook_core::transcript::ToolResultDetails::Mcp { server, remote_tool, images, .. } =
                details.result
        {
            assert_eq!(server, "browser");
            if remote_tool == "browser_take_screenshot" {
                assert!(!images.is_empty());
                assert!(images.iter().all(|i| i.width > 0 && i.height > 0));
                screenshots += 1;
            }
        }
    }
    assert_eq!(actions, ["navigate", "snapshot", "type", "click", "wait", "snapshot", "screenshot", "close"]);
    assert_eq!(screenshots, 1);
    if expect_pixels {
        assert!(provider.image_requests.lock().unwrap().iter().any(|count| *count > 0));
        assert!(!observed.contains("browser.include_images=false"));
    } else {
        assert!(
            provider.image_requests.lock().unwrap().iter().all(|count| *count == 0),
            "text-only delivery still retains the screenshot without sending its pixels"
        );
        assert!(observed.contains("browser.include_images=false"));
    }
    assert!(
        std::fs::read_dir(&rook.workspace).unwrap().next().is_none(),
        "browser artifacts cannot modify the user's workspace"
    );
    assert!(rook.execution(session).unwrap().iter().all(|r| r.pending.is_none()));
}
