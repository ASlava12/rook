//! Driving a turn from the browser.
//!
//! One websocket per conversation. The turn streams back as it happens, and when
//! the policy wants an approval the socket is what asks — so the web UI is a way
//! to *use* the agent rather than only to read what it did.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use rook_core::agent::AgentLoop;
use rook_llm::Delta;
use rook_proto::{ApprovalDecision, ChatEvent, ClientMessage};
use rook_tools::policy::{Approval, ChannelApprover};

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

    let (approver, relay) = approver(outbound.clone());
    // Per connection, not per prompt: an approval granted for "the run" has to
    // survive the turn it was granted in.
    let policy =
        std::sync::Arc::new(tokio::sync::OnceCell::<std::sync::Arc<rook_tools::policy::Policy>>::new());
    let mut running: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else { continue };
        let Ok(incoming) = serde_json::from_str::<ClientMessage>(&text) else { continue };

        match incoming {
            ClientMessage::Approval { id, decision } => approver.answer(
                &id,
                match decision {
                    ApprovalDecision::Once => Approval::Once,
                    ApprovalDecision::ForRun => Approval::ForRun,
                    ApprovalDecision::Deny => Approval::Deny("the user declined".into()),
                },
            ),
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
                    policy.clone(),
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
    relay.abort();
    drop(outbound);
    let _ = writer.await;
}

async fn turn(
    state: Arc<AppState>,
    approver: Arc<ChannelApprover>,
    policy: Arc<tokio::sync::OnceCell<Arc<rook_tools::policy::Policy>>>,
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

    let provider = match rook_llm::from_spec_with(
        &rook.config.agent.model,
        rook.config.agent.stream_idle(),
        rook.config.agent.context_window,
    ) {
        Ok(provider) => provider,
        Err(e) => return report(&outbound, e.to_string()),
    };

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    agent.policy = policy.get_or_init(|| async { rook_core::agent::policy_for(&rook) }).await.clone();
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

/// Relays approval requests to the browser and routes the answers back.
fn approver(
    outbound: mpsc::UnboundedSender<ChatEvent>,
) -> (Arc<ChannelApprover>, tokio::task::JoinHandle<()>) {
    let (requests, mut incoming) = mpsc::unbounded_channel::<rook_tools::policy::ApprovalRequest>();
    let relay = tokio::spawn(async move {
        while let Some(request) = incoming.recv().await {
            let sent = outbound.send(ChatEvent::Approval {
                id: request.id,
                tool: request.tool,
                action: request.action,
            });
            if sent.is_err() {
                break;
            }
        }
    });
    (Arc::new(ChannelApprover::new(requests, std::time::Duration::from_secs(300))), relay)
}
