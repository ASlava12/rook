//! Driving a turn from the browser.
//!
//! One websocket per conversation. The turn streams back as it happens, and when
//! the policy wants an approval the socket is what asks — so the web UI is a way
//! to *use* the agent rather than only to read what it did.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc, oneshot};

use rook_core::agent::AgentLoop;
use rook_llm::Delta;
use rook_proto::{ApprovalDecision, ChatEvent, ClientMessage};
use rook_tools::policy::{Approval, Approver, Risk};

use crate::AppState;

pub async fn upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| serve(socket, state))
}

async fn serve(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();
    let (outbound, mut queued) = mpsc::unbounded_channel::<ChatEvent>();

    // One writer task: the turn, the approver and the error path all emit
    // concurrently, and a socket has a single writer.
    let writer = tokio::spawn(async move {
        while let Some(event) = queued.recv().await {
            let Ok(text) = serde_json::to_string(&event) else { continue };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let approver = Arc::new(SocketApprover::new(outbound.clone()));
    let mut running: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else { continue };
        let Ok(incoming) = serde_json::from_str::<ClientMessage>(&text) else { continue };

        match incoming {
            ClientMessage::Approval { id, decision } => approver.answer(&id, decision),
            ClientMessage::Cancel => {
                if let Some(handle) = running.take() {
                    handle.abort();
                }
            }
            ClientMessage::Prompt { session, text } => {
                if running.as_ref().is_some_and(|h| !h.is_finished()) {
                    let _ = outbound.send(ChatEvent::Error {
                        message: "a turn is already running on this connection".into(),
                    });
                    continue;
                }
                running = Some(tokio::spawn(turn(
                    state.clone(),
                    approver.clone(),
                    outbound.clone(),
                    session,
                    text,
                )));
            }
        }
    }

    if let Some(handle) = running {
        handle.abort();
    }
    drop(outbound);
    let _ = writer.await;
}

async fn turn(
    state: Arc<AppState>,
    approver: Arc<SocketApprover>,
    outbound: mpsc::UnboundedSender<ChatEvent>,
    session: Option<String>,
    prompt: String,
) {
    // Owned so the guard outlives this task's spawn point.
    let rook = state.rook.clone().read_owned().await;

    let session = match session.as_deref().and_then(rook_store::parse_session_id) {
        Some(id) => id,
        None => match rook.start_session(prompt.lines().next().unwrap_or("web")) {
            Ok(id) => id,
            Err(e) => return report(&outbound, e.to_string()),
        },
    };
    let _ = outbound.send(ChatEvent::Started { session: rook_store::format_session_id(session) });

    let provider = match rook_llm::from_spec(&rook.config.agent.model, rook.config.agent.stream_idle()) {
        Ok(provider) => provider,
        Err(e) => return report(&outbound, e.to_string()),
    };

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    agent.approver = approver;

    let mcp = rook.connect_mcp().await;
    for (server, tools) in &mcp.servers {
        agent.tools.register_server(server.clone(), tools.clone());
    }

    let emit = outbound.clone();
    let result = agent
        .run_with(&prompt, |delta| {
            let event = match delta {
                Delta::Text(text) => ChatEvent::Text { text: text.clone() },
                Delta::Reasoning(text) => ChatEvent::Reasoning { text: text.clone() },
                Delta::ToolCall(call) => ChatEvent::Tool { name: call.name.clone() },
                Delta::Done { .. } => return,
            };
            let _ = emit.send(event);
        })
        .await;
    mcp.shutdown().await;

    match result {
        Ok(outcome) => {
            let _ = outbound.send(ChatEvent::Done {
                steps: outcome.steps,
                input_tokens: outcome.input_tokens,
                output_tokens: outcome.output_tokens,
                delegated: outcome.delegated,
                compactions: outcome.compactions,
            });
        }
        Err(e) => report(&outbound, e.to_string()),
    }
}

fn report(outbound: &mpsc::UnboundedSender<ChatEvent>, message: String) {
    let _ = outbound.send(ChatEvent::Error { message });
}

/// Asks the browser, and waits.
struct SocketApprover {
    outbound: mpsc::UnboundedSender<ChatEvent>,
    waiting: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
    next_id: AtomicU64,
}

impl SocketApprover {
    fn new(outbound: mpsc::UnboundedSender<ChatEvent>) -> Self {
        Self { outbound, waiting: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) }
    }

    fn answer(&self, id: &str, decision: ApprovalDecision) {
        if let Ok(mut waiting) = self.waiting.try_lock()
            && let Some(tx) = waiting.remove(id)
        {
            let _ = tx.send(decision);
        }
    }
}

#[async_trait]
impl Approver for SocketApprover {
    async fn ask(&self, tool: &str, risk: &Risk) -> Approval {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id.clone(), tx);

        let sent = self.outbound.send(ChatEvent::Approval {
            id: id.clone(),
            tool: tool.to_string(),
            action: risk.describe(),
        });
        if sent.is_err() {
            return Approval::Deny("the browser disconnected".into());
        }

        // A tab closed mid-prompt would otherwise leave the turn waiting
        // forever, holding the store's read lock with it.
        match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(ApprovalDecision::Once)) => Approval::Once,
            Ok(Ok(ApprovalDecision::ForRun)) => Approval::ForRun,
            Ok(Ok(ApprovalDecision::Deny)) => Approval::Deny("the user declined".into()),
            Ok(Err(_)) => Approval::Deny("the approval was dropped".into()),
            Err(_) => {
                self.waiting.lock().await.remove(&id);
                Approval::Deny("no answer within five minutes".into())
            }
        }
    }
}
