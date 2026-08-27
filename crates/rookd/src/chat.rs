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

use rook_core::agent::{AgentLoop, Progress};
use rook_llm::Delta;
use rook_proto::AskQuestion;
use rook_proto::{ApprovalDecision, ChatEvent, ClientMessage};
use rook_tools::ask::{AskRequest, ChannelAsker};
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

    let patience = state.rook.read().await.config.agent.answer_timeout();
    let (approver, relay) = approver(outbound.clone(), patience);
    let (asker, ask_relay) = asker(outbound.clone(), patience);
    // Per connection, not per prompt: connecting MCP servers and starting
    // language servers is expensive, and an approval granted for the run has to
    // survive the turn it was granted in.
    let shared: Arc<tokio::sync::OnceCell<Shared>> = Arc::new(tokio::sync::OnceCell::new());
    // Settings are cheap and wanted before the first prompt, so they are not in
    // the cell with the expensive things.
    let settings = Arc::new(Settings::new(&*state.rook.read().await));
    let _ = outbound.send(settings.describe());
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
            ClientMessage::Answers { id, answers } => asker.answer(&id, answers),
            ClientMessage::Setting { name, value } => {
                let _ = match settings.set(&name, &value) {
                    Ok(()) => outbound.send(settings.describe()),
                    Err(message) => outbound.send(ChatEvent::Error { message }),
                };
            }
            ClientMessage::Cancel => {
                if let Some(handle) = running.take() {
                    handle.abort();
                    // The browser only leaves its working state on Done or
                    // Error; aborting silently leaves it stuck forever.
                    let _ = outbound.send(ChatEvent::Cancelled);
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
                    Connection {
                        approver: approver.clone(),
                        asker: asker.clone(),
                        settings: settings.clone(),
                    },
                    shared.clone(),
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
    ask_relay.abort();
    drop(outbound);
    let _ = writer.await;
}

/// What the connection gives a turn: who answers its questions, and what the
/// user has set for the rest of the session.
#[derive(Clone)]
struct Connection {
    approver: Arc<ChannelApprover>,
    asker: Arc<ChannelAsker>,
    settings: Arc<Settings>,
}

async fn turn(
    state: Arc<AppState>,
    connection: Connection,
    shared: Arc<tokio::sync::OnceCell<Shared>>,
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

    let shared = shared
        .get_or_init(|| async {
            Shared { servers: rook_core::agent::servers_for(&rook), mcp: Arc::new(rook.connect_mcp().await) }
        })
        .await;

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    agent.policy = connection.settings.policy.clone();
    agent.effort = connection.settings.effort();
    agent.approver = connection.approver;
    agent.ask_via(connection.asker);
    rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp);

    let emit = outbound.clone();
    let result = agent
        .run_with(&prompt, |progress| {
            let event = match progress {
                Progress::Delta(Delta::Text(text)) => ChatEvent::Text { text: text.clone() },
                Progress::Delta(Delta::Reasoning(text)) => ChatEvent::Reasoning { text: text.clone() },
                Progress::Delta(Delta::ToolCall(call)) => ChatEvent::Tool { name: call.name.clone() },
                Progress::Delegated { task, done, total } => {
                    ChatEvent::Reasoning { text: format!("\n  [{done}/{total}] {task}") }
                }
                Progress::ToolDone { name, failed } => ChatEvent::ToolDone { name: name.to_string(), failed },
                Progress::Spent { input, output, cached } => {
                    ChatEvent::Spent { input_tokens: input, output_tokens: output, cached_tokens: cached }
                }
                Progress::Delta(Delta::Done { .. }) => return,
            };
            let _ = emit.send(event);
        })
        .await;

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

/// What a connection keeps between turns.
struct Shared {
    servers: Arc<rook_core::lsp::Servers>,
    mcp: Arc<rook_core::McpSession>,
}

/// What the browser may change for the rest of the connection.
struct Settings {
    policy: Arc<rook_tools::policy::Policy>,
    effort: std::sync::RwLock<rook_llm::Effort>,
}

impl Settings {
    fn new(rook: &rook_core::Rook) -> Self {
        Self {
            policy: rook_core::agent::policy_for(rook),
            effort: std::sync::RwLock::new(rook.config.agent.effort()),
        }
    }

    fn effort(&self) -> rook_llm::Effort {
        *self.effort.read().unwrap_or_else(|e| e.into_inner())
    }

    fn describe(&self) -> ChatEvent {
        ChatEvent::Settings {
            mode: self.policy.mode().as_str().into(),
            effort: self.effort().as_str().into(),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        match name {
            "mode" => rook_tools::policy::Mode::parse(value)
                .map(|mode| self.policy.set_mode(mode))
                .ok_or_else(|| format!("no mode {value:?}")),
            "effort" => rook_llm::Effort::parse(value)
                .map(|effort| *self.effort.write().unwrap_or_else(|e| e.into_inner()) = effort)
                .ok_or_else(|| format!("no effort {value:?}")),
            other => Err(format!("no setting {other:?}")),
        }
    }
}

/// Relays the agent's questions to the browser and routes the answers back.
fn asker(
    outbound: mpsc::UnboundedSender<ChatEvent>,
    patience: std::time::Duration,
) -> (Arc<ChannelAsker>, tokio::task::JoinHandle<()>) {
    let (requests, mut incoming) = mpsc::unbounded_channel::<AskRequest>();
    let relay = tokio::spawn(async move {
        while let Some(request) = incoming.recv().await {
            let questions = request
                .questions
                .into_iter()
                .map(|q| AskQuestion { question: q.question, choices: q.choices, multi: q.multi })
                .collect();
            if outbound.send(ChatEvent::Ask { id: request.id, questions }).is_err() {
                break;
            }
        }
    });
    (Arc::new(ChannelAsker::new(requests, patience)), relay)
}

/// Relays approval requests to the browser and routes the answers back.
fn approver(
    outbound: mpsc::UnboundedSender<ChatEvent>,
    patience: std::time::Duration,
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
    (Arc::new(ChannelApprover::new(requests, patience)), relay)
}
