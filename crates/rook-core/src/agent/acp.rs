//! ACP subprocess turns use the ordinary child admission, journal and tool dispatch.
use super::delegation::Crew;
use super::*;
use crate::acp_executor::Client;
use serde_json::{Value, json};

fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}
impl AgentLoop<'_> {
    pub(super) async fn run_acp(
        &mut self,
        prompt: &str,
        progress: &mut impl FnMut(Progress<'_>),
    ) -> Result<TurnOutcome> {
        let mut timing =
            crate::diagnostics::Timer::start(self.rook, self.session, crate::diagnostics::Phase::AcpRequest);
        self.tool_ctx.trace = timing.trace();
        self.tool_ctx.propagate_trace = self.rook.config.telemetry.propagate_trace;
        let result = self.run_acp_inner(prompt, progress).await;
        timing.finish(
            if result.is_ok() {
                crate::diagnostics::Status::Completed
            } else {
                crate::diagnostics::Status::Failed
            },
            None,
        );
        result
    }

    async fn run_acp_inner(
        &mut self,
        prompt: &str,
        progress: &mut impl FnMut(Progress<'_>),
    ) -> Result<TurnOutcome> {
        let name = self.external_executor.clone().ok_or_else(|| bad("ACP executor was not selected"))?;
        if self.rook.config.sandbox.network_policy.enabled {
            return Err(bad("external ACP execution cannot enforce the configured domain network policy"));
        }
        let cfg = &self.rook.config.acp_executor;
        let profile = cfg.profiles.get(&name).ok_or_else(|| bad("Unknown configured ACP executor"))?;
        if !profile.trusted {
            return Err(bad("ACP executor requires trusted = true in operator configuration"));
        }
        if !self.turn_options().attachments.is_empty() {
            return Err(bad("ACP subprocess delegation currently accepts text only"));
        }
        self.begin_turn(prompt, progress).await?;
        self.approver = std::sync::Arc::new(super::acp_tools::Approver {
            inner: self.approver.clone(),
            vault: self.vault.clone(),
        });
        self.init_tool_cycles()?;
        // An external errand owns its jobs for its entire single child turn.
        // Its cleanup must never stop a job retained by the parent frontend.
        self.tools.register(std::sync::Arc::new(rook_tools::jobs::JobTool));
        if let Some(parent) = self.tool_ctx.secrets.clone() {
            self.tool_ctx.secrets =
                Some(std::sync::Arc::new(super::acp_tools::Secrets { parent, vault: self.vault.clone() }));
        }
        self.tool_ctx.jobs = Some(std::sync::Arc::new(rook_tools::jobs::Jobs::scoped(
            self.rook.config.sandbox.max_background_jobs,
            self.rook.config.sandbox.max_output_bytes,
            self.session,
            self.tool_ctx.secrets.clone().unwrap_or_else(|| self.vault.clone()),
        )));
        self.tools.register(std::sync::Arc::new(super::acp_tools::Read {
            file_bytes: cfg.file_bytes,
            result_bytes: cfg.frame_bytes,
            vault: self.vault.clone(),
        }));
        if let Some(reason) = self.gate_risk("acp_executor", &json!({"executor":name}),
            rook_tools::policy::Risk::External { name: format!("ACP executor {name}"), claims_read_only: false },
            effects::Shown::Text("Run the configured external agent. It can use its own tools independently of Rook; client callbacks use Rook permissions and checkpoints.")).await {
            return Err(bad(reason));
        }
        if self
            .composite_cost_unknown
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(bad(
                "An external/delegated agent has unverified model usage; inspect that child before starting another family request",
            ));
        }
        let missing = rook_llm::Response {
            message: Message::assistant(""),
            stop_reason: rook_llm::StopReason::EndTurn,
            usage: Default::default(),
            model: String::new(),
        };
        let receipt =
            crate::model_route::Receipt::new("external ACP", "ordinary", None, &missing, &self.vault);
        let bytes = crate::persistence::encode_with_limit(&receipt, crate::model_route::MAX_BYTES)?;
        self.rook.log(
            self.session,
            EventKind::Note,
            crate::model_route::LABEL,
            std::str::from_utf8(&bytes).map_err(|_| bad("ACP cost receipt encoding failed"))?,
        )?;
        self.composite_cost_unknown.store(true, std::sync::atomic::Ordering::Release);
        let receipt = self.record_background("harness:ACP executor", &name)?;
        let mut env = std::collections::BTreeMap::new();
        let mut env_bytes = 0usize;
        for (name, given) in &profile.env {
            if given.len() > 16384 || name.len() > 256 || profile.env.len() > 128 {
                return Err(bad(
                    "Declared ACP environment exceeds its entry/name/value bound; no process was started",
                ));
            }
            let value = if let Some(alias) = given.strip_prefix("secret:") {
                rook_tools::Secrets::value(self.vault.as_ref(), alias).ok_or_else(|| {
                    bad(format!("ACP environment {name} needs an available rook secrets alias"))
                })?
            } else {
                given.clone()
            };
            if value.len() > 16384
                || value.contains('\0')
                || name.len() + value.len() > 65536usize.saturating_sub(env_bytes)
            {
                return Err(bad(
                    "Resolved ACP environment exceeds its value/64 KiB total bound; no process was started",
                ));
            }
            env_bytes += name.len() + value.len();
            let lower = name.to_ascii_lowercase();
            if given.starts_with("secret:")
                || [
                    "token",
                    "secret",
                    "password",
                    "authorization",
                    "credential",
                    "cookie",
                    "api_key",
                    "access_key",
                ]
                .iter()
                .any(|word| lower.contains(word))
            {
                self.vault.also_hide(&value);
            }
            env.insert(name.clone(), value);
        }
        let mut client = Client::start(profile, cfg, &self.rook.workspace, &env)?;
        let result = if let Some(by) = self.by {
            tokio::time::timeout_at(tokio::time::Instant::from_std(by), self.acp_conversation(&mut client, prompt, progress)).await
                .map_err(|_| bad("Parent deadline reached during external ACP execution; inspect the retained child session"))
                .and_then(|result|result)
        } else {
            self.acp_conversation(&mut client, prompt, progress).await
        };
        client.close().await;
        let diagnostic = self.vault.redact(&client.diagnostic());
        if !diagnostic.is_empty() {
            self.rook.log(self.session, EventKind::Note, "ACP stderr", &diagnostic)?;
        }
        let result = result.map_err(|error| {
            if diagnostic.is_empty() {
                error
            } else {
                bad(format!("{error}\nExternal ACP stderr: {diagnostic}"))
            }
        });
        // A lost response never becomes permission to repeat an external operation.
        if result.is_ok() {
            receipt.finish("External ACP prompt returned; completion is the external agent's report")?;
        }
        result
    }
    async fn acp_conversation(
        &self,
        client: &mut Client<'_>,
        prompt: &str,
        progress: &mut impl FnMut(Progress<'_>),
    ) -> Result<TurnOutcome> {
        let initialize = client
            .handshake(
                "initialize",
                json!({"protocolVersion":1,
            "clientInfo":{"name":"rook","version":env!("CARGO_PKG_VERSION")},
            "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":self.tool_ctx.terminals.is_none()}}),
            )
            .await?;
        if initialize.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err(bad("ACP external agent does not support protocol version 1"));
        }
        let setup =
            client.handshake("session/new", json!({"cwd":self.rook.workspace,"mcpServers":[]})).await?;
        let session = setup
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(|| bad("ACP external agent returned an invalid session ID"))?
            .to_owned();
        self.rook.log(
            self.session,
            EventKind::Note,
            "ACP session",
            &self.vault.redact(&json!({"executor":self.external_executor,"sessionId":session}).to_string()),
        )?;
        let crew = self.crew(0);
        let (mut nursery, _) = Nursery::new(
            self.rook.config.agent.max_parallel_subagents,
            self.rook.config.agent.delegation_progress_entries,
        );
        let mut outcome = TurnOutcome {
            steps: 1,
            stopped: "external_report".into(),
            reply: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            tools_called: Vec::new(),
            skills_loaded: Vec::new(),
            skills_written: Vec::new(),
            facts_learned: Vec::new(),
            facts_forgotten: Vec::new(),
            files_changed: Vec::new(),
            delegated: Vec::new(),
            compactions: 0,
            decisions: Vec::new(),
            open_questions: Vec::new(),
        };
        let transmitted = {
            #[derive(serde::Serialize)]
            struct References<'a> {
                authority: &'static str,
                delegated: Option<&'a str>,
                prompt_hook: Option<&'a str>,
                session_hook: Option<&'a str>,
            }
            #[derive(serde::Serialize)]
            struct Prompt<'a> {
                task: &'a str,
                references: References<'a>,
            }
            let session_context = self.session_context.lock().unwrap_or_else(|e| e.into_inner());
            let hook = session_context.as_deref().filter(|s| !s.is_empty());
            let frame_bytes = self.rook.config.acp_executor.frame_bytes;
            if self.external_context.is_none() && self.prompt_context.is_none() && hook.is_none() {
                if prompt.len() > frame_bytes {
                    return Err(bad("ACP task exceeds acp_executor.frame_bytes"));
                }
                prompt.to_owned()
            } else {
                let input = Prompt {
                    task: prompt,
                    references: References {
                        authority: "data",
                        delegated: self.external_context.as_deref(),
                        prompt_hook: self.prompt_context.as_deref(),
                        session_hook: hook,
                    },
                };
                let bytes = crate::persistence::encode_with_limit(&input, frame_bytes)?;
                String::from_utf8(bytes).map_err(|_| bad("ACP prompt serialization is not UTF-8"))?
            }
        };
        let mut params = json!({"sessionId":session,"prompt":[{"type":"text","text":transmitted}]});
        if self.tool_ctx.propagate_trace
            && let Some(parent) = self.tool_ctx.trace.as_ref().and_then(rook_llm::TraceContext::traceparent)
        {
            params["_meta"] = json!({"traceparent":parent});
        }
        let request = client.request("session/prompt", params).await?;
        let mut text_bytes = 0usize;
        let mut callbacks = 0u32;
        let mut callback_ids = std::collections::HashSet::new();
        let mut terminals = super::acp_tools::Terminals {
            jobs: self.tool_ctx.jobs.clone().ok_or_else(|| bad("ACP job registry was not prepared"))?,
            active: Default::default(),
            owned: Vec::new(),
            secret_bytes: 0,
            pending: &self.launched_job,
        };
        // Context-size updates are not billing receipts. No native-model prices or
        // zero-cost estimates are assigned to an independently running agent.
        self.composite_cost_unknown.store(true, std::sync::atomic::Ordering::Release);
        self.rook.log(
            self.session,
            EventKind::Note,
            "ACP cost",
            "External ACP model usage/cost is not verified; no further family model allowance is assumed",
        )?;
        loop {
            let received = if let Some(by) = self.by {
                tokio::time::timeout_at(tokio::time::Instant::from_std(by), client.receive()).await
                    .map_err(|_| bad("Parent deadline reached while waiting for ACP; inspect the external child session"))??
            } else {
                client.receive().await?
            };
            if received.get("method").is_none() {
                if received.get("id").and_then(Value::as_u64) != Some(request) {
                    return Err(bad("ACP returned a stale or unknown response ID"));
                }
                if received.get("error").is_some() {
                    return Err(bad("ACP prompt failed; inspect its retained child history before retrying"));
                }
                let stop = received
                    .pointer("/result/stopReason")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("ACP prompt result has no stop reason"))?;
                outcome.open_questions.push(format!("External agent reported {stop}; model usage/cost and task completion require independent inspection"));
                self.rook.log(self.session, EventKind::Note, "ACP stop", stop)?;
                self.rook.log(self.session, EventKind::AssistantMessage, "ACP response", &outcome.reply)?;
                return Ok(outcome);
            }
            let method = received.get("method").and_then(Value::as_str).unwrap_or("");
            let params = received.get("params").unwrap_or(&Value::Null);
            if params.get("sessionId").and_then(Value::as_str) != Some(session.as_str()) {
                return Err(bad("ACP message belongs to another session"));
            }
            if method == "session/update" && received.get("id").is_none() {
                let update = params.get("update").ok_or_else(|| bad("ACP session update is missing"))?;
                let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
                if matches!(kind, "agent_message_chunk" | "agent_thought_chunk") {
                    let content =
                        update.get("content").ok_or_else(|| bad("ACP message content is missing"))?;
                    if content.get("type").and_then(Value::as_str) != Some("text") {
                        return Err(bad("ACP external agent returned unsupported non-text content"));
                    }
                    let raw = content
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad("ACP text content is invalid"))?;
                    text_bytes = text_bytes
                        .checked_add(raw.len())
                        .ok_or_else(|| bad("ACP output byte count overflow"))?;
                    if text_bytes > self.rook.config.acp_executor.result_bytes {
                        return Err(bad(
                            "ACP output exceeds acp_executor.result_bytes; inspect retained child history",
                        ));
                    }
                    // Retain/redact complete fragments before they reach any frontend.
                    let text = self.vault.redact(raw);
                    if kind == "agent_message_chunk" {
                        self.rook.log(self.session, EventKind::Note, "ACP message fragment", &text)?;
                        outcome.reply.push_str(&text);
                        progress(Progress::Delta(&Delta::Text(text)));
                    } else {
                        self.rook.log(self.session, EventKind::Reasoning, "ACP thought", &text)?;
                        progress(Progress::Delta(&Delta::Reasoning(text)));
                    }
                } else {
                    // Foreign tool notifications describe work already executed by the
                    // external agent. They must never be dispatched a second time.
                    let body = self.vault.redact(&update.to_string());
                    self.rook.log(self.session, EventKind::Note, "ACP observation", &body)?;
                    progress(Progress::Working { call: "ACP", said: "External agent reported progress" });
                }
                continue;
            }
            let id = received.get("id").cloned().ok_or_else(|| bad("Unsupported ACP notification"))?;
            if !(id.as_u64().is_some() || id.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 256)) {
                return Err(bad("ACP callback ID must be a bounded string or nonnegative integer"));
            }
            callbacks = callbacks.saturating_add(1);
            if callbacks > self.max_steps {
                return Err(bad("ACP callback ceiling reached; raise agent.max_steps or use a smaller task"));
            }
            if !callback_ids.insert(id.to_string()) {
                return Err(bad("ACP callback ID was already completed; no operation is executed twice"));
            }
            let response = match method {
                "fs/read_text_file" => {
                    let call = rook_llm::ToolCall {
                        id: format!("acp-{callbacks}"),
                        name: "acp_read_text".into(),
                        arguments: params.clone(),
                    };
                    progress(Progress::Delta(&Delta::ToolCall(call.clone())));
                    let (result, failed, seq) =
                        self.dispatch_recorded(&call, &mut outcome, progress, &crew, &mut nursery).await?;
                    outcome.tools_called.push(call.name.clone());
                    progress(Progress::ToolDone { name: &call.name, failed, result_seq: seq });
                    if failed {
                        json!({"error":{"code":-32000,"message":self.vault.redact(&result)}})
                    } else {
                        // Post-tool reports remain in history, outside the actual file body.
                        let mut decoder = serde_json::Deserializer::from_str(&result).into_iter::<Value>();
                        let content = decoder
                            .next()
                            .and_then(std::result::Result::ok)
                            .and_then(|v| v.get("content").cloned())
                            .filter(Value::is_string)
                            .ok_or_else(|| bad("Native ACP read result has no raw text content"))?;
                        json!({"result":{"content":content}})
                    }
                }
                "fs/write_text_file" => {
                    let path = params
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad("ACP write path is missing"))?;
                    let content = params
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad("ACP write content is missing"))?;
                    let call = rook_llm::ToolCall {
                        id: format!("acp-{callbacks}"),
                        name: "write_file".into(),
                        arguments: json!({"path":path,"content":content}),
                    };
                    progress(Progress::Delta(&Delta::ToolCall(call.clone())));
                    let (result, failed, seq) =
                        self.dispatch_recorded(&call, &mut outcome, progress, &crew, &mut nursery).await?;
                    outcome.tools_called.push(call.name.clone());
                    progress(Progress::ToolDone { name: &call.name, failed, result_seq: seq });
                    if failed {
                        json!({"error":{"code":-32000,"message":self.vault.redact(&result)}})
                    } else {
                        json!({"result":{}})
                    }
                }
                "terminal/create"
                | "terminal/output"
                | "terminal/wait_for_exit"
                | "terminal/kill"
                | "terminal/release" => {
                    if self.tool_ctx.terminals.is_some() {
                        json!({"error":{"code":-32601,"message":"This frontend owns terminal execution; native ACP terminal capability was not advertised"}})
                    } else {
                        match self
                            .acp_terminal(
                                method,
                                params,
                                callbacks,
                                &mut terminals,
                                &mut outcome,
                                progress,
                                &crew,
                                &mut nursery,
                            )
                            .await
                        {
                            Ok(value) => json!({"result":value}),
                            Err(error) => {
                                json!({"error":{"code":-32000,"message":self.vault.redact(&error.to_string())}})
                            }
                        }
                    }
                }
                "session/request_permission" => {
                    let tool =
                        params.get("toolCall").ok_or_else(|| bad("ACP permission lacks its tool call"))?;
                    if !tool
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty() && s.len() <= 256)
                    {
                        return Err(bad("ACP permission lacks a bounded tool call identity"));
                    }
                    let title = tool.get("title").and_then(Value::as_str).unwrap_or("External ACP operation");
                    let options = params
                        .get("options")
                        .and_then(Value::as_array)
                        .ok_or_else(|| bad("ACP permission options are missing"))?;
                    let mut option_ids = std::collections::HashSet::new();
                    if options.is_empty()
                        || options.len() > 16
                        || options.iter().any(|o| {
                            let Some(id) = o.get("optionId").and_then(Value::as_str) else { return true };
                            id.is_empty() || id.len() > 256 || !option_ids.insert(id)
                        })
                    {
                        return Err(bad(
                            "ACP permission options must have unique bounded identities; at most 16",
                        ));
                    }
                    let once = options
                        .iter()
                        .find(|o| o.get("kind").and_then(Value::as_str) == Some("allow_once"))
                        .and_then(|o| o.get("optionId"))
                        .and_then(Value::as_str);
                    let refused = self.gate_risk("acp_permission", tool,
                        rook_tools::policy::Risk::External {name:self.vault.redact(title),claims_read_only:false},
                        effects::Shown::Text("External agent permission request; selecting an option does not execute the operation again in Rook.")).await;
                    if refused.is_none()
                        && let Some(option) = once
                    {
                        json!({"result":{"outcome":{"outcome":"selected","optionId":option}}})
                    } else {
                        json!({"result":{"outcome":{"outcome":"cancelled"}}})
                    }
                }
                _ => {
                    json!({"error":{"code":-32601,"message":"ACP client method is not supported; consult initialized capabilities"}})
                }
            };
            let mut response = response;
            response["jsonrpc"] = json!("2.0");
            response["id"] = id;
            client.send(&response).await?;
        }
    }
}

impl<'a> AgentLoop<'a> {
    #[allow(clippy::too_many_arguments)]
    async fn acp_terminal<'f>(
        &self,
        method: &str,
        params: &Value,
        request: u32,
        terminals: &mut super::acp_tools::Terminals<'_>,
        outcome: &mut TurnOutcome,
        progress: &mut impl FnMut(Progress<'_>),
        crew: &'f Crew<'a>,
        nursery: &mut Nursery<'f>,
    ) -> Result<Value>
    where
        'a: 'f,
    {
        let maximum = self.tool_ctx.max_output_bytes.min(self.rook.config.acp_executor.frame_bytes / 8);
        let output_limit = match params.get("outputByteLimit") {
            None => maximum,
            Some(value) => value
                .as_u64()
                .map(|n| n.min(maximum as u64) as usize)
                .ok_or_else(|| bad("ACP outputByteLimit must be a nonnegative integer"))?,
        };
        let call = if method == "terminal/create" {
            if terminals.active.len() >= self.rook.config.sandbox.max_background_jobs {
                return Err(bad("ACP terminal limit reached; release a terminal before creating another"));
            }
            let command = super::acp_tools::command(params).map_err(|error| bad(error.to_string()))?;
            if let Some(env) = params.get("env").and_then(Value::as_array) {
                for entry in env {
                    let name = entry.get("name").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
                    if [
                        "secret",
                        "token",
                        "password",
                        "authorization",
                        "credential",
                        "cookie",
                        "api_key",
                        "access_key",
                    ]
                    .iter()
                    .any(|part| name.contains(part))
                        && let Some(value) = entry.get("value").and_then(Value::as_str)
                    {
                        if value.len()
                            > self.rook.config.acp_executor.frame_bytes.saturating_sub(terminals.secret_bytes)
                        {
                            return Err(bad(
                                "Cumulative ACP credential bytes exceed the frame budget; split the task or raise acp_executor.frame_bytes",
                            ));
                        }
                        terminals.secret_bytes += value.len();
                        self.vault.also_hide(value);
                    }
                }
            }
            let mut arguments = json!({"command":command,"background":true});
            if let Some(cwd) = params.get("cwd") {
                let cwd = cwd
                    .as_str()
                    .filter(|s| s.len() <= 4096 && std::path::Path::new(s).is_absolute())
                    .ok_or_else(|| bad("ACP terminal cwd must be a bounded absolute directory"))?;
                arguments["cwd"] = json!(cwd);
            }
            rook_llm::ToolCall { id: format!("acp-{request}"), name: "run_command".into(), arguments }
        } else {
            let id = params
                .get("terminalId")
                .and_then(Value::as_str)
                .filter(|id| terminals.active.contains_key(*id))
                .ok_or_else(|| bad("ACP terminal ID is foreign, released or unknown"))?;
            let mut arguments = json!({"id":id});
            if matches!(method, "terminal/kill" | "terminal/release") {
                arguments["stop"] = json!(true);
            }
            if method == "terminal/wait_for_exit" {
                arguments["wait_secs"] = json!(self.tool_ctx.command_timeout.as_secs().max(1));
            }
            rook_llm::ToolCall { id: format!("acp-{request}"), name: "job".into(), arguments }
        };
        let displayed = rook_llm::ToolCall {
            arguments: serde_json::from_str(&self.vault.redact(&call.arguments.to_string()))?,
            ..call.clone()
        };
        progress(Progress::Delta(&Delta::ToolCall(displayed)));
        let receipt = self.dispatch_receipt(&call, outcome, progress, crew, nursery).await?;
        outcome.tools_called.push(call.name.clone());
        progress(Progress::ToolDone { name: &call.name, failed: receipt.failed, result_seq: receipt.seq });
        if let Some(id) = &receipt.job {
            terminals.owned.push(id.clone());
        }
        if receipt.failed {
            return Err(bad(receipt.text));
        }
        if method == "terminal/create" {
            let id = receipt.job.ok_or_else(|| {
                bad("Native terminal creation has no owned job receipt; inspect execution recovery")
            })?;
            terminals.active.insert(id.clone(), output_limit);
            return Ok(json!({"terminalId":id}));
        }
        let id = call
            .arguments
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("ACP terminal callback lost its job identity"))?;
        if method == "terminal/release" {
            terminals.active.remove(id);
            terminals.jobs.release(id);
            return Ok(json!({}));
        }
        let job =
            terminals.jobs.get(id).ok_or_else(|| bad("ACP owned terminal receipt is no longer retained"))?;
        if method == "terminal/wait_for_exit" {
            if job.exit_code.is_none() {
                return Err(bad(
                    "Terminal is still running after the configured wait; inspect output, cancel or repeat only the wait",
                ));
            }
            return Ok(json!({"exitCode":job.exit_code.filter(|code|*code>=0),"signal":Value::Null}));
        }
        if method == "terminal/kill" {
            return Ok(json!({}));
        }
        let limit = *terminals.active.get(id).ok_or_else(|| bad("ACP terminal was released"))?;
        let omitted = job.output.len().saturating_sub(limit);
        let start =
            job.output.char_indices().map(|(at, _)| at).find(|at| *at >= omitted).unwrap_or(job.output.len());
        let raw = job.output.get(start..).unwrap_or("");
        let output = self.vault.redact(raw);
        let mut response =
            json!({"output":output,"truncated":omitted>0 || terminals.jobs.output_truncated(id)==Some(true)});
        if let Some(code) = job.exit_code {
            response["exitStatus"] = json!({"exitCode":(code>=0).then_some(code),"signal":Value::Null});
        }
        Ok(response)
    }
}
