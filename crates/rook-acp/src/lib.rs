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
//!
//! When the client says it can serve them, file reads and writes go to the
//! editor rather than the disk, so the agent sees the buffer the user is
//! looking at instead of the version last saved. Commands run in the editor's
//! terminal when it has one, so a build is watched rather than reported. The
//! approval modes are offered as session modes, so an editor's menu and
//! `sandbox.stance` are the same knob.

mod preview;
pub mod protocol;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

use rook_core::Rook;
use rook_core::agent::{AgentLoop, Progress};
use rook_llm::Delta;
use rook_tools::ask::{Answer, Question};
use rook_tools::policy::{Approval, Approver, Risk};

use protocol::{Error, Incoming, PROTOCOL_VERSION};

async fn next_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(frame)
                    .map(Some)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            };
        }
        let newline = bytes.iter().position(|b| *b == b'\n');
        let take = newline.map_or(bytes.len(), |at| at + 1);
        if take > rook_core::attachments::MAX_FRAME_BYTES.saturating_sub(frame.len()) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "ACP frame exceeds 16 MiB"));
        }
        frame.extend_from_slice(&bytes[..take]);
        reader.consume(take);
        if newline.is_some() {
            return String::from_utf8(frame)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
    }
}

/// Run the server on stdin/stdout until the client closes the connection.
pub async fn serve_stdio(rook: Rook) -> std::io::Result<()> {
    serve(rook, BufReader::new(tokio::io::stdin()), tokio::io::stdout()).await
}

/// The server over any pair of streams, so it can be driven by a test as well as
/// by an editor.
pub async fn serve<R, W>(rook: Rook, mut reader: R, mut sink: W) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let rook = Arc::new(rook);
    let (outbound, mut queued) = rook_core::delivery::channel(
        rook.config.server.chat_queue_events,
        rook.config.server.chat_queue_bytes,
    );
    let closed = Arc::new(tokio::sync::Notify::new());
    let writer_closed = closed.clone();

    let mut writer = tokio::spawn(async move {
        while let Some(frame) = queued.recv().await {
            if sink.write_all(frame.text.as_bytes()).await.is_err()
                || sink.write_all(b"\n").await.is_err()
                || sink.flush().await.is_err()
            {
                break;
            }
        }
        writer_closed.notify_one();
    });

    // Built once for the connection: an editor sends many prompts, and
    // reconnecting MCP or restarting a language server for each is wasted time.
    let policy = rook_core::agent::policy_for(&rook.config);
    let settings =
        Arc::new(Settings { policy: policy.clone(), effort: RwLock::new(rook.config.agent.effort()) });
    let servers = rook_core::agent::servers_for(&rook.config, &rook.workspace);
    let mcp = Arc::new(rook.connect_mcp().await);
    let jobs = rook_core::agent::jobs_for(&rook.config);
    // Filled in by `initialize`, and read when a turn starts: the client may
    // serve its unsaved buffers, and the protocol forbids asking if it cannot.
    let client_files: Arc<Mutex<ClientFiles>> = Default::default();

    let peer = Arc::new(Peer::new(outbound, closed));
    let mut turn: Option<Turn> = None;

    let read_result = loop {
        let next = tokio::select! {
            line = next_frame(&mut reader) => line,
            _ = peer.closed.notified() => Err(std::io::Error::other("ACP delivery closed or exceeded its configured queue budget")),
        };
        let line = match next {
            Ok(Some(line)) => line,
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        };
        let Ok(message) = serde_json::from_str::<Incoming>(&line) else {
            tracing::debug!("unparsable ACP frame");
            continue;
        };

        // A reply to something we asked, rather than a request of its own.
        if message.method.is_none() {
            if let Some(id) = message.id.as_ref().and_then(|i| i.as_u64()) {
                let result = match (message.result, message.error) {
                    (Some(result), None) => Ok(result),
                    (_, Some(error)) => {
                        Err(serde_json::from_value(error).unwrap_or_else(|e| Error::internal(e.to_string())))
                    }
                    _ => Err(Error::internal("response has neither result nor error")),
                };
                peer.resolve(id, result).await;
            }
            continue;
        }

        let method = message.method.unwrap_or_default();
        let params = message.params.unwrap_or(serde_json::Value::Null);

        match (method.as_str(), message.id) {
            ("session/cancel", request_id) => {
                let target = params["sessionId"].as_str();
                let restricted = match target.map(|session| preview::restricted(&rook, session)).transpose() {
                    Ok(restricted) => restricted.unwrap_or(false),
                    Err(error) => {
                        if let Some(id) = request_id {
                            peer.respond(&id, Err(error));
                        }
                        continue;
                    }
                };
                if restricted {
                    if let Some(id) = request_id {
                        peer.respond(
                            &id,
                            Err(Error::invalid_params(
                                "child cancellation is not advertised by this runtime",
                            )),
                        );
                    }
                    continue;
                }
                if turn.as_ref().is_some_and(|turn| target.is_none_or(|target| target == turn.session))
                    && let Some(turn) = turn.take()
                {
                    turn.cancel(&peer);
                }
            }
            ("session/prompt", Some(id)) => {
                let request = match serde_json::from_value::<protocol::Prompt>(params) {
                    Ok(request) => request,
                    _ => {
                        peer.respond(&id, Err(Error::invalid_params("a prompt requires a valid sessionId")));
                        continue;
                    }
                };
                let Some(session) = rook_store::parse_session_id(&request.session_id) else {
                    peer.respond(&id, Err(Error::invalid_params("not a session id")));
                    continue;
                };
                let meta = match rook.store.get_session(session) {
                    Ok(Some(meta)) => meta,
                    Ok(None) => {
                        peer.respond(&id, Err(Error::invalid_params("no such session")));
                        continue;
                    }
                    Err(error) => {
                        peer.respond(&id, Err(Error::internal(error.to_string())));
                        continue;
                    }
                };
                if meta.tags.iter().any(|tag| tag == "subtask") {
                    peer.respond(
                        &id,
                        Err(Error::invalid_params(
                            "agent-created child sessions cannot accept client prompts",
                        )),
                    );
                    continue;
                }
                let session_id = request.session_id.clone();
                let (text, attachments) = match request.content() {
                    Ok(content) => content,
                    Err(error) => {
                        peer.respond(&id, Err(error));
                        continue;
                    }
                };
                if let Some(turn) = turn.take() {
                    turn.cancel(&peer);
                }
                let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
                // Read out of the lock before the spawn: a guard living to the
                // end of the statement would travel into the task with it.
                let effort = *settings.effort.read().unwrap_or_else(|e| e.into_inner());
                let client = *client_files.lock().await;
                let display = Arc::new(preview::Preview::new(
                    rook.clone(),
                    peer.clone(),
                    session,
                    client.subagents,
                    client.compaction,
                ));
                turn = Some(Turn {
                    session: session_id.clone(),
                    id: id.clone(),
                    replied: replied.clone(),
                    display: display.clone(),
                    handle: tokio::spawn(prompt(
                        rook.clone(),
                        peer.clone(),
                        Shared {
                            policy: policy.clone(),
                            servers: servers.clone(),
                            mcp: mcp.clone(),
                            jobs: jobs.clone(),
                        },
                        TurnSetup { client, effort, display },
                        id,
                        PreparedPrompt { session, session_id, text, attachments },
                        replied,
                    )),
                });
            }
            (_, Some(id)) => {
                let mut recovery = None;
                let mut current = None;
                if method == "initialize" {
                    *client_files.lock().await = ClientFiles::from_initialize(&params);
                }
                let restricted = if method.starts_with("session/") {
                    match params["sessionId"]
                        .as_str()
                        .map(|session| preview::restricted(&rook, session))
                        .transpose()
                    {
                        Ok(restricted) => restricted.unwrap_or(false),
                        Err(error) => {
                            peer.respond(&id, Err(error));
                            continue;
                        }
                    }
                } else {
                    false
                };
                if restricted {
                    peer.respond(
                        &id,
                        Err(Error::invalid_params(
                            "client operations on agent-created child sessions are not advertised",
                        )),
                    );
                    continue;
                }
                if matches!(method.as_str(), "session/load" | "session/resume")
                    && let Some(session) = params["sessionId"].as_str().and_then(rook_store::parse_session_id)
                    && rook.store.get_session(session).ok().flatten().is_some()
                {
                    let client = *client_files.lock().await;
                    // Replay before the response is historical. Confirm only
                    // states this connection actually observed after load succeeds.
                    current = turn
                        .as_ref()
                        .filter(|turn| client.subagents && turn.session == params["sessionId"])
                        .map(|turn| turn.display.clone());
                    if client.subagents || client.compaction {
                        let recovering = preview::Preview::new(
                            rook.clone(),
                            peer.clone(),
                            session,
                            client.subagents,
                            client.compaction,
                        );
                        match recovering.recover() {
                            Ok(truncated) => recovery = Some(truncated),
                            Err(error) => {
                                peer.respond(&id, Err(Error::internal(error.to_string())));
                                continue;
                            }
                        }
                    }
                }
                let mut outcome = dispatch(&rook, &settings, &method, params);
                if let (Ok(result), Some(truncated)) = (&mut outcome, recovery) {
                    result["_meta"]["rook"]["recovery"] = serde_json::json!({
                        "truncated":truncated,"maxSessions":64,"maxCompactions":32,"eventsPerSession":128,
                        "childHistoryReplayed":false,
                    });
                }
                let succeeded = outcome.is_ok();
                peer.respond(&id, outcome);
                if succeeded && let Some(current) = current {
                    current.snapshot();
                }
            }
            // A notification we do not handle is not an error.
            (_, None) => tracing::debug!("ignoring notification {method}"),
        }
    };

    if let Some(turn) = turn {
        turn.handle.abort();
        let _ = turn.handle.await;
    }
    mcp.shutdown().await;
    servers.shutdown().await;
    drop(peer);
    if read_result.is_err() {
        writer.abort();
    }
    // Once stdin closes there can be no further editor replies. Give already
    // admitted output a short drain grace, then release a blocked stdout writer.
    if tokio::time::timeout(std::time::Duration::from_secs(5), &mut writer).await.is_err() {
        writer.abort();
        let _ = writer.await;
        return Err(std::io::Error::other("ACP client closed input without draining output"));
    }
    read_result
}

/// Settings a client may change for the rest of the connection.
///
/// The policy already holds the mode, because a turn reads it there; effort has
/// nowhere else to live.
struct Settings {
    policy: Arc<rook_tools::policy::Policy>,
    effort: RwLock<rook_llm::Effort>,
}

impl Settings {
    fn describe(&self) -> serde_json::Value {
        protocol::config_options(self.policy.stance(), *self.effort.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn set(&self, id: &str, value: &str) -> Result<(), Error> {
        match id {
            "mode" => {
                let mode = protocol::mode_from_id(value)
                    .ok_or_else(|| Error::invalid_params(format!("no mode {value:?}")))?;
                self.policy.set_stance(mode);
            }
            "effort" => {
                let effort = rook_llm::Effort::parse(value)
                    .ok_or_else(|| Error::invalid_params(format!("no effort {value:?}")))?;
                *self.effort.write().unwrap_or_else(|e| e.into_inner()) = effort;
            }
            other => return Err(Error::invalid_params(format!("no setting {other:?}"))),
        }
        Ok(())
    }
}

fn dispatch(
    rook: &Rook,
    settings: &Settings,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, Error> {
    match method {
        "session/set_stance" => {
            settings.set("mode", params["modeId"].as_str().unwrap_or_default())?;
            Ok(serde_json::json!({}))
        }

        "session/set_config_option" => {
            let id = params["configId"].as_str().unwrap_or_default();
            // A boolean option would arrive as one; none of ours are, and a
            // value of the wrong shape should say so rather than be coerced.
            let value =
                params["value"].as_str().ok_or_else(|| Error::invalid_params("value must be a string"))?;
            settings.set(id, value)?;
            Ok(serde_json::json!({ "configOptions": settings.describe() }))
        }

        "initialize" => Ok(serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentInfo": { "name": "rook", "version": rook_core::AGENT_VERSION },
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
            },
            "authMethods": [],
        })),

        "session/new" => {
            // Parsed to reject a malformed request before a session exists for
            // it, though nothing in it is needed: the workspace is the one the
            // process was started in, and the title comes from the first prompt.
            let _: protocol::NewSession =
                serde_json::from_value(params).map_err(|e| Error::invalid_params(e.to_string()))?;
            let session = rook.start_session("").map_err(|e| Error::internal(e.to_string()))?;
            Ok(serde_json::json!({
                "sessionId": rook_store::format_session_id(session),
                "modes": protocol::modes(settings.policy.stance()),
                "configOptions": settings.describe(),
            }))
        }

        // Resuming an existing session needs nothing beyond checking it exists:
        // the loop replays the log on its own.
        "session/load" | "session/resume" => {
            let request: protocol::SessionRef =
                serde_json::from_value(params).map_err(|e| Error::invalid_params(e.to_string()))?;
            let id = rook_store::parse_session_id(&request.session_id)
                .ok_or_else(|| Error::invalid_params("not a session id"))?;
            match rook.store.get_session(id) {
                Ok(Some(_)) => Ok(serde_json::json!({
                    "modes": protocol::modes(settings.policy.stance()),
                    "configOptions": settings.describe(),
                })),
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

/// A running turn and the request it owes an answer to.
///
/// The reply is claimed before it is sent, by whichever of the turn and the
/// canceller gets there first — a JSON-RPC id answered twice is as wrong as one
/// never answered, and aborting a task that was about to reply is a real race.
struct Turn {
    display: Arc<preview::Preview>,
    session: String,
    id: serde_json::Value,
    handle: tokio::task::JoinHandle<()>,
    replied: Arc<std::sync::atomic::AtomicBool>,
}

impl Turn {
    fn cancel(self, peer: &Peer) {
        self.handle.abort();
        if !self.replied.swap(true, std::sync::atomic::Ordering::SeqCst) {
            peer.respond(&self.id, Ok(serde_json::json!({ "stopReason": "cancelled" })));
        }
    }
}

/// What the connection keeps between prompts.
#[derive(Clone)]
struct Shared {
    policy: Arc<rook_tools::policy::Policy>,
    servers: Arc<rook_core::lsp::Servers>,
    mcp: Arc<rook_core::McpSession>,
    jobs: Arc<rook_tools::jobs::Jobs>,
}

async fn prompt(
    rook: Arc<Rook>,
    peer: Arc<Peer>,
    shared: Shared,
    setup: TurnSetup,
    id: serde_json::Value,
    request: PreparedPrompt,
    replied: Arc<std::sync::atomic::AtomicBool>,
) {
    let answer = |outcome| {
        if !replied.swap(true, std::sync::atomic::Ordering::SeqCst) {
            peer.respond(&id, outcome);
        }
    };
    let PreparedPrompt { session, session_id, text, attachments } = request;

    let provider = match rook_core::models::configured(&rook.config) {
        Ok(provider) => provider,
        Err(e) => return answer(Err(Error::internal(e.to_string()))),
    };

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    let display = setup.display;
    agent.observer = Some(display.clone());
    agent.options.attachments = attachments;
    agent.policy = shared.policy.clone();
    rook_core::agent::equip(&mut agent, shared.servers.clone(), &shared.mcp, shared.jobs.clone());
    let editor = Arc::new(EditorApprover {
        peer: peer.clone(),
        session: session_id.clone(),
        patience: rook.config.agent.answer_timeout(),
        deciding: rook.config.agent.decide_alone_after(),
        children: setup.client.subagents,
        display: display.clone(),
    });
    agent.approver = editor.clone();
    agent.ask_via(editor);
    agent.effort = setup.effort;
    if setup.client.read {
        agent.tool_ctx.files = Some(Arc::new(EditorFiles {
            peer: peer.clone(),
            session: session_id.clone(),
            can_write: setup.client.write,
            workspace: rook.workspace.clone(),
            allow_outside: rook.config.sandbox.allow_outside_workspace,
            children: setup.client.subagents,
        }));
    }
    if setup.client.terminal {
        agent.tool_ctx.terminals = Some(Arc::new(EditorTerminals {
            peer: peer.clone(),
            session: session_id.clone(),
            timeout: agent.tool_ctx.command_timeout,
            children: setup.client.subagents,
        }));
    }

    // Before the loop borrows the agent: a call names its paths the way somebody
    // standing in this project would, which is the same phrase the CLI, the
    // window and the browser show.
    let here = rook.workspace.clone();
    // Ids are handed out in call order and consumed in the same order, which is
    // how a completion is matched to the call it finishes: the loop dispatches
    // a step's calls in the order the model asked for them.
    let started = AtomicU64::new(0);
    let finished = AtomicU64::new(0);
    // Which message the chunks being streamed belong to. A turn says several
    // things in turn — it works something out, calls a tool, works out the
    // next thing, answers — and the protocol's way of showing that is this id
    // changing. Sending none left an editor with the lot as one run of text.
    let message = AtomicU64::new(0);
    // What is being streamed right now: nothing, thinking, or saying. A change
    // is where one message ends and the next begins, and so is a tool call —
    // the thinking after one is not the thinking before it.
    let streaming = std::sync::atomic::AtomicU8::new(0);
    let part = |kind: u8| -> String {
        if streaming.swap(kind, Ordering::Relaxed) != kind {
            message.fetch_add(1, Ordering::Relaxed);
        }
        format!("msg_{}", message.load(Ordering::Relaxed))
    };
    let result = agent
        .run_with(&text, |progress| {
            let update = match progress {
                Progress::ExtensionUi(rook_proto::ChatEvent::Agent { text, .. }) if !text.is_empty() => {
                    protocol::agent_thought_chunk(&session_id, &format!("{text}\n"), &part(1))
                }
                Progress::ExtensionUi(_) => return,
                Progress::Delta(Delta::Effort(report)) => protocol::agent_thought_chunk(
                    &session_id,
                    &format!("[request: {}]\n", report.describe()),
                    &part(1),
                ),
                Progress::Delta(Delta::Text(text)) => {
                    display.narrative(session, text, &part(2), false);
                    return;
                }
                Progress::Delta(Delta::Reasoning(text)) => {
                    display.narrative(session, text, &part(1), true);
                    return;
                }
                Progress::Delta(Delta::ToolCall(call)) => {
                    // Ends whatever was being streamed: the thinking after a
                    // call is not a continuation of the thinking before it.
                    streaming.store(0, Ordering::Relaxed);
                    protocol::tool_call(
                        &session_id,
                        &format!("call_{}", started.fetch_add(1, Ordering::Relaxed)),
                        preview::prefix(&call.name, 256),
                        &rook_core::calls::doing(&call.name, Some(&call.arguments), &here),
                        protocol::tool_kind(&call.name),
                    )
                }
                // The editor already has a tool call open for the delegation;
                // this is progress within it, which reads as a thought.
                Progress::Delegated { task, done, total } => protocol::agent_thought_chunk(
                    &session_id,
                    &format!("[{done}/{total}] {}\n", preview::prefix(task, 512)),
                    &part(1),
                ),
                // The editor has nothing else to show while the model reads a
                // long prompt, and on a local one that is minutes; a thought is
                // where an editor puts what is happening but is not the answer.
                Progress::Waiting { secs, patience } => protocol::agent_thought_chunk(
                    &session_id,
                    &format!("  {}\n", rook_core::calls::waiting(secs, patience)),
                    &part(1),
                ),
                Progress::Delegating { at, doing } => protocol::agent_thought_chunk(
                    &session_id,
                    &format!("  {}\n", rook_core::calls::delegating(at, doing)),
                    &part(1),
                ),
                // The editor has the call open already; this says it is still
                // going and whether anything is happening in it, which is the
                // one thing an open call does not say by itself.
                Progress::FollowUp { id } => protocol::agent_thought_chunk(
                    &session_id,
                    &format!("Starting follow-up {id}\n"),
                    &part(1),
                ),
                Progress::Working { call, said } => {
                    protocol::agent_thought_chunk(&session_id, &format!("  {call}: {said}\n"), &part(1))
                }
                // What the person said while it ran, at the moment it is taken
                // up. Until now a message typed mid-turn was queued with no end
                // to the wait in sight.
                Progress::Heard { text, .. } => protocol::agent_thought_chunk(
                    &session_id,
                    &format!(
                        "  ✓ taken up: {text}
"
                    ),
                    &part(1),
                ),
                Progress::ToolDone { failed, .. } => protocol::tool_call_done(
                    &session_id,
                    &format!("call_{}", finished.fetch_add(1, Ordering::Relaxed)),
                    failed,
                ),
                Progress::Context { used, size } => protocol::usage_update(&session_id, used, size),
                // Cumulative token spend is not context usage or a monetary
                // cost. Only the dedicated context event maps to ACP usage.
                // Nothing to show: the block is the wire's copy of what
                // `Reasoning` already streamed to the person.
                // A step counter has no slot either: the editor draws its
                // progress from the tool calls it is shown.
                Progress::Spent { .. }
                | Progress::Turn { .. }
                | Progress::PromptAdmitted { .. }
                | Progress::Step { .. }
                | Progress::Delta(
                    Delta::Done { .. }
                    | Delta::ReasoningDone(_)
                    | Delta::Dispatch(_)
                    | Delta::ResponseMetadata { .. },
                ) => {
                    return;
                }
            };
            peer.notify("session/update", update);
        })
        .await;

    // Decisions and change summaries go out as the last thing said.
    if let Ok(outcome) = &result {
        let mut said = String::new();
        if let Some(note) = outcome.changed_note() {
            said.push_str(&format!("\n\n{note}"));
        }
        for text in &outcome.decisions {
            said.push_str(&format!("\n\nDecided: {text}"));
        }
        for text in &outcome.open_questions {
            said.push_str(&format!("\n\nOpen question: {text}"));
        }
        if !said.is_empty() {
            // Its own message: what a turn adds at the end is not a
            // continuation of the last thing it streamed.
            peer.notify("session/update", protocol::agent_message_chunk(&session_id, &said, &part(2)));
        }
    }
    answer(match result {
        Ok(outcome) => Ok(serde_json::json!({ "stopReason": stop_reason(&outcome.stopped) })),
        Err(e) => Err(Error::internal(e.to_string())),
    });
}

fn stop_reason(stopped: &str) -> &'static str {
    match stopped {
        "max_steps" => "max_turn_requests",
        "max_tokens" => "max_tokens",
        "refusal" => "refusal",
        _ => "end_turn",
    }
}

/// The other end of the connection: notifications out, requests out, replies in.
struct Peer {
    outbound: rook_core::delivery::Sender,
    closed: Arc<tokio::sync::Notify>,
    waiting: std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, Error>>>>,
    next_id: AtomicU64,
}

impl Peer {
    fn new(outbound: rook_core::delivery::Sender, closed: Arc<tokio::sync::Notify>) -> Self {
        Self { outbound, closed, waiting: std::sync::Mutex::new(HashMap::new()), next_id: AtomicU64::new(1) }
    }

    fn send(&self, value: &impl serde::Serialize) {
        if self.outbound.try_send_serialized(value).is_err() {
            self.closed.notify_one();
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
        let limit = (method != "terminal/wait_for_exit").then(|| std::time::Duration::from_secs(60));
        self.request_within(method, params, limit).await
    }

    /// `None` waits as long as the client takes, which is right for a terminal
    /// whose command is still running and wrong for a question to a person who
    /// may never come back to it.
    async fn request_within(
        &self,
        method: &str,
        params: serde_json::Value,
        limit: Option<std::time::Duration>,
    ) -> Option<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
            if waiting.len() >= 256 {
                return None;
            }
            waiting.insert(id, tx);
        }
        let _pending = Pending { peer: self, id };
        self.send(&protocol::Request { jsonrpc: "2.0", id, method, params });
        let answer = match limit {
            None => rx.await.ok(),
            Some(limit) => tokio::time::timeout(limit, rx).await.ok().and_then(Result::ok),
        };
        if answer.is_none() {
            // Nothing will resolve it now, and the entry would otherwise sit in
            // the map for the life of the connection.
            self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        }
        answer.and_then(|result| match result {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!("editor refused {method}: {}", error.message);
                None
            }
        })
    }

    async fn resolve(&self, id: u64, result: Result<serde_json::Value, Error>) {
        if let Some(tx) = self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            let _ = tx.send(result);
        }
    }
}

struct Pending<'a> {
    peer: &'a Peer,
    id: u64,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.peer.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// Turns the permission policy's question into the editor's approval dialog.
struct EditorApprover {
    children: bool,
    display: Arc<preview::Preview>,
    peer: Arc<Peer>,
    session: String,
    patience: std::time::Duration,
    /// How long a question waits, which is longer than an approval does: a
    /// denial is safe and a turn abandoned mid-way is not.
    deciding: std::time::Duration,
}

#[async_trait]
impl Approver for EditorApprover {
    fn for_session(&self, session: &str) -> Option<Arc<dyn Approver>> {
        self.children.then(|| {
            Arc::new(Self {
                peer: self.peer.clone(),
                session: session.into(),
                patience: self.patience,
                deciding: self.deciding,
                children: true,
                display: self.display.clone(),
            }) as Arc<dyn Approver>
        })
    }
    async fn ask(&self, tool: &str, risk: &Risk, preview: Option<&str>) -> Approval {
        let _waiting = self.display.action(&self.session);
        // Do not copy an arbitrarily long command/path list merely to render
        // its approval. A shortened operation title cannot support informed
        // approval, so oversized subjects remain unanswered.
        let subject_limit = (self.peer.outbound.byte_limit().saturating_sub(3072) / 12).clamp(128, 8192);
        let subject = match risk {
            Risk::Write(paths) => paths.iter().try_fold(0usize, |used, path| {
                used.checked_add(path.len() + 2).filter(|size| *size <= subject_limit)
            }),
            Risk::Execute(text) | Risk::Network(text) => Some(text.len()),
            Risk::External { name, .. } => Some(name.len()),
            _ => Some(0),
        };
        if subject.is_none_or(|size| size > subject_limit) {
            return Approval::Unanswered(
                "the operation is too large to display safely; split it into smaller calls".into(),
            );
        }
        // As a text block rather than the schema's `diff`, which wants the whole
        // of both texts so the editor can render its own view. What is on offer
        // here is one already rendered, and the same string every front end
        // shows.
        let maximum = (self.peer.outbound.byte_limit().saturating_sub(3072) / 12).min(64 * 1024);
        let shown = preview.map(|text| preview::prefix(text, maximum));
        let mut content: Vec<serde_json::Value> = shown
            .map(|text| {
                vec![serde_json::json!({ "type": "content", "content": { "type": "text", "text": text } })]
            })
            .unwrap_or_default();
        let shortened = shown.zip(preview).is_some_and(|(shown, full)| shown.len() != full.len());
        if shortened {
            content.push(serde_json::json!({
                "type":"content","content":{"type":"text","text":"Preview shortened. Inspect the operation in the saved session before approving."},
            }));
        }
        let mut params = serde_json::json!({
            "sessionId": self.session,
            "toolCall": {
                "toolCallId": self.display.approval_id(&self.session, tool),
                "title": risk.describe(),
                "content": content,
            },
            "options": [
                { "optionId": "once",   "name": "Allow once",    "kind": "allow_once" },
                { "optionId": "always", "name": "Always allow",  "kind": "allow_always" },
                { "optionId": "reject", "name": "Reject",        "kind": "reject_once" },
            ],
        });
        if shortened {
            params["_meta"] = serde_json::json!({"rook":{"previewTruncated":true}});
        }

        let Some(answer) =
            self.peer.request_within("session/request_permission", params, Some(self.patience)).await
        else {
            // Unanswered, not denied: nobody decided anything. Told apart so
            // the model is not sent looking for a fault in a call that never
            // ran.
            return Approval::Unanswered(format!(
                "the editor did not answer within {}s — raise `[agent] answer_timeout_secs` if that \
                 is too short",
                self.patience.as_secs()
            ));
        };
        // A `cancelled` outcome means the turn is being torn down, not that the
        // user chose to refuse this one thing.
        match answer.pointer("/outcome/outcome").and_then(|o| o.as_str()) {
            Some("selected") => match answer.pointer("/outcome/optionId").and_then(|o| o.as_str()) {
                Some("once") => Approval::Once,
                Some("always") => Approval::ForRun,
                _ => Approval::Deny("the user rejected it".into()),
            },
            _ => Approval::Unanswered("the request was cancelled".into()),
        }
    }
}

/// Puts the agent's questions to the editor as its approval dialog, which is
/// the only thing ACP offers that a person answers.
///
/// It renders options and nothing else, so a free-text question comes back
/// skipped rather than pretending: the model is then told to decide for itself,
/// which is the honest outcome of asking somewhere no one can type.
#[async_trait]
impl rook_tools::ask::Asker for EditorApprover {
    async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
        let mut answers = Vec::with_capacity(questions.len());
        for (i, q) in questions.iter().enumerate() {
            answers.push(match q.choices.is_empty() {
                true => q.unanswered(),
                false => self.choose(i, q).await,
            });
        }
        answers
    }
}

impl EditorApprover {
    async fn choose(&self, index: usize, q: &Question) -> Answer {
        let options: Vec<_> = q
            .choices
            .iter()
            .enumerate()
            .map(|(i, choice)| {
                serde_json::json!({ "optionId": i.to_string(), "name": choice, "kind": "allow_once" })
            })
            .collect();
        let params = serde_json::json!({
            "sessionId": self.session,
            "toolCall": { "toolCallId": format!("ask_{index}"), "title": q.question },
            "options": options,
        });

        let picked = self
            .peer
            .request_within("session/request_permission", params, Some(self.deciding))
            .await
            .filter(|a| a.pointer("/outcome/outcome").and_then(|o| o.as_str()) == Some("selected"))
            .and_then(|a| a.pointer("/outcome/optionId")?.as_str()?.parse::<usize>().ok())
            .and_then(|i| q.choices.get(i).cloned());

        Answer { question: q.question.clone(), chosen: picked.into_iter().collect() }
    }
}

/// The editor's files, which include buffers it has not saved.
///
/// An agent that reads around them sees the file as it was before the user's
/// last change and edits it back, which is the confusing failure this exists to
/// prevent. Only used when the client said in `initialize` that it can serve
/// them: the protocol requires the agent not to ask otherwise.
struct EditorFiles {
    children: bool,
    peer: Arc<Peer>,
    session: String,
    can_write: bool,
    workspace: std::path::PathBuf,
    allow_outside: bool,
}

#[async_trait]
impl rook_tools::Files for EditorFiles {
    fn for_session(&self, session: &str) -> Option<Arc<dyn rook_tools::Files>> {
        self.children.then(|| {
            Arc::new(Self {
                peer: self.peer.clone(),
                session: session.into(),
                can_write: self.can_write,
                workspace: self.workspace.clone(),
                allow_outside: self.allow_outside,
                children: true,
            }) as Arc<dyn rook_tools::Files>
        })
    }
    async fn read(&self, path: &Path) -> rook_tools::Result<String> {
        let params = serde_json::json!({ "sessionId": self.session, "path": path });
        let answer = self.peer.request("fs/read_text_file", params).await.ok_or_else(|| {
            rook_tools::ToolError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other("the editor closed the connection"),
            }
        })?;
        answer["content"].as_str().map(str::to_string).ok_or_else(|| rook_tools::ToolError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("the editor answered {answer}")),
        })
    }

    async fn write(&self, path: &Path, contents: &str) -> rook_tools::Result<()> {
        if !self.can_write {
            // Falling back rather than refusing: the client can read buffers and
            // not write them, and a write to disk is still correct then.
            let mut ctx = rook_tools::ToolContext::new(self.workspace.clone());
            ctx.allow_outside_workspace = self.allow_outside;
            return ctx.write_text(path, contents).await;
        }
        let params = serde_json::json!({ "sessionId": self.session, "path": path, "content": contents });
        self.peer.request("fs/write_text_file", params).await.map(|_| ()).ok_or_else(|| {
            rook_tools::ToolError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other("the editor closed the connection"),
            }
        })
    }
}

/// What this turn takes from the connection: what the client can do for it, and
/// what the user has set.
struct TurnSetup {
    display: Arc<preview::Preview>,
    client: ClientFiles,
    effort: rook_llm::Effort,
}

struct PreparedPrompt {
    session: u128,
    session_id: String,
    text: String,
    attachments: Vec<rook_proto::Attachment>,
}

/// What the client said it can do, from the `initialize` request.
#[derive(Clone, Copy, Default)]
struct ClientFiles {
    read: bool,
    write: bool,
    terminal: bool,
    subagents: bool,
    compaction: bool,
}

impl ClientFiles {
    fn from_initialize(params: &serde_json::Value) -> Self {
        let capabilities = &params["clientCapabilities"];
        let fs = &capabilities["fs"];
        Self {
            read: fs["readTextFile"] == true,
            write: fs["writeTextFile"] == true,
            terminal: capabilities["terminal"] == true,
            subagents: params["protocolVersion"] == 1 && capabilities["subagents"].is_object(),
            compaction: params["protocolVersion"] == 1 && capabilities["session"]["compaction"].is_object(),
        }
    }
}

/// The editor's terminal, so a build the agent starts is one the user can watch.
///
/// Four calls per command — create, wait, read, release — because the protocol
/// separates them. The release is not optional: the client holds the terminal
/// open until the agent says it is done with it.
struct EditorTerminals {
    children: bool,
    peer: Arc<Peer>,
    session: String,
    timeout: std::time::Duration,
}

impl EditorTerminals {
    async fn call(&self, method: &str, params: serde_json::Value) -> rook_tools::Result<serde_json::Value> {
        self.peer
            .request(method, params)
            .await
            .ok_or_else(|| rook_tools::ToolError::Denied(format!("the editor did not answer {method}")))
    }

    fn about(&self, terminal: &str) -> serde_json::Value {
        serde_json::json!({ "sessionId": self.session, "terminalId": terminal })
    }
}

#[async_trait]
impl rook_tools::Terminals for EditorTerminals {
    fn for_session(&self, session: &str) -> Option<Arc<dyn rook_tools::Terminals>> {
        self.children.then(|| {
            Arc::new(Self {
                peer: self.peer.clone(),
                session: session.into(),
                timeout: self.timeout,
                children: true,
            }) as Arc<dyn rook_tools::Terminals>
        })
    }
    async fn run(
        &self,
        command: &str,
        cwd: &Path,
        output_limit: usize,
    ) -> rook_tools::Result<rook_tools::Ran> {
        let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("/bin/sh", "-c") };
        let created = self
            .call(
                "terminal/create",
                serde_json::json!({
                    "sessionId": self.session,
                    "command": shell,
                    "args": [flag, command],
                    "cwd": cwd,
                    "outputByteLimit": output_limit,
                }),
            )
            .await?;
        let terminal = created["terminalId"]
            .as_str()
            .ok_or_else(|| rook_tools::ToolError::Denied(format!("no terminalId in {created}")))?
            .to_string();

        let waited =
            tokio::time::timeout(self.timeout, self.call("terminal/wait_for_exit", self.about(&terminal)))
                .await;
        let timed_out = waited.is_err();
        if timed_out {
            let _ = self.call("terminal/kill", self.about(&terminal)).await;
        }

        // Read before releasing: the client is free to forget the terminal the
        // moment it is released.
        let read = self.call("terminal/output", self.about(&terminal)).await;
        let _ = self.call("terminal/release", self.about(&terminal)).await;
        let read = read?;

        Ok(rook_tools::Ran {
            output: read["output"].as_str().unwrap_or_default().to_string(),
            exit_code: read["exitStatus"]["exitCode"].as_i64().unwrap_or(-1) as i32,
            truncated: read["truncated"] == true,
            timed_out,
        })
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn preview_capabilities_require_explicit_v1_object_values() {
        for version in [serde_json::Value::Null, serde_json::json!(1), serde_json::json!(2)] {
            let caps = ClientFiles::from_initialize(&serde_json::json!({
                "protocolVersion":version,"clientCapabilities":{"subagents":{},"session":{"compaction":{}}},
            }));
            assert_eq!(caps.subagents, version == 1);
            assert_eq!(caps.compaction, version == 1);
        }
    }
    #[tokio::test]
    async fn pending_editor_requests_are_bounded_and_cancelled_owners_release_all_entries() {
        let (send, mut requests) = rook_core::delivery::channel(4096, 65536);
        let peer = Arc::new(Peer::new(send, Arc::new(tokio::sync::Notify::new())));
        let mut tasks = Vec::new();
        for _ in 0..256 {
            let asking = peer.clone();
            tasks.push(tokio::spawn(async move {
                asking.request_within("read", serde_json::json!({}), None).await
            }));
        }
        for _ in 0..256 {
            requests.recv().await.unwrap();
        }
        assert_eq!(peer.waiting.lock().unwrap().len(), 256, "the bound must be reached");
        assert!(peer.request_within("excess", serde_json::json!({}), None).await.is_none());
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        assert!(peer.waiting.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn editor_errors_cannot_be_reported_as_successful_writes() {
        use rook_tools::Files;
        let (send, mut requests) = rook_core::delivery::channel(8, 65536);
        let peer = Arc::new(Peer::new(send, Arc::new(tokio::sync::Notify::new())));
        let editor = EditorFiles {
            children: false,
            peer: peer.clone(),
            session: "s".into(),
            can_write: true,
            workspace: "/tmp".into(),
            allow_outside: false,
        };
        let write = tokio::spawn(async move { editor.write(Path::new("/tmp/x"), "text").await });
        let request: serde_json::Value = serde_json::from_str(&requests.recv().await.unwrap().text).unwrap();
        peer.resolve(request["id"].as_u64().unwrap(), Err(Error::internal("write refused"))).await;
        assert!(write.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn cancelling_a_peer_request_releases_its_pending_entry() {
        let (send, mut requests) = rook_core::delivery::channel(8, 65536);
        let peer = Arc::new(Peer::new(send, Arc::new(tokio::sync::Notify::new())));
        let asking = peer.clone();
        let task = tokio::spawn(async move { asking.request("read", serde_json::json!({})).await });
        requests.recv().await.unwrap();
        assert_eq!(peer.waiting.lock().unwrap().len(), 1);
        task.abort();
        let _ = task.await;
        assert!(peer.waiting.lock().unwrap().is_empty());
    }
    #[test]
    fn stop_reasons_use_the_engines_wire_spelling() {
        assert_eq!(stop_reason("max_tokens"), "max_tokens");
        assert_eq!(stop_reason("refusal"), "refusal");
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    #[tokio::test]
    async fn a_blocked_writer_keeps_its_lease_and_exceeding_capacity_closes_the_connection() {
        use tokio::io::AsyncReadExt;
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut config = rook_core::Config::default();
        config.server.chat_queue_events = 1;
        let rook = Rook::from_parts(
            rook_store::Store::open(store.path()).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            Default::default(),
            workspace.path().to_owned(),
        );
        let inspection = rook.for_workspace(rook.workspace.clone());
        let (mut input, server_input) = tokio::io::duplex(4096);
        let (server_output, mut output) = tokio::io::duplex(1);
        let serving = tokio::spawn(serve(rook, BufReader::new(server_input), server_output));
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":1}}\n").await.unwrap();
        output.read_exact(&mut [0; 1]).await.unwrap();
        // A one-byte pipe cannot finish the initialize response. The dequeued
        // frame still consumes the sole event lease when the next reply arrives.
        input
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\"params\":{\"cwd\":\".\"}}\n",
            )
            .await
            .unwrap();
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(60), serving).await.unwrap().unwrap();
        assert!(result.unwrap_err().to_string().contains("queue budget"));
        assert_eq!(
            inspection.store.list_sessions().unwrap().len(),
            1,
            "the completed mutation remains durable despite lost delivery"
        );
    }
    #[tokio::test]
    async fn an_acp_frame_is_bounded_before_its_newline_arrives() {
        let bytes = vec![b'x'; rook_core::attachments::MAX_FRAME_BYTES + 1];
        let mut reader = BufReader::with_capacity(1024, bytes.as_slice());
        assert_eq!(next_frame(&mut reader).await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        let mut reader = BufReader::with_capacity(2, b"one\ntwo\n".as_slice());
        assert_eq!(next_frame(&mut reader).await.unwrap().as_deref(), Some("one\n"));
        assert_eq!(next_frame(&mut reader).await.unwrap().as_deref(), Some("two\n"));
        assert!(next_frame(&mut reader).await.unwrap().is_none());
    }
}
