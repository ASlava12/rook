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

mod history;
mod preview;
pub mod protocol;
mod sessions;
mod setup;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
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
    let settings = Arc::new(Settings { policy: policy.clone(), effort: rook.config.agent.effort() });
    let runtimes = Arc::new(std::sync::Mutex::new(HashMap::<u128, Shared>::new()));
    // Filled in by `initialize`, and read when a turn starts: the client may
    // serve its unsaved buffers, and the protocol forbids asking if it cannot.
    let client_files: Arc<Mutex<ClientFiles>> = Default::default();

    let peer = Arc::new(Peer::new(outbound, closed));
    let mut turn: Option<Turn> = None;
    let mut opening: Option<Opening> = None;
    let mut next_opening = 0u64;

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
                    turn.cancel(&peer).await;
                }
                if opening.as_ref().is_some_and(|active| {
                    target.is_none_or(|target| rook_store::parse_session_id(target) == active.session())
                }) && let Some(active) = opening.take()
                {
                    active.cancel(&peer, &runtimes).await;
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
                let (selected_mode, effort) = match settings.choice(&rook, session) {
                    Ok(choice) => choice,
                    Err(error) => {
                        peer.respond(&id, Err(error));
                        continue;
                    }
                };
                if !runtimes.lock().unwrap_or_else(|error| error.into_inner()).contains_key(&session)
                    && runtimes.lock().unwrap_or_else(|error| error.into_inner()).len()
                        >= rook.config.server.max_acp_sessions
                {
                    peer.respond(&id, Err(Error::invalid_params(format!(
                        "server.max_acp_sessions={} reached; close an active session before starting another",
                        rook.config.server.max_acp_sessions,
                    ))));
                    continue;
                }
                if let Some(turn) = turn.take() {
                    turn.cancel(&peer).await;
                }
                let existing =
                    runtimes.lock().unwrap_or_else(|error| error.into_inner()).get(&session).cloned();
                let shared = match existing {
                    Some(shared) => {
                        if !shared.ready.load(Ordering::Acquire) {
                            peer.respond(
                                &id,
                                Err(Error::invalid_params(
                                    "session setup is still initializing; wait for its response or close it",
                                )),
                            );
                            continue;
                        }
                        shared
                    }
                    None => {
                        let prepared = match setup::prepare(
                            &rook,
                            Some(session),
                            &serde_json::json!({"sessionId":session_id}),
                        ) {
                            Ok(prepared) => prepared,
                            Err(error) => {
                                peer.respond(&id, Err(error));
                                continue;
                            }
                        };
                        let project = match prepared.build(&rook) {
                            Ok(project) => project,
                            Err(error) => {
                                peer.respond(&id, Err(error));
                                continue;
                            }
                        };
                        let shared = Shared::new(project, selected_mode).await;
                        {
                            let mut pool = runtimes.lock().unwrap_or_else(|error| error.into_inner());
                            if !pool.contains_key(&session)
                                && pool.len() >= rook.config.server.max_acp_sessions
                            {
                                peer.respond(&id, Err(Error::invalid_params("server.max_acp_sessions reached during prompt preparation; close an active session")));
                                continue;
                            }
                            pool.insert(session, shared.clone());
                        }
                        shared
                    }
                };
                let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
                // Read out of the lock before the spawn: a guard living to the
                // end of the statement would travel into the task with it.
                shared.policy.set_stance(selected_mode);
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
                        shared.rook.clone(),
                        peer.clone(),
                        shared,
                        TurnSetup { client, effort, display },
                        id,
                        PreparedPrompt { session, session_id, text, attachments },
                        replied,
                    )),
                });
            }
            ("session/new" | "session/resume" | "session/load", Some(id)) => {
                if opening.as_ref().is_some_and(|active| !active.handle.is_finished()) {
                    peer.respond(
                        &id,
                        Err(Error::invalid_params(
                            "one ACP setup request is already initializing; cancel/close it before another",
                        )),
                    );
                    continue;
                }
                if let Some(previous) = opening.take() {
                    previous.cancel(&peer, &runtimes).await;
                }
                let session = if method != "session/new" {
                    match sessions::existing(&rook, &params) {
                        Ok(session) => {
                            if preview::restricted(&rook, &rook_store::format_session_id(session))
                                .unwrap_or(true)
                            {
                                peer.respond(
                                    &id,
                                    Err(Error::invalid_params("child resume is not advertised")),
                                );
                                continue;
                            }
                            Some(session)
                        }
                        Err(error) => {
                            peer.respond(&id, Err(error));
                            continue;
                        }
                    }
                } else {
                    None
                };
                let prepared = match setup::prepare(&rook, session, &params) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        peer.respond(&id, Err(error));
                        continue;
                    }
                };
                let full = {
                    let pool = runtimes.lock().unwrap_or_else(|error| error.into_inner());
                    !prepared.names.is_empty()
                        && pool.len() >= rook.config.server.max_acp_sessions
                        && session.is_none_or(|session| !pool.contains_key(&session))
                };
                if full {
                    peer.respond(
                        &id,
                        Err(Error::invalid_params(
                            "server.max_acp_sessions reached; close an active session before connecting MCP",
                        )),
                    );
                    continue;
                }
                next_opening = next_opening.saturating_add(1);
                let epoch = next_opening;
                let identity = Arc::new(std::sync::Mutex::new(session));
                let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let output = replied.clone();
                let current = identity.clone();
                let base = rook.clone();
                let selections = settings.clone();
                let pool = runtimes.clone();
                let connection = peer.clone();
                let request_id = id.clone();
                let client = *client_files.lock().await;
                let executing = turn.as_ref().is_some_and(|active| {
                    !active.handle.is_finished() && session == rook_store::parse_session_id(&active.session)
                });
                let current_view = turn
                    .as_ref()
                    .filter(|active| {
                        client.subagents && session == rook_store::parse_session_id(&active.session)
                    })
                    .map(|active| active.display.clone());
                opening = Some(Opening {
                    session: identity,
                    id,
                    replied,
                    epoch,
                    handle: tokio::spawn(async move {
                        let outcome = async {
                            let wanted = prepared.names.clone();
                            let built = tokio::task::spawn_blocking(move || prepared.build(&base)).await
                                .map_err(|_| Error::internal("workspace discovery could not finish"))??;
                            let session = match session {
                                Some(session) => session,
                                None => built.start_session("").map_err(|_| Error::internal("could not create the ACP session"))?,
                            };
                            *current.lock().unwrap_or_else(|error| error.into_inner()) = Some(session);
                            let (mode, _) = selections.choice(&built, session)?;
                            let existing = pool.lock().unwrap_or_else(|error| error.into_inner()).get(&session).cloned();
                            let shared = match existing {
                                Some(shared) if shared.rook.workspace == built.workspace
                                    && shared.rook.mcp_servers().eq(built.mcp_servers()) => Some(shared),
                                Some(_) if executing => return Err(Error::invalid_params("session is executing; cancel or finish its turn before replacing MCP setup")),
                                Some(_) => {
                                    let old = pool.lock().unwrap_or_else(|error| error.into_inner()).remove(&session);
                                    if let Some(old) = old { old.shutdown().await; }
                                    if wanted.is_empty() { None } else {
                                        let mut shared = Shared::new(built.clone(), mode).await;
                                        shared.epoch = epoch; shared.ready.store(false, Ordering::Release);
                                        pool.lock().unwrap_or_else(|error| error.into_inner()).insert(session, shared.clone());
                                        Some(shared)
                                    }
                                },
                                None if wanted.is_empty() => None,
                                None => {
                                    let mut shared = Shared::new(built.clone(), mode).await;
                                    shared.epoch = epoch;
                                    shared.ready.store(false, Ordering::Release);
                                    {
                                        let mut pool = pool.lock().unwrap_or_else(|error| error.into_inner());
                                        if !pool.contains_key(&session) && pool.len() >= built.config.server.max_acp_sessions {
                                            return Err(Error::invalid_params("server.max_acp_sessions reached during setup; close an active session"));
                                        }
                                        pool.insert(session, shared.clone());
                                    }
                                    Some(shared)
                                }
                            };
                            if let Some(shared) = &shared {
                                let mcp = shared.mcp.get_or_init(|| async { Arc::new(shared.rook.connect_mcp().await) }).await;
                                if wanted.iter().any(|name| !mcp.report().servers.iter().any(|server| &server.name == name && server.state == "connected")) {
                                    let removed = pool.lock().unwrap_or_else(|error| error.into_inner()).remove(&session);
                                    if let Some(shared) = removed { shared.shutdown().await; }
                                    return Err(Error::internal(format!("requested MCP initialization failed; retained session {} can be inspected/retried explicitly", rook_store::format_session_id(session))));
                                }
                                shared.ready.store(true, Ordering::Release);
                            }
                            let (mode, effort) = selections.choice(&built, session)?;
                            if let Some(shared) = &shared { shared.policy.set_stance(mode); }
                            let mut result = serde_json::json!({"modes":protocol::modes(mode),"configOptions":protocol::config_options(mode,effort)});
                            if method == "session/new" { result["sessionId"] = rook_store::format_session_id(session).into(); }
                            if built.config.telemetry.propagate_trace && let Some(trace) = built.session_trace(session).map_err(|error| protocol::Error::internal(error.to_string()))? {
                                result["_meta"] = serde_json::json!({"traceparent":trace.traceparent()});
                            }
                            Ok((result, built, session))
                        }.await;
                        match outcome {
                            Ok((result, project, session)) if method == "session/load" => {
                                // This task owns replay too: aborting setup destroys its
                                // history producer rather than detaching another task.
                                let mut result = Ok(result);
                                if client.subagents || client.compaction {
                                    let recovery = preview::Preview::new(
                                        project.clone(),
                                        connection.clone(),
                                        session,
                                        client.subagents,
                                        client.compaction,
                                    );
                                    result = recovery.recover().map_err(|_| Error::internal("historical preview recovery failed"))
                                        .map(|truncated| {
                                            let mut result = result.unwrap_or_else(|_| serde_json::json!({}));
                                            result["_meta"]["rook"]["recovery"] = serde_json::json!({"truncated":truncated,"maxSessions":64,"maxCompactions":32,"eventsPerSession":128,"childHistoryReplayed":false});
                                            result
                                        });
                                }
                                if result.is_ok() {
                                    result = history::replay(&project, &connection, session).await.and_then(|through| result.map(|mut result| {
                                        result["_meta"]["rook"]["history"] = serde_json::json!({"through":through,"rootComplete":true}); result
                                    }));
                                }
                                let succeeded = result.is_ok();
                                if connection.respond_wait(&request_id, result).await.is_ok() {
                                    output.store(true, Ordering::Release);
                                    if succeeded && let Some(view) = current_view {
                                        view.snapshot();
                                    }
                                }
                            }
                            Ok((result, _, _)) => {
                                connection.respond(&request_id, Ok(result));
                                output.store(true, Ordering::Release);
                            }
                            Err(error) => {
                                connection.respond(&request_id, Err(error));
                                output.store(true, Ordering::Release);
                            }
                        }
                    }),
                });
            }
            (_, Some(id)) => {
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
                if method == "session/close" {
                    let session = match sessions::existing(&rook, &params) {
                        Ok(session) => session,
                        Err(error) => {
                            peer.respond(&id, Err(error));
                            continue;
                        }
                    };
                    if turn
                        .as_ref()
                        .is_some_and(|active| rook_store::parse_session_id(&active.session) == Some(session))
                        && let Some(active) = turn.take()
                    {
                        active.cancel(&peer).await;
                    }
                    if opening.as_ref().is_some_and(|active| active.session() == Some(session))
                        && let Some(active) = opening.take()
                    {
                        active.cancel(&peer, &runtimes).await;
                    }
                    let removed = runtimes.lock().unwrap_or_else(|error| error.into_inner()).remove(&session);
                    if let Some(shared) = removed {
                        shared.shutdown().await;
                    }
                    peer.respond(&id, Ok(serde_json::json!({})));
                    continue;
                }
                let selected_session = params["sessionId"].as_str().and_then(rook_store::parse_session_id);
                let mut outcome = dispatch(&rook, &settings, &method, params);
                let succeeded = outcome.is_ok();
                if succeeded && method == "session/set_stance" {
                    for (&session, runtime) in
                        runtimes.lock().unwrap_or_else(|error| error.into_inner()).iter()
                    {
                        match settings.choice(&rook, session) {
                            Ok((mode, _)) => runtime.policy.set_stance(mode),
                            Err(error) => {
                                outcome = Err(error);
                                break;
                            }
                        }
                    }
                } else if succeeded
                    && matches!(method.as_str(), "session/set_mode" | "session/set_config_option")
                    && let Some(session) = selected_session
                    && let Some(runtime) =
                        runtimes.lock().unwrap_or_else(|error| error.into_inner()).get(&session)
                {
                    match settings.choice(&rook, session) {
                        Ok((mode, _)) => runtime.policy.set_stance(mode),
                        Err(error) => outcome = Err(error),
                    }
                }
                peer.respond(&id, outcome);
            }
            // A notification we do not handle is not an error.
            (_, None) => tracing::debug!("ignoring notification {method}"),
        }
    };

    if let Some(turn) = turn {
        turn.handle.abort();
        let _ = turn.handle.await;
    }
    if let Some(opening) = opening {
        opening.handle.abort();
        let _ = opening.handle.await;
    }
    let equipment = std::mem::take(&mut *runtimes.lock().unwrap_or_else(|error| error.into_inner()));
    for shared in equipment.into_values() {
        shared.shutdown().await;
    }
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
    effort: rook_llm::Effort,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionSettings {
    version: u8,
    mode: Option<rook_tools::policy::Stance>,
    effort: Option<rook_llm::Effort>,
}

impl Settings {
    fn saved(&self, rook: &Rook, session: u128) -> Result<SessionSettings, Error> {
        let raw = rook
            .store
            .kv_get_limited(&format!("acp-settings/{session:032x}"), 1024)
            .map_err(|error| Error::internal(error.to_string()))?;
        let Some(raw) = raw else {
            return Ok(SessionSettings { version: 1, mode: None, effort: None });
        };
        let saved: SessionSettings = serde_json::from_slice(&raw).map_err(|_| {
            Error::internal("invalid saved ACP session settings; inspect this session's state")
        })?;
        if saved.version != 1 {
            return Err(Error::internal("unsupported saved ACP session settings version"));
        }
        Ok(saved)
    }

    fn choice(
        &self,
        rook: &Rook,
        session: u128,
    ) -> Result<(rook_tools::policy::Stance, rook_llm::Effort), Error> {
        let saved = self.saved(rook, session)?;
        Ok((saved.mode.unwrap_or_else(|| self.policy.stance()), saved.effort.unwrap_or(self.effort)))
    }

    fn describe_in(&self, rook: &Rook, session: u128) -> Result<serde_json::Value, Error> {
        let (mode, effort) = self.choice(rook, session)?;
        Ok(protocol::config_options(mode, effort))
    }

    fn set_in(&self, rook: &Rook, session: u128, id: &str, value: &str) -> Result<(), Error> {
        if id.len() > 64 || value.len() > 64 {
            return Err(Error::invalid_params(
                "ACP setting identifiers and values must contain at most 64 bytes",
            ));
        }
        let mut saved = self.saved(rook, session)?;
        match id {
            "mode" => {
                saved.mode = Some(
                    protocol::mode_from_id(value)
                        .ok_or_else(|| Error::invalid_params(format!("no mode {value:?}")))?,
                )
            }
            "effort" => {
                saved.effort = Some(
                    rook_llm::Effort::parse(value)
                        .ok_or_else(|| Error::invalid_params(format!("no effort {value:?}")))?,
                )
            }
            other => return Err(Error::invalid_params(format!("no setting {other:?}"))),
        }
        // Only closed enums and a version byte can enter this fixed record.
        // Input length validation precedes parsing/diagnostics and enum encoding.
        let bytes = serde_json::to_vec(&saved).map_err(|error| Error::internal(error.to_string()))?;
        rook.store
            .kv_update_session_values(session, &[(&format!("acp-settings/{session:032x}"), &bytes)])
            .map_err(|error| Error::internal(error.to_string()))
    }
    fn set_legacy_mode(&self, value: &str) -> Result<(), Error> {
        if value.len() > 64 {
            return Err(Error::invalid_params("ACP mode value must contain at most 64 bytes"));
        }
        let mode = protocol::mode_from_id(value)
            .ok_or_else(|| Error::invalid_params(format!("no mode {value:?}")))?;
        self.policy.set_stance(mode);
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
            settings.set_legacy_mode(params["modeId"].as_str().unwrap_or_default())?;
            Ok(serde_json::json!({}))
        }
        "session/set_mode" => {
            let session = sessions::existing(rook, &params)?;
            settings.set_in(rook, session, "mode", params["modeId"].as_str().unwrap_or_default())?;
            Ok(serde_json::json!({}))
        }

        "session/set_config_option" => {
            let id = params["configId"].as_str().unwrap_or_default();
            // A boolean option would arrive as one; none of ours are, and a
            // value of the wrong shape should say so rather than be coerced.
            let value =
                params["value"].as_str().ok_or_else(|| Error::invalid_params("value must be a string"))?;
            let session = sessions::existing(rook, &params)?;
            settings.set_in(rook, session, id, value)?;
            Ok(serde_json::json!({ "configOptions": settings.describe_in(rook, session)? }))
        }

        "initialize" => Ok(serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentInfo": { "name": "rook", "version": rook_core::AGENT_VERSION },
            "agentCapabilities": {
                "loadSession": true,
                "mcpCapabilities": { "http": true, "sse": false },
                "sessionCapabilities": { "list": {}, "resume": {}, "close": {} },
                "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
            },
            "authMethods": [],
        })),

        "session/list" => sessions::list(rook, params),

        "authenticate" | "logout" => Ok(serde_json::json!({})),
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
    async fn cancel(self, peer: &Peer) {
        self.handle.abort();
        // Await destruction before replacement or close acknowledges cleanup.
        // Otherwise old editor requests and terminal guards outlive the response.
        let _ = self.handle.await;
        if !self.replied.swap(true, std::sync::atomic::Ordering::SeqCst) {
            peer.respond(&self.id, Ok(serde_json::json!({ "stopReason": "cancelled" })));
        }
    }
}

struct Opening {
    session: Arc<std::sync::Mutex<Option<u128>>>,
    id: serde_json::Value,
    replied: Arc<std::sync::atomic::AtomicBool>,
    epoch: u64,
    handle: tokio::task::JoinHandle<()>,
}
impl Opening {
    fn session(&self) -> Option<u128> {
        *self.session.lock().unwrap_or_else(|error| error.into_inner())
    }
    async fn cancel(self, peer: &Peer, pool: &std::sync::Mutex<HashMap<u128, Shared>>) {
        let session = self.session();
        self.handle.abort();
        let _ = self.handle.await;
        if !self.replied.swap(true, Ordering::AcqRel) {
            if let Some(session) = session {
                let removed = {
                    let mut pool = pool.lock().unwrap_or_else(|error| error.into_inner());
                    if pool.get(&session).is_some_and(|shared| shared.epoch == self.epoch) {
                        pool.remove(&session)
                    } else {
                        None
                    }
                };
                if let Some(shared) = removed {
                    shared.shutdown().await;
                }
            }
            peer.respond(
                &self.id,
                Err(Error::internal("session setup cancelled; retained history is unchanged")),
            );
        }
    }
}

/// What the connection keeps between prompts.
#[derive(Clone)]
struct Shared {
    rook: Arc<Rook>,
    epoch: u64,
    ready: Arc<std::sync::atomic::AtomicBool>,
    policy: Arc<rook_tools::policy::Policy>,
    servers: Arc<rook_core::lsp::Servers>,
    mcp: Arc<tokio::sync::OnceCell<Arc<rook_core::McpSession>>>,
    jobs: Arc<rook_tools::jobs::Jobs>,
}

impl Shared {
    async fn new(rook: Arc<Rook>, stance: rook_tools::policy::Stance) -> Self {
        let policy = rook_core::agent::policy_for(&rook.config);
        policy.set_stance(stance);
        Self {
            epoch: 0,
            ready: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            policy,
            servers: rook_core::agent::servers_for(&rook.config, &rook.workspace),
            mcp: Arc::new(tokio::sync::OnceCell::new()),
            jobs: rook_core::agent::jobs_for(&rook.config),
            rook,
        }
    }

    async fn shutdown(self) {
        // Jobs own local process groups and remote leases. Drop them before
        // waiting on protocol shutdown, which may need its own patience.
        drop(self.jobs);
        if let Some(mcp) = self.mcp.get() {
            mcp.shutdown().await;
        }
        self.servers.shutdown().await;
    }
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

    let provider = match rook.provider_for_turn(session, None) {
        Ok(provider) => provider,
        Err(e) => return answer(Err(Error::internal(e.to_string()))),
    };

    let mut agent = AgentLoop::new(&rook, provider.into(), session);
    for value in setup::secrets(&rook) {
        agent.hide_connection_secret(&value);
    }
    let display = setup.display;
    agent.observer = Some(display.clone());
    agent.options.attachments = attachments;
    agent.policy = shared.policy.clone();
    // Connection initialization belongs to the owned turn: the input reader
    // must remain available to cancel/close a server that never handshakes.
    let mcp = shared.mcp.get_or_init(|| async { Arc::new(rook.connect_mcp().await) }).await;
    rook_core::agent::equip(&mut agent, shared.servers.clone(), mcp, shared.jobs.clone());
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

    async fn notify_wait(&self, method: &str, params: &(impl serde::Serialize + Sync)) -> Result<(), Error> {
        self.send_wait(&protocol::Notification { jsonrpc: "2.0", method, params }).await
    }

    async fn respond_wait(
        &self,
        id: &serde_json::Value,
        outcome: Result<serde_json::Value, Error>,
    ) -> Result<(), Error> {
        let (result, error) = match outcome {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error)),
        };
        self.send_wait(&protocol::Response { jsonrpc: "2.0", id, result, error }).await
    }

    async fn send_wait(&self, value: &(impl serde::Serialize + Sync)) -> Result<(), Error> {
        if rook_core::delivery::encoded_size(value, self.outbound.byte_limit()).is_none() {
            return Err(Error::internal(
                "retained ACP history frame exceeds server.chat_queue_bytes; increase it to display the complete retained image",
            ));
        }
        if self.outbound.send_serialized(value).await.is_err() {
            self.closed.notify_one();
            return Err(Error::internal(
                "ACP history frame exceeds server.chat_queue_bytes or delivery closed; increase the queue byte limit for retained images",
            ));
        }
        Ok(())
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

    fn cleanup_request(&self, method: &str, params: serde_json::Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(&protocol::Request { jsonrpc: "2.0", id, method, params });
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
        self.read_limited(path, 16 * 1024 * 1024).await
    }
    async fn read_limited(&self, path: &Path, maximum: usize) -> rook_tools::Result<String> {
        let params = serde_json::json!({"sessionId":self.session,"path":path});
        let answer = self.peer.request("fs/read_text_file", params).await.ok_or_else(|| {
            rook_tools::ToolError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other("the editor closed the connection"),
            }
        })?;
        let text = answer
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| rook_tools::ToolError::Denied("The editor returned no text content".into()))?;
        if text.len() > maximum {
            return Err(rook_tools::ToolError::Denied(format!(
                "Editor content exceeds the {maximum}-byte raw read limit; no text copy was accepted"
            )));
        }
        Ok(text.to_owned())
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
        let answer = self.peer.request("fs/write_text_file", params).await.ok_or_else(|| {
            rook_tools::ToolError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other("the editor closed the connection"),
            }
        })?;
        if !answer.is_null() && !answer.is_object() {
            return Err(rook_tools::ToolError::Denied(
                "The editor returned an invalid write acknowledgement; inspect its buffer before retrying"
                    .into(),
            ));
        }
        Ok(())
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

struct TerminalCleanup {
    peer: Arc<Peer>,
    session: String,
    terminal: Option<String>,
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.take() {
            let params = serde_json::json!({ "sessionId": self.session, "terminalId": terminal });
            // Cancellation cannot await editor replies. Request cleanup on the
            // still-live connection; this does not certify the remote process died.
            self.peer.cleanup_request("terminal/kill", params.clone());
            self.peer.cleanup_request("terminal/release", params);
        }
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
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .ok_or_else(|| {
                rook_tools::ToolError::Denied(
                    "editor terminalId must contain 1..=1024 bytes; creation remains uncertain".into(),
                )
            })?
            .to_string();

        let mut cleanup = TerminalCleanup {
            peer: self.peer.clone(),
            session: self.session.clone(),
            terminal: Some(terminal.clone()),
        };
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
        if self.call("terminal/release", self.about(&terminal)).await.is_ok() {
            cleanup.terminal = None;
        }
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
    #[cfg(unix)]
    #[tokio::test]
    async fn session_equipment_reuses_owned_jobs_and_grants_then_releases_only_that_session() {
        use rook_tools::policy::{Decision, Stance};
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            rook_store::Store::open(state.path()).unwrap(),
            rook_core::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            workspace.path().to_path_buf(),
        );
        let rook = Arc::new(rook);
        let a = Shared::new(rook.clone(), Stance::Assist).await;
        let b = Shared::new(rook.clone(), Stance::Assist).await;
        let risk = Risk::Execute("touch owned-marker".into());
        assert!(!matches!(a.policy.decide(&risk), Decision::Allow));
        a.policy.grant_for_run(&risk.subject());
        assert!(matches!(a.policy.decide(&risk), Decision::Allow));
        assert!(
            !matches!(b.policy.decide(&risk), Decision::Allow),
            "a session grant cannot reach another session"
        );
        let original_jobs = a.jobs.identity();
        let reused = a.clone();
        assert!(Arc::ptr_eq(&a.jobs, &reused.jobs));
        assert!(Arc::ptr_eq(&a.policy, &reused.policy));
        drop(reused);
        a.jobs.start(r#"printf '%s' "$$" > owned-a.pid; exec sleep 60"#, &rook.workspace, None).unwrap();
        b.jobs.start(r#"printf '%s' "$$" > owned-b.pid; exec sleep 60"#, &rook.workspace, None).unwrap();
        async fn pid(path: &Path) -> String {
            tokio::time::timeout(std::time::Duration::from_secs(60), async {
                loop {
                    if let Ok(pid) = std::fs::read_to_string(path)
                        && !pid.is_empty()
                    {
                        return pid;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("owned command must actually start")
        }
        fn alive(pid: &str) -> bool {
            rook_contain::quietly(&mut std::process::Command::new("kill"))
                .args(["-0", pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        }
        async fn stopped(pid: &str) {
            tokio::time::timeout(std::time::Duration::from_secs(60), async {
                while alive(pid) {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("owned command process must actually die");
        }
        let pid_a = pid(&rook.workspace.join("owned-a.pid")).await;
        let pid_b = pid(&rook.workspace.join("owned-b.pid")).await;
        assert!(alive(&pid_a) && alive(&pid_b), "both physical processes must start before cleanup");
        a.shutdown().await;
        stopped(&pid_a).await;
        assert!(alive(&pid_b), "closing one session must not end another session's jobs");
        let resumed = Shared::new(rook.clone(), Stance::Assist).await;
        assert_ne!(resumed.jobs.identity(), original_jobs, "resume after close has fresh equipment");
        assert!(!matches!(resumed.policy.decide(&risk), Decision::Allow), "close releases run grants");
        resumed.shutdown().await;
        b.shutdown().await;
        stopped(&pid_b).await;
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
