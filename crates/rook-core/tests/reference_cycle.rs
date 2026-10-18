#[path = "../../rook-mcp/tests/support/catalog_http.rs"]
mod catalog_http;
use catalog_http::Fixture;
use rook_core::{
    Config, Rook, Vault,
    agent::{AgentLoop, policy_for},
    mcp_apps::Request as AppRequest,
};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
use serde_json::json;
use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

fn engine(root: &std::path::Path, fixture: &Fixture, deferred: bool) -> Rook {
    let mut config = Config::default();
    config.agent.install_servers = false;
    config.agent.plan_first = false;
    config.agent.one_script = false;
    config.memory.enabled = false;
    config.web.enabled = false;
    config.learning.automatic = false;
    config.model_catalog.cache_enabled = false;
    config.mcp_connections.apps_enabled = true;
    if deferred {
        config.mcp_catalog.max_server_tools = 0;
    }
    config.mcp.push(rook_mcp::ServerConfig {
        name: "catalog".into(),
        url: Some(fixture.url.clone()),
        refresh_tools: true,
        ..Default::default()
    });
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "1"),
        Default::default(),
        root.into(),
    )
}
struct Script {
    step: Mutex<usize>,
    deferred: bool,
}
#[async_trait::async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "scripted/catalog"
    }
    fn context_window(&self) -> usize {
        64000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let classifier = request
            .messages
            .iter()
            .any(|m| m.role == rook_llm::Role::System && m.content.contains("Classify whether an assistant"));
        let mut message = Message::user("");
        message.role = rook_llm::Role::Assistant;
        if classifier {
            message.content = "{\"action\":\"finish\"}".into();
        } else {
            let mut step = self.step.lock().unwrap();
            assert!(
                request
                    .messages
                    .iter()
                    .any(|m| m.content.contains("Advance the catalog, discover the new tool and call it")),
                "catalog refresh must preserve the user's task"
            );
            if *step > 0 {
                assert!(
                    request
                        .messages
                        .iter()
                        .any(|m| m.role == rook_llm::Role::Tool && m.content.contains("advance")),
                    "catalog refresh must preserve admitted tool results"
                );
            }
            let call = match *step {
                0 => {
                    assert!(request.tools.iter().any(|tool| tool.name == "mcp_call"));
                    if self.deferred {
                        Some(("mcp_call", json!({"name":"catalog__advance","arguments":{}})))
                    } else {
                        Some(("catalog__advance", json!({})))
                    }
                }
                1 => {
                    assert!(!request.tools.iter().any(|tool| tool.name == "catalog__old"));
                    assert_eq!(request.tools.iter().any(|tool| tool.name == "catalog__new"), !self.deferred);
                    Some(("mcp_tools", json!({"query":"new"})))
                }
                2 => {
                    let result =
                        request.messages.iter().rev().find(|m| m.role == rook_llm::Role::Tool).unwrap();
                    assert!(result.content.contains("catalog__new"), "{}", result.content);
                    Some(("mcp_call", json!({"name":"catalog__new","arguments":{}})))
                }
                3 => {
                    message.content = "ok".into();
                    None
                }
                _ => panic!(
                    "unexpected additional work-model generation: {:?}",
                    request
                        .messages
                        .iter()
                        .map(|m| (m.role, m.content.chars().take(140).collect::<String>()))
                        .collect::<Vec<_>>()
                ),
            };
            if let Some((name, arguments)) = call {
                message.tool_calls.push(ToolCall {
                    id: format!("call-{step}"),
                    name: name.into(),
                    arguments,
                });
            }
            *step += 1;
        }
        Ok(Response {
            stop_reason: if message.tool_calls.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolUse
            },
            message,
            usage: Usage { input_tokens: 10, output_tokens: 2, ..Default::default() },
            model: "fixture".into(),
        })
    }
}

#[tokio::test]
async fn one_agent_turn_discovers_and_calls_new_mcp_tools_in_direct_and_deferred_catalogs() {
    for deferred in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let fixture = Fixture::new(false).await;
        let rook = engine(root.path(), &fixture, deferred);
        let manager = rook.connect_mcp().await;
        let session = rook.start_session("catalog update within one turn").unwrap();
        let provider = Arc::new(Script { step: Mutex::new(0), deferred });
        let mut agent = AgentLoop::new(&rook, provider.clone(), session);
        agent.tools.register_mcp_catalog(manager.servers(), rook.config.mcp_catalog);
        // Exercise the ordinary chat stance with explicit fixture-tool grants;
        // autonomous goal checkers use a separate scripted protocol.
        let (policy, issues) = rook_tools::policy::Policy::compile(
            rook_tools::policy::Stance::Assist,
            &[],
            &[],
            &rook.config.sandbox.deny,
        );
        assert!(issues.is_empty(), "{issues:?}");
        policy.grant_for_run("catalog__advance");
        policy.grant_for_run("catalog__new");
        agent.policy = Arc::new(policy);
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            agent.run("Advance the catalog, discover the new tool and call it"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.reply, "ok");
        assert_eq!(*provider.step.lock().unwrap(), 4);
        assert_eq!(*fixture.state.calls.lock().unwrap(), ["advance", "new"]);
        let report = manager.report();
        assert_eq!(report.servers[0].tools, 2);
        assert_eq!(report.servers[0].catalog.generation, 2);
        assert!(!report.servers[0].catalog.stale);
        manager.shutdown().await;
    }
}

#[tokio::test]
async fn app_consent_is_retired_on_catalog_change_even_when_refresh_fails_and_cannot_cross_reconnect() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(false).await;
    let rook = engine(root.path(), &fixture, false);
    let manager = rook.connect_mcp().await;
    let session = rook.start_session("catalog-bound app consent").unwrap();
    let policy = policy_for(&rook.config);
    let vault = Vault::empty();
    let open = |generation| AppRequest::Open {
        server: "catalog".into(),
        uri: "ui://catalog/view".into(),
        generation,
    };
    let app = rook.mcp_app_request(session, &manager, &policy, &vault, open(1)).await.unwrap();
    let id = app["app"].as_str().unwrap().to_owned();
    let pending = rook
        .mcp_app_request(
            session,
            &manager,
            &policy,
            &vault,
            AppRequest::Prepare { app: id.clone(), tool: "old".into(), arguments: json!({}) },
        )
        .await
        .unwrap();
    let server = manager.servers()[0].0.clone();
    server.call_tool("advance", &json!({})).await.unwrap();
    assert!(
        rook.mcp_app_request(
            session,
            &manager,
            &policy,
            &vault,
            AppRequest::Confirm { app: id, nonce: pending["nonce"].as_str().unwrap().into(), approved: true }
        )
        .await
        .is_err()
    );
    assert!(
        rook.mcp_app_request(session, &manager, &policy, &vault, open(1)).await.is_err(),
        "stale declarations cannot open a new app"
    );
    server.refresh_tool_catalog().await;
    let app = rook.mcp_app_request(session, &manager, &policy, &vault, open(1)).await.unwrap();
    let id = app["app"].as_str().unwrap().to_owned();
    let pending = rook
        .mcp_app_request(
            session,
            &manager,
            &policy,
            &vault,
            AppRequest::Prepare { app: id.clone(), tool: "new".into(), arguments: json!({}) },
        )
        .await
        .unwrap();
    fixture.state.fail.store(true, Ordering::SeqCst);
    server.call_tool("advance", &json!({})).await.unwrap();
    server.refresh_tool_catalog().await;
    assert!(manager.report().servers[0].catalog.error.is_some());
    assert!(
        rook.mcp_app_request(
            session,
            &manager,
            &policy,
            &vault,
            AppRequest::Confirm { app: id, nonce: pending["nonce"].as_str().unwrap().into(), approved: true }
        )
        .await
        .is_err()
    );
    fixture.state.fail.store(false, Ordering::SeqCst);
    manager.reconnect("catalog", rook.mcp_servers(), &rook.config.proxy.for_mcp()).await.unwrap();
    assert!(rook.mcp_app_request(session, &manager, &policy, &vault, open(1)).await.is_err());
    let app = rook.mcp_app_request(session, &manager, &policy, &vault, open(2)).await.unwrap();
    let id = app["app"].as_str().unwrap().to_owned();
    let pending = rook
        .mcp_app_request(
            session,
            &manager,
            &policy,
            &vault,
            AppRequest::Prepare { app: id.clone(), tool: "new".into(), arguments: json!({}) },
        )
        .await
        .unwrap();
    let nonce = pending["nonce"].as_str().unwrap().to_owned();
    rook.mcp_app_request(
        session,
        &manager,
        &policy,
        &vault,
        AppRequest::Confirm { app: id.clone(), nonce: nonce.clone(), approved: true },
    )
    .await
    .unwrap();
    assert!(
        rook.mcp_app_request(
            session,
            &manager,
            &policy,
            &vault,
            AppRequest::Confirm { app: id, nonce, approved: true }
        )
        .await
        .is_err()
    );
    assert_eq!(*fixture.state.calls.lock().unwrap(), ["advance", "advance", "new"]);
    manager.shutdown().await;
    server.shutdown().await;
}

#[tokio::test]
async fn enabling_refresh_does_not_probe_tools_on_resource_only_connections() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(true).await;
    fixture.state.resources_only.store(true, Ordering::SeqCst);
    let rook = engine(root.path(), &fixture, false);
    let manager = rook.connect_mcp().await;
    let mut tools = rook_tools::ToolBox::default();
    tools.register_mcp_catalog(manager.servers(), rook.config.mcp_catalog);
    assert!(!tools.mcp_catalog_changed());
    tools.refresh_mcp_catalog(rook.config.mcp_catalog, Duration::from_secs(10)).await;
    assert_eq!(fixture.state.lists.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 0);
    assert!(manager.report().servers[0].resources);
    assert_eq!(manager.report().servers[0].tools, 0);
    assert!(!tools.names().is_empty(), "resource discovery remains installed");
    manager.shutdown().await;
}

#[test]
fn background_family_pages_are_lazy_and_saved_activity_never_proves_an_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.branches.page_entries = 1;
    config.branches.scan_sessions = 2;
    let rook = Rook::from_parts(
        rook_store::Store::open(dir.path().join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "1"),
        Default::default(),
        dir.path().into(),
    );
    let parent = rook.start_session("parent").unwrap();
    let child = rook.fork_for_subtask(parent, "child").unwrap();
    let grandchild = rook.fork_for_subtask(child, "grandchild").unwrap();
    rook.fork_for_subtask(parent, "second child").unwrap();
    let saved = rook_core::activity::Tracker::new(child, 30).snapshot(true);
    rook_core::activity::begin(&rook.store, child, &saved).unwrap();
    let page = rook_core::background::page(&rook, child, &Default::default(), None).unwrap();
    assert_eq!(page.tree.selected.id, rook_store::format_session_id(child));
    assert_eq!(page.tree.ancestors.len(), 1);
    let selected = page.entries.iter().find(|e| e.session == page.tree.selected.id).unwrap();
    assert!(selected.owner_live.is_none());
    assert!(selected.activity.as_ref().unwrap().owner_live.is_none());
    assert!(!page.jobs_observed);
    let mut query = Default::default();
    let mut found = Vec::new();
    let mut pages = 0;
    loop {
        let page = rook_core::background::page(&rook, child, &query, None).unwrap();
        assert!(page.tree.children.len() <= 1 && page.tree.scanned_sessions <= 2);
        found.extend(page.tree.children.iter().map(|n| n.id.clone()));
        pages += 1;
        if let Some(next) = page.tree.next {
            query = rook_core::branches::Query { after: Some(next) };
        } else {
            break;
        }
        assert!(pages < 10);
    }
    assert!(pages > 1, "the configured scan allowance was actually exceeded");
    assert_eq!(found, [rook_store::format_session_id(grandchild)]);
    assert!(
        rook_core::background::page(
            &rook,
            parent,
            &rook_core::branches::Query { after: Some("bad".into()) },
            None
        )
        .is_err()
    );
}
