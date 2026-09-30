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
use rook_core::work::managed;
use rook_llm::Delta;
use rook_proto::AskQuestion;
use rook_proto::work::{Action, Conversation, Run, Start, Status, Steer};
use rook_proto::{ApprovalDecision, ChatEvent, ClientMessage};
use rook_tools::ask::{AskRequest, ChannelAsker};
use rook_tools::policy::{Approval, ChannelApprover};

use crate::AppState;

use rook_core::delivery;
mod replay;

/// `?workspace=` names the project this conversation is in, defaulting to the
/// daemon's own. A connection is bound to one for its life, because a project is
/// what a conversation is about — not something a single prompt changes.
#[derive(serde::Deserialize)]
pub struct Where {
    workspace: Option<std::path::PathBuf>,
    #[serde(default)]
    live_snapshots: bool,
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
    ws.max_message_size(rook_core::attachments::MAX_FRAME_BYTES)
        .max_frame_size(rook_core::attachments::MAX_FRAME_BYTES)
        .on_upgrade(move |socket| serve(socket, engine, equipment, state, here.live_snapshots))
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

/// Host is untrusted input, even when Origin repeats it (DNS rebinding).
/// Explicit proxy aliases are configuration, not values supplied by the caller.
pub async fn trusted_authority(
    State(allowed): State<Vec<String>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let authority = request.headers().get(axum::http::header::HOST).and_then(|h| h.to_str().ok());
    let trusted = authority.is_some_and(|host| {
        if allowed.iter().any(|a| a == host) {
            return true;
        }
        host.parse::<axum::http::uri::Authority>().is_ok_and(|a| {
            let name = a.host().trim_matches(['[', ']']);
            name.eq_ignore_ascii_case("localhost")
                || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
        })
    });
    if !trusted || !from_this_daemon(request.headers()) {
        return (axum::http::StatusCode::FORBIDDEN, "untrusted Host or Origin").into_response();
    }
    next.run(request).await
}

async fn serve(
    socket: WebSocket,
    engine: Arc<tokio::sync::RwLock<rook_core::Rook>>,
    shared: Arc<tokio::sync::OnceCell<Shared>>,
    state: Arc<AppState>,
    live_snapshots: bool,
) {
    let (sink, mut stream) = socket.split();
    let limits = engine.read().await.config.server.clone();
    let (outbound, queued) = delivery::channel(limits.chat_queue_events, limits.chat_queue_bytes);

    // One writer task: the turn, the approver and the error path all emit
    // concurrently, and a socket has a single writer.
    let writer = tokio::spawn(write_frames(sink, queued));

    // Settings are cheap and wanted before the first prompt, so they are not in
    // the cell with the expensive things.
    let settings = Arc::new(Settings::new(&*engine.read().await));
    let _ = outbound.send(settings.describe()).await;

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
                // The turn's, while one is running here: changing a stance in a
                // window that is watching a turn means changing that turn's.
                let theirs = attached(&state, &watching).await.filter(|l| l.running());
                let setting = theirs.as_ref().map(|l| l.settings.clone()).unwrap_or(settings.clone());
                if let Err(message) = setting.set(&name, &value) {
                    let _ = outbound.send(ChatEvent::Error { message }).await;
                    continue;
                }
                let mut persistence_error = None;
                if let Some(session) = watching.as_ref().map(|w| w.session) {
                    let rook = engine.read().await;
                    if let Ok(Some(run)) = session_goal(&rook, session)
                        && !run.status.terminal()
                        && let Err(error) = managed::update(&rook, &run.id, |saved| {
                            if let Some(c) = &mut saved.run.conversation {
                                c.model = setting.model();
                                c.effort = setting.effort().as_str().into();
                                c.stance = setting.policy.stance().as_str().into();
                            }
                            Ok(())
                        })
                    {
                        persistence_error =
                            Some(format!("setting changed, but could not save it for restart: {error}"));
                    }
                }
                if let Some(error) = persistence_error {
                    report_window(&outbound, error).await;
                }
                let _ = outbound.send(setting.describe()).await;
            }
            ClientMessage::Cancel => {
                let Some(session) = watching.as_ref().map(|w| w.session) else { continue };
                let id = rook_store::format_session_id(session);
                let goal = session_goal(&*engine.read().await, session);
                match goal {
                    Ok(Some(run)) if !run.status.terminal() => {
                        let paused = managed::control(&*engine.read().await, &id, Action::Pause);
                        match paused {
                            Ok(_) => {
                                let _ = outbound
                                    .send(ChatEvent::Agent {
                                        receipt: None,
                                        text:
                                            "Pausing goal after the active operation; /continue resumes it."
                                                .into(),
                                    })
                                    .await;
                            }
                            Err(error) => report_window(&outbound, error.to_string()).await,
                        }
                        continue;
                    }
                    Err(error) => {
                        report_window(&outbound, error).await;
                        continue;
                    }
                    _ => {}
                }
                let cancelled = state.live.write().await.remove(&session);
                if let Some(live) = cancelled {
                    live.stop();
                    // The browser only leaves its working state on Done or
                    // Error; aborting silently leaves it stuck forever.
                    let _ = outbound.send(ChatEvent::Cancelled).await;
                }
            }
            ClientMessage::Attach { session } => {
                let Some(id) = rook_store::parse_session_id(&session) else {
                    let _ =
                        outbound.send(ChatEvent::Error { message: format!("no session {session:?}") }).await;
                    continue;
                };
                let live = state.live.read().await.get(&id).cloned();
                if let Some(previous) = watching.take() {
                    previous.carrying.abort();
                }
                let running = live.as_ref().is_some_and(|l| l.running());
                let _ = outbound.send(ChatEvent::Attached { session, running }).await;
                if let Some(live) = live {
                    // What it is running under, not what this window was
                    // showing: a footer reading `autonomous` over a turn in
                    // `assist` explains none of the approvals it asks for.
                    if running {
                        let _ = outbound.send(live.settings.describe()).await;
                    }
                    watching = Some(watch(&live, id, outbound.clone(), watching, live_snapshots));
                }
            }
            ClientMessage::Prompt { session, text, options } => {
                // The browser types it too, and it is the same thing there.
                let text = match rook_core::agent::carrying_on(&text) {
                    true => rook_core::agent::CARRY_ON.to_string(),
                    false => text,
                };
                let id = match session.as_deref().and_then(rook_store::parse_session_id) {
                    Some(id) => Some(id),
                    None if session.is_some() => None,
                    None => {
                        let started = engine.read().await.start_session("");
                        match started {
                            Ok(id) => Some(id),
                            Err(e) => {
                                report_window(&outbound, e.to_string()).await;
                                continue;
                            }
                        }
                    }
                };
                let Some(id) = id else {
                    report_window(&outbound, format!("no session {:?}", session.unwrap_or_default())).await;
                    continue;
                };
                let requested_goal = text.strip_prefix("/goal ").map(str::trim).filter(|s| !s.is_empty());
                let goal = session_goal(&*engine.read().await, id);
                let existing = match goal {
                    Ok(run) => run.filter(|r| !r.status.terminal()),
                    Err(error) => {
                        report_window(&outbound, error).await;
                        continue;
                    }
                };
                if requested_goal.is_some() || existing.is_some() {
                    if !options.attachments.is_empty() && existing.is_some() {
                        report_window(&outbound, "Attachments cannot be added to a running goal.".into())
                            .await;
                        continue;
                    }
                    let (goal_engine, _) = match where_it_belongs(&state, id).await {
                        Ok(Some(theirs)) => theirs,
                        Ok(None) => (engine.clone(), shared.clone()),
                        Err(error) => {
                            report_window(&outbound, error).await;
                            continue;
                        }
                    };
                    let promotion = if existing.is_none() {
                        state.live.read().await.get(&id).filter(|live| live.running()).cloned()
                    } else {
                        None
                    };
                    let starting = existing.is_none() && promotion.is_none();
                    let selected = promotion
                        .as_ref()
                        .map(|live| live.settings.clone())
                        .unwrap_or_else(|| settings.clone());
                    let previously_watched = watching.as_ref().and_then(|w| w.live.upgrade());
                    let mut interjected = None;
                    let result = {
                        let rook = goal_engine.read().await;
                        if let Some(run) = existing {
                            (|| {
                                if !rook_core::agent::carrying_on(&text) && text != rook_core::agent::CARRY_ON
                                {
                                    let (_, notice) = managed::steer_noticed(
                                        &rook,
                                        &run.id,
                                        Steer {
                                            id: rook_store::format_session_id(rook_store::new_session_id()),
                                            text: text.clone(),
                                        },
                                    )?;
                                    interjected = Some(notice);
                                }
                                if !run.status.runnable() {
                                    managed::control(&rook, &run.id, Action::Resume)
                                } else {
                                    Ok(run)
                                }
                            })()
                        } else {
                            managed::start(
                                &rook,
                                Start {
                                    goal: requested_goal.unwrap_or_default().into(),
                                    workspace: None,
                                    autonomous: false,
                                    max_iterations: Some(0),
                                    max_tokens: Some(0),
                                    max_seconds: Some(0),
                                    conversation: Some(Conversation {
                                        session: rook_store::format_session_id(id),
                                        model: selected.model(),
                                        effort: selected.effort().as_str().into(),
                                        stance: selected.policy.stance().as_str().into(),
                                        options,
                                    }),
                                },
                            )
                        }
                    };
                    if let Some(receipt) = interjected {
                        let _ = outbound
                            .send(ChatEvent::Interjected { receipt: Some(receipt), text: text.clone() })
                            .await;
                    }
                    match result {
                        Ok(run) => match crate::work::join_conversation(&state, &run).await {
                            Ok(live) => {
                                if promotion.is_some() {
                                    let receipt = managed::steer_noticed(
                                        &*goal_engine.read().await,
                                        &run.id,
                                        Steer {
                                            id: rook_store::format_session_id(rook_store::new_session_id()),
                                            text: text.clone(),
                                        },
                                    );
                                    match receipt {
                                        Ok((_, receipt)) => {
                                            let _ = outbound
                                                .send(ChatEvent::Interjected {
                                                    receipt: Some(receipt),
                                                    text: text.clone(),
                                                })
                                                .await;
                                        }
                                        Err(error) => report_window(&outbound, error.to_string()).await,
                                    }
                                }
                                let _ = outbound.send(live.settings.describe()).await;
                                if !previously_watched
                                    .as_ref()
                                    .is_some_and(|previous| Arc::ptr_eq(previous, &live))
                                {
                                    watching = Some(carry_view(
                                        &live,
                                        id,
                                        outbound.clone(),
                                        watching,
                                        live_snapshots,
                                        !starting,
                                    ));
                                }
                            }
                            Err(error) => report_window(&outbound, error).await,
                        },
                        Err(error) => report_window(&outbound, error.to_string()).await,
                    }
                    continue;
                }
                // Typed while that session's turn runs, it goes to the turn:
                // the window had to wait or cancel, and cancelling loses
                // everything the turn had done to say one sentence to it.
                let current = state.live.read().await.get(&id).filter(|l| l.running()).cloned();
                if let Some(live) = current {
                    if !options.attachments.is_empty() {
                        report_window(&outbound, "Attachments cannot be added to a running turn; wait for it to finish or stop it first.".into()).await;
                        continue;
                    }
                    let receipt = rook_core::message_queue::submit_noticed(
                        &*engine.read().await,
                        id,
                        Steer {
                            id: rook_store::format_session_id(rook_store::new_session_id()),
                            text: text.clone(),
                        },
                    );
                    let (_, receipt) = match receipt {
                        Ok(value) => value,
                        Err(error) => {
                            report_window(&outbound, error.to_string()).await;
                            continue;
                        }
                    };
                    let _ = outbound.send(ChatEvent::Interjected { receipt: Some(receipt), text }).await;
                    // Steering the turn already on screen is not a rejoin:
                    // replacing its view erases local submission receipts.
                    if !watching.as_ref().is_some_and(|w| w.live.ptr_eq(&Arc::downgrade(&live))) {
                        watching = Some(watch(&live, id, outbound.clone(), watching, live_snapshots));
                    }
                    continue;
                }
                // Before the turn, because a setting changed while the daemon
                // ran took a restart — and the restart was something a person
                // had to be told to do.
                if let Some(said) = state.config_if_changed().await {
                    let _ = outbound.send(ChatEvent::Text { text: format!("({said})\n") }).await;
                }
                // A session is somewhere, and continuing one runs it there
                // rather than wherever the window happens to be. A turn
                // resumed from a connection that named no project got the
                // daemon's own workspace: it audited one repository from
                // inside another, was refused the paths it had been reading
                // all along, and said so — which is how this was found.
                let (engine, shared) = match where_it_belongs(&state, id).await {
                    Ok(Some(theirs)) => theirs,
                    Ok(None) => (engine.clone(), shared.clone()),
                    Err(why) => {
                        report_window(&outbound, why).await;
                        continue;
                    }
                };
                let live = begin(&state, &engine, &shared, &settings, id, text, options).await;
                watching = Some(carry_view(&live, id, outbound.clone(), watching, live_snapshots, false));
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

/// Where a session's turns run: the workspace it was started in, always.
///
/// Not the window's. A session's checkpoints were taken there, its reads were
/// relative to it, and a rewind restores into it — a turn continued anywhere
/// else is a conversation about one project carried out in another. That
/// happened: an audit resumed from a connection that named no project got the
/// daemon's own workspace, was refused the paths it had been reading all along,
/// asked for more latitude to get past the refusals, and was given it.
///
/// A workspace that is gone is a refusal rather than a fallback. Falling back
/// to the window's is exactly the thing above, arrived at politely.
pub(crate) async fn where_it_belongs(
    state: &Arc<AppState>,
    session: u128,
) -> Result<Option<(Arc<tokio::sync::RwLock<rook_core::Rook>>, Arc<tokio::sync::OnceCell<Shared>>)>, String> {
    // Unknown here is a session this daemon has not seen — a new one, which
    // starts where the window is standing.
    let Ok(Some(meta)) = state.rook.read().await.store.get_session(session) else { return Ok(None) };
    let theirs = std::path::PathBuf::from(&meta.workspace);
    let engine = state.engine_for(Some(&theirs)).await.map_err(|why| {
        format!(
            "this session belongs to {} and that is where it has to continue, but {why}",
            theirs.display()
        )
    })?;
    let shared = state.equipment_for(&engine).await;
    Ok(Some((engine, shared)))
}

/// A window's view of one live turn.
struct Watching {
    session: u128,
    live: std::sync::Weak<Live>,
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
    to_window: delivery::Sender,
    previous: Option<Watching>,
    live_snapshots: bool,
) -> Watching {
    carry_view(live, session, to_window, previous, live_snapshots, true)
}

fn carry_view(
    live: &Arc<Live>,
    session: u128,
    to_window: delivery::Sender,
    previous: Option<Watching>,
    live_snapshots: bool,
    mut replace: bool,
) -> Watching {
    if let Some(previous) = previous {
        previous.carrying.abort();
    }
    let watched = Arc::downgrade(live);
    let live = live.clone();
    let carrying = tokio::spawn(async move {
        let mut approvals_changed = live.approver.changes();
        let mut questions_changed = live.asker.changes();
        loop {
            let (mut coming, missed, truncated) = live.join();
            // Starting a new turn keeps this window's prior conversation and
            // optimistic prompt. Reconnect/lag replaces the view; a fresh
            // turn does so only if it already outgrew the replay budget.
            if live_snapshots && (replace || truncated) {
                let approvals = missed.iter().filter_map(|e| match e { ChatEvent::Approval { id, .. } => Some(id.clone()), _ => None }).collect();
                let questions = missed.iter().filter_map(|e| match e { ChatEvent::Ask { id, .. } => Some(id.clone()), _ => None }).collect();
                if to_window.send(ChatEvent::Snapshot {
                    session: rook_store::format_session_id(session), running: live.running(), truncated,
                    approvals, questions,
                }).await.is_err() { return; }
            } else if truncated && to_window.send(ChatEvent::Text {
                text: "\n[live view refreshed; recent output follows; saved conversation is in history]\n".into(),
            }).await.is_err() { return; }
            if to_window.send(live.settings.describe()).await.is_err() {
                return;
            }
            for event in missed {
                if to_window.send(event).await.is_err() {
                    return;
                }
            }
            loop {
                tokio::select! {
                    changed = approvals_changed.changed(), if live_snapshots => {
                        if changed.is_err() || send_inputs(&live, &to_window).await.is_err() { return; }
                    }
                    changed = questions_changed.changed(), if live_snapshots => {
                        if changed.is_err() || send_inputs(&live, &to_window).await.is_err() { return; }
                    }
                    notification = coming.recv() => match notification {
                        Ok(sequence) => {
                            let event = live.backlog.lock().unwrap_or_else(|e| e.into_inner()).get(sequence);
                            let Some(event) = event else { break };
                            // Current requests arrive from their authoritative map,
                            // including resolution; an old relay event is not state.
                            if live_snapshots && matches!(event, ChatEvent::Approval { .. } | ChatEvent::Ask { .. }) { continue; }
                            if to_window.send(event).await.is_err() { return; }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
            // The observer has fallen out of the retained range. Rejoin at
            // one atomic snapshot/subscription boundary; the turn keeps going.
            replace = true;
        }
    });
    Watching { session, live: watched, carrying }
}

async fn send_inputs(live: &Live, to_window: &delivery::Sender) -> Result<(), delivery::Closed> {
    let events = live.inputs();
    let approvals = events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Approval { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    let questions = events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Ask { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    to_window.send(ChatEvent::Inputs { approvals, questions }).await?;
    for event in events {
        to_window.send(event).await?;
    }
    Ok(())
}

/// Start a turn that belongs to the daemon.
async fn begin(
    state: &Arc<AppState>,
    engine: &Arc<tokio::sync::RwLock<rook_core::Rook>>,
    shared: &Arc<tokio::sync::OnceCell<Shared>>,
    settings: &Arc<Settings>,
    session: u128,
    prompt: String,
    options: rook_proto::TurnOptions,
) -> Arc<Live> {
    let input_limits = engine.read().await.config.user_input;
    let patience = engine.read().await.config.agent.answer_timeout();
    // A question waits longer than an approval, and for the opposite reason:
    // an unanswered approval is denied and nothing was changed, while a turn
    // that stops on an unanswered question throws away everything it did to
    // reach it.
    let deciding = engine.read().await.config.agent.decide_alone_after();

    // What the turn writes into. One receiver, which fans it out to every
    // window attached and to the backlog for the next one.
    let (from_turn, events) = mpsc::unbounded_channel::<ChatEvent>();
    let (approver, relay) = approver(from_turn.clone(), patience, input_limits);
    let (asker, ask_relay) = asker(from_turn.clone(), deciding, input_limits);

    let (said, _) = tokio::sync::broadcast::channel::<u64>(BROADCAST);
    let replay_limits = engine.read().await.config.server.clone();
    let backlog = Arc::new(std::sync::Mutex::new(replay::Replay::new(
        replay_limits.chat_replay_events,
        replay_limits.chat_replay_bytes,
        replay_limits.chat_queue_bytes,
    )));

    // Counted while it runs: a daemon asked to stop should say what stopping
    // would interrupt rather than find out after.
    let counted = state.turn_started();
    let running_turn = turn(
        engine.clone(),
        Connection { approver: approver.clone(), asker: asker.clone(), settings: settings.clone(), options },
        shared.clone(),
        from_turn,
        session,
        prompt,
    );
    let helpers = vec![relay.abort_handle(), ask_relay.abort_handle()];
    let ending = helpers.clone();
    let task = tokio::spawn({
        let (said, backlog) = (said.clone(), backlog.clone());
        async move {
            // Dropped with the future, so a cancelled turn stops being counted
            // where it stops running.
            let _counted = counted;
            everything_it_says(running_turn, events, said, backlog, ending).await;
        }
    });

    Arc::new(Live { task, helpers, said, backlog, approver, asker, settings: settings.clone() })
}

fn session_goal(rook: &rook_core::Rook, session: u128) -> Result<Option<Run>, String> {
    managed::for_session(rook, session).map_err(|e| e.to_string())
}

/// Recovery recreates the same live conversation, so joining it uses the
/// existing session picker, stream and approval controls.
pub(crate) async fn resume_goal(state: &Arc<AppState>, run: &Run) -> Result<Arc<Live>, String> {
    let conversation = run.conversation.as_ref().ok_or("not a conversation goal")?;
    let session = rook_store::parse_session_id(&conversation.session).ok_or("invalid session")?;
    let engine = state.engine_for(Some(std::path::Path::new(&run.workspace))).await?;
    let shared = state.equipment_for(&engine).await;
    let settings = Arc::new(Settings::new(&*engine.read().await));
    settings.set("stance", &conversation.stance)?;
    settings.set("effort", &conversation.effort)?;
    if let Some(model) = &conversation.model {
        settings.set("model", model)?;
    }
    Ok(begin(state, &engine, &shared, &settings, session, run.goal.clone(), conversation.options.clone())
        .await)
}

/// How long one frame may take to reach a client before the socket counts as
/// gone rather than slow.
///
/// A `send` on a socket nobody is reading blocks once the kernel's buffer
/// fills, and there is no error to notice. The queue is independently bounded
/// by encoded bytes and frames; this deadline releases a connection whose
/// reader has gone away without ending the daemon-owned turn.
const SEND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Every event, in order, until the socket stops taking them.
///
/// Its own function so the deadline can be tested with a sink that never
/// completes, rather than by holding a real socket unread for thirty seconds.
async fn write_frames<S>(mut sink: S, mut queued: delivery::Receiver)
where
    S: SinkExt<Message> + Unpin,
{
    while let Some(mut frame) = queued.recv().await {
        let text = std::mem::take(&mut frame.text);
        // Both endings are one ending: this socket is not taking frames.
        // Closing the view leaves the daemon-owned turn running.
        match tokio::time::timeout(SEND_DEADLINE, sink.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let _ = tokio::time::timeout(SEND_DEADLINE, sink.close()).await;
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
    ///
    /// Abort handles rather than the tasks themselves, so the turn can end them
    /// when it finishes and `Cancel` can end them when it does not — the same
    /// three either way, without either owner having to be the only one.
    helpers: Vec<tokio::task::AbortHandle>,
    said: tokio::sync::broadcast::Sender<u64>,
    /// What was said before anyone attached, oldest first.
    ///
    /// Bounded, like everything else that accumulates here: a turn that runs
    /// for an hour with no window open would otherwise hold every token it
    /// produced. Past the bound the oldest go, because the end of a turn is
    /// what somebody joining it wants.
    backlog: Arc<std::sync::Mutex<replay::Replay>>,
    approver: Arc<ChannelApprover>,
    asker: Arc<ChannelAsker>,
    /// What this turn is actually running under.
    ///
    /// The connection that started it chose them, and once a turn outlived its
    /// connection the two stopped being the same thing: a window showed its own
    /// stance in the footer while the turn ran under the one it was started
    /// with, so `autonomous` on the screen asked for approvals because the turn
    /// was in `assist`. A window that joins a turn is shown the turn's, and
    /// changes those rather than its own.
    settings: Arc<Settings>,
}

/// Runs a turn and carries what it says, to the last word.
///
/// The two used to be separate tasks, and the ending raced: a turn's last word
/// — `Done`, or the error that ended it — is put in the queue as the turn
/// returns, and the next line aborted the task that empties the queue. Whether
/// that task had been polled in between was the scheduler's business. Once it
/// had not, and the result was the worst shape a failure can take: eleven
/// minutes of real work, a window still drawing `working…` over it, and a
/// daemon reporting `turns_running: 0` beside it. Nothing was wrong and nothing
/// said so.
async fn everything_it_says(
    running_turn: impl std::future::Future<Output = ()>,
    mut events: mpsc::UnboundedReceiver<ChatEvent>,
    said: tokio::sync::broadcast::Sender<u64>,
    backlog: Arc<std::sync::Mutex<replay::Replay>>,
    ending: Vec<tokio::task::AbortHandle>,
) {
    tokio::pin!(running_turn);
    loop {
        tokio::select! {
            // What has been said goes out before the turn is noticed to have
            // ended, so the two cannot swap places.
            biased;
            Some(event) = events.recv() => fan_out(&backlog, &said, event),
            () = &mut running_turn => break,
        }
    }
    // Nothing more will be asked or answered, and the relays hold senders that
    // would otherwise keep the queue open for as long as the registry keeps the
    // turn.
    for helper in ending {
        helper.abort();
    }
    // The turn has returned and the relays are stopped, so nothing is still
    // being sent: what is left in the queue is the turn's last word, and
    // reading it out is exact rather than a pause long enough to probably be
    // enough.
    while let Ok(event) = events.try_recv() {
        fan_out(&backlog, &said, event);
    }
}

/// One thing a turn said, to everyone who will ever want it: the windows
/// attached now, and the backlog for a window that attaches later.
fn fan_out(
    backlog: &std::sync::Mutex<replay::Replay>,
    said: &tokio::sync::broadcast::Sender<u64>,
    event: ChatEvent,
) {
    let mut kept = backlog.lock().unwrap_or_else(|e| e.into_inner());
    let sequence = kept.push(event);
    // Payload lives only in the bounded replay. The broadcast ring contains
    // sequence notifications, so old broadcasts cannot pin evicted payloads.
    let _ = said.send(sequence);
}

/// How far behind a window may fall before it is told rather than quietly
/// skipped. Only sequence numbers live in this ring; payloads live in the
/// count/byte-bounded replay and a lagging observer takes a fresh snapshot.
const BROADCAST: usize = 4_096;

impl Live {
    pub(crate) fn queue_notice(&self, receipt: rook_proto::queue::Notice, text: String) {
        if self.running() {
            let status = match receipt.status {
                rook_proto::queue::Status::Queued => "queued",
                rook_proto::queue::Status::Accepted => "accepted",
                rook_proto::queue::Status::Withdrawn => "withdrawn",
            };
            let text = if receipt.status == rook_proto::queue::Status::Queued {
                format!("↩ {text}")
            } else {
                format!("Message {status}: {text}")
            };
            fan_out(&self.backlog, &self.said, ChatEvent::Agent { text, receipt: Some(receipt) });
        }
    }
    pub(crate) fn needs_input(&self) -> bool {
        self.running() && (self.approver.is_waiting() || self.asker.is_waiting())
    }

    /// Assembled from parts, so the registry's own bookkeeping can be asked
    /// about without starting a turn to ask it.
    #[doc(hidden)]
    #[cfg(test)]
    pub fn for_test(
        task: tokio::task::JoinHandle<()>,
        helpers: Vec<tokio::task::AbortHandle>,
        said: tokio::sync::broadcast::Sender<u64>,
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
            settings: Arc::new(Settings::for_test()),
        }
    }

    pub fn running(&self) -> bool {
        !self.task.is_finished()
    }

    /// Everything said so far, then everything said from now on.
    ///
    /// The replay boundary and subscription are atomic with publication.
    /// Input controls come from live requests; a historical approval is not
    /// evidence that anything still waits for a person's decision.
    fn join(&self) -> (tokio::sync::broadcast::Receiver<u64>, Vec<ChatEvent>, bool) {
        let kept = self.backlog.lock().unwrap_or_else(|e| e.into_inner());
        let live = self.said.subscribe();
        let (events, truncated) = kept.snapshot();
        let mut missed: Vec<_> = events
            .into_iter()
            .filter(|event| !matches!(event, ChatEvent::Approval { .. } | ChatEvent::Ask { .. }))
            .collect();
        missed.extend(self.inputs());
        (live, missed, truncated)
    }

    fn inputs(&self) -> Vec<ChatEvent> {
        let mut missed = Vec::new();
        for request in self.approver.current() {
            missed.push(ChatEvent::Approval {
                id: request.id,
                tool: request.tool,
                action: request.action,
                preview: request.preview,
                kind: request.kind,
            });
        }
        for request in self.asker.current() {
            missed.push(ChatEvent::Ask {
                id: request.id,
                questions: request
                    .questions
                    .into_iter()
                    .map(|q| AskQuestion { question: q.question, choices: q.choices, multi: q.multi })
                    .collect(),
            });
        }
        missed
    }

    /// End the turn and everything that was carrying it.
    fn stop(&self) {
        self.task.abort();
        for helper in &self.helpers {
            helper.abort();
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.stop();
        while !self.task.is_finished() {
            tokio::task::yield_now().await;
        }
    }
}

#[derive(Clone)]
struct Connection {
    options: rook_proto::TurnOptions,
    approver: Arc<ChannelApprover>,
    asker: Arc<ChannelAsker>,
    settings: Arc<Settings>,
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
    let rook = engine.clone().read_owned().await;
    // Resolved by the caller, because the daemon has to register the turn
    // under its session before it starts one — a turn that names itself after
    // it is already running cannot be joined while it does so.
    let _ = outbound.send(ChatEvent::Started { session: rook_store::format_session_id(session) });

    match session_goal(&rook, session) {
        Ok(Some(run)) if run.status.runnable() => {
            drop(rook);
            goal_turn(&engine, &connection, &shared, &outbound, session, &run.id).await;
            return;
        }
        Err(error) => return ended_badly(&rook, session, &outbound, error),
        _ => {}
    }

    // The connection's choice where it has made one, and the configured
    // endpoint otherwise. Read here rather than carried in: a turn takes the
    // setting as it stands when the turn starts, which is what "the next turn"
    // means to somebody who has just switched.
    let named = connection.settings.model();
    let provider = match rook_core::models::chosen(&rook.config, named.as_deref()) {
        Ok(provider) => provider,
        Err(e) => return ended_badly(&rook, session, &outbound, e.to_string()),
    };

    let equipment = shared.clone();
    let shared = shared.get_or_init(|| Shared::for_project(&rook)).await;

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    agent.policy = connection.settings.policy.clone();
    agent.effort = connection.settings.effort();
    agent.approver = connection.approver.clone();
    agent.ask_via(connection.asker.clone());
    agent.options = connection.options.clone();
    rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp, shared.jobs.clone());

    let emit = outbound.clone();
    // Cloned out before the loop borrows the agent: a call's phrase names paths
    // the way somebody standing in this project would.
    let workspace = rook.workspace.clone();
    let result = agent
        .run_with(&prompt, |progress| {
            if let Some(event) = as_event(progress, &workspace) {
                let _ = emit.send(event);
            }
        })
        .await;

    // /goal can promote a turn already in flight. Keep its observer, approval
    // channels and settings instead of ending the stream between stages.
    if let Ok(Some(run)) = session_goal(&rook, session)
        && run.status.runnable()
    {
        drop(agent);
        drop(rook);
        goal_turn(&engine, &connection, &equipment, &outbound, session, &run.id).await;
        return;
    }
    match result {
        Ok(outcome) => ended_outcome(&outbound, outcome),
        Err(e) => ended_badly(&rook, session, &outbound, e.to_string()),
    }
}

fn ended_outcome(outbound: &mpsc::UnboundedSender<ChatEvent>, outcome: rook_core::agent::TurnOutcome) {
    for text in &outcome.facts_learned {
        let _ = outbound.send(ChatEvent::Remembered { text: text.clone() });
    }
    for text in &outcome.facts_forgotten {
        let _ = outbound.send(ChatEvent::Forgot { text: text.clone() });
    }
    let _ = outbound.send(ChatEvent::Done {
        reply: Some(outcome.reply.clone()),
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

async fn goal_turn(
    engine: &Arc<tokio::sync::RwLock<rook_core::Rook>>,
    connection: &Connection,
    equipment: &tokio::sync::OnceCell<Shared>,
    outbound: &mpsc::UnboundedSender<ChatEvent>,
    session: u128,
    id: &str,
) {
    let shared = {
        let rook = engine.read().await;
        equipment.get_or_init(|| Shared::for_project(&rook)).await
    };
    let _ = outbound.send(ChatEvent::Agent { receipt: None, text: "Goal started in this session; continuing automatically between stages. Ctrl-C pauses; /continue resumes.".into() });
    let mut announced_retry = None;
    loop {
        let rook = engine.read().await;
        let run = match managed::read(&rook, id) {
            Ok(saved) => saved.run,
            Err(error) => return ended_badly(&rook, session, outbound, error.to_string()),
        };
        if !run.status.runnable() {
            return ended_goal(&rook, outbound, session, run, connection, shared).await;
        }
        if let Some(at) = run.next_attempt_at.filter(|at| *at > managed::now()) {
            if announced_retry != Some(at) {
                let _ = outbound.send(ChatEvent::Agent {
                    receipt: None,
                    text: format!("Goal saved; retry in {}s: {}", at - managed::now().min(at), run.reason),
                });
                announced_retry = Some(at);
            }
            drop(rook);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        let result = managed::advance(
            &rook,
            id,
            |session| {
                let named = connection.settings.model();
                let provider = rook_core::models::chosen(&rook.config, named.as_deref())?;
                let mut agent = AgentLoop::new(&rook, provider.into(), session);
                agent.policy = connection.settings.policy.clone();
                agent.effort = connection.settings.effort();
                agent.approver = connection.approver.clone();
                agent.ask_via(connection.asker.clone());
                agent.options = connection.options.clone();
                if run.iterations > 0 {
                    agent.options.attachments.clear();
                }
                rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp, shared.jobs.clone());
                Ok(agent)
            },
            |progress| {
                if let Some(event) = as_event(progress, &rook.workspace) {
                    let _ = outbound.send(event);
                }
            },
        )
        .await;
        match result {
            Ok(run) if !run.status.runnable() => {
                return ended_goal(&rook, outbound, session, run, connection, shared).await;
            }
            Ok(run) if run.status == Status::Queued => {
                let _ = outbound.send(ChatEvent::Agent {
                    receipt: None,
                    text: format!(
                        "Continuing goal in this session (stage {}).",
                        run.iterations.saturating_add(1)
                    ),
                });
            }
            Err(error) => {
                let _ = managed::update(&rook, id, |s| {
                    if s.run.status.runnable() {
                        s.run.status = Status::Blocked;
                        s.run.reason = error.to_string().chars().take(2048).collect();
                    }
                    Ok(())
                });
                return ended_badly(&rook, session, outbound, error.to_string());
            }
            _ => {}
        }
        // Release the engine between stages: keeping its read guard for days
        // would prevent configuration reload and maintenance for the same days.
        drop(rook);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn ended_goal(
    rook: &rook_core::Rook,
    outbound: &mpsc::UnboundedSender<ChatEvent>,
    session: u128,
    run: Run,
    connection: &Connection,
    shared: &Shared,
) {
    let followups_ready = if run.status == Status::Completed {
        match rook_core::message_queue::followups::ready(rook, session) {
            Ok(ready) => ready,
            Err(error) => return ended_badly(rook, session, outbound, error.to_string()),
        }
    } else {
        false
    };
    if followups_ready {
        let named = connection.settings.model();
        let provider = match rook_core::models::chosen(&rook.config, named.as_deref()) {
            Ok(provider) => provider,
            Err(error) => return ended_badly(rook, session, outbound, error.to_string()),
        };
        let mut agent = AgentLoop::new(rook, provider.into(), session);
        agent.policy = connection.settings.policy.clone();
        agent.effort = connection.settings.effort();
        agent.approver = connection.approver.clone();
        agent.ask_via(connection.asker.clone());
        rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp, shared.jobs.clone());
        match agent
            .run_followups(|progress| {
                if let Some(event) = as_event(progress, &rook.workspace) {
                    let _ = outbound.send(event);
                }
            })
            .await
        {
            Ok(Some(outcome)) => {
                ended_outcome(outbound, outcome);
                return;
            }
            Ok(None) => {}
            Err(error) => return ended_badly(rook, session, outbound, error.to_string()),
        }
    }
    let last = rook.completed_turn(session).ok().flatten();
    let stopped = match run.status {
        Status::Completed => "end_turn",
        Status::Paused | Status::Cancelled => "work_paused",
        _ => "blocked",
    };
    let _ = outbound.send(ChatEvent::Done {
        reply: Some(if run.reply.is_empty() { run.reason.clone() } else { run.reply }),
        steps: last.as_ref().map_or(0, |o| o.steps),
        input_tokens: last.as_ref().map_or(0, |o| o.input_tokens),
        output_tokens: last.as_ref().map_or(0, |o| o.output_tokens),
        delegated: last.as_ref().map_or_else(Vec::new, |o| o.delegated.clone()),
        compactions: last.as_ref().map_or(0, |o| o.compactions),
        decisions: Vec::new(),
        open_questions: if stopped == "blocked" { vec![run.reason] } else { Vec::new() },
        files_changed: last.map_or_else(Vec::new, |o| o.files_changed),
        stopped: stopped.into(),
    });
}

/// What a window is told about one step of a turn.
///
/// `None` for the two deltas that only mark the end of a stream the window has
/// already been given.
fn as_event(progress: Progress<'_>, workspace: &std::path::Path) -> Option<ChatEvent> {
    Some(match progress {
        Progress::Delta(Delta::Effort(report)) => ChatEvent::ModelRequest {
            model: report.provider.clone(),
            requested_effort: report.requested.as_str().into(),
            effort: report.applied.describe(),
        },
        Progress::Delta(Delta::Text(text)) => ChatEvent::Text { text: text.clone() },
        Progress::Delta(Delta::Reasoning(text)) => ChatEvent::Reasoning { text: text.clone() },
        Progress::Delta(Delta::ToolCall(call)) => ChatEvent::Tool {
            name: call.name.clone(),
            doing: rook_core::calls::doing(&call.name, Some(&call.arguments), workspace),
        },
        // A sub-agent working is not the model thinking. Both of these were
        // `Reasoning` over the socket and `Agent` when the turn ran in the
        // window itself, so the same work read as two different things
        // depending on which side of a socket somebody was watching from.
        Progress::Delegated { task, done, total } => {
            ChatEvent::Agent { receipt: None, text: format!("  [{done}/{total}] {task}") }
        }
        // Counted from one, because the reader is a person and the first
        // sub-agent is the first, not the zeroth.
        Progress::Delegating { at, doing } => ChatEvent::Agent {
            receipt: None,
            text: format!("    {}", rook_core::calls::delegating(at, doing)),
        },
        Progress::FollowUp { id } => ChatEvent::FollowUp { id: id.into() },
        Progress::Working { call, said } => {
            ChatEvent::ToolWorking { name: call.to_string(), said: said.to_string() }
        }
        // What the person said while it ran, at the moment it is taken up.
        Progress::Heard { text, receipt } => {
            ChatEvent::Agent { receipt: receipt.cloned(), text: format!("  ✓ taken up: {text}") }
        }
        Progress::ToolDone { name, failed } => ChatEvent::ToolDone { name: name.to_string(), failed },
        Progress::Step { at, of } => ChatEvent::Step { at, of },
        // A model that has been asked and has not begun to answer, said the
        // same way a tool that is taking a while is: a line that is replaced
        // rather than added to, because it is one fact changing.
        Progress::Waiting { secs, patience } => ChatEvent::ToolWorking {
            name: "model".to_string(),
            said: rook_core::calls::waiting(secs, patience),
        },
        Progress::Context { used, size } => ChatEvent::Context { used, size },
        Progress::Spent { input, output, cached } => {
            ChatEvent::Spent { input_tokens: input, output_tokens: output, cached_tokens: cached }
        }
        Progress::Delta(Delta::Done { .. } | Delta::ReasoningDone(_)) => return None,
    })
}

/// A turn ending badly, said to whoever is watching and written into the
/// session either way.
///
/// It used to be only said. The reason went to the window as one message and
/// nowhere else, so a window that had closed — or a message lost on the way,
/// which happened — left a session that simply stopped: two hundred and
/// eighteen events, no ending, and nothing anywhere to say why. The store
/// outlives the window, the connection and the daemon, and `session show` is
/// where someone looks afterwards.
fn ended_badly(
    rook: &rook_core::Rook,
    session: u128,
    outbound: &mpsc::UnboundedSender<ChatEvent>,
    message: String,
) {
    if let Err(e) = rook.log(session, rook_store::EventKind::Note, "failed", &message) {
        tracing::warn!("could not record why the turn ended: {e}");
    }
    report(outbound, message);
}

async fn report_window(outbound: &delivery::Sender, message: String) {
    let _ = outbound.send(ChatEvent::Failed { message }).await;
}

fn report(outbound: &mpsc::UnboundedSender<ChatEvent>, message: String) {
    let _ = outbound.send(ChatEvent::Failed { message });
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
    pub(crate) servers: Arc<rook_core::lsp::Servers>,
    pub(crate) mcp: Arc<rook_core::McpSession>,
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
    /// The endpoint this connection's next turn runs on, where it is not the
    /// configured one.
    ///
    /// The next one, not the one in flight: moving a turn between endpoints
    /// part-way throws away the cached prefix of everything it has sent and
    /// hands its next step to a model that did not write the last one. So this
    /// is the one setting here that a running turn does not take up.
    model: std::sync::RwLock<Option<String>>,
    /// The one this connection was opened against, for checking a name against
    /// `[models]` without reaching back through the registry for a lock that
    /// this is holding one side of.
    config: rook_core::Config,
}

impl Settings {
    fn new(rook: &rook_core::Rook) -> Self {
        Self {
            policy: rook_core::agent::policy_for(&rook.config),
            effort: std::sync::RwLock::new(rook.config.agent.effort()),
            model: std::sync::RwLock::new(None),
            config: rook.config.clone(),
        }
    }

    /// A policy and an effort that no config was read for, so the registry's
    /// own bookkeeping can be tested without a project on disk. Reached only
    /// through `Live::for_test`, which is the seam.
    #[cfg(test)]
    fn for_test() -> Self {
        let (policy, _) =
            rook_tools::policy::Policy::compile(rook_tools::policy::Stance::ALL[0], &[], &[], &[]);
        Self {
            policy: Arc::new(policy),
            effort: std::sync::RwLock::new(rook_llm::Effort::ALL[0]),
            model: std::sync::RwLock::new(None),
            config: rook_core::Config::default(),
        }
    }

    fn effort(&self) -> rook_llm::Effort {
        *self.effort.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The endpoint chosen here, where one has been.
    fn model(&self) -> Option<String> {
        self.model.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn describe(&self) -> ChatEvent {
        ChatEvent::Settings {
            mode: self.policy.stance().as_str().into(),
            effort: self.effort().as_str().into(),
            stances: rook_tools::policy::Stance::ALL.iter().map(|s| s.as_str().to_string()).collect(),
            efforts: rook_llm::Effort::ALL.iter().map(|e| e.as_str().to_string()).collect(),
            // What it is running on and what else it could: a page that drew
            // its own list would be drawing its memory of the file rather than
            // the file, and the file is what the engine reads.
            model: self.model().unwrap_or_else(|| self.config.agent.model.clone()),
            models: self.config.models.keys().cloned().collect(),
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
            // Checked through the function that will build it, so a name with a
            // typo in it says so while somebody is still looking at what they
            // typed rather than at the top of the next turn.
            "model" => rook_core::models::usable(&self.config, value).map(|()| {
                *self.model.write().unwrap_or_else(|e| e.into_inner()) = Some(value.to_string());
            }),
            other => Err(format!("no setting {other:?}")),
        }
    }
}

/// Relays the agent's questions to the browser and routes the answers back.
pub(crate) fn asker(
    outbound: mpsc::UnboundedSender<ChatEvent>,
    patience: std::time::Duration,
    limits: rook_tools::pending::Limits,
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
    (Arc::new(ChannelAsker::new(requests, patience, limits)), relay)
}

/// Relays approval requests to the browser and routes the answers back.
pub(crate) fn approver(
    outbound: mpsc::UnboundedSender<ChatEvent>,
    patience: std::time::Duration,
    limits: rook_tools::pending::Limits,
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
    (Arc::new(ChannelApprover::new(requests, patience, limits)), relay)
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    /// The page draws its selects from these lists, so a stance the engine
    /// grows appears without the page learning its name — and one it loses
    /// disappears rather than sitting in a menu as a choice that errors.
    #[test]
    fn the_settings_event_carries_the_engines_own_lists() {
        let mut config = rook_core::Config::default();
        config.models.insert("next-door".into(), rook_core::ModelSource::default());
        let settings = Settings {
            policy: rook_core::agent::policy_for(&config),
            effort: std::sync::RwLock::new(config.agent.effort()),
            model: std::sync::RwLock::new(None),
            config: config.clone(),
        };
        let ChatEvent::Settings { mode, effort, stances, efforts, model, models } = settings.describe()
        else {
            panic!("describe() is the settings event");
        };
        let expected: Vec<String> =
            rook_tools::policy::Stance::ALL.iter().map(|s| s.as_str().to_string()).collect();
        assert_eq!(stances, expected);
        assert!(stances.contains(&mode), "the current stance is one of the offered: {mode} in {stances:?}");
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert!(efforts.contains(&effort), "{effort} in {efforts:?}");
        // The endpoints are the same kind of list and are read the same way: a
        // window draws what the engine says it has rather than its own memory
        // of the file.
        assert_eq!(models, ["next-door"]);
        assert_eq!(model, config.agent.model, "nothing switched yet, so it is the configured one");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live turn with nothing actually running in it, to ask the questions a
    /// window joining one asks.
    fn parked(said: tokio::sync::broadcast::Sender<u64>) -> Live {
        let (to_turn, _held) = mpsc::unbounded_channel::<ChatEvent>();
        let (approver, relay) =
            approver(to_turn.clone(), std::time::Duration::from_secs(1), Default::default());
        let (asker, ask_relay) = asker(to_turn, std::time::Duration::from_secs(1), Default::default());
        Live {
            task: tokio::spawn(std::future::pending()),
            helpers: vec![relay.abort_handle(), ask_relay.abort_handle()],
            said,
            backlog: Default::default(),
            approver,
            asker,
            settings: Arc::new(Settings::for_test()),
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
        let (said, _) = tokio::sync::broadcast::channel::<u64>(16);
        let live = parked(said.clone());

        // Said before anybody was watching.
        for i in 0..3 {
            let event = text(&format!("before {i}"));
            let mut kept = live.backlog.lock().unwrap();
            kept.push(event);
        }

        let (mut coming, missed, _) = live.join();
        assert_eq!(missed.len(), 3, "everything said before the window arrived");
        assert!(matches!(&missed[0], ChatEvent::Text { text } if text == "before 0"), "oldest first");

        fan_out(&live.backlog, &said, text("after"));
        let sequence = coming.recv().await.expect("and then what happens next");
        let next = live.backlog.lock().unwrap().get(sequence).unwrap();
        assert!(matches!(&next, ChatEvent::Text { text } if text == "after"));

        assert!(live.running(), "a turn nobody is watching is still running");
        live.stop();
        // The task is aborted, which the runtime completes at the next yield.
        tokio::task::yield_now().await;
        assert!(!live.running(), "and `Cancel` is the thing that ends it");
    }

    #[tokio::test]
    async fn joining_replays_current_questions_even_if_their_old_events_are_gone() {
        use rook_tools::ask::{Asker, Question};
        let (said, _) = tokio::sync::broadcast::channel(16);
        let live = parked(said);
        let questions = [Question { question: "Which branch?".into(), choices: vec![], multi: false }];
        let mut asking = Box::pin(live.asker.ask(&questions));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(asking.as_mut().poll(cx).is_pending())).await
        );
        live.backlog.lock().unwrap().push(ChatEvent::Ask { id: "obsolete".into(), questions: vec![] });
        let (_, replay, _) = live.join();
        assert_eq!(replay.len(), 1, "an obsolete request must not be replayed");
        let ChatEvent::Ask { id, questions } = &replay[0] else { panic!("current question missing") };
        assert_eq!(questions[0].question, "Which branch?");
        assert_ne!(id, "obsolete");
        live.asker.answer(id, vec![vec!["main".into()]]);
        assert_eq!(asking.await[0].chosen, ["main"]);
        assert!(live.join().1.is_empty(), "answered questions are no longer controls");
        live.stop();
    }

    #[tokio::test]
    async fn concurrent_publication_and_join_deliver_every_event_exactly_once() {
        for _ in 0..20 {
            let (said, _) = tokio::sync::broadcast::channel(1024);
            let live = Arc::new(parked(said));
            let publishing = live.clone();
            let publisher = std::thread::spawn(move || {
                for i in 0..400 {
                    fan_out(&publishing.backlog, &publishing.said, text(&i.to_string()));
                }
            });
            let (mut coming, mut replay, _) = live.join();
            publisher.join().unwrap();
            while let Ok(sequence) = coming.try_recv() {
                replay.push(live.backlog.lock().unwrap().get(sequence).unwrap());
            }
            let values: Vec<_> = replay
                .into_iter()
                .map(|event| {
                    let ChatEvent::Text { text } = event else { panic!("expected text") };
                    text.parse::<usize>().unwrap()
                })
                .collect();
            assert_eq!(values, (0..400).collect::<Vec<_>>(), "no duplicate or gap at the join boundary");
            live.stop();
        }
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
        let (outbound, queued) = delivery::channel(8, 4096);
        for text in ["one", "two", "three"] {
            outbound.send(ChatEvent::Text { text: text.into() }).await.unwrap();
        }

        let writer = tokio::spawn(write_frames(Stalls { took: took.clone() }, queued));
        // Held open, as a stalled client holds it: the writer has to end on the
        // deadline rather than because the queue closed.
        let done = tokio::time::timeout(SEND_DEADLINE * 3, writer).await;

        assert!(done.is_ok(), "the writer let go of the socket");
        assert_eq!(took.load(std::sync::atomic::Ordering::SeqCst), 1, "after the one frame it took");
        drop(outbound);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_but_live_socket_backpressures_its_relay_and_receives_the_ending() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let sent = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = futures_util::sink::unfold(received.clone(), |received, message| async move {
            // Every send beats SEND_DEADLINE, so that timeout cannot bound
            // the backlog of a producer that is faster than this consumer.
            tokio::time::sleep(SEND_DEADLINE / 3).await;
            received.lock().unwrap().push(message);
            Ok::<_, std::convert::Infallible>(received)
        });
        let (outbound, queued) = delivery::channel(3, 4096);
        let writer = tokio::spawn(write_frames(Box::pin(sink), queued));
        let produced = sent.clone();
        let producer = tokio::spawn(async move {
            for _ in 0..100 {
                outbound.send(text(&"x".repeat(1000))).await.unwrap();
                produced.fetch_add(1, Ordering::SeqCst);
            }
            outbound.send(ChatEvent::Cancelled).await.unwrap();
        });
        // Yield without advancing paused time: the writer owns one frame,
        // two can wait, and the relay cannot enqueue the other 97.
        while sent.load(Ordering::SeqCst) < 3 {
            tokio::task::yield_now().await;
        }
        tokio::task::yield_now().await;
        assert_eq!(sent.load(Ordering::SeqCst), 3, "the queue limit was actually reached");
        assert!(!producer.is_finished());
        producer.await.unwrap();
        writer.await.unwrap();
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 101);
        assert!(matches!(received.last(), Some(Message::Text(text)) if text.contains("cancelled")));
    }

    #[tokio::test]
    async fn answering_a_question_clears_it_in_every_view_without_new_turn_output() {
        use rook_tools::ask::{Asker, Question};
        let (said, _) = tokio::sync::broadcast::channel(8);
        let live = Arc::new(parked(said));
        let questions = [Question { question: "Choose a branch".into(), choices: vec![], multi: false }];
        let mut asking = Box::pin(live.asker.ask(&questions));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(asking.as_mut().poll(cx).is_pending())).await
        );
        let id = live.asker.current()[0].id.clone();
        let (one, mut first) = delivery::channel(8, 4096);
        let (two, mut second) = delivery::channel(8, 4096);
        let first_watch = watch(&live, 7, one, None, true);
        let second_watch = watch(&live, 7, two, None, true);
        for frames in [&mut first, &mut second] {
            loop {
                let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let event: ChatEvent = serde_json::from_str(&frame.text).unwrap();
                if matches!(event, ChatEvent::Ask { id: seen, .. } if seen == id) {
                    break;
                }
            }
        }
        live.asker.answer(&id, vec![vec!["main".into()]]);
        assert_eq!(asking.await[0].chosen, ["main"]);
        assert!(
            live.backlog.lock().unwrap().snapshot().0.is_empty(),
            "no progress event triggers this update"
        );
        for frames in [&mut first, &mut second] {
            loop {
                let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let event: ChatEvent = serde_json::from_str(&frame.text).unwrap();
                if let ChatEvent::Inputs { approvals, questions } = event {
                    assert!(approvals.is_empty() && questions.is_empty());
                    break;
                }
            }
        }
        first_watch.carrying.abort();
        second_watch.carrying.abort();
        live.stop();
    }

    #[tokio::test]
    async fn shortened_live_endings_report_partial_history_without_breaking_legacy_clients() {
        let old: Where = serde_json::from_str("{}").unwrap();
        assert!(!old.live_snapshots, "old clients do not opt into new event variants");
        for snapshots in [false, true] {
            let (said, _) = tokio::sync::broadcast::channel(8);
            let mut live = parked(said);
            live.backlog = Arc::new(std::sync::Mutex::new(replay::Replay::new(8, 8192, 4096)));
            let live = Arc::new(live);
            let (out, mut frames) = delivery::channel(8, 4096);
            let watching = watch(&live, 7, out, None, snapshots);
            // Wait for initial replay before publishing: the test is about
            // an attached live subscriber, not only a later reconnect.
            loop {
                let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if matches!(
                    serde_json::from_str::<ChatEvent>(&frame.text).unwrap(),
                    ChatEvent::Settings { .. }
                ) {
                    break;
                }
            }
            fan_out(&live.backlog, &live.said, ChatEvent::Failed { message: "x".repeat(5000) });
            let mut marked = false;
            loop {
                let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames.recv())
                    .await
                    .unwrap()
                    .unwrap();
                let event: ChatEvent = serde_json::from_str(&frame.text).unwrap();
                assert!(snapshots || !matches!(event, ChatEvent::Snapshot { .. } | ChatEvent::Inputs { .. }));
                match event {
                    ChatEvent::Snapshot { truncated: true, .. } => marked = true,
                    ChatEvent::Text { text } if text.contains("live view refreshed") => marked = true,
                    ChatEvent::Failed { message } => {
                        assert!(marked, "shortened results must be marked before the client exits");
                        assert!(message.contains("error shortened"));
                        break;
                    }
                    _ => {}
                }
            }
            watching.carrying.abort();
            live.stop();
        }
    }

    #[tokio::test]
    async fn a_lagging_view_converges_to_a_snapshot_and_the_terminal_outcome() {
        let (said, _) = tokio::sync::broadcast::channel(2);
        let mut live = parked(said);
        live.backlog = Arc::new(std::sync::Mutex::new(replay::Replay::new(3, 4096, 4096)));
        let live = Arc::new(live);
        let (out, mut frames) = delivery::channel(1, 4096);
        let watching = carry_view(&live, 7, out, None, true, false);
        let first = frames.recv().await.unwrap();
        let event: ChatEvent = serde_json::from_str(&first.text).unwrap();
        assert!(matches!(event, ChatEvent::Settings { .. }), "a fresh turn does not erase the conversation");
        // The sole queue slot remains in flight while more events are
        // produced than either the replay or notification ring can retain.
        for n in 0..20 {
            fan_out(&live.backlog, &live.said, text(&format!("part {n}")));
        }
        fan_out(&live.backlog, &live.said, ChatEvent::Cancelled);
        let (kept, truncated) = live.backlog.lock().unwrap().snapshot();
        assert!(truncated);
        assert_eq!(kept.len(), 3, "the retained event limit was actually reached");
        drop(first);
        let mut recovered = false;
        let mut finished = false;
        for _ in 0..10 {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames.recv())
                .await
                .unwrap()
                .unwrap();
            let event: ChatEvent = serde_json::from_str(&frame.text).unwrap();
            if matches!(event, ChatEvent::Snapshot { truncated: true, .. }) {
                recovered = true;
            }
            if matches!(event, ChatEvent::Cancelled) {
                finished = true;
                break;
            }
        }
        assert!(recovered, "a skipped delta must trigger snapshot replacement");
        assert!(finished, "the recovered view must receive the terminal result");
        assert!(live.running(), "resynchronizing a view must not cancel daemon-owned work");
        watching.carrying.abort();
        live.stop();
    }

    /// A turn's last word reaches the window that was watching it all along.
    ///
    /// The turn puts its ending in the queue as it returns, and nothing else
    /// ever will. This turn has no `.await` in it, so it finishes on its first
    /// poll — which means a fan-out living in its own task has not been polled
    /// once by the time the turn is over, and aborting it there loses
    /// everything the turn said. That shipped. The window it left behind drew
    /// `working…` over a turn that had been finished for minutes, while the
    /// daemon beside it answered `turns_running: 0`.
    #[tokio::test]
    async fn a_turns_last_word_reaches_the_window_that_was_watching_all_along() {
        let (from_turn, events) = mpsc::unbounded_channel::<ChatEvent>();
        let (said, mut watching) = tokio::sync::broadcast::channel::<u64>(8);
        let backlog: Arc<std::sync::Mutex<replay::Replay>> = Default::default();

        let turn = {
            let out = from_turn.clone();
            async move {
                let _ = out.send(ChatEvent::Text { text: "the work".into() });
                let _ = out.send(ChatEvent::Cancelled);
            }
        };
        drop(from_turn);

        everything_it_says(turn, events, said, backlog.clone(), vec![]).await;

        let watched: Vec<ChatEvent> = std::iter::from_fn(|| watching.try_recv().ok())
            .map(|sequence| backlog.lock().unwrap().get(sequence).unwrap())
            .collect();
        assert_eq!(watched.len(), 2, "the window watching saw {watched:?}");
        assert!(matches!(watched.last(), Some(ChatEvent::Cancelled)), "it ended on {watched:?}");

        let kept: Vec<ChatEvent> = backlog.lock().unwrap().snapshot().0;
        assert!(
            matches!(kept.last(), Some(ChatEvent::Cancelled)),
            "a window attaching afterwards reads {kept:?} and would wait for an ending that had passed"
        );
    }

    /// A turn that ends badly says why in the session, not only to the window.
    ///
    /// The reason used to be one message to whoever was watching, and a message
    /// can be lost — one was. What it left behind was a session of two hundred
    /// and eighteen events that simply stopped: no ending, no error, and
    /// nothing in `session show` to say the model host had gone quiet. A
    /// failure nobody can read is the same as no failure at all.
    #[tokio::test]
    async fn a_turn_that_ends_badly_says_why_in_the_session_and_not_only_to_the_window() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = rook_store::Store::open(home.path().join("store")).unwrap();
        let mut config = rook_core::Config::default();
        // No provider answers to this, so the turn cannot start and says so
        // without a network in the test.
        config.agent.model = "no-such-provider/no-such-model".into();
        let rook = rook_core::Rook::from_parts(
            store,
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::discover(&[]).0,
            workspace.path().to_path_buf(),
        );
        let session = rook.start_session("a turn that cannot start").unwrap();

        let (outbound, mut heard) = mpsc::unbounded_channel::<ChatEvent>();
        let (approver, _relay) =
            approver(outbound.clone(), std::time::Duration::from_secs(1), Default::default());
        let (asker, _ask_relay) =
            asker(outbound.clone(), std::time::Duration::from_secs(1), Default::default());
        let engine = Arc::new(tokio::sync::RwLock::new(rook));
        turn(
            engine.clone(),
            Connection {
                options: Default::default(),
                approver,
                asker,
                settings: Arc::new(Settings::for_test()),
            },
            Default::default(),
            outbound,
            session,
            "find the leak".into(),
        )
        .await;

        let said: Vec<ChatEvent> = std::iter::from_fn(|| heard.try_recv().ok()).collect();
        let told = said.iter().find_map(|e| match e {
            ChatEvent::Failed { message } => Some(message.clone()),
            _ => None,
        });
        let told = told.unwrap_or_else(|| panic!("the window was told nothing: {said:?}"));

        let events = engine.read().await.store.events(session, 0, 100).unwrap();
        let written = events.iter().find(|e| e.record.label == "failed");
        let written = written.unwrap_or_else(|| {
            let kinds: Vec<_> = events.iter().map(|e| (e.seq, e.record.label.clone())).collect();
            panic!("the session stops without saying why: {kinds:?}")
        });
        assert_eq!(
            engine.read().await.store.get(&written.record.body).unwrap(),
            told.as_bytes(),
            "what the window was told and what the session records are the same reason"
        );
    }

    #[test]
    fn accepted_request_effort_is_separate_from_the_configured_setting() {
        let report = rook_llm::EffortReport {
            provider: "fallback-model".into(),
            requested: rook_llm::Effort::Max,
            applied: rook_llm::EffortUse::Parameter { name: "reasoning_effort", value: "high".into() },
        };
        let delta = rook_llm::Delta::Effort(report);
        let event = as_event(Progress::Delta(&delta), std::path::Path::new("."));
        let value = serde_json::to_value(event.unwrap()).unwrap();
        assert_eq!(value["type"], "model_request");
        assert_eq!(value["model"], "fallback-model");
        assert_eq!(value["requested_effort"], "max");
        assert_eq!(value["effort"], "sent reasoning_effort=high");
        assert!(serde_json::from_value::<ChatEvent>(value).is_ok());
    }

    #[test]
    fn acceptance_metadata_survives_the_wire_without_turning_text_into_authority() {
        let receipt = rook_proto::queue::Notice {
            session: "session".into(),
            reference: "goal.generation.message".into(),
            revision: 7,
            status: rook_proto::queue::Status::Accepted,
        };
        let event = as_event(
            Progress::Heard { text: "edited text", receipt: Some(&receipt) },
            std::path::Path::new("."),
        );
        let json = serde_json::to_value(event.unwrap()).unwrap();
        assert_eq!(json["type"], "agent", "legacy clients still understand the event kind");
        let ChatEvent::Agent { text, receipt: Some(actual) } = serde_json::from_value(json).unwrap() else {
            panic!("missing identity")
        };
        assert_eq!(actual, receipt);
        assert!(text.contains("edited text"));
        let legacy: ChatEvent =
            serde_json::from_value(serde_json::json!({"type":"agent","text":"[work instruction message]"}))
                .unwrap();
        assert!(matches!(legacy, ChatEvent::Agent { receipt: None, .. }));
    }

    /// A sub-agent working is told apart from the model thinking.
    ///
    /// A turn run in the window itself said `Agent` and a turn run through the
    /// daemon said `Reasoning`, so the same work was styled as the model's own
    /// thoughts for anybody watching over a socket — which is everybody now
    /// that a turn belongs to the daemon. One engine, and the front ends were
    /// being told two different stories about it.
    #[tokio::test]
    async fn a_sub_agent_working_is_not_reported_as_the_model_thinking() {
        let here = std::path::Path::new("/tmp");
        let working = as_event(rook_core::agent::Progress::Delegating { at: 0, doing: "run pwd" }, here);
        let Some(ChatEvent::Agent { text, .. }) = working else {
            panic!("a sub-agent's step came back as {working:?}");
        };
        assert!(text.contains("run pwd"), "and it says what the sub-agent is doing: {text:?}");
        assert!(text.contains('1'), "counted from one, for a person: {text:?}");

        let counted =
            as_event(rook_core::agent::Progress::Delegated { task: "audit", done: 2, total: 3 }, here);
        assert!(
            matches!(counted, Some(ChatEvent::Agent { receipt: None, .. })),
            "and so is the count of them: {counted:?}"
        );

        // The model's own thinking stays what it is.
        let thought = rook_llm::Delta::Reasoning("let me see".into());
        let thinking = as_event(rook_core::agent::Progress::Delta(&thought), here);
        assert!(matches!(thinking, Some(ChatEvent::Reasoning { .. })), "{thinking:?}");
    }
}
