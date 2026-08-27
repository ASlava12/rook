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
//! `sandbox.mode` are the same knob.

pub mod protocol;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use rook_core::Rook;
use rook_core::agent::{AgentLoop, Progress};
use rook_llm::Delta;
use rook_tools::ask::{Answer, Question};
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
    let settings =
        Arc::new(Settings { policy: policy.clone(), effort: RwLock::new(rook.config.agent.effort()) });
    let servers = rook_core::agent::servers_for(&rook);
    let mcp = Arc::new(rook.connect_mcp().await);
    // Filled in by `initialize`, and read when a turn starts: the client may
    // serve its unsaved buffers, and the protocol forbids asking if it cannot.
    let client_files: Arc<Mutex<ClientFiles>> = Default::default();

    let peer = Arc::new(Peer::new(outbound));
    let mut lines = reader.lines();
    let mut turn: Option<Turn> = None;

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
                if let Some(turn) = turn.take() {
                    turn.cancel(&peer);
                }
            }
            ("session/prompt", Some(id)) => {
                if let Some(turn) = turn.take() {
                    turn.cancel(&peer);
                }
                let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
                // Read out of the lock before the spawn: a guard living to the
                // end of the statement would travel into the task with it.
                let effort = *settings.effort.read().unwrap();
                turn = Some(Turn {
                    id: id.clone(),
                    replied: replied.clone(),
                    handle: tokio::spawn(prompt(
                        rook.clone(),
                        peer.clone(),
                        Shared { policy: policy.clone(), servers: servers.clone(), mcp: mcp.clone() },
                        TurnSetup { client: *client_files.lock().await, effort },
                        id,
                        params,
                        replied,
                    )),
                });
            }
            (_, Some(id)) => {
                if method == "initialize" {
                    *client_files.lock().await = ClientFiles::from_initialize(&params);
                }
                let outcome = dispatch(&rook, &settings, &method, params);
                peer.respond(&id, outcome);
            }
            // A notification we do not handle is not an error.
            (_, None) => tracing::debug!("ignoring notification {method}"),
        }
    }

    if let Some(turn) = turn {
        turn.handle.abort();
    }
    mcp.shutdown().await;
    servers.shutdown().await;
    drop(peer);
    let _ = writer.await;
    Ok(())
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
        protocol::config_options(self.policy.mode(), *self.effort.read().unwrap())
    }

    fn set(&self, id: &str, value: &str) -> Result<(), Error> {
        match id {
            "mode" => {
                let mode = protocol::mode_from_id(value)
                    .ok_or_else(|| Error::invalid_params(format!("no mode {value:?}")))?;
                self.policy.set_mode(mode);
            }
            "effort" => {
                let effort = rook_llm::Effort::parse(value)
                    .ok_or_else(|| Error::invalid_params(format!("no effort {value:?}")))?;
                *self.effort.write().unwrap() = effort;
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
        "session/set_mode" => {
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
            Ok(serde_json::json!({
                "sessionId": rook_store::format_session_id(session),
                "modes": protocol::modes(settings.policy.mode()),
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
                    "modes": protocol::modes(settings.policy.mode()),
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
}

async fn prompt(
    rook: Arc<Rook>,
    peer: Arc<Peer>,
    shared: Shared,
    setup: TurnSetup,
    id: serde_json::Value,
    params: serde_json::Value,
    replied: Arc<std::sync::atomic::AtomicBool>,
) {
    let answer = |outcome| {
        if !replied.swap(true, std::sync::atomic::Ordering::SeqCst) {
            peer.respond(&id, outcome);
        }
    };
    let request: protocol::Prompt = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(e) => return answer(Err(Error::invalid_params(e.to_string()))),
    };
    let Some(session) = rook_store::parse_session_id(&request.session_id) else {
        return answer(Err(Error::invalid_params("not a session id")));
    };

    let text = request.prompt.iter().filter_map(ContentBlock::render).collect::<Vec<_>>().join("\n");
    if text.trim().is_empty() {
        return answer(Err(Error::invalid_params("the prompt has no text")));
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
    let editor = Arc::new(EditorApprover { peer: peer.clone(), session: request.session_id.clone() });
    agent.approver = editor.clone();
    agent.ask_via(editor);
    agent.effort = setup.effort;
    if setup.client.read {
        agent.tool_ctx.files = Some(Arc::new(EditorFiles {
            peer: peer.clone(),
            session: request.session_id.clone(),
            can_write: setup.client.write,
        }));
    }
    if setup.client.terminal {
        agent.tool_ctx.terminals = Some(Arc::new(EditorTerminals {
            peer: peer.clone(),
            session: request.session_id.clone(),
            timeout: agent.tool_ctx.command_timeout,
        }));
    }

    // Ids are handed out in call order and consumed in the same order, which is
    // how a completion is matched to the call it finishes: the loop dispatches
    // a step's calls in the order the model asked for them.
    let started = AtomicU64::new(0);
    let finished = AtomicU64::new(0);
    let result = agent
        .run_with(&text, |progress| {
            let update = match progress {
                Progress::Delta(Delta::Text(text)) => {
                    protocol::agent_message_chunk(&request.session_id, text)
                }
                Progress::Delta(Delta::Reasoning(text)) => {
                    protocol::agent_thought_chunk(&request.session_id, text)
                }
                Progress::Delta(Delta::ToolCall(call)) => protocol::tool_call(
                    &request.session_id,
                    &format!("call_{}", started.fetch_add(1, Ordering::Relaxed)),
                    &call.name,
                    protocol::tool_kind(&call.name),
                ),
                // The editor already has a tool call open for the delegation;
                // this is progress within it, which reads as a thought.
                Progress::Delegated { task, done, total } => {
                    protocol::agent_thought_chunk(&request.session_id, &format!("[{done}/{total}] {task}\n"))
                }
                Progress::ToolDone { failed, .. } => protocol::tool_call_done(
                    &request.session_id,
                    &format!("call_{}", finished.fetch_add(1, Ordering::Relaxed)),
                    failed,
                ),
                Progress::Delta(Delta::Done { .. }) => return,
            };
            peer.notify("session/update", update);
        })
        .await;

    answer(match result {
        Ok(outcome) => Ok(serde_json::json!({ "stopReason": stop_reason(&outcome.stopped) })),
        Err(e) => Err(Error::internal(e.to_string())),
    });
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
            .request("session/request_permission", params)
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
    peer: Arc<Peer>,
    session: String,
    can_write: bool,
}

#[async_trait]
impl rook_tools::Files for EditorFiles {
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
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| rook_tools::ToolError::Io { path: parent.to_path_buf(), source: e })?;
            }
            return tokio::fs::write(path, contents)
                .await
                .map_err(|e| rook_tools::ToolError::Io { path: path.to_path_buf(), source: e });
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
#[derive(Clone, Copy)]
struct TurnSetup {
    client: ClientFiles,
    effort: rook_llm::Effort,
}

/// What the client said it can do, from the `initialize` request.
#[derive(Clone, Copy, Default)]
struct ClientFiles {
    read: bool,
    write: bool,
    terminal: bool,
}

impl ClientFiles {
    fn from_initialize(params: &serde_json::Value) -> Self {
        let capabilities = &params["clientCapabilities"];
        let fs = &capabilities["fs"];
        Self {
            read: fs["readTextFile"] == true,
            write: fs["writeTextFile"] == true,
            terminal: capabilities["terminal"] == true,
        }
    }
}

/// The editor's terminal, so a build the agent starts is one the user can watch.
///
/// Four calls per command — create, wait, read, release — because the protocol
/// separates them. The release is not optional: the client holds the terminal
/// open until the agent says it is done with it.
struct EditorTerminals {
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
