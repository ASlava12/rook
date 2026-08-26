//! The agent loop.
//!
//! Deliberately small. Everything that varies — the model, the tools, the skills
//! — is behind a trait or a data structure, so the loop itself stays something a
//! person can read in one sitting and reason about.
//!
//! Two behaviours are built in rather than bolted on:
//!
//! * **Progressive disclosure.** The system prompt carries skill *cards* and tool
//!   *stubs*; bodies and full schemas arrive only when the model asks for them
//!   via `load_skill`. A library of a hundred skills costs a few hundred tokens
//!   a turn instead of tens of thousands.
//! * **Compaction before overflow.** The budget is checked before each request.
//!   An agent that discovers the limit by being rejected has already lost the
//!   turn, and usually the task with it.

use futures_util::StreamExt;
use rook_llm::{Assembler, Delta, Message, Provider, Request, Role, StopReason, ToolSpec};
use rook_store::EventKind;
use rook_tools::policy::{Approver, Decision, Policy, Unattended};
use rook_tools::{ToolBox, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::context::{ContextBudget, estimate_tokens};
use crate::error::{CoreError, Result};
use crate::hooks::{self, Hooks};
use crate::service::Rook;

/// Pseudo-tools: implemented by the loop rather than the toolbox, because they
/// need the agent's own state.
pub const LOAD_SKILL: &str = "load_skill";
pub const REMEMBER: &str = "remember";
pub const FORGET: &str = "forget";
pub const RECALL: &str = "recall";
pub const DELEGATE: &str = "delegate";

/// How deep delegation may nest. One level of sub-delegation is useful for
/// splitting a task; beyond that the token cost compounds faster than the work
/// gets done.
pub const MAX_DEPTH: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnOutcome {
    pub steps: u32,
    pub stopped: String,
    pub reply: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub tools_called: Vec<String>,
    pub skills_loaded: Vec<String>,
    pub facts_learned: Vec<String>,
    /// Sessions of sub-agents this turn ran, for reading their detail later.
    pub delegated: Vec<String>,
    pub compactions: u32,
}

pub struct AgentLoop<'a> {
    pub rook: &'a Rook,
    /// Shared rather than owned so a delegated child can reuse the connection
    /// instead of building a second HTTP client per sub-task.
    pub provider: std::sync::Arc<dyn Provider>,
    pub tools: ToolBox,
    pub tool_ctx: ToolContext,
    pub session: u128,
    pub policy: std::sync::Arc<Policy>,
    pub hooks: std::sync::Arc<Hooks>,
    /// What the `session_start` hooks contributed, computed once.
    session_context: std::sync::Mutex<Option<String>>,
    /// Consulted whenever the policy says to ask. Refuses by default, so an
    /// unattended run cannot silently do something nobody reviewed.
    pub approver: std::sync::Arc<dyn Approver>,
    pub depth: u32,
    pub max_steps: u32,
    budget: ContextBudget,
}

impl<'a> AgentLoop<'a> {
    pub fn new(rook: &'a Rook, provider: std::sync::Arc<dyn Provider>, session: u128) -> Self {
        let mut tool_ctx = ToolContext::new(rook.workspace.clone());
        tool_ctx.max_output_bytes = rook.config.sandbox.max_output_bytes;
        tool_ctx.command_timeout = std::time::Duration::from_secs(rook.config.sandbox.command_timeout_secs);

        let (hooks, bad_hooks) = Hooks::compile(&rook.config.hooks);
        for error in bad_hooks {
            tracing::warn!("ignoring unusable hook matcher: {error}");
        }

        let sandbox = &rook.config.sandbox;
        let (policy, bad_rules) = Policy::compile(sandbox.mode, &sandbox.allow, &sandbox.ask, &sandbox.deny);
        for error in bad_rules {
            tracing::warn!("ignoring unusable sandbox rule: {error}");
        }
        let budget = ContextBudget::new(provider.context_window(), rook.config.agent.compact_at);
        Self {
            rook,
            provider,
            tools: ToolBox::standard(),
            tool_ctx,
            session,
            policy: std::sync::Arc::new(policy),
            hooks: std::sync::Arc::new(hooks),
            session_context: std::sync::Mutex::new(None),
            approver: std::sync::Arc::new(Unattended),
            depth: 0,
            max_steps: rook.config.agent.max_steps,
            budget,
        }
    }

    /// The system prompt: identity, environment, and the skill catalog.
    ///
    /// The environment block matters more than it looks. A model told it is on
    /// FreeBSD with BSD userland stops reaching for `sed -i` with a GNU argument
    /// order, which is the single most common cross-platform failure in agent
    /// transcripts.
    pub fn system_prompt(&self, prompt: &str) -> String {
        let env = &self.rook.env;
        let mut s = String::new();
        s.push_str(
            "You are Rook, an autonomous agent working in a local workspace.\n\
             Work in small verified steps. Prefer reading before editing. State what you did.\n\n",
        );
        s.push_str(&format!(
            "## Environment\nos: {} ({} userland)\narch: {}\nworkspace: {}\n",
            env.os,
            env.userland,
            env.arch,
            self.rook.workspace.display()
        ));
        if !env.languages.is_empty() {
            let langs: Vec<String> = env.languages.iter().map(|(k, v)| format!("{k} {v}")).collect();
            s.push_str(&format!("toolchains: {}\n", langs.join(", ")));
        }
        if !env.tools.is_empty() {
            let tools: Vec<String> = env.tools.iter().map(|(k, v)| format!("{k} {v}")).collect();
            s.push_str(&format!("tools: {}\n", tools.join(", ")));
        }

        let facts = if self.rook.config.memory.enabled {
            self.rook.recall(prompt, self.rook.config.memory.context_budget_tokens).unwrap_or_default()
        } else {
            Vec::new()
        };
        if !facts.is_empty() {
            s.push_str(
                "\n## Memory\nThings you were told to remember. \
                 Correct one with `forget` when it turns out to be wrong.\n",
            );
            for fact in facts {
                s.push_str(&format!("- [{}] {}\n", fact.id, fact.text));
            }
        }

        if let Ok(extra) = self.session_context.lock()
            && let Some(text) = extra.as_deref()
        {
            s.push_str(&format!("\n## From this workspace\n{text}\n"));
        }

        let cards = self.rook.catalog();
        let applicable: Vec<_> = cards.iter().filter(|c| c.applicable).collect();
        if !applicable.is_empty() {
            s.push_str(&format!(
                "\n## Skills\nCall `{LOAD_SKILL}` with a name to read its instructions before using it.\n"
            ));
            for c in applicable {
                s.push_str(&format!("- {} ({}): {}\n", c.name, c.version, c.description));
            }
        }
        s
    }

    /// Rebuild the conversation from the session log, starting after the most
    /// recent compaction.
    ///
    /// A compaction is a durable checkpoint, not a per-turn trim: once the log
    /// records one, every later turn — and every later process — starts from its
    /// summary instead of replaying the span again.
    ///
    /// The log is the only record of a turn; without replaying it every call
    /// would start from nothing, and `--session` would continue a session in
    /// name only. Tool calls and their results are paired by adjacency, which
    /// holds because the loop logs each call immediately before its result.
    fn history(&self) -> Result<Vec<Message>> {
        let (from_seq, summary) = self.rook.last_compaction(self.session)?;
        let events = self.rook.store.events(self.session, from_seq, usize::MAX)?;
        let mut messages = Vec::with_capacity(events.len() + 1);
        if let Some(summary) = summary {
            messages.push(Message::user(format!(
                "[Summary of earlier work in this session, which has been compacted out of \
                 context. The full transcript is still in the session log.]\n\n{summary}"
            )));
        }
        let mut open_call: Option<String> = None;

        for event in events {
            let body = match self.rook.store.get(&event.record.body) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => continue,
            };
            match event.record.kind {
                EventKind::UserMessage => messages.push(Message::user(body)),
                EventKind::AssistantMessage => messages.push(Message::assistant(body)),
                EventKind::SkillLoaded => {
                    messages.push(Message::user(format!("[skill {} loaded]\n{body}", event.record.label)))
                }
                EventKind::ToolCall => {
                    let id = format!("call_{}", event.seq);
                    messages.push(Message {
                        role: Role::Assistant,
                        content: String::new(),
                        tool_calls: vec![rook_llm::ToolCall {
                            id: id.clone(),
                            name: event.record.label.clone(),
                            arguments: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
                        }],
                        tool_call_id: None,
                    });
                    open_call = Some(id);
                }
                EventKind::ToolResult => {
                    // A result with no preceding call would make the message
                    // list invalid for the provider, so drop it rather than
                    // send something that will be rejected.
                    if let Some(id) = open_call.take() {
                        messages.push(Message::tool_result(id, body));
                    }
                }
                _ => {}
            }
        }
        Ok(messages)
    }

    fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs =
            if self.rook.config.agent.lazy_tools { self.tools.stubs() } else { self.tools.specs() };
        specs.push(ToolSpec {
            name: LOAD_SKILL.into(),
            description: "Load a skill's full instructions into context by name.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        });
        if self.rook.config.memory.enabled {
            specs.push(ToolSpec {
                name: REMEMBER.into(),
                description: "Remember something for future sessions. Use it for durable facts                               — preferences, conventions, decisions — not for what is already in                               this conversation."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "One self-contained fact." },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "scope": { "type": "string", "enum": ["global", "project"], "default": "project" },
                        "pinned": { "type": "boolean", "description": "Always keep in context." }
                    },
                    "required": ["text"]
                }),
            });
            specs.push(ToolSpec {
                name: FORGET.into(),
                description: "Drop a remembered fact by its id, once it is wrong or stale.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            });
            specs.push(ToolSpec {
                name: RECALL.into(),
                description: "Search memory for facts beyond the ones already in context.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            });
        }
        if self.depth < MAX_DEPTH {
            specs.push(ToolSpec {
                name: DELEGATE.into(),
                description: "Hand a self-contained sub-task to a fresh agent and get back only                               its conclusion. Use it when a step would otherwise fill this                               conversation with detail you do not need to keep — a wide search,                               a long file survey, an independent verification."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "The whole assignment. The sub-agent cannot see this conversation."
                        },
                        "context": {
                            "type": "string",
                            "enum": ["none", "recent"],
                            "default": "none",
                            "description": "`recent` also gives it the last few exchanges."
                        },
                        "max_steps": { "type": "integer" }
                    },
                    "required": ["task"]
                }),
            });
        }
        specs
    }

    /// Run one user turn to completion.
    pub async fn run(&mut self, prompt: &str) -> Result<TurnOutcome> {
        self.run_with(prompt, |_| {}).await
    }

    /// Run a turn, reporting each fragment as it arrives.
    ///
    /// `on_delta` sees text as the model produces it and tool calls once they
    /// are complete; the turn's bookkeeping is unaffected by whether anyone is
    /// watching.
    pub async fn run_with<F: FnMut(&Delta)>(&mut self, prompt: &str, mut on_delta: F) -> Result<TurnOutcome> {
        self.run_session_hooks().await;
        let gate = self
            .hooks
            .run(hooks::Event::Prompt, prompt, &self.payload(serde_json::json!({ "prompt": prompt })))
            .await;
        if let Some(rook_tools::policy::Decision::Deny(why)) = gate.decision {
            return Err(CoreError::Other(format!("the turn was refused before it began: {why}")));
        }

        self.rook.log(self.session, EventKind::UserMessage, "", prompt)?;
        if let Some(context) = gate.context() {
            self.rook.log(self.session, EventKind::Note, "hook", &context)?;
        }

        // The prompt was just logged, so replaying the session already ends
        // with it: the log is the only source of truth for what was said.
        let mut messages = vec![Message::system(self.system_prompt(prompt))];
        messages.extend(self.history()?);
        let mut outcome = TurnOutcome {
            steps: 0,
            stopped: "end_turn".into(),
            reply: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            tools_called: Vec::new(),
            skills_loaded: Vec::new(),
            facts_learned: Vec::new(),
            delegated: Vec::new(),
            compactions: 0,
        };

        while outcome.steps < self.max_steps {
            outcome.steps += 1;

            if self.budget.needs_compaction(measure(&messages)) {
                outcome.compactions += 1;
                self.compact().await;
                messages = vec![Message::system(self.system_prompt(prompt))];
                messages.extend(self.history()?);
            }

            let mut request = Request::new(messages.clone());
            request.tools = self.tool_specs();
            let mut stream =
                self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
            let mut assembler = Assembler::default();
            while let Some(delta) = stream.next().await {
                let delta = delta.map_err(|e| CoreError::Other(e.to_string()))?;
                on_delta(&delta);
                assembler.push(delta);
            }
            if !assembler.reasoning().is_empty() {
                self.rook.log(self.session, EventKind::Reasoning, "", assembler.reasoning()).ok();
            }
            let response = assembler.finish();

            outcome.input_tokens += response.usage.input_tokens;
            outcome.output_tokens += response.usage.output_tokens;

            if !response.message.content.is_empty() {
                self.rook.store.append_event(
                    self.session,
                    rook_store::NewEvent::new(
                        EventKind::AssistantMessage,
                        rook_store::Kind::Message,
                        response.message.content.as_bytes(),
                    )
                    .label(&response.model)
                    .usage(response.usage.input_tokens, response.usage.output_tokens),
                )?;
                outcome.reply = response.message.content.clone();
            }

            if response.stop_reason != StopReason::ToolUse || response.message.tool_calls.is_empty() {
                outcome.stopped = format!("{:?}", response.stop_reason);
                self.finish(&outcome).await;
                return Ok(outcome);
            }

            messages.push(response.message.clone());
            for call in &response.message.tool_calls {
                let result = self.dispatch(call, &mut outcome).await;
                messages.push(Message::tool_result(&call.id, result));
            }
        }

        outcome.stopped = "max_steps".into();
        self.finish(&outcome).await;
        Ok(outcome)
    }

    async fn dispatch(&self, call: &rook_llm::ToolCall, outcome: &mut TurnOutcome) -> String {
        self.rook.log(self.session, EventKind::ToolCall, &call.name, &call.arguments.to_string()).ok();

        outcome.tools_called.push(call.name.clone());

        if call.name == DELEGATE {
            let text = self.delegate(&call.arguments, outcome).await;
            self.rook.log(self.session, EventKind::ToolResult, DELEGATE, &text).ok();
            return text;
        }

        match call.name.as_str() {
            REMEMBER | FORGET | RECALL => {
                let text = self.memory_tool(&call.name, &call.arguments, outcome);
                self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
                return text;
            }
            _ => {}
        }

        if call.name == LOAD_SKILL {
            let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            return match self.rook.skills.resolve(name, &self.rook.env) {
                Ok(resolved) => {
                    outcome.skills_loaded.push(resolved.skill.id());
                    self.rook
                        .log(self.session, EventKind::SkillLoaded, &resolved.skill.id(), &resolved.body)
                        .ok();
                    resolved.body
                }
                // The reason matters: "needs docker >=27" is actionable, "not
                // found" sends the model looking for a typo that is not there.
                // It is logged as well as returned: a skill that never loaded is
                // otherwise invisible when reading the transcript afterwards.
                Err(e) => {
                    let message = format!("could not load skill {name:?}: {e}");
                    self.rook.log(self.session, EventKind::Error, LOAD_SKILL, &message).ok();
                    message
                }
            };
        }

        if let Some(refusal) = self.gate(call).await {
            self.rook.log(self.session, EventKind::ToolResult, &call.name, &refusal).ok();
            return refusal;
        }

        self.checkpoint_before(call);
        let result = self.tools.call(&self.tool_ctx, &call.name, &call.arguments).await;
        let text = match result {
            Ok(o) => o.content,
            Err(e) => format!("tool error: {e}"),
        };
        let text = match self.after_tool(call, &text).await {
            Some(extra) => format!("{text}\n\n{extra}"),
            None => text,
        };
        self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
        text
    }

    /// `post_tool` hooks, whose output the model sees appended to the result.
    async fn after_tool(&self, call: &rook_llm::ToolCall, result: &str) -> Option<String> {
        if self.hooks.is_empty() {
            return None;
        }
        let payload = self.payload(serde_json::json!({
            "tool": call.name,
            "input": call.arguments,
            "result": result,
        }));
        self.hooks.run(hooks::Event::PostTool, &call.name, &payload).await.context()
    }

    /// Run a sub-task in its own session and return only what it concluded.
    ///
    /// The child's full transcript stays in the store, linked to this session by
    /// its parent, so the detail is recoverable without ever entering this
    /// conversation's context — which is the entire point.
    async fn delegate(&self, args: &serde_json::Value, outcome: &mut TurnOutcome) -> String {
        let task = args.get("task").and_then(|t| t.as_str()).unwrap_or("").trim();
        if task.is_empty() {
            return "delegate needs a task".into();
        }

        let child_session = match self.rook.fork_for_subtask(self.session, task) {
            Ok(id) => id,
            Err(e) => return format!("could not start a sub-agent: {e}"),
        };

        if args.get("context").and_then(|c| c.as_str()) == Some("recent")
            && let Ok(recent) = self.recent_exchanges(6)
        {
            self.rook.log(child_session, EventKind::Note, "inherited", &recent).ok();
        }

        let mut child = AgentLoop::new(self.rook, self.provider.clone(), child_session);
        child.depth = self.depth + 1;
        child.tools = self.tools.clone();
        child.tool_ctx = self.tool_ctx.clone();
        child.policy = self.policy.clone();
        child.approver = self.approver.clone();
        child.hooks = self.hooks.clone();
        if let Some(steps) = args.get("max_steps").and_then(|s| s.as_u64()) {
            child.max_steps = steps as u32;
        }

        // Boxed because this is `run` calling itself through a tool call.
        let result = Box::pin(child.run(task)).await;
        let id = rook_store::format_session_id(child_session);
        outcome.delegated.push(id.clone());

        match result {
            Ok(child_outcome) => {
                outcome.input_tokens += child_outcome.input_tokens;
                outcome.output_tokens += child_outcome.output_tokens;
                format!(
                    "sub-agent {id} finished after {} steps ({}):\n{}",
                    child_outcome.steps, child_outcome.stopped, child_outcome.reply
                )
            }
            Err(e) => format!("sub-agent {id} failed: {e}"),
        }
    }

    /// The last `count` exchanges as plain text, for a child asked to inherit
    /// context. Deliberately flattened: the child gets what was said, not a
    /// replayable tool-call history it cannot answer for.
    fn recent_exchanges(&self, count: usize) -> Result<String> {
        let entries = self.rook.transcript(self.session, 0, usize::MAX, 2000)?;
        let mut tail: Vec<String> = entries
            .iter()
            .rev()
            .filter(|e| e.kind == "user" || e.kind == "assistant")
            .take(count)
            .map(|e| format!("{}: {}", e.kind, e.body))
            .collect();
        tail.reverse();
        Ok(format!("Context from the conversation that delegated this:\n{}", tail.join("\n")))
    }

    fn memory_tool(&self, name: &str, args: &serde_json::Value, outcome: &mut TurnOutcome) -> String {
        let string = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            REMEMBER => {
                let text = string("text");
                if text.trim().is_empty() {
                    return "remember needs non-empty text".into();
                }
                let scope = match string("scope").as_str() {
                    "global" => crate::memory::Scope::Global,
                    _ => crate::memory::Scope::Project(self.rook.workspace.display().to_string()),
                };
                let tags = args
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let mut fact = crate::memory::Fact::new(text, scope).with_tags(tags).from_turn(
                    self.session,
                    self.rook.store.get_session(self.session).ok().flatten().map(|m| m.next_seq).unwrap_or(0),
                );
                fact.pinned = args.get("pinned").and_then(|p| p.as_bool()).unwrap_or(false);
                let id = fact.id.clone();
                match self.rook.remember(fact, Some(format!("learned in turn {}", outcome.steps))) {
                    Ok(true) => {
                        outcome.facts_learned.push(id.clone());
                        format!("remembered as [{id}]")
                    }
                    Ok(false) => format!("already remembered as [{id}]"),
                    Err(e) => format!("could not remember: {e}"),
                }
            }
            FORGET => match self.rook.forget(&string("id"), Some("forgotten by the agent".into())) {
                Ok(Some(fact)) => format!("forgot [{}] {}", fact.id, fact.text),
                Ok(None) => format!("no fact {:?} to forget", string("id")),
                Err(e) => format!("could not forget: {e}"),
            },
            _ => {
                match self.rook.recall(&string("query"), self.rook.config.memory.context_budget_tokens * 2) {
                    Ok(facts) if facts.is_empty() => "nothing remembered about that".into(),
                    Ok(facts) => {
                        facts.iter().map(|f| format!("[{}] {}", f.id, f.text)).collect::<Vec<_>>().join("\n")
                    }
                    Err(e) => format!("could not recall: {e}"),
                }
            }
        }
    }

    /// Shrink the context window this loop budgets against, so a test can reach
    /// compaction without a hundred thousand tokens of fixture.
    pub fn set_window_for_test(&mut self, window: usize) {
        self.budget = ContextBudget::new(window, self.rook.config.agent.compact_at);
    }

    /// Skip every prompt: run whatever the deny list does not forbid.
    pub fn allow_everything_not_denied(&mut self) {
        let sandbox = &self.rook.config.sandbox;
        let (policy, _) = Policy::compile(rook_tools::policy::Mode::Auto, &sandbox.allow, &[], &sandbox.deny);
        self.policy = std::sync::Arc::new(policy);
    }

    async fn run_session_hooks(&self) {
        if self.hooks.is_empty() || self.session_context.lock().is_ok_and(|c| c.is_some()) {
            return;
        }
        let outcome =
            self.hooks.run(hooks::Event::SessionStart, "", &self.payload(serde_json::json!({}))).await;
        if let Ok(mut slot) = self.session_context.lock() {
            *slot = Some(outcome.context().unwrap_or_default());
        }
    }

    async fn finish(&self, outcome: &TurnOutcome) {
        if self.hooks.is_empty() {
            return;
        }
        let payload = self.payload(serde_json::json!({
            "steps": outcome.steps,
            "stopped": outcome.stopped,
            "reply": outcome.reply,
            "input_tokens": outcome.input_tokens,
            "output_tokens": outcome.output_tokens,
        }));
        self.hooks.run(hooks::Event::TurnEnd, &outcome.stopped, &payload).await;
    }

    fn payload(&self, mut extra: serde_json::Value) -> serde_json::Value {
        extra["session"] = rook_store::format_session_id(self.session).into();
        extra["cwd"] = self.rook.workspace.display().to_string().into();
        extra["model"] = self.rook.config.agent.model.clone().into();
        extra
    }

    /// Consult the policy and any `pre_tool` hooks, and the user when the answer
    /// is to ask. Returns the refusal to hand back to the model, or `None` when
    /// the call may proceed.
    async fn gate(&self, call: &rook_llm::ToolCall) -> Option<String> {
        let tool = self.tools.get(&call.name)?;
        let risk = tool.risk(&call.arguments);

        // The policy runs first so a hook cannot unlock what the deny list
        // forbids; everything else, a hook may override.
        let mut decision = self.policy.decide(&risk);
        if !matches!(decision, Decision::Deny(_)) && !self.hooks.is_empty() {
            let payload = self.payload(serde_json::json!({
                "tool": call.name,
                "input": call.arguments,
                "action": risk.describe(),
            }));
            let outcome = self.hooks.run(hooks::Event::PreTool, &call.name, &payload).await;
            if let Some(hooked) = outcome.decision {
                decision = hooked;
            }
        }

        match decision {
            Decision::Allow => None,
            Decision::Deny(why) => Some(format!("refused: {why}")),
            Decision::Ask => match self.approver.ask(&call.name, &risk).await {
                rook_tools::policy::Approval::Once => None,
                rook_tools::policy::Approval::ForRun => {
                    self.policy.grant_for_run(&risk.subject());
                    None
                }
                rook_tools::policy::Approval::Deny(why) => Some(format!("refused: {why}")),
            },
        }
    }

    /// Snapshot whatever a mutating tool is about to touch, so `rook session
    /// rewind` can put the files back. Read-only tools report no paths and cost
    /// nothing here.
    fn checkpoint_before(&self, call: &rook_llm::ToolCall) {
        let Some(tool) = self.tools.get(&call.name) else { return };
        let paths: Vec<std::path::PathBuf> = tool
            .touched_paths(&call.arguments)
            .iter()
            .filter_map(|p| self.tool_ctx.resolve(p).ok())
            .collect();
        if let Err(e) = self.rook.checkpoint_paths(self.session, &call.name, &paths) {
            tracing::warn!("checkpoint before {} failed: {e}", call.name);
        }
    }
}

/// Replace the earlier part of the session with a summary of it.
///
/// Summarised by the model rather than elided, because an agent that has
/// forgotten what it did twenty turns ago repeats it. If the summary cannot be
/// produced — a provider error, a span that will not fit — it falls back to a
/// marker, since a failed compaction must not wedge the turn.
impl AgentLoop<'_> {
    async fn compact(&self) {
        let note = match self.summarise_span().await {
            Ok(note) => note,
            Err(e) => {
                tracing::warn!("summarising for compaction failed: {e}");
                format!("compacted without a summary: {e}")
            }
        };
        self.rook.log(self.session, EventKind::Compaction, "auto", &note).ok();
    }

    async fn summarise_span(&self) -> Result<String> {
        let (from_seq, _) = self.rook.last_compaction(self.session)?;
        let entries = self.rook.transcript(self.session, from_seq, usize::MAX, 8_000)?;

        // Keep the recent tail live; only what falls before it is summarised.
        let keep = self.budget.threshold() / 3;
        let mut kept = 0;
        let mut split = entries.len();
        for entry in entries.iter().rev() {
            kept += estimate_tokens(&entry.body);
            if kept > keep {
                break;
            }
            split -= 1;
        }
        if split < 2 {
            return Err(CoreError::Other("not enough history to compact".into()));
        }

        let span = &entries[..split];
        let through_seq = span.last().map(|e| e.seq).unwrap_or(0);
        let transcript = render_span(span, self.budget.usable() / 2);

        let request = Request::new(vec![Message::system(SUMMARY_INSTRUCTIONS), Message::user(transcript)]);
        let mut stream = self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
        let mut assembler = Assembler::default();
        while let Some(delta) = stream.next().await {
            assembler.push(delta.map_err(|e| CoreError::Other(e.to_string()))?);
        }
        let summary = assembler.finish().message.content;
        if summary.trim().is_empty() {
            return Err(CoreError::Other("the model returned an empty summary".into()));
        }

        Ok(serde_json::to_string(&serde_json::json!({
            "through_seq": through_seq,
            "dropped_events": span.len(),
            "summary": summary,
        }))?)
    }
}

const SUMMARY_INSTRUCTIONS: &str = "\
You are compacting an agent's working transcript so it can keep going with less \
context. Write a summary that lets the agent resume without re-reading what you \
were given. Use these sections, omitting any that are empty:

## Goal
What the user actually asked for, in their terms.

## Done
What was established or changed, with file paths and the specific facts that \
matter — names, signatures, numbers, error messages. Not a narration of steps.

## Open
What is unfinished, what was tried and failed, and what was decided against and \
why, so it is not retried.

Be concrete and terse. Facts the agent would otherwise have to rediscover are \
worth more than prose.";

/// Flatten a span of the transcript for summarising, keeping the most recent
/// part when it will not all fit — a summarisation request that overflows is
/// how compaction fails exactly when it is needed most.
fn render_span(entries: &[crate::TranscriptEntry], budget_tokens: usize) -> String {
    let mut lines = Vec::new();
    let mut used = 0;
    for entry in entries.iter().rev() {
        let line = format!("[{}] {}: {}", entry.seq, entry.kind, entry.body);
        used += estimate_tokens(&line);
        if used > budget_tokens && !lines.is_empty() {
            lines.push("[earlier still, elided]".to_string());
            break;
        }
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n\n")
}

fn measure(messages: &[Message]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content) + 4).sum()
}
