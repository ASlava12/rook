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
use rook_tools::{ToolBox, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::context::{ContextBudget, estimate_tokens};
use crate::error::{CoreError, Result};
use crate::service::Rook;

/// Pseudo-tools: implemented by the loop rather than the toolbox, because they
/// need the agent's own state.
pub const LOAD_SKILL: &str = "load_skill";
pub const REMEMBER: &str = "remember";
pub const FORGET: &str = "forget";
pub const RECALL: &str = "recall";

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
    pub compactions: u32,
}

pub struct AgentLoop<'a> {
    pub rook: &'a Rook,
    pub provider: Box<dyn Provider>,
    pub tools: ToolBox,
    pub tool_ctx: ToolContext,
    pub session: u128,
    budget: ContextBudget,
}

impl<'a> AgentLoop<'a> {
    pub fn new(rook: &'a Rook, provider: Box<dyn Provider>, session: u128) -> Self {
        let mut tool_ctx = ToolContext::new(rook.workspace.clone());
        tool_ctx.max_output_bytes = rook.config.sandbox.max_output_bytes;
        tool_ctx.command_timeout = std::time::Duration::from_secs(rook.config.sandbox.command_timeout_secs);
        tool_ctx.allow = rook.config.sandbox.allow.clone();
        tool_ctx.deny = rook.config.sandbox.deny.clone();
        let budget = ContextBudget::new(provider.context_window(), rook.config.agent.compact_at);
        Self { rook, provider, tools: ToolBox::standard(), tool_ctx, session, budget }
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

    /// Rebuild the conversation from the session log.
    ///
    /// The log is the only record of a turn; without replaying it every call
    /// would start from nothing, and `--session` would continue a session in
    /// name only. Tool calls and their results are paired by adjacency, which
    /// holds because the loop logs each call immediately before its result.
    fn history(&self) -> Result<Vec<Message>> {
        let events = self.rook.store.events(self.session, 0, usize::MAX)?;
        let mut messages = Vec::with_capacity(events.len());
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
        self.rook.log(self.session, EventKind::UserMessage, "", prompt)?;

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
            compactions: 0,
        };

        while outcome.steps < self.rook.config.agent.max_steps {
            outcome.steps += 1;

            if self.budget.needs_compaction(measure(&messages)) {
                outcome.compactions += 1;
                let note = compact(&mut messages, self.budget.threshold() / 2);
                self.rook.log(self.session, EventKind::Compaction, "auto", &note)?;
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
                return Ok(outcome);
            }

            messages.push(response.message.clone());
            for call in &response.message.tool_calls {
                let result = self.dispatch(call, &mut outcome).await;
                messages.push(Message::tool_result(&call.id, result));
            }
        }

        outcome.stopped = "max_steps".into();
        Ok(outcome)
    }

    async fn dispatch(&self, call: &rook_llm::ToolCall, outcome: &mut TurnOutcome) -> String {
        self.rook.log(self.session, EventKind::ToolCall, &call.name, &call.arguments.to_string()).ok();

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

        outcome.tools_called.push(call.name.clone());
        self.checkpoint_before(call);
        let result = self.tools.call(&self.tool_ctx, &call.name, &call.arguments).await;
        let text = match result {
            Ok(o) => o.content,
            Err(e) => format!("tool error: {e}"),
        };
        self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
        text
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

fn measure(messages: &[Message]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content) + 4).sum()
}

/// Mechanical compaction: keep the system prompt, the first user message and the
/// most recent exchanges; elide the middle with an explicit marker.
///
/// The full history is never lost — it is in the store, addressable by session
/// and sequence — so this drops what is in *context*, not what happened.
fn compact(messages: &mut Vec<Message>, target_tokens: usize) -> String {
    if messages.len() <= 4 {
        return "nothing to compact".into();
    }
    let head = 2.min(messages.len());
    let mut keep_tail = 0usize;
    let mut used = 0usize;
    for m in messages.iter().rev() {
        used += estimate_tokens(&m.content) + 4;
        if used > target_tokens {
            break;
        }
        keep_tail += 1;
    }
    keep_tail = keep_tail.max(2);
    if head + keep_tail >= messages.len() {
        return "nothing to compact".into();
    }

    let dropped = messages.len() - head - keep_tail;
    let tail = messages.split_off(messages.len() - keep_tail);
    messages.truncate(head);
    messages.push(Message {
        role: Role::User,
        content: format!(
            "[{dropped} earlier messages elided to stay within the context window. \
             The full transcript is in the session log and can be re-read by sequence number.]"
        ),
        tool_calls: vec![],
        tool_call_id: None,
    });
    messages.extend(tail);
    format!("compacted: dropped {dropped} messages from context")
}
