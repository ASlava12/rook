//! An Agent Client Protocol server.
//!
//! ACP is how editors talk to agents: JSON-RPC 2.0 over stdio, v1 stable, with
//! Zed, JetBrains and Neovim on the client side. Speaking it means one
//! implementation instead of a plugin per editor.
//!
//! The mapping is close to direct. Streamed deltas become `session/update`
//! notifications; the permission policy's approver becomes
//! `session/request_permission`, so an editor's approval dialog and the
//! terminal's `[y/a/n]` are the same decision reaching the same policy.

pub mod protocol;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use rook_core::Rook;
use rook_core::agent::AgentLoop;
use rook_llm::Delta;
use rook_tools::policy::{Approval, Approver, Risk};

use protocol::{ContentBlock, Error, Incoming, PROTOCOL_VERSION};

/// Run the server on stdin/stdout until the client closes the connection.
pub async fn serve_stdio(rook: Rook) -> std::io::Result<()> {
    serve(rook, BufReader::new(tokio::io::stdin()), tokio::io::stdout()).await
}

/// The server over any pair of streams, so it can be driven by a test as well as
/// by an editor.
pub async fn serve<R, W>(rook: Rook, reader: R, mut sink: W) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let rook = Arc::new(rook);
    let (outbound, mut queued) = mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        while let Some(line) = queued.recv().await {
            if sink.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                break;
            }
            let _ = sink.flush().await;
        }
    });

    // Built once for the connection: an editor sends many prompts, and
    // reconnecting MCP or restarting a language server for each is wasted time.
    let policy = rook_core::agent::policy_for(&rook);
    let servers = rook_core::agent::servers_for(&rook);
    let mcp = Arc::new(rook.connect_mcp().await);

    let peer = Arc::new(Peer::new(outbound));
    let mut lines = reader.lines();
    let mut turn: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(line) = lines.next_line().await? {
        let Ok(message) = serde_json::from_str::<Incoming>(&line) else {
            tracing::debug!("unparsable line: {line}");
            continue;
        };

        // A reply to something we asked, rather than a request of its own.
        if message.method.is_none() {
            if let Some(id) = message.id.as_ref().and_then(|i| i.as_u64()) {
                peer.resolve(id, message.result.unwrap_or(serde_json::Value::Null));
            }
            continue;
        }

        let method = message.method.unwrap_or_default();
        let params = message.params.unwrap_or(serde_json::Value::Null);

        match (method.as_str(), message.id) {
            ("session/cancel", _) => {
                if let Some(handle) = turn.take() {
                    handle.abort();
                }
            }
            ("session/prompt", Some(id)) => {
                if let Some(handle) = turn.take() {
                    handle.abort();
                }
                turn = Some(tokio::spawn(prompt(
                    rook.clone(),
                    peer.clone(),
                    Shared { policy: policy.clone(), servers: servers.clone(), mcp: mcp.clone() },
                    id,
                    params,
                )));
            }
            (_, Some(id)) => {
                let outcome = dispatch(&rook, &method, params);
                peer.respond(&id, outcome);
            }
            // A notification we do not handle is not an error.
            (_, None) => tracing::debug!("ignoring notification {method}"),
        }
    }

    if let Some(handle) = turn {
        handle.abort();
    }
    mcp.shutdown().await;
    servers.shutdown().await;
    drop(peer);
    let _ = writer.await;
    Ok(())
}

fn dispatch(rook: &Rook, method: &str, params: serde_json::Value) -> Result<serde_json::Value, Error> {
    match method {
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentInfo": { "name": "rook", "version": rook_core::AGENT_VERSION },
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": { "image": false, "audio": false, "embeddedContext": false },
            },
            "authMethods": [],
        })),

        "session/new" => {
            let request: protocol::NewSession =
                serde_json::from_value(params).map_err(|e| Error::invalid_params(e.to_string()))?;
            let session = rook
                .start_session(&format!("acp {}", request.cwd))
                .map_err(|e| Error::internal(e.to_string()))?;
            Ok(serde_json::json!({ "sessionId": rook_store::format_session_id(session) }))
        }

        // Resuming an existing session needs nothing beyond checking it exists:
        // the loop replays the log on its own.
        "session/load" | "session/resume" => {
            let request: protocol::SessionRef =
                serde_json::from_value(params).map_err(|e| Error::invalid_params(e.to_string()))?;
            let id = rook_store::parse_session_id(&request.session_id)
                .ok_or_else(|| Error::invalid_params("not a session id"))?;
            match rook.store.get_session(id) {
                Ok(Some(_)) => Ok(serde_json::json!({})),
                Ok(None) => Err(Error::invalid_params("no such session")),
                Err(e) => Err(Error::internal(e.to_string())),
            }
        }

        "session/list" => {
            let sessions = rook.sessions().map_err(|e| Error::internal(e.to_string()))?;
            Ok(serde_json::json!({
                "sessions": sessions.iter().map(|s| serde_json::json!({
                    "sessionId": rook_store::format_session_id(s.id),
                    "title": s.title,
                    "cwd": s.workspace,
                })).collect::<Vec<_>>()
            }))
        }

        "authenticate" | "session/close" | "logout" => Ok(serde_json::json!({})),
        other => Err(Error::method_not_found(other)),
    }
}

/// What the connection keeps between prompts.
#[derive(Clone)]
struct Shared {
    policy: Arc<rook_tools::policy::Policy>,
    servers: Arc<rook_core::lsp::Servers>,
    mcp: Arc<rook_core::McpSession>,
}

async fn prompt(
    rook: Arc<Rook>,
    peer: Arc<Peer>,
    shared: Shared,
    id: serde_json::Value,
    params: serde_json::Value,
) {
    let request: protocol::Prompt = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(e) => return peer.respond(&id, Err(Error::invalid_params(e.to_string()))),
    };
    let Some(session) = rook_store::parse_session_id(&request.session_id) else {
        return peer.respond(&id, Err(Error::invalid_params("not a session id")));
    };

    let text = request.prompt.iter().filter_map(ContentBlock::render).collect::<Vec<_>>().join("\n");
    if text.trim().is_empty() {
        return peer.respond(&id, Err(Error::invalid_params("the prompt has no text")));
    }

    let provider = match rook_llm::from_spec_with(
        &rook.config.agent.model,
        rook.config.agent.stream_idle(),
        rook.config.agent.context_window,
    ) {
        Ok(provider) => provider,
        Err(e) => return peer.respond(&id, Err(Error::internal(e.to_string()))),
    };

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    agent.policy = shared.policy.clone();
    agent.servers = shared.servers.clone();
    rook_core::lsp::register(&mut agent.tools, shared.servers.clone());
    for (server, tools) in &shared.mcp.servers {
        agent.tools.register_server(server.clone(), tools.clone());
    }
    agent.approver = Arc::new(EditorApprover { peer: peer.clone(), session: request.session_id.clone() });

    let calls = AtomicU64::new(0);
    let result = agent
        .run_with(&text, |delta| {
            let update = match delta {
                Delta::Text(text) => protocol::agent_message_chunk(&request.session_id, text),
                Delta::Reasoning(text) => protocol::agent_thought_chunk(&request.session_id, text),
                Delta::ToolCall(call) => protocol::tool_call(
                    &request.session_id,
                    &format!("call_{}", calls.fetch_add(1, Ordering::Relaxed)),
                    &call.name,
                    protocol::tool_kind(&call.name),
                ),
                Delta::Done { .. } => return,
            };
            peer.notify("session/update", update);
        })
        .await;

    let response = match result {
        Ok(outcome) => Ok(serde_json::json!({ "stopReason": stop_reason(&outcome.stopped) })),
        Err(e) => Err(Error::internal(e.to_string())),
    };
    peer.respond(&id, response);
}

fn stop_reason(stopped: &str) -> &'static str {
    match stopped {
        "max_steps" => "max_turn_requests",
        "MaxTokens" => "max_tokens",
        "Refusal" => "refusal",
        _ => "end_turn",
    }
}

/// The other end of the connection: notifications out, requests out, replies in.
struct Peer {
    outbound: mpsc::UnboundedSender<String>,
    waiting: Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>,
    next_id: AtomicU64,
}

impl Peer {
    fn new(outbound: mpsc::UnboundedSender<String>) -> Self {
        Self { outbound, waiting: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) }
    }

    fn send(&self, value: &impl serde::Serialize) {
        if let Ok(line) = serde_json::to_string(value) {
            let _ = self.outbound.send(line);
        }
    }

    fn notify(&self, method: &str, params: serde_json::Value) {
        self.send(&protocol::Notification { jsonrpc: "2.0", method, params });
    }

    fn respond(&self, id: &serde_json::Value, outcome: Result<serde_json::Value, Error>) {
        let (result, error) = match outcome {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error)),
        };
        self.send(&protocol::Response { jsonrpc: "2.0", id, result, error });
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id, tx);
        self.send(&protocol::Request { jsonrpc: "2.0", id, method, params });
        rx.await.ok()
    }

    fn resolve(&self, id: u64, result: serde_json::Value) {
        if let Ok(mut waiting) = self.waiting.try_lock()
            && let Some(tx) = waiting.remove(&id)
        {
            let _ = tx.send(result);
        }
    }
}

/// Turns the permission policy's question into the editor's approval dialog.
struct EditorApprover {
    peer: Arc<Peer>,
    session: String,
}

#[async_trait]
impl Approver for EditorApprover {
    async fn ask(&self, tool: &str, risk: &Risk) -> Approval {
        let params = serde_json::json!({
            "sessionId": self.session,
            "toolCall": { "toolCallId": format!("approval_{tool}"), "title": risk.describe() },
            "options": [
                { "optionId": "once",   "name": "Allow once",    "kind": "allow_once" },
                { "optionId": "always", "name": "Always allow",  "kind": "allow_always" },
                { "optionId": "reject", "name": "Reject",        "kind": "reject_once" },
            ],
        });

        let Some(answer) = self.peer.request("session/request_permission", params).await else {
            return Approval::Deny("the editor closed the connection".into());
        };
        // A `cancelled` outcome means the turn is being torn down, not that the
        // user chose to refuse this one thing.
        match answer.pointer("/outcome/outcome").and_then(|o| o.as_str()) {
            Some("selected") => match answer.pointer("/outcome/optionId").and_then(|o| o.as_str()) {
                Some("once") => Approval::Once,
                Some("always") => Approval::ForRun,
                _ => Approval::Deny("the user rejected it".into()),
            },
            _ => Approval::Deny("the request was cancelled".into()),
        }
    }
}
