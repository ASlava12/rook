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
    ws.on_upgrade(move |socket| serve(socket, engine, equipment, state))
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
    state: Arc<AppState>,
) {
    let (sink, mut stream) = socket.split();
    let (outbound, queued) = mpsc::unbounded_channel::<ChatEvent>();

    // One writer task: the turn, the approver and the error path all emit
    // concurrently, and a socket has a single writer.
    let writer = tokio::spawn(write_frames(sink, queued));

    // Settings are cheap and wanted before the first prompt, so they are not in
    // the cell with the expensive things.
    let settings = Arc::new(Settings::new(&*engine.read().await));
    let _ = outbound.send(settings.describe());

    // Which turn this window is watching, and the task carrying it here. The
    // turn itself is the daemon's; this is only the view of it.
    let mut watching: Option<Watching> = None;

    while let Some(Ok(message)) = stream.next().await {
        let Message::Text(text) = message else { continue };
        let Ok(incoming) = serde_json::from_str::<ClientMessage>(&text) else { continue };

        match incoming {
            ClientMessage::Approval { id, decision } => {
                if let Some(live) = attached(&state, &watching).await {
                    live.approver.answer(
                        &id,
                        match decision {
                            ApprovalDecision::Once => Approval::Once,
                            ApprovalDecision::ForRun => Approval::ForRun,
                            ApprovalDecision::KindForRun => Approval::KindForRun,
                            ApprovalDecision::Deny => Approval::declined(),
                        },
                    );
                }
            }
            ClientMessage::Answers { id, answers } => {
                if let Some(live) = attached(&state, &watching).await {
                    live.asker.answer(&id, answers);
                }
            }
            ClientMessage::Setting { name, value } => {
                let _ = match settings.set(&name, &value) {
                    Ok(()) => outbound.send(settings.describe()),
                    Err(message) => outbound.send(ChatEvent::Error { message }),
                };
            }
            ClientMessage::Cancel => {
                let Some(session) = watching.as_ref().map(|w| w.session) else { continue };
                if let Some(live) = state.live.write().await.remove(&session) {
                    live.stop();
                    // The browser only leaves its working state on Done or
                    // Error; aborting silently leaves it stuck forever.
                    let _ = outbound.send(ChatEvent::Cancelled);
                }
            }
            ClientMessage::Attach { session } => {
                let Some(id) = rook_store::parse_session_id(&session) else {
                    let _ = outbound.send(ChatEvent::Error { message: format!("no session {session:?}") });
                    continue;
                };
                let live = state.live.read().await.get(&id).cloned();
                let running = live.as_ref().is_some_and(|l| l.running());
                let _ = outbound.send(ChatEvent::Attached { session, running });
                if let Some(live) = live {
                    watching = Some(watch(&live, id, outbound.clone(), watching));
                }
            }
            ClientMessage::Prompt { session, text } => {
                // The browser types it too, and it is the same thing there.
                let text = match rook_core::agent::carrying_on(&text) {
                    true => rook_core::agent::CARRY_ON.to_string(),
                    false => text,
                };
                let id = match session.as_deref().and_then(rook_store::parse_session_id) {
                    Some(id) => Some(id),
                    None if session.is_some() => None,
                    None => match engine.read().await.start_session("") {
                        Ok(id) => Some(id),
                        Err(e) => {
                            report(&outbound, e.to_string());
                            continue;
                        }
                    },
                };
                let Some(id) = id else {
                    report(&outbound, format!("no session {:?}", session.unwrap_or_default()));
                    continue;
                };
                // Typed while that session's turn runs, it goes to the turn:
                // the window had to wait or cancel, and cancelling loses
                // everything the turn had done to say one sentence to it.
                if let Some(live) = state.live.read().await.get(&id).filter(|l| l.running()).cloned() {
                    live.interjections.say(&text);
                    let _ = outbound.send(ChatEvent::Interjected { text });
                    watching = Some(watch(&live, id, outbound.clone(), watching));
                    continue;
                }
                // Before the turn, because a setting changed while the daemon
                // ran took a restart — and the restart was something a person
                // had to be told to do.
                if let Some(said) = state.config_if_changed().await {
                    let _ = outbound.send(ChatEvent::Text { text: format!("({said})\n") });
                }
                let live = begin(&state, &engine, &shared, &settings, id, text).await;
                watching = Some(watch(&live, id, outbound.clone(), watching));
                state.remember(id, live).await;
            }
        }
    }

    // The window is gone; the turn is not. Only the view of it ends here —
    // which is the whole point: an hour of work used to end with a closed tab.
    if let Some(watching) = watching {
        watching.carrying.abort();
    }
    drop(outbound);
    let _ = writer.await;
}

/// A window's view of one live turn.
struct Watching {
    session: u128,
    carrying: tokio::task::JoinHandle<()>,
}

/// The live turn this window is watching, if it is still registered.
async fn attached(state: &Arc<AppState>, watching: &Option<Watching>) -> Option<Arc<Live>> {
    let session = watching.as_ref()?.session;
    state.live.read().await.get(&session).cloned()
}

/// Carry one turn's events to one window: what it missed, then the rest.
///
/// Replaces whatever this window was watching before, so a window that moves
/// between sessions does not end up with two turns writing into it.
fn watch(
    live: &Arc<Live>,
    session: u128,
    to_window: mpsc::UnboundedSender<ChatEvent>,
    previous: Option<Watching>,
) -> Watching {
    if let Some(previous) = previous {
        previous.carrying.abort();
    }
    let (mut coming, missed) = live.join();
    let carrying = tokio::spawn(async move {
        for event in missed {
            if to_window.send(event).is_err() {
                return;
            }
        }
        loop {
            match coming.recv().await {
                Ok(event) => {
                    if to_window.send(event).is_err() {
                        return;
                    }
                }
                // Behind by more than the channel holds. The turn is fine and
                // this window is not: say so rather than silently skipping,
                // because a gap in a transcript reads as work that never
                // happened.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    let text = format!("\n[{missed} events not shown — this window fell behind]\n");
                    if to_window.send(ChatEvent::Text { text }).is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    Watching { session, carrying }
}

/// Start a turn that belongs to the daemon.
async fn begin(
    state: &Arc<AppState>,
    engine: &Arc<tokio::sync::RwLock<rook_core::Rook>>,
    shared: &Arc<tokio::sync::OnceCell<Shared>>,
    settings: &Arc<Settings>,
    session: u128,
    prompt: String,
) -> Arc<Live> {
    let patience = engine.read().await.config.agent.answer_timeout();
    // A question waits longer than an approval, and for the opposite reason:
    // an unanswered approval is denied and nothing was changed, while a turn
    // that stops on an unanswered question throws away everything it did to
    // reach it.
    let deciding = engine.read().await.config.agent.decide_alone_after();

    // What the turn writes into. One receiver, which fans it out to every
    // window attached and to the backlog for the next one.
    let (from_turn, mut events) = mpsc::unbounded_channel::<ChatEvent>();
    let (approver, relay) = approver(from_turn.clone(), patience);
    let (asker, ask_relay) = asker(from_turn.clone(), deciding);
    let interjections: Arc<rook_core::agent::Interjections> = Default::default();

    let (said, _) = tokio::sync::broadcast::channel::<ChatEvent>(BROADCAST);
    let backlog: Arc<std::sync::Mutex<std::collections::VecDeque<ChatEvent>>> = Default::default();
    let fan = {
        let (said, backlog) = (said.clone(), backlog.clone());
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                {
                    let mut kept = backlog.lock().unwrap_or_else(|e| e.into_inner());
                    if kept.len() >= BACKLOG {
                        kept.pop_front();
                    }
                    kept.push_back(event.clone());
                }
                // An error here is nobody attached, which is ordinary now.
                let _ = said.send(event);
            }
        })
    };

    // Counted while it runs: a daemon asked to stop should say what stopping
    // would interrupt rather than find out after.
    let counted = state.turn_started();
    let running_turn = turn(
        engine.clone(),
        Connection {
            approver: approver.clone(),
            asker: asker.clone(),
            settings: settings.clone(),
            interjections: interjections.clone(),
        },
        shared.clone(),
        from_turn,
        session,
        prompt,
    );
    let task = tokio::spawn(async move {
        // Dropped with the future, so a cancelled turn stops being counted
        // where it stops running.
        let _counted = counted;
        running_turn.await;
    });

    Arc::new(Live {
        task,
        helpers: vec![relay, ask_relay, fan],
        said,
        backlog,
        approver,
        asker,
        interjections,
    })
}

/// How long one frame may take to reach a client before the socket counts as
/// gone rather than slow.
///
/// A `send` on a socket nobody is reading blocks once the kernel's buffer
/// fills, and there is no error to notice: the writer waits, the turn goes on
/// producing deltas into the queue in front of it, and neither ever ends. A
/// browser tab that has been throttled to a stop looks exactly like this. Half
/// a minute for one frame is not slow, it is away — and the queue is what makes
/// this a bound rather than a nicety, because it grows for as long as the writer
/// is stuck.
const SEND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Every event, in order, until the socket stops taking them.
///
/// Its own function so the deadline can be tested with a sink that never
/// completes, rather than by holding a real socket unread for thirty seconds.
async fn write_frames<S>(mut sink: S, mut queued: mpsc::UnboundedReceiver<ChatEvent>)
where
    S: SinkExt<Message> + Unpin,
{
    while let Some(event) = queued.recv().await {
        let Ok(text) = serde_json::to_string(&event) else { continue };
        // Both endings are one ending: this socket is not taking frames.
        // Dropping the sink closes the write half, which is what tells the
        // client — and closes the read half's loop, which stops the turn.
        match tokio::time::timeout(SEND_DEADLINE, sink.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
}

/// What the connection gives a turn: who answers its questions, and what the
/// user has set for the rest of the session.
/// A turn the daemon is running, and everything a window needs to join it.
///
/// The turn used to be a `JoinHandle` held by one socket, aborted when that
/// socket closed. An hour of work ended with a window and nothing said why —
/// and there was no way to leave a turn running and come back to it, which is
/// the thing a long turn most needs.
///
/// It is here instead, and a window is a view. What the turn says goes to
/// everyone attached and into a bounded backlog for whoever attaches next; the
/// approver and the asker belong to the turn, so a question put while nobody
/// was watching is still there to answer when somebody is.
pub struct Live {
    /// Aborting this is what `Cancel` means, and the only thing that ends a
    /// turn early. A window closing is a window closing.
    task: tokio::task::JoinHandle<()>,
    /// The relays and the fan-out, which have nothing to do once the turn is
    /// gone and would otherwise outlive it as tasks nobody can reach.
    helpers: Vec<tokio::task::JoinHandle<()>>,
    said: tokio::sync::broadcast::Sender<ChatEvent>,
    /// What was said before anyone attached, oldest first.
    ///
    /// Bounded, like everything else that accumulates here: a turn that runs
    /// for an hour with no window open would otherwise hold every token it
    /// produced. Past the bound the oldest go, because the end of a turn is
    /// what somebody joining it wants.
    backlog: Arc<std::sync::Mutex<std::collections::VecDeque<ChatEvent>>>,
    approver: Arc<ChannelApprover>,
    asker: Arc<ChannelAsker>,
    interjections: Arc<rook_core::agent::Interjections>,
}

/// Enough to read the end of a long turn, and far short of holding all of one.
const BACKLOG: usize = 2_000;

/// How far behind a window may fall before it is told rather than quietly
/// skipped. A delta is a few words, so this is a paragraph or two of slack for
/// a tab the browser has throttled.
const BROADCAST: usize = 4_096;

impl Live {
    /// Assembled from parts, so the registry's own bookkeeping can be asked
    /// about without starting a turn to ask it.
    #[doc(hidden)]
    pub fn for_test(
        task: tokio::task::JoinHandle<()>,
        helpers: Vec<tokio::task::JoinHandle<()>>,
        said: tokio::sync::broadcast::Sender<ChatEvent>,
        approver: Arc<ChannelApprover>,
        asker: Arc<ChannelAsker>,
    ) -> Self {
        Self {
            task,
            helpers,
            said,
            backlog: Default::default(),
            approver,
            asker,
            interjections: Default::default(),
        }
    }

    pub fn running(&self) -> bool {
        !self.task.is_finished()
    }

    /// Everything said so far, then everything said from now on.
    ///
    /// Subscribed before the backlog is read, so an event that lands between
    /// the two is seen twice rather than not at all — a repeated line is a
    /// blemish and a missing one is a turn that looks stuck.
    fn join(&self) -> (tokio::sync::broadcast::Receiver<ChatEvent>, Vec<ChatEvent>) {
        let live = self.said.subscribe();
        let missed = self.backlog.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect();
        (live, missed)
    }

    /// End the turn and everything that was carrying it.
    fn stop(&self) {
        self.task.abort();
        for helper in &self.helpers {
            helper.abort();
        }
    }
}

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
    session: u128,
    prompt: String,
) {
    // Owned so the guard outlives this task's spawn point.
    let rook = engine.read_owned().await;
    // Resolved by the caller, because the daemon has to register the turn
    // under its session before it starts one — a turn that names itself after
    // it is already running cannot be joined while it does so.
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
    // Cloned out before the loop borrows the agent: a call's phrase names paths
    // the way somebody standing in this project would.
    let workspace = rook.workspace.clone();
    let result = agent
        .run_with(&prompt, |progress| {
            let event = match progress {
                Progress::Delta(Delta::Text(text)) => ChatEvent::Text { text: text.clone() },
                Progress::Delta(Delta::Reasoning(text)) => ChatEvent::Reasoning { text: text.clone() },
                Progress::Delta(Delta::ToolCall(call)) => ChatEvent::Tool {
                    name: call.name.clone(),
                    doing: rook_core::calls::doing(&call.name, Some(&call.arguments), &workspace),
                },
                Progress::Delegated { task, done, total } => {
                    ChatEvent::Reasoning { text: format!("\n  [{done}/{total}] {task}") }
                }
                // Counted from one, because the reader is a person and the
                // first sub-agent is the first, not the zeroth.
                Progress::Delegating { at, doing } => ChatEvent::Reasoning {
                    text: format!("\n    {}", rook_core::calls::delegating(at, doing)),
                },
                Progress::ToolDone { name, failed } => ChatEvent::ToolDone { name: name.to_string(), failed },
                Progress::Step { at, of } => ChatEvent::Step { at, of },
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
                files_changed: outcome.files_changed,
                stopped: outcome.stopped,
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
pub(crate) fn asker(
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
pub(crate) fn approver(
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
                kind: request.kind,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A live turn with nothing actually running in it, to ask the questions a
    /// window joining one asks.
    fn parked(said: tokio::sync::broadcast::Sender<ChatEvent>) -> Live {
        let (to_turn, _held) = mpsc::unbounded_channel::<ChatEvent>();
        let (approver, relay) = approver(to_turn.clone(), std::time::Duration::from_secs(1));
        let (asker, ask_relay) = asker(to_turn, std::time::Duration::from_secs(1));
        Live {
            task: tokio::spawn(std::future::pending()),
            helpers: vec![relay, ask_relay],
            said,
            backlog: Default::default(),
            approver,
            asker,
            interjections: Default::default(),
        }
    }

    fn text(what: &str) -> ChatEvent {
        ChatEvent::Text { text: what.to_string() }
    }

    /// A window that joins a turn already running has to be given what it
    /// missed, or it shows a turn that is halfway through something and looks
    /// as if it started there. And what it missed is bounded: a turn running
    /// for an hour with nobody watching would otherwise hold every token.
    #[tokio::test]
    async fn joining_a_running_turn_gives_what_was_missed_and_then_the_rest() {
        let (said, _) = tokio::sync::broadcast::channel::<ChatEvent>(16);
        let live = parked(said.clone());

        // Said before anybody was watching.
        for i in 0..3 {
            let event = text(&format!("before {i}"));
            let mut kept = live.backlog.lock().unwrap();
            kept.push_back(event);
        }

        let (mut coming, missed) = live.join();
        assert_eq!(missed.len(), 3, "everything said before the window arrived");
        assert!(matches!(&missed[0], ChatEvent::Text { text } if text == "before 0"), "oldest first");

        let _ = said.send(text("after"));
        let next = coming.recv().await.expect("and then what happens next");
        assert!(matches!(&next, ChatEvent::Text { text } if text == "after"));

        assert!(live.running(), "a turn nobody is watching is still running");
        live.stop();
        // The task is aborted, which the runtime completes at the next yield.
        tokio::task::yield_now().await;
        assert!(!live.running(), "and `Cancel` is the thing that ends it");
    }

    /// The backlog is what a window joining late reads, so it keeps the end of
    /// the turn rather than the start of it.
    #[test]
    fn the_backlog_keeps_the_end_of_a_turn_rather_than_all_of_it() {
        let backlog: std::sync::Mutex<std::collections::VecDeque<ChatEvent>> = Default::default();
        for i in 0..BACKLOG + 50 {
            let mut kept = backlog.lock().unwrap();
            if kept.len() >= BACKLOG {
                kept.pop_front();
            }
            kept.push_back(text(&format!("{i}")));
        }
        let kept = backlog.lock().unwrap();
        assert_eq!(kept.len(), BACKLOG, "bounded, and reached — or this proves nothing");
        assert!(
            matches!(kept.back(), Some(ChatEvent::Text { text }) if text == &format!("{}", BACKLOG + 49)),
            "the newest is the one kept"
        );
    }

    /// A sink that accepts a frame and then never finishes another, which is
    /// what a socket whose reader has stopped does once the buffer fills.
    struct Stalls {
        took: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl futures_util::Sink<Message> for Stalls {
        type Error = std::convert::Infallible;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            match self.took.load(std::sync::atomic::Ordering::SeqCst) {
                0 => std::task::Poll::Ready(Ok(())),
                // Pending forever, and never waking: nothing is coming.
                _ => std::task::Poll::Pending,
            }
        }
        fn start_send(self: std::pin::Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            self.took.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// The writer waited forever on a socket nobody was reading, and the turn
    /// went on filling the queue in front of it: neither the connection nor the
    /// memory it was using ever ended.
    ///
    /// Time is paused, so this is the deadline being tested and not thirty
    /// seconds being spent.
    #[tokio::test(start_paused = true)]
    async fn a_socket_that_stops_taking_frames_is_let_go_of() {
        let took = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (outbound, queued) = mpsc::unbounded_channel::<ChatEvent>();
        for text in ["one", "two", "three"] {
            outbound.send(ChatEvent::Text { text: text.into() }).unwrap();
        }

        let writer = tokio::spawn(write_frames(Stalls { took: took.clone() }, queued));
        // Held open, as a stalled client holds it: the writer has to end on the
        // deadline rather than because the queue closed.
        let done = tokio::time::timeout(SEND_DEADLINE * 3, writer).await;

        assert!(done.is_ok(), "the writer let go of the socket");
        assert_eq!(took.load(std::sync::atomic::Ordering::SeqCst), 1, "after the one frame it took");
        drop(outbound);
    }
}
