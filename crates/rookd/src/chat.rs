//! Driving a turn from the browser.
//!
//! One websocket per conversation. The turn streams back as it happens, and when
//! the policy wants an approval the socket is what asks — so the web UI is a way
//! to *use* the agent rather than only to read what it did.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use rook_core::agent::{AgentLoop, Progress};
use rook_llm::Delta;
use rook_proto::AskQuestion;
use rook_proto::{ApprovalDecision, ChatEvent, ClientMessage};
use rook_tools::ask::{AskRequest, ChannelAsker};
use rook_tools::policy::{Approval, ChannelApprover};

use crate::AppState;

/// `?workspace=` names the project this conversation is in, defaulting to the
/// daemon's own. A connection is bound to one for its life, because a project is
/// what a conversation is about — not something a single prompt changes.
#[derive(serde::Deserialize)]
pub struct Where {
    workspace: Option<std::path::PathBuf>,
}

pub async fn upgrade(
    ws: WebSocketUpgrade,
    axum::extract::Query(here): axum::extract::Query<Where>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let engine = match state.engine_for(here.workspace.as_deref()).await {
        Ok(engine) => engine,
        Err(why) => return (axum::http::StatusCode::BAD_REQUEST, why).into_response(),
    };
    let equipment = state.equipment_for(&engine).await;
    ws.on_upgrade(move |socket| serve(socket, engine, equipment))
}

/// Refuses the upgrade before anything else looks at the request.
///
/// A layer rather than a check inside the handler: the handler cannot run until
/// `WebSocketUpgrade` has extracted, so a decision made there is made after the
/// framework has already answered a malformed upgrade — and a rule about who
/// may connect belongs in front of the connecting, not inside it.
pub async fn only_from_this_daemon(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !from_this_daemon(request.headers()) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "this websocket only accepts connections from the daemon's own page",
        )
            .into_response();
    }
    next.run(request).await
}

/// Whether the upgrade came from the page this daemon serves.
///
/// A websocket is outside the same-origin policy and is not preflighted, so any
/// page the user has open can connect to a daemon on loopback — and what this
/// one reaches is a turn: tools, the workspace, the transcript, and a setting
/// that widens what runs without asking. The other endpoints are covered by
/// their JSON content type forcing a preflight that no CORS header answers;
/// this one has nothing equivalent, so it checks for itself.
///
/// A request with no `Origin` is not a browser — curl, an editor, the tests —
/// and is left alone; a browser always sends one.
fn from_this_daemon(headers: &axum::http::HeaderMap) -> bool {
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return true;
    };
    let (Ok(origin), Some(Ok(host))) =
        (origin.to_str(), headers.get(axum::http::header::HOST).map(|h| h.to_str()))
    else {
        return false;
    };
    origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) == Some(host)
}

async fn serve(
    socket: WebSocket,
    engine: Arc<tokio::sync::RwLock<rook_core::Rook>>,
    shared: Arc<tokio::sync::OnceCell<Shared>>,
) {
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

    let patience = engine.read().await.config.agent.answer_timeout();
    let (approver, relay) = approver(outbound.clone(), patience);
    let (asker, ask_relay) = asker(outbound.clone(), patience);
    // What this browser has typed mid-turn, which is per connection — unlike
    // the servers and the background commands in `shared`, which belong to the
    // project and outlive any one socket.
    let interjections: Arc<rook_core::agent::Interjections> = Default::default();
    // Settings are cheap and wanted before the first prompt, so they are not in
    // the cell with the expensive things.
    let settings = Arc::new(Settings::new(&*engine.read().await));
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
                    ApprovalDecision::Deny => Approval::declined(),
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
                // Typed while a turn runs, it goes to the turn: the browser had
                // to wait or cancel, and cancelling loses everything the turn
                // had done to say one sentence to it.
                if running.as_ref().is_some_and(|h| !h.is_finished()) {
                    interjections.say(&text);
                    let _ = outbound.send(ChatEvent::Interjected { text });
                    continue;
                }
                running = Some(tokio::spawn(turn(
                    engine.clone(),
                    Connection {
                        approver: approver.clone(),
                        asker: asker.clone(),
                        settings: settings.clone(),
                        interjections: interjections.clone(),
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
    interjections: Arc<rook_core::agent::Interjections>,
}

async fn turn(
    engine: Arc<tokio::sync::RwLock<rook_core::Rook>>,
    connection: Connection,
    shared: Arc<tokio::sync::OnceCell<Shared>>,
    outbound: mpsc::UnboundedSender<ChatEvent>,
    session: Option<String>,
    prompt: String,
) {
    // Owned so the guard outlives this task's spawn point.
    let rook = engine.read_owned().await;

    let session = match session.as_deref().and_then(rook_store::parse_session_id) {
        Some(id) => id,
        None => match rook.start_session("") {
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

    let shared = shared.get_or_init(|| Shared::for_project(&rook)).await;

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    agent.policy = connection.settings.policy.clone();
    agent.effort = connection.settings.effort();
    agent.approver = connection.approver;
    agent.ask_via(connection.asker);
    agent.interjections = connection.interjections.clone();
    rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp, shared.jobs.clone());

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
                Progress::Delegating { task, tool } => {
                    ChatEvent::Reasoning { text: format!("\n    {task}: {tool}") }
                }
                Progress::ToolDone { name, failed } => ChatEvent::ToolDone { name: name.to_string(), failed },
                Progress::Spent { input, output, cached } => {
                    ChatEvent::Spent { input_tokens: input, output_tokens: output, cached_tokens: cached }
                }
                Progress::Delta(Delta::Done { .. } | Delta::ReasoningDone(_)) => return,
            };
            let _ = emit.send(event);
        })
        .await;

    match result {
        Ok(outcome) => {
            for text in &outcome.facts_learned {
                let _ = outbound.send(ChatEvent::Remembered { text: text.clone() });
            }
            for text in &outcome.facts_forgotten {
                let _ = outbound.send(ChatEvent::Forgot { text: text.clone() });
            }
            let _ = outbound.send(ChatEvent::Done {
                steps: outcome.steps,
                input_tokens: outcome.input_tokens,
                output_tokens: outcome.output_tokens,
                delegated: outcome.delegated,
                compactions: outcome.compactions,
                decisions: outcome.decisions,
                open_questions: outcome.open_questions,
            });
        }
        Err(e) => report(&outbound, e.to_string()),
    }
}

fn report(outbound: &mpsc::UnboundedSender<ChatEvent>, message: String) {
    let _ = outbound.send(ChatEvent::Error { message });
}

/// What a connection keeps between turns.
/// What a turn needs and nobody wants rebuilt: the language-server pool, the
/// MCP session and the commands left running.
///
/// Per project rather than per connection, because for a daemon the front end
/// is the daemon. Rebuilt on every socket, a browser reload re-indexed every
/// language server, respawned every MCP server, and killed every background
/// command the agent had started.
pub struct Shared {
    servers: Arc<rook_core::lsp::Servers>,
    mcp: Arc<rook_core::McpSession>,
    pub jobs: Arc<rook_tools::jobs::Jobs>,
}

impl Shared {
    pub async fn for_project(rook: &rook_core::Rook) -> Self {
        Self {
            servers: rook_core::agent::servers_for(&rook.config, &rook.workspace),
            mcp: Arc::new(rook.connect_mcp().await),
            jobs: rook_core::agent::jobs_for(&rook.config),
        }
    }
}

/// What the browser may change for the rest of the connection.
struct Settings {
    policy: Arc<rook_tools::policy::Policy>,
    effort: std::sync::RwLock<rook_llm::Effort>,
}

impl Settings {
    fn new(rook: &rook_core::Rook) -> Self {
        Self {
            policy: rook_core::agent::policy_for(&rook.config),
            effort: std::sync::RwLock::new(rook.config.agent.effort()),
        }
    }

    fn effort(&self) -> rook_llm::Effort {
        *self.effort.read().unwrap_or_else(|e| e.into_inner())
    }

    fn describe(&self) -> ChatEvent {
        ChatEvent::Settings {
            mode: self.policy.stance().as_str().into(),
            effort: self.effort().as_str().into(),
            stances: rook_tools::policy::Stance::ALL.iter().map(|s| s.as_str().to_string()).collect(),
            efforts: rook_llm::Effort::ALL.iter().map(|e| e.as_str().to_string()).collect(),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        match name {
            "stance" | "mode" => rook_tools::policy::Stance::parse(value)
                .map(|mode| self.policy.set_stance(mode))
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
                preview: request.preview,
            });
            if sent.is_err() {
                break;
            }
        }
    });
    (Arc::new(ChannelApprover::new(requests, patience)), relay)
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    /// The page draws its selects from these lists, so a stance the engine
    /// grows appears without the page learning its name — and one it loses
    /// disappears rather than sitting in a menu as a choice that errors.
    #[test]
    fn the_settings_event_carries_the_engines_own_lists() {
        let config = rook_core::Config::default();
        let settings = Settings {
            policy: rook_core::agent::policy_for(&config),
            effort: std::sync::RwLock::new(config.agent.effort()),
        };
        let ChatEvent::Settings { mode, effort, stances, efforts } = settings.describe() else {
            panic!("describe() is the settings event");
        };
        let expected: Vec<String> =
            rook_tools::policy::Stance::ALL.iter().map(|s| s.as_str().to_string()).collect();
        assert_eq!(stances, expected);
        assert!(stances.contains(&mode), "the current stance is one of the offered: {mode} in {stances:?}");
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert!(efforts.contains(&effort), "{effort} in {efforts:?}");
    }
}
