//! Optional, conversation-bound app leases. UI calls never inherit blanket approval.
use crate::{CoreError, McpSession, Result, Rook, Vault};
use rook_tools::{
    Tool, ToolContext,
    policy::{Decision, Policy},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_HTML: usize = 262144;
const MAX_ARGS: usize = 32768;
#[derive(Default)]
pub(crate) struct Pool(Mutex<BTreeMap<String, App>>);
struct App {
    session: u128,
    server: Arc<rook_mcp::Server>,
    generation: u64,
    catalog_generation: u64,
    uri: String,
    opened: Instant,
    uncertain: bool,
    declined: bool,
    pending: Option<Pending>,
}
struct Pending {
    nonce: String,
    tool: String,
    args: Value,
    at: Instant,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    Open { server: String, uri: String, generation: u64 },
    Close { app: String },
    Prepare { app: String, tool: String, arguments: Value },
    Confirm { app: String, nonce: String, approved: bool },
}
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 65536 {
        return Err(bad("MCP App request exceeds 64 KiB"));
    }
    if text.trim().is_empty() {
        return Ok(Request::List);
    }
    serde_json::from_str(text).map_err(Into::into)
}
fn bad(message: &str) -> CoreError {
    CoreError::Other(message.into())
}
fn identity() -> String {
    rook_store::format_session_id(rook_store::new_session_id())
}
fn uri_of(tool: &rook_mcp::ToolDescriptor) -> Option<&str> {
    tool.meta
        .pointer("/ui/resourceUri")
        .and_then(Value::as_str)
        .filter(|uri| uri.starts_with("ui://") && uri.len() <= 4096 && !uri.chars().any(char::is_control))
}
fn permitted(policy: &Policy, tool: &dyn Tool, args: &Value) -> Result<()> {
    match policy.decide(&tool.risk(args)) {
        Decision::Deny(_) => Err(bad("MCP App operation is denied by the current permission policy")),
        _ => Ok(()),
    }
}
impl Rook {
    pub async fn mcp_app_request(
        &self,
        session: u128,
        manager: &McpSession,
        policy: &Policy,
        vault: &Vault,
        request: Request,
    ) -> Result<Value> {
        if !self.config.mcp_connections.apps_enabled {
            return Err(bad("MCP Apps are disabled; explicitly enable mcp_connections.apps_enabled"));
        }
        if self.config.sandbox.network_policy.enabled {
            return Err(bad("MCP Apps are unavailable with mandatory domain containment"));
        }
        let owner = self
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
        if std::path::Path::new(&owner.workspace) != self.workspace {
            return Err(bad("MCP App session belongs to another workspace"));
        }
        if matches!(request, Request::Prepare { .. } | Request::Confirm { approved: true, .. })
            && crate::experiments::frozen(self, session)?
        {
            return Err(bad("measured experiment source is frozen; app tools cannot change it"));
        }
        let connections = manager.bound_servers();
        let generation = |name: &str| {
            connections
                .iter()
                .find(|(server, _, _)| server.name() == name)
                .and_then(|(server, _, _)| manager.connection_generation(server))
        };
        {
            let mut apps = manager.apps.0.lock().unwrap_or_else(|e| e.into_inner());
            apps.retain(|_, app| {
                app.opened.elapsed() < Duration::from_secs(600)
                    && generation(app.server.name()) == Some(app.generation)
                    && connections.iter().any(|(server, revision, _)| {
                        Arc::ptr_eq(server, &app.server) && *revision == app.catalog_generation && {
                            let current = server.catalog_status();
                            current.generation == *revision && !current.stale
                        }
                    })
            });
        }
        if let Request::List = request {
            let mut items = Vec::new();
            let mut remaining = MAX_HTML;
            for (server, _, tools) in &connections {
                for tool in tools {
                    if let Some(uri) = uri_of(tool) {
                        if tool.name.len() > 256 || server.name().len() > 256 {
                            return Err(bad("MCP App source identifiers exceed their byte limits"));
                        }
                        if items.len() >= 128 {
                            return Err(bad("too many MCP Apps; narrow configured servers"));
                        }
                        let fallback = vault.redact(&rook_tools::elide_middle(&tool.description, 512));
                        let entry = json!({"server":server.name(),"uri":uri,"tool":tool.name,"generation":generation(server.name()),"fallback":fallback});
                        let charged = crate::persistence::encode_with_limit(&entry, remaining)?;
                        remaining = remaining.saturating_sub(charged.len());
                        items.push(entry);
                    }
                }
            }
            return Ok(
                json!({"items":items,"interactive":"browser frontend; CLI/TUI show static descriptions"}),
            );
        }
        if let Request::Open { server, uri, generation: expected } = request {
            if generation(&server) != Some(expected) {
                return Err(bad("MCP connection changed; refresh the app list"));
            }
            let (connection, catalog_generation, tools) = connections
                .iter()
                .find(|(s, _, _)| s.name() == server)
                .ok_or_else(|| bad("MCP server is not connected"))?;
            let current = connection.catalog_status();
            if current.stale || current.generation != *catalog_generation {
                return Err(bad(
                    "MCP tool catalog changed; reconnect or run another turn, then refresh the app list",
                ));
            }
            if !tools.iter().any(|tool| uri_of(tool) == Some(uri.as_str())) {
                return Err(bad("UI resource was not declared by this server's installed tools"));
            }
            let risk = rook_tools::policy::Risk::External {
                name: rook_tools::mcp::namespaced(&server, "read_mcp_resource"),
                claims_read_only: true,
            };
            if matches!(policy.decide(&risk), Decision::Deny(_)) {
                return Err(bad("UI resource read denied by current policy"));
            }
            if manager.apps.0.lock().unwrap_or_else(|e| e.into_inner()).len() >= 8 {
                return Err(bad("at most eight MCP Apps can be open; close an existing view"));
            }
            let read = connection
                .read_resource(&uri)
                .await
                .map_err(|_| bad("MCP App resource read failed; inspect connection status"))?;
            if read.contents.len() != 1 {
                return Err(bad("MCP App requires exactly one HTML resource"));
            }
            let resource = &read.contents[0];
            if resource.uri != uri || resource.mime_type.as_deref() != Some("text/html;profile=mcp-app") {
                return Err(bad("resource does not match the declared MCP App URI/type"));
            }
            let html = match (&resource.text, &resource.blob) {
                (Some(text), None) if text.len() <= MAX_HTML => text.clone(),
                (None, Some(blob)) if blob.len() <= MAX_HTML * 4 / 3 + 4 => {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(blob)
                        .map_err(|_| bad("invalid HTML base64"))?;
                    if bytes.len() > MAX_HTML {
                        return Err(bad("MCP App exceeds 256 KiB"));
                    }
                    String::from_utf8(bytes).map_err(|_| bad("MCP App HTML must be UTF-8"))?
                }
                _ => return Err(bad("MCP App exceeds 256 KiB or has no HTML")),
            };
            let id = identity();
            let mut apps = manager.apps.0.lock().unwrap_or_else(|e| e.into_inner());
            if apps.len() >= 8 {
                return Err(bad("MCP App capacity was reached while opening"));
            }
            apps.insert(
                id.clone(),
                App {
                    session,
                    server: connection.clone(),
                    generation: expected,
                    catalog_generation: *catalog_generation,
                    uri: uri.clone(),
                    opened: Instant::now(),
                    uncertain: false,
                    declined: false,
                    pending: None,
                },
            );
            return Ok(
                json!({"app":id,"server":server,"uri":uri,"generation":expected,"version":crate::AGENT_VERSION,"html":vault.redact(&html),"sandbox":{"connectDomains":[],"resourceDomains":[],"frameDomains":[],"baseUriDomains":[]}}),
            );
        }
        let (id, close) = match &request {
            Request::Close { app } => (app.clone(), true),
            Request::Prepare { app, .. } | Request::Confirm { app, .. } => (app.clone(), false),
            _ => return Err(bad("invalid app operation")),
        };
        let (server, pending, name) = {
            let mut apps = manager.apps.0.lock().unwrap_or_else(|e| e.into_inner());
            let app = apps
                .get_mut(&id)
                .filter(|app| app.session == session)
                .ok_or_else(|| bad("MCP App is closed, expired or belongs to another session"))?;
            if close {
                apps.remove(&id);
                return Ok(json!({"closed":true}));
            }
            if app.declined {
                return Err(bad(
                    "requests from this view were declined or expired; close and explicitly reopen the app",
                ));
            }
            if app.uncertain {
                return Err(bad(
                    "previous app call outcome is uncertain; inspect effects and close this view",
                ));
            }
            let tools = connections
                .iter()
                .find(|(s, _, _)| Arc::ptr_eq(s, &app.server))
                .map(|(_, _, t)| t)
                .ok_or_else(|| bad("MCP connection changed"))?;
            match request {
                Request::Prepare { tool, arguments, .. } => {
                    crate::persistence::encode_with_limit(&arguments, MAX_ARGS)?;
                    if let Some(pending) = &app.pending {
                        if pending.at.elapsed() >= Duration::from_secs(120) {
                            app.declined = true;
                            app.pending = None;
                            return Err(bad("app request expired; close and explicitly reopen the app"));
                        }
                        return Err(bad("answer or dismiss the pending app request first"));
                    }
                    let descriptor = tools
                        .iter()
                        .find(|d| d.name == tool)
                        .ok_or_else(|| bad("app may call only installed tools on its own server"))?;
                    let validator = jsonschema::options()
                        .build(&descriptor.input_schema)
                        .map_err(|_| bad("MCP tool schema could not be validated"))?;
                    if !validator.is_valid(&arguments) {
                        return Err(bad("MCP App arguments do not match the tool schema"));
                    }
                    let adapter = rook_tools::mcp::McpTool::new(app.server.clone(), descriptor.clone());
                    permitted(policy, &adapter, &arguments)?;
                    let nonce = identity();
                    let visible = vault.redact(&serde_json::to_string(&arguments)?);
                    app.pending = Some(Pending {
                        nonce: nonce.clone(),
                        tool: tool.clone(),
                        args: arguments,
                        at: Instant::now(),
                    });
                    return Ok(
                        json!({"app":id,"nonce":nonce,"server":app.server.name(),"uri":app.uri,"generation":app.generation,"tool":tool,"arguments":visible,"expires_in":120}),
                    );
                }
                Request::Confirm { nonce, approved, .. } => {
                    if !app.pending.as_ref().is_some_and(|p| p.nonce == nonce) {
                        return Err(bad("app confirmation does not match the pending request"));
                    }
                    let pending = app.pending.take().ok_or_else(|| bad("app request already answered"))?;
                    if !approved {
                        app.declined = true;
                        return Ok(
                            json!({"isError":true,"content":[{"type":"text","text":"User declined the app request."}]}),
                        );
                    }
                    if pending.at.elapsed() >= Duration::from_secs(120) {
                        app.declined = true;
                        return Err(bad("app confirmation expired; no tool was called"));
                    }
                    let descriptor = tools
                        .iter()
                        .find(|d| d.name == pending.tool)
                        .ok_or_else(|| bad("tool disappeared"))?;
                    let adapter = rook_tools::mcp::McpTool::new(app.server.clone(), descriptor.clone());
                    permitted(policy, &adapter, &pending.args)?;
                    app.uncertain = true;
                    (app.server.clone(), pending, adapter.name().to_owned())
                }
                _ => return Err(bad("invalid app operation")),
            }
        };
        let (hooks, errors) = crate::hooks::Hooks::compile(&self.config.hooks);
        if !errors.is_empty() || self.config.hooks.iter().any(|hook| hook.ui || hook.ui_stream) {
            return Err(bad(
                "app calls with invalid or interactive hooks must use the ordinary agent interaction channel",
            ));
        }
        let payload = json!({"workspace": self.workspace, "session": rook_store::format_session_id(session), "tool": name, "arguments": pending.args});
        let checked = hooks.run(crate::hooks::Event::PreTool, &name, &payload).await;
        if matches!(checked.decision, Some(Decision::Deny(_) | Decision::Ask)) {
            if let Some(app) = manager.apps.0.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
                app.uncertain = false;
            }
            return Err(bad(
                "pre_tool hook refused the app call or requires separate agent approval; no tool was called",
            ));
        }
        // An interrupted invocation leaves a durable unknown receipt and cannot
        // be replayed by another confirmation or a later browser reconnect.
        self.log(
            session,
            rook_store::EventKind::Note,
            "rook:mcp-app:v1",
            &json!({"app":id,"tool":name,"state":"outcome_unknown"}).to_string(),
        )?;
        self.store.flush()?;
        let descriptor = connections
            .iter()
            .find(|(s, _, _)| Arc::ptr_eq(s, &server))
            .and_then(|(_, _, tools)| tools.iter().find(|d| d.name == pending.tool))
            .ok_or_else(|| bad("tool unavailable"))?;
        let adapter = rook_tools::mcp::McpTool::new(server, descriptor.clone());
        let mut context = ToolContext::new(self.workspace.clone());
        context.session = Some(session);
        context.max_output_bytes = 32768;
        let outcome = tokio::time::timeout(Duration::from_secs(45), adapter.call(&context, &pending.args))
            .await
            .map_err(|_| bad("app call timed out; outcome unknown, inspect effects"))?
            .map_err(|_| bad("app call failed; outcome unknown, inspect effects"))?;
        let _ = hooks.run(crate::hooks::Event::PostTool, &name, &json!({"workspace":self.workspace,"session":rook_store::format_session_id(session),"tool":name,"arguments":pending.args,"result":outcome.content,"is_error":outcome.is_error})).await;
        let text = vault.redact(&outcome.content);
        self.log(
            session,
            rook_store::EventKind::Note,
            "rook:mcp-app:v1",
            &json!({"app":id,"tool":name,"state":"answered","is_error":outcome.is_error,"text":text})
                .to_string(),
        )?;
        self.store.flush()?;
        if let Some(app) = manager.apps.0.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
            app.uncertain = outcome.is_error;
        }
        Ok(json!({"isError":outcome.is_error,"content":[{"type":"text","text":text}]}))
    }
}
