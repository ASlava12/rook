//! The agent loop.
//!
//! Deliberately small. Everything that varies — the model, the tools, the skills
//! — is behind a trait or a data structure, so the loop itself stays something a
//! person can read in one sitting and reason about.
//!
//! Two behaviours are built in rather than bolted on:
//!
//! * **Progressive disclosure.** The system prompt carries skill *cards* and
//!   tool *stubs*. A skill's body arrives only when the model asks for it via
//!   `load_skill`, so a library of a hundred skills costs a few hundred tokens
//!   a turn instead of tens of thousands. A tool stub keeps every argument's
//!   name and type and drops only the prose around them — a tool advertised
//!   without its shape cannot be called at all.
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

/// Hand a loop the long-lived parts the front end owns.
///
/// A new `AgentLoop` is built for every turn and these are not, so what a turn
/// inherits is one question — and it was being answered separately by the CLI,
/// the TUI, the daemon and the editor bridge. Whatever is added here reaches all
/// four; three of four is how a capability quietly goes missing from one.
pub fn equip(
    agent: &mut AgentLoop<'_>,
    servers: std::sync::Arc<crate::lsp::Servers>,
    mcp: &crate::McpSession,
) {
    agent.servers = servers.clone();
    crate::lsp::register(&mut agent.tools, servers);
    for (server, tools) in &mcp.servers {
        agent.tools.register_server(server.clone(), tools.clone());
    }
}

/// Build the language-server pool from configuration.
///
/// Exposed for the same reason as [`policy_for`], and more urgently: a pool
/// dropped at the end of a turn takes its running servers with it, and
/// rust-analyzer spends seconds indexing the workspace every time it starts.
pub fn servers_for(rook: &Rook) -> std::sync::Arc<crate::lsp::Servers> {
    crate::lsp::Servers::new(crate::lsp::for_workspace(&rook.config, &rook.workspace), &rook.workspace)
}

/// What the file and command tools are bounded by, from configuration.
///
/// Exposed for the same reason as [`policy_for`]: a turn is not the only thing
/// that runs a tool. `rook mcp serve` runs them for somebody else's client, and
/// two places deciding separately what a tool may write to is how one of them
/// ends up with a boundary the other does not have.
pub fn tool_context(rook: &Rook) -> ToolContext {
    let sandbox = &rook.config.sandbox;
    let mut ctx = ToolContext::new(rook.workspace.clone());
    ctx.max_output_bytes = sandbox.max_output_bytes;
    ctx.command_timeout = std::time::Duration::from_secs(sandbox.command_timeout_secs);
    ctx.allow_outside_workspace = sandbox.allow_outside_workspace;
    ctx
}

/// Build the approval policy from configuration.
///
/// Exposed because "allow this for the rest of the run" has to outlive a single
/// turn: an interactive front end builds one policy for the session and hands it
/// to every loop, or the user is asked again the moment they said not to be.
pub fn policy_for(rook: &Rook) -> std::sync::Arc<Policy> {
    let sandbox = &rook.config.sandbox;
    let (policy, unusable) = Policy::compile(sandbox.mode, &sandbox.allow, &sandbox.ask, &sandbox.deny);
    for error in unusable {
        tracing::warn!("ignoring unusable sandbox rule: {error}");
    }
    std::sync::Arc::new(policy)
}

/// A skill's own files, named so instructions that mention them can be followed.
///
/// The format allows a skill to bundle scripts and references, and the body
/// refers to them by relative path — which the agent cannot act on without
/// knowing where the skill lives. Nothing is appended for a skill that is only a
/// `SKILL.md`, which is most of them.
fn bundled(skill: &rook_skills::Skill) -> String {
    const MOST: usize = 20;
    let files: Vec<String> = skill
        .resources()
        .into_iter()
        .filter(|rel| rel != std::path::Path::new("SKILL.md"))
        .filter(|rel| !rel.starts_with("variants"))
        // Forward slashes on every platform, as the manifest already stores
        // them: the skill's own body refers to these files by relative path,
        // and advertising `scripts\check.sh` beside a body that says
        // `scripts/check.sh` leaves the model two spellings to reconcile.
        .map(|rel| rel.display().to_string().replace('\\', "/"))
        .collect();
    if files.is_empty() {
        return String::new();
    }
    let listed: Vec<String> = files.iter().take(MOST).map(|f| format!("\n- {f}")).collect();
    let more = match files.len().saturating_sub(MOST) {
        0 => String::new(),
        n => format!("\n- …and {n} more"),
    };
    format!("\n\nBundled with this skill, under {}:{}{more}", skill.dir.display(), listed.join(""))
}

impl TurnOutcome {
    /// What a turn changed about what the agent believes, for the same line
    /// that reports what it changed on disk. Empty when it changed nothing.
    pub fn memory_note(&self) -> Option<String> {
        let mut said = Vec::new();
        for text in &self.facts_learned {
            said.push(format!("remembered: {text}"));
        }
        for text in &self.facts_forgotten {
            said.push(format!("forgot: {text}"));
        }
        (!said.is_empty()).then(|| said.join("\n"))
    }
}

/// The loop's own tools that change something, and are therefore not offered to
/// a checker. `delegate` is here because a checker that can start an agent with
/// the writing tools has not been stopped from writing, only from doing it
/// itself.
const CHANGES_THINGS: &[&str] = &[WRITE_SKILL, FIND_SKILL, REMEMBER, FORGET, DELEGATE, VERIFY];

/// The verdict a checker committed to, if it committed to one.
fn verdict_in(reply: &str) -> Option<&str> {
    reply.lines().rev().find_map(|line| line.trim().strip_prefix("VERDICT:")).map(str::trim)
}

/// The head of a sub-task, for a progress line. A task is a whole instruction —
/// a live one ran to two hundred characters — and repeating it on every step
/// buries what the step actually was.
fn short(task: &str) -> &str {
    let line = task.lines().next().unwrap_or(task);
    match line.char_indices().nth(48) {
        Some((cut, _)) => &line[..cut],
        None => line,
    }
}

/// What a turn reports as it goes.
///
/// Stream deltas, and what only the loop knows: the provider's stream ends when
/// the model stops asking for a tool, not when the tool has run. A front end
/// with only the deltas shows every call as still working.
pub enum Progress<'a> {
    Delta(&'a Delta),
    ToolDone {
        name: &'a str,
        failed: bool,
    },
    /// One delegated sub-task finished. They run concurrently and the parent
    /// waits for all of them, so without this a delegation is minutes of
    /// silence that cannot be told from a hang.
    Delegated {
        task: &'a str,
        done: usize,
        total: usize,
    },
    /// A sub-task called a tool. Several run at once, so the task is named
    /// alongside: without this a delegation that takes minutes shows a counter
    /// that does not move, which reads the same as a hang.
    Delegating {
        task: &'a str,
        tool: &'a str,
    },
    /// What the turn has spent, after each reply from the model. A turn that
    /// runs for minutes across a dozen steps otherwise shows no cost at all
    /// until it is over and the number can no longer change a decision.
    Spent {
        input: u32,
        output: u32,
        cached: u32,
    },
}

/// Answer a tool call the log never answered.
///
/// A process killed between logging a call and logging its result leaves the
/// pair half-written. Every provider refuses a request where an assistant asked
/// for a tool and nothing replied, so without this the session could never be
/// resumed — and saying what happened is more use to the model than a blank.
fn close_open_call(messages: &mut Vec<Message>, open: &mut Option<String>) {
    if let Some(id) = open.take() {
        messages.push(Message::tool_result(id, "no result was recorded: the turn did not finish"));
    }
}

/// Pseudo-tools: implemented by the loop rather than the toolbox, because they
/// need the agent's own state.
pub const LOAD_SKILL: &str = "load_skill";
pub const WRITE_SKILL: &str = "write_skill";
pub const FIND_SKILL: &str = "find_skill";
pub const REMEMBER: &str = "remember";
pub const FORGET: &str = "forget";
pub const RECALL: &str = "recall";
pub const DELEGATE: &str = "delegate";
pub const VERIFY: &str = "verify";

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
    /// Input tokens served from the prompt cache instead of reprocessed.
    pub cached_tokens: u32,
    pub tools_called: Vec<String>,
    pub skills_loaded: Vec<String>,
    pub skills_written: Vec<String>,
    /// What the agent remembered, as text: the id is for `memory rm`, this is
    /// for a person reading what a turn did.
    pub facts_learned: Vec<String>,
    /// Facts the agent dropped. Reported beside what it learnt, because an
    /// agent quietly removing what it was told to remember is the same failure
    /// as one quietly remembering something nobody can see.
    pub facts_forgotten: Vec<String>,
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
    pub servers: std::sync::Arc<crate::lsp::Servers>,
    /// What the `session_start` hooks contributed, computed once.
    session_context: std::sync::Mutex<Option<String>>,
    /// Consulted whenever the policy says to ask. Refuses by default, so an
    /// unattended run cannot silently do something nobody reviewed.
    pub approver: std::sync::Arc<dyn Approver>,
    pub depth: u32,
    pub max_steps: u32,
    pub effort: rook_llm::Effort,
    budget: ContextBudget,
    /// Sub-agents started so far, shared with every child so one that delegates
    /// again is charged to the turn that began it rather than being handed a
    /// fresh allowance at each level.
    spawned: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// This loop is checking somebody else's claim, and may not change anything.
    ///
    /// Taking the writing tools out of the toolbox is not enough on its own: the
    /// loop adds six of its own that the toolbox never held, and two of them —
    /// writing a skill, and delegating to an agent that can write — are ways
    /// round the very restriction.
    checking: bool,
}

impl<'a> AgentLoop<'a> {
    pub fn new(rook: &'a Rook, provider: std::sync::Arc<dyn Provider>, session: u128) -> Self {
        let tool_ctx = tool_context(rook);

        // No language servers until a front end hands them over with `equip`.
        // A loop is rebuilt for every turn, so a pool built here is rebuilt with
        // it — and worse, the tools registered from it hold that pool, so what
        // `equip` set afterwards was never what answered. A workspace with no
        // Rust in it was offered rust-analyzer for exactly this reason.
        let servers = crate::lsp::Servers::new(Vec::new(), &rook.workspace);
        let tools = ToolBox::standard();

        let (hooks, bad_hooks) = Hooks::compile(&rook.config.hooks);
        for error in bad_hooks {
            tracing::warn!("ignoring unusable hook matcher: {error}");
        }

        let budget = ContextBudget::new(provider.context_window(), rook.config.agent.compact_at);
        Self {
            rook,
            provider,
            tools,
            tool_ctx,
            session,
            policy: policy_for(rook),
            hooks: std::sync::Arc::new(hooks),
            servers,
            session_context: std::sync::Mutex::new(None),
            approver: std::sync::Arc::new(Unattended),
            depth: 0,
            max_steps: rook.config.agent.max_steps,
            effort: rook.config.agent.effort(),
            budget,
            spawned: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            checking: false,
        }
    }

    /// Give the model a way to put a question to the person, which registers
    /// the tool rather than storing a handle: a front end that cannot reach
    /// anyone never advertises it, and never pays for its schema.
    pub fn ask_via(&mut self, asker: std::sync::Arc<dyn rook_tools::ask::Asker>) {
        self.tools.register(std::sync::Arc::new(rook_tools::ask::AskUser(asker)));
    }

    /// The system prompt: identity, environment, and the skill catalog.
    ///
    /// The environment block matters more than it looks. A model told it is on
    /// FreeBSD with BSD userland stops reaching for `sed -i` with a GNU argument
    /// order, which is the single most common cross-platform failure in agent
    /// transcripts.
    /// Deliberately independent of the current prompt.
    ///
    /// Everything here renders at the front of the request, so anything that
    /// varies per turn invalidates the cached prefix behind it. Recalled memory
    /// used to live here and now travels next to the prompt instead.
    pub fn system_prompt(&self) -> String {
        let env = self.rook.env();
        let mut s = String::new();
        s.push_str(
            "You are Rook, an autonomous agent working in a local workspace.\n\
             Work in small verified steps. Prefer reading before editing. State what you did.\n",
        );
        if self.rook.config.agent.plan_first {
            s.push_str(
                "For anything that takes more than one step, say the plan in a sentence or two \
                 before acting, and say so when it changes. Do not keep a checklist.\n",
            );
        }
        if let Ok(Some(goal)) = self.rook.goal(self.session) {
            s.push_str(&format!("\nThe user's goal for this session: {goal}\n"));
        }
        s.push('\n');
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

        if let Ok(extra) = self.session_context.lock()
            && let Some(text) = extra.as_deref().filter(|t| !t.trim().is_empty())
        {
            s.push_str(&format!("\n## From this workspace\n{text}\n"));
        }

        let cards = self.rook.catalog();
        let applicable: Vec<_> = cards.iter().filter(|c| c.applicable).collect();
        if !applicable.is_empty() {
            s.push_str("\n## Skills\n");
            let listed = if self.rook.config.agent.lazy_skills {
                self.skill_cards(&mut s, &applicable)
            } else {
                self.skill_bodies(&mut s, &applicable)
            };
            // Named rather than silently dropped: a model that cannot see a
            // skill and is not told any exist will not go looking for one.
            if let Some(omitted) = applicable.len().checked_sub(listed).filter(|n| *n > 0) {
                s.push_str(&format!(
                    "\n…and {omitted} more not shown. `{LOAD_SKILL}` answers an unknown name with \
                     what it does have, so describe what you need.\n"
                ));
            }
        }
        s
    }

    fn skill_cards(&self, s: &mut String, applicable: &[&rook_skills::SkillCard]) -> usize {
        s.push_str(&format!("Call `{LOAD_SKILL}` with a name to read its instructions before using it.\n"));
        let cap = self.rook.config.agent.max_skill_cards;
        for c in applicable.iter().take(cap) {
            // No version: `load_skill` takes a name, and `resolve` picks the
            // version from the environment — so a version here is ~100 tokens
            // per fifty skills that the model cannot act on.
            s.push_str(&format!("- {}: {}\n", c.name, c.description));
        }
        applicable.len().min(cap)
    }

    /// Every applicable skill's instructions inline, for a model too small to be
    /// trusted to call `load_skill` for itself.
    ///
    /// Bounded by a share of the context window rather than a count: bodies vary
    /// from a paragraph to several pages, and a library that filled the window
    /// would leave no room for the work.
    fn skill_bodies(&self, s: &mut String, applicable: &[&rook_skills::SkillCard]) -> usize {
        let mut left = self.budget.window / 4;
        let mut shown = 0;
        for card in applicable {
            let Ok(resolved) = self.rook.skills().resolve(&card.name, self.rook.env()) else { continue };
            if card.body_tokens > left {
                break;
            }
            left -= card.body_tokens;
            shown += 1;
            s.push_str(&format!("\n### {} ({})\n{}\n", card.name, card.version, resolved.body.trim()));
        }
        shown
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
    /// holds because the loop logs each call immediately before its result —
    /// except when the process died between the two, and then the pairing has
    /// to be completed here or the session can never be resumed.
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
                EventKind::UserMessage => {
                    close_open_call(&mut messages, &mut open_call);
                    messages.push(Message::user(body))
                }
                EventKind::AssistantMessage => {
                    close_open_call(&mut messages, &mut open_call);
                    messages.push(Message::assistant(body))
                }
                EventKind::SkillLoaded => {
                    messages.push(Message::user(format!("[skill {} loaded]\n{body}", event.record.label)))
                }
                EventKind::ToolCall => {
                    close_open_call(&mut messages, &mut open_call);
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
                        cache: false,
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
        close_open_call(&mut messages, &mut open_call);
        Ok(messages)
    }

    /// Facts worth putting in front of the model for this prompt, if any.
    fn recalled(&self, prompt: &str) -> Option<String> {
        if !self.rook.config.memory.enabled {
            return None;
        }
        let facts = self.rook.recall(prompt, self.rook.config.memory.context_budget_tokens).ok()?;
        if facts.is_empty() {
            return None;
        }
        let lines: Vec<String> = facts.iter().map(|f| format!("- [{}] {}", f.id, f.text)).collect();
        Some(format!(
            "Things you were told to remember that look relevant here. Correct one with \
             `forget` when it turns out to be wrong.\n{}",
            lines.join("\n")
        ))
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let lazy = self.rook.config.agent.lazy_tools;
        let mut specs = if lazy { self.tools.stubs() } else { self.tools.specs() };
        let checking = self.checking;
        let mut push = |spec: ToolSpec| {
            if checking && CHANGES_THINGS.contains(&spec.name.as_str()) {
                return;
            }
            specs.push(if lazy { spec.stub() } else { spec })
        };
        push(ToolSpec {
            name: LOAD_SKILL.into(),
            description: "Load a skill's full instructions into context by name. An unknown \
                          name comes back with the skills that do match it, so a description \
                          works when the exact name is not known."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        });
        push(ToolSpec {
            name: FIND_SKILL.into(),
            description: "Search the configured sources for a skill, and install one by name. \
                          For when nothing here covers what is being asked and fetching beats \
                          writing from scratch. Installing is approved like any write: it puts \
                          instructions on the machine that later sessions follow."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "words to match against a name or description" },
                    "install": { "type": "string", "description": "the exact name to install. Search first: a name no source offers comes back with the closest one." }
                }
            }),
        });
        push(ToolSpec {
            name: WRITE_SKILL.into(),
            description: "Write down a repeatable procedure, with the tools it needs beside it, \
                          so a later session does not work it out again. For what took real \
                          effort — a build incantation, a platform quirk — not for what this \
                          conversation already says. When no tool does the job, put one in \
                          `files` and have the body call it; a shebang makes it runnable. \
                          `requires` scopes it to where it holds."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "lower-case, hyphenated" },
                    "description": { "type": "string", "description": "when to use it, in one line" },
                    "body": { "type": "string", "description": "markdown instructions" },
                    "keywords": { "type": "array", "items": { "type": "string" } },
                    "files": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "By relative name: a script the body runs, a template it fills."
                    },
                    "requires": {
                        "type": "object",
                        "properties": {
                            "os": { "type": "array", "items": { "type": "string" } },
                            "arch": { "type": "array", "items": { "type": "string" } },
                            "userland": { "type": "array", "items": { "type": "string" } },
                            "language": { "type": "object", "additionalProperties": { "type": "string" } },
                            "tool": { "type": "object", "additionalProperties": { "type": "string" } }
                        }
                    }
                },
                "required": ["name", "description", "body"]
            }),
        });
        if self.rook.config.memory.enabled {
            push(ToolSpec {
                name: REMEMBER.into(),
                description: "Remember something for future sessions. Use it for durable facts — preferences, conventions, decisions — not for what is already in this conversation."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "One self-contained fact." },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "scope": { "type": "string", "enum": ["global", "project"], "default": "project" },
                        "pinned": { "type": "boolean", "description": "Recall this ahead of anything that merely matches. It still costs context, so pin only what is true every turn." }
                    },
                    "required": ["text"]
                }),
            });
            push(ToolSpec {
                name: FORGET.into(),
                description: "Drop a remembered fact by its id, once it is wrong or stale.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            });
            push(ToolSpec {
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
            push(ToolSpec {
                name: VERIFY.into(),
                description: "Have a claim checked by an agent that did not make it and cannot edit anything. Use it before reporting work done."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "claim": {
                            "type": "string",
                            "description": "Stated so it can be wrong: `the tests pass`, not `the code is better`."
                        },
                        "settles": { "type": "string", "description": "What would decide it — a command, a file." }
                    },
                    "required": ["claim"]
                }),
            });
            push(ToolSpec {
                name: DELEGATE.into(),
                description: "Hand a self-contained sub-task to a fresh agent and get back only its conclusion. Use it when a step would otherwise fill this conversation with detail you do not need to keep — a wide search, a long file survey, an independent verification."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        // A bare `task` is still accepted, and deliberately not
                        // advertised: a model that saw both filled both, which
                        // ran every sub-task twice.
                        "tasks": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "One assignment per entry, run at the same time. A sub-agent cannot see this conversation, so each has to stand alone. Use this rather than calling delegate repeatedly."
                        },
                        "context": {
                            "type": "string",
                            "default": "none",
                            "description": "What the sub-task starts with. `recent` hands it the \
                                            last few exchanges; anything else is passed to it \
                                            verbatim, which is where a file it would otherwise \
                                            have to go and read belongs."
                        },
                        "max_steps": { "type": "integer" }
                    }
                }),
            });
        }
        // Sorted so the rendered prefix is byte-identical between turns:
        // tools render first, and a reordered list invalidates everything.
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Run one user turn to completion.
    pub async fn run(&mut self, prompt: &str) -> Result<TurnOutcome> {
        self.run_with(prompt, |_| {}).await
    }

    /// Answer a question about the conversation without joining it.
    ///
    /// One call, no tools, no loop. The exchange is recorded as a note, which
    /// the history replay skips — so asking what a piece of code does mid-task
    /// neither costs the agent a tool round trip nor leaves anything in the
    /// context it will carry for the rest of the session.
    pub async fn aside<F: FnMut(&Delta)>(&self, question: &str, mut on_delta: F) -> Result<String> {
        let mut messages = vec![cacheable(Message::system(self.system_prompt()))];
        messages.extend(self.history()?);
        messages.push(Message::user(format!(
            "{question}\n\n(Answer from what you already know here. Do not act, and do not \
             offer to — this is an aside, not an instruction.)"
        )));

        let mut request = Request::new(messages);
        request.max_output_tokens = 1024;
        // An aside is a question about work already done, not the work.
        request.effort = Some(rook_llm::Effort::Low);

        let mut stream = self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
        let mut assembler = Assembler::default();
        while let Some(delta) = stream.next().await {
            let delta = delta.map_err(|e| CoreError::Other(e.to_string()))?;
            on_delta(&delta);
            assembler.push(delta).map_err(|e| CoreError::Other(e.to_string()))?;
        }

        let response = assembler.finish();
        // A model that answers an aside with a tool call has nothing to say and
        // no way to act; an empty pane would leave that looking like a hang.
        let answer = match response.message.content.trim() {
            "" if !response.message.tool_calls.is_empty() => {
                "(the model tried to use a tool instead of answering; ask it as a normal message)".to_string()
            }
            "" => "(the model returned nothing)".to_string(),
            text => text.to_string(),
        };
        self.rook.log(self.session, EventKind::Note, "btw", &format!("Q: {question}\nA: {answer}")).ok();
        Ok(answer)
    }

    /// Run a turn, reporting each fragment as it arrives.
    ///
    /// `on_progress` sees text as the model produces it and tool calls once they
    /// are complete; the turn's bookkeeping is unaffected by whether anyone is
    /// watching.
    pub async fn run_with<F: FnMut(Progress<'_>)>(
        &mut self,
        prompt: &str,
        mut on_progress: F,
    ) -> Result<TurnOutcome> {
        self.rook.name_session_from(self.session, prompt).ok();
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
        let mut messages = vec![cacheable(Message::system(self.system_prompt()))];
        messages.extend(self.history()?);
        self.mark_stable_prefix(&mut messages);
        if let Some(memory) = self.recalled(prompt) {
            // Just before the newest turn: memory varies with the prompt, and
            // anything volatile belongs after everything worth caching.
            messages.insert(messages.len().saturating_sub(1), Message::user(memory));
        }
        let mut outcome = TurnOutcome {
            steps: 0,
            stopped: "end_turn".into(),
            reply: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            tools_called: Vec::new(),
            skills_loaded: Vec::new(),
            skills_written: Vec::new(),
            facts_learned: Vec::new(),
            facts_forgotten: Vec::new(),
            delegated: Vec::new(),
            compactions: 0,
        };

        let mut worth_compacting = true;
        while outcome.steps < self.max_steps {
            outcome.steps += 1;

            // Once per turn that it achieves something. A span too small to
            // summarise leaves the context where it was, so the next step would
            // ask again, and the step after that — spending a summarisation
            // call each time to stay exactly as full as it already is.
            if worth_compacting && self.budget.needs_compaction(measure(&messages)) {
                let before = measure(&messages);
                outcome.compactions += 1;
                self.compact().await;
                messages = vec![cacheable(Message::system(self.system_prompt()))];
                messages.extend(self.history()?);
                self.mark_stable_prefix(&mut messages);
                worth_compacting = measure(&messages) < before;
            }

            // Compaction summarises history; it cannot make one message smaller.
            // A pasted build log larger than the window would otherwise be sent
            // whole and come back as a provider error about a limit the user
            // never saw.
            let used = measure(&messages);
            if used > self.budget.usable() {
                return Err(CoreError::Llm(rook_llm::LlmError::ContextOverflow {
                    used,
                    window: self.budget.usable(),
                }));
            }

            let mut request = Request::new(messages.clone());
            request.tools = self.tool_specs();
            request.effort = Some(self.effort);
            let mut stream =
                self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
            let mut assembler = Assembler::default();
            while let Some(delta) = stream.next().await {
                let delta = delta.map_err(|e| CoreError::Other(e.to_string()))?;
                on_progress(Progress::Delta(&delta));
                assembler.push(delta).map_err(|e| CoreError::Other(e.to_string()))?;
            }
            if !assembler.reasoning().is_empty() {
                self.rook.log(self.session, EventKind::Reasoning, "", assembler.reasoning()).ok();
            }
            let response = assembler.finish();

            outcome.input_tokens += response.usage.input_tokens;
            outcome.output_tokens += response.usage.output_tokens;
            outcome.cached_tokens += response.usage.cache_read_tokens;
            on_progress(Progress::Spent {
                input: outcome.input_tokens,
                output: outcome.output_tokens,
                cached: outcome.cached_tokens,
            });

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
                outcome.stopped = response.stop_reason.as_str().into();
                self.finish(&outcome).await;
                return Ok(outcome);
            }

            messages.push(response.message.clone());
            for call in &response.message.tool_calls {
                let (result, failed) = self.dispatch(call, &mut outcome, &mut on_progress).await;
                on_progress(Progress::ToolDone { name: &call.name, failed });
                messages.push(Message::tool_result(&call.id, result));
            }
        }

        outcome.stopped = "max_steps".into();
        self.finish(&outcome).await;
        Ok(outcome)
    }

    /// The text the model sees, and whether the call failed — which the outcome
    /// knows and the text only hints at.
    async fn dispatch(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> (String, bool) {
        self.rook.log(self.session, EventKind::ToolCall, &call.name, &call.arguments.to_string()).ok();

        outcome.tools_called.push(call.name.clone());

        // Not advertised to a checker, and refused as well: a name a model
        // produces without being shown it is still a name it can produce.
        if self.checking && CHANGES_THINGS.contains(&call.name.as_str()) {
            let refusal = format!("{}: a check may not change anything", call.name);
            return (refusal, true);
        }

        if call.name == VERIFY {
            let text = self.verify(&call.arguments, outcome, on_progress).await;
            self.rook.log(self.session, EventKind::ToolResult, VERIFY, &text).ok();
            return (text, false);
        }

        if call.name == DELEGATE {
            let text = self.delegate(&call.arguments, outcome, on_progress).await;
            self.rook.log(self.session, EventKind::ToolResult, DELEGATE, &text).ok();
            return (text, false);
        }

        match call.name.as_str() {
            REMEMBER | FORGET | RECALL => {
                let text = self.memory_tool(&call.name, &call.arguments, outcome);
                self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
                return (text, false);
            }
            _ => {}
        }

        if call.name == LOAD_SKILL {
            let name = call.arguments.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            return match self.rook.skills().resolve(name, self.rook.env()) {
                Ok(resolved) => {
                    outcome.skills_loaded.push(resolved.skill.id());
                    let body = format!("{}{}", resolved.body, bundled(&resolved.skill));
                    self.rook.log(self.session, EventKind::SkillLoaded, &resolved.skill.id(), &body).ok();
                    (body, false)
                }
                // The reason matters: "needs docker >=27" is actionable, "not
                // found" sends the model looking for a typo that is not there.
                // It is logged as well as returned: a skill that never loaded is
                // otherwise invisible when reading the transcript afterwards.
                Err(e) => {
                    let mut message = format!("could not load skill {name:?}: {e}");
                    for card in self.rook.skills().search(name, self.rook.env(), 5) {
                        message.push_str(&format!("\n- {}: {}", card.name, card.description));
                    }
                    self.rook.log(self.session, EventKind::Error, LOAD_SKILL, &message).ok();
                    (message, true)
                }
            };
        }

        if call.name == FIND_SKILL {
            let query = call.arguments.get("query").and_then(|q| q.as_str()).unwrap_or_default();
            let Some(name) = call.arguments.get("install").and_then(|n| n.as_str()) else {
                // Searching reads; only installing writes.
                let (offered, errors) = self.rook.skills_offered(query, false);
                let listed: Vec<String> = offered
                    .iter()
                    .take(10)
                    .map(|o| format!("- {}: {}", o.name, o.description.chars().take(160).collect::<String>()))
                    .collect();
                let text = match listed.is_empty() {
                    true => format!("no source offers a skill matching {query:?}. {}", errors.join("; ")),
                    false => format!(
                        "{}\n\nInstall one by name with `install`, or write your own.",
                        listed.join("\n")
                    ),
                };
                self.rook.log(self.session, EventKind::ToolResult, FIND_SKILL, &text).ok();
                return (text, false);
            };

            let target = crate::paths::user_skills_dir().join(name);
            let risk = rook_tools::policy::Risk::Write(vec![target.display().to_string()]);
            if let Some(refusal) = self.gate_risk(FIND_SKILL, &call.arguments, risk).await {
                self.rook.log(self.session, EventKind::Error, FIND_SKILL, &refusal).ok();
                return (refusal, true);
            }
            return match self.rook.install_skill(name) {
                Ok(path) => {
                    outcome.skills_written.push(name.to_string());
                    let message = format!(
                        "installed skill {name:?} to {}. Load it to see what it says.",
                        path.display()
                    );
                    self.rook.log(self.session, EventKind::Note, FIND_SKILL, &message).ok();
                    (message, false)
                }
                Err(e) => {
                    let message = format!("could not install {name:?}: {e}");
                    self.rook.log(self.session, EventKind::Error, FIND_SKILL, &message).ok();
                    (message, true)
                }
            };
        }

        if call.name == WRITE_SKILL {
            // It writes files a user would call theirs, so it answers to the
            // policy like any other write.
            let target = crate::paths::user_skills_dir()
                .join(call.arguments.get("name").and_then(|n| n.as_str()).unwrap_or("?"));
            // Every file by name, not just the directory: a skill that lays
            // down a script is asking to write a program, and the approval
            // should say which.
            let mut writing = vec![target.join("SKILL.md").display().to_string()];
            if let Some(files) = call.arguments.get("files").and_then(|f| f.as_object()) {
                writing.extend(files.keys().map(|rel| target.join(rel).display().to_string()));
            }
            let risk = rook_tools::policy::Risk::Write(writing);
            if let Some(refusal) = self.gate_risk(WRITE_SKILL, &call.arguments, risk).await {
                self.rook.log(self.session, EventKind::Error, WRITE_SKILL, &refusal).ok();
                return (refusal, true);
            }
            return match serde_json::from_value(call.arguments.clone())
                .map_err(|e| CoreError::Other(format!("{WRITE_SKILL}: {e}")))
                .and_then(|skill: crate::service::AuthoredSkill| {
                    self.rook.write_skill(&skill).map(|path| (skill.name, path))
                }) {
                Ok((name, path)) => {
                    outcome.skills_written.push(name.clone());
                    let message = format!("wrote skill {name:?} to {}", path.display());
                    // A note rather than a kind of its own: a new `EventKind`
                    // is a record older builds cannot decode, and the log is
                    // just as readable with the fact in the label.
                    self.rook.log(self.session, EventKind::Note, WRITE_SKILL, &message).ok();
                    (message, false)
                }
                Err(e) => {
                    let message = format!("could not write the skill: {e}");
                    self.rook.log(self.session, EventKind::Error, WRITE_SKILL, &message).ok();
                    (message, true)
                }
            };
        }

        if let Some(refusal) = self.gate(call).await {
            self.rook.log(self.session, EventKind::ToolResult, &call.name, &refusal).ok();
            return (refusal, true);
        }

        let unprotected = self.checkpoint_before(call);
        let outcome = match self.tools.call(&self.tool_ctx, &call.name, &call.arguments).await {
            Ok(o) => o,
            Err(e) => rook_tools::ToolOutcome::error(format!("tool error: {e}")),
        };
        let mut text = match self.after_tool(call, &outcome).await {
            Some(extra) => format!("{}\n\n{extra}", outcome.content),
            None => outcome.content,
        };
        if let Some(note) = unprotected {
            text.push_str(&format!("\n\n{note}"));
        }
        self.rook.log(self.session, EventKind::ToolResult, &call.name, &text).ok();
        (text, outcome.is_error)
    }

    /// `post_tool` hooks, whose output the model sees appended to the result.
    ///
    /// The whole outcome, not just its text: `meta` is where a tool says which
    /// MCP server answered, whether a command timed out, and how much of a file
    /// was returned — the facts a hook would otherwise have to parse back out of
    /// prose written for a model.
    async fn after_tool(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &rook_tools::ToolOutcome,
    ) -> Option<String> {
        if self.hooks.is_empty() {
            return None;
        }
        let payload = self.payload(serde_json::json!({
            "tool": call.name,
            "input": call.arguments,
            "result": outcome.content,
            "is_error": outcome.is_error,
            "truncated": outcome.truncated,
            "full_bytes": outcome.full_bytes,
            "meta": outcome.meta,
        }));
        self.hooks.run(hooks::Event::PostTool, &call.name, &payload).await.context()
    }

    /// Run a sub-task in its own session and return only what it concluded.
    ///
    /// The child's full transcript stays in the store, linked to this session by
    /// its parent, so the detail is recoverable without ever entering this
    /// conversation's context — which is the entire point.
    async fn delegate(
        &self,
        args: &serde_json::Value,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> String {
        let tasks = requested_tasks(args);
        if tasks.is_empty() {
            return "delegate needs a task, or a list of tasks".into();
        }

        // Anything that is not one of the two words is context the parent wrote
        // out for the child. A live model filled this with the file it had just
        // read, expecting it to arrive; the enum meant it was dropped and the
        // child read the file again.
        let inherited = match args.get("context").and_then(|c| c.as_str()).map(str::trim) {
            None | Some("") | Some("none") => None,
            Some("recent") => self.recent_exchanges(6).ok(),
            Some(given) => Some(given.to_string()),
        };
        // Only ever shortens: the ceiling is the parent's, and this argument was
        // written by the model, so taken at face value it is the model that
        // decides how long its own sub-agents may run.
        let max_steps =
            args.get("max_steps").and_then(|s| s.as_u64()).map(|s| (s as u32).min(self.max_steps));

        // The list of tasks is written by the model too, and nothing else bounds
        // its length: without this one tool call is tasks x max_steps model
        // calls, and a child that delegates again multiplies that.
        let ceiling = self.rook.config.agent.max_subagents_per_turn;
        let claimed = self.spawned.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |started| (started + tasks.len() <= ceiling).then_some(started + tasks.len()),
        );
        if let Err(started) = claimed {
            return format!(
                "this turn has started {started} sub-agents already and {} more would pass the \
                 limit of {ceiling}. Do the rest here, delegate fewer at a time, or raise \
                 `[agent] max_subagents_per_turn`.",
                tasks.len()
            );
        }

        // Bounded rather than unbounded: the sub-tasks share one token budget and
        // one provider, and a model asked to check twenty things will ask for
        // twenty at once.
        let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(
            self.rook.config.agent.max_parallel_subagents.max(1),
        ));
        let total = tasks.len();
        let (doing, mut steps) = tokio::sync::mpsc::unbounded_channel::<(usize, String)>();
        let running: futures_util::stream::FuturesUnordered<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let limit = limit.clone();
                let inherited = inherited.clone();
                let doing = doing.clone();
                async move {
                    let _permit = limit.acquire().await;
                    (i, self.run_subtask(task, inherited.as_deref(), max_steps, doing, i).await)
                }
            })
            .collect();
        // The senders the children hold are clones; this one would keep the
        // channel open after the last of them finished.
        drop(doing);

        // Unordered so each one is reported the moment it lands, then put back
        // in the order they were asked for: a report that shuffles itself by
        // finishing time is harder to read than the list that produced it.
        let mut results: Vec<Option<_>> = (0..total).map(|_| None).collect();
        let mut done = 0;
        let mut stream = running;
        loop {
            tokio::select! {
                // Biased so a step already waiting is reported before the branch
                // that can end the loop is even polled. Unbiased, `select!`
                // chooses at random among ready branches, and on the last round
                // both are: the children have finished and their last tool names
                // are still in the channel.
                biased;
                Some((i, tool)) = steps.recv() => {
                    on_progress(Progress::Delegating { task: short(&tasks[i]), tool: &tool });
                }
                landed = stream.next() => match landed {
                    Some((i, result)) => {
                        done += 1;
                        on_progress(Progress::Delegated { task: &tasks[i], done, total });
                        results[i] = Some(result);
                    }
                    None => break,
                },
            }
        }
        // Bias orders the two branches; it does not stop the last child sending
        // between the final poll and the break. Every sender is dropped by now,
        // so this drains what is left and cannot block.
        while let Ok((i, tool)) = steps.try_recv() {
            on_progress(Progress::Delegating { task: short(&tasks[i]), tool: &tool });
        }

        let mut report = Vec::with_capacity(total);
        for (task, result) in tasks.iter().zip(results.into_iter().flatten()) {
            match result {
                Ok((id, child)) => {
                    outcome.delegated.push(id.clone());
                    outcome.input_tokens += child.input_tokens;
                    outcome.output_tokens += child.output_tokens;
                    // Or the turn reports the children's input against only its
                    // own cache, and the ratio a person reads is wrong.
                    outcome.cached_tokens += child.cached_tokens;
                    report.push(format!(
                        "### {task}\nsub-agent {id}, {} steps ({}):\n{}",
                        child.steps, child.stopped, child.reply
                    ));
                }
                Err(e) => report.push(format!("### {task}\nfailed: {e}")),
            }
        }
        report.join("\n\n")
    }

    /// Check a claim in a context that did not make it.
    ///
    /// The author is the worst judge of its own work: it knows what it meant,
    /// which is exactly the thing under question. So the checking happens in a
    /// fresh session that is told the claim and nothing about why it should be
    /// believed.
    ///
    /// Two things make this a mechanism rather than an instruction. The checker
    /// is handed a toolbox with the writing tools taken out — it cannot repair
    /// what it was asked to judge, and a verifier that fixes things has stopped
    /// verifying. And it is asked for a verdict in a fixed shape, so "it looks
    /// fine" is a failure to answer rather than an answer.
    ///
    /// It is not isolation: `run_command` can still write, and closing that
    /// needs the sandbox the roadmap describes. What it is is the difference
    /// between a rule the model weighs and a tool it does not have.
    async fn verify(
        &self,
        args: &serde_json::Value,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> String {
        let claim = args.get("claim").and_then(|c| c.as_str()).unwrap_or("").trim();
        if claim.is_empty() {
            return "verify needs a claim to check".into();
        }
        let settles = args.get("settles").and_then(|s| s.as_str()).unwrap_or("").trim();

        let ceiling = self.rook.config.agent.max_subagents_per_turn;
        let claimed = self.spawned.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |started| (started < ceiling).then_some(started + 1),
        );
        if claimed.is_err() {
            return format!(
                "this turn has already started {ceiling} sub-agents, which is the limit \
                 (`[agent] max_subagents_per_turn`)."
            );
        }

        let mut instruction = format!("{VERDICT_INSTRUCTIONS}\n\nThe claim:\n{claim}");
        if !settles.is_empty() {
            instruction.push_str(&format!("\n\nWhat the author says would settle it:\n{settles}"));
        }

        let (doing, mut steps) = tokio::sync::mpsc::unbounded_channel::<(usize, String)>();
        let running = self.run_checker(&instruction, doing);
        tokio::pin!(running);
        let checked = loop {
            tokio::select! {
                biased;
                Some((_, tool)) = steps.recv() => on_progress(Progress::Delegating {
                    task: short(claim),
                    tool: &tool,
                }),
                done = &mut running => break done,
            }
        };
        while let Ok((_, tool)) = steps.try_recv() {
            on_progress(Progress::Delegating { task: short(claim), tool: &tool });
        }

        match checked {
            Ok((id, child)) => {
                outcome.delegated.push(id.clone());
                outcome.input_tokens += child.input_tokens;
                outcome.output_tokens += child.output_tokens;
                outcome.cached_tokens += child.cached_tokens;
                match verdict_in(&child.reply) {
                    Some(_) => format!("checked by {id}:\n{}", child.reply),
                    // Not treated as passing: a check that would not commit is
                    // the outcome this exists to make visible.
                    None => format!(
                        "checked by {id}, and it did not answer with a verdict, so the claim \
                         is unchecked:\n{}",
                        child.reply
                    ),
                }
            }
            Err(e) => format!("could not check {claim:?}: {e}"),
        }
    }

    async fn run_checker(
        &self,
        instruction: &str,
        doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
    ) -> Result<(String, TurnOutcome)> {
        let session = self.rook.fork_for_subtask(self.session, instruction)?;
        let mut child = AgentLoop::new(self.rook, self.provider.clone(), session);
        child.depth = self.depth + 1;
        child.tools = self.tools.without(&["write_file", "edit_file"]);
        child.tool_ctx = self.tool_ctx.clone();
        child.policy = self.policy.clone();
        child.approver = self.approver.clone();
        child.hooks = self.hooks.clone();
        child.servers = self.servers.clone();
        child.spawned = self.spawned.clone();
        child.checking = true;
        // Not lowered the way a delegated errand is: an errand is bounded work
        // to get through, and a check is the judgement the parent could not make
        // for itself.
        child.effort = self.effort;

        let outcome = Box::pin(child.run_with(instruction, |progress| {
            if let Progress::Delta(Delta::ToolCall(call)) = progress {
                let _ = doing.send((0, call.name.clone()));
            }
        }))
        .await?;
        Ok((rook_store::format_session_id(session), outcome))
    }

    async fn run_subtask(
        &self,
        task: &str,
        inherited: Option<&str>,
        max_steps: Option<u32>,
        doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
        index: usize,
    ) -> Result<(String, TurnOutcome)> {
        let session = self.rook.fork_for_subtask(self.session, task)?;
        if let Some(context) = inherited {
            self.rook.log(session, EventKind::Note, "inherited", context).ok();
        }

        let mut child = AgentLoop::new(self.rook, self.provider.clone(), session);
        child.depth = self.depth + 1;
        child.tools = self.tools.clone();
        child.tool_ctx = self.tool_ctx.clone();
        child.policy = self.policy.clone();
        child.approver = self.approver.clone();
        // Deliberately not `ask_via`: a subagent the user did not start should
        // not interrupt them, and its parent is the one holding the context to
        // judge the answer.
        child.hooks = self.hooks.clone();
        child.servers = self.servers.clone();
        child.spawned = self.spawned.clone();
        // A sub-task is a bounded errand, and lower effort means fewer and more
        // consolidated tool calls rather than a worse answer.
        child.effort = rook_llm::Effort::Low;
        if let Some(steps) = max_steps {
            child.max_steps = steps;
        }

        // Boxed because this is `run` calling itself through a tool call. The
        // channel carries only tool names, so it holds at most one short string
        // per step the children are already bounded to.
        let outcome = Box::pin(child.run_with(task, |progress| {
            if let Progress::Delta(Delta::ToolCall(call)) = progress {
                let _ = doing.send((index, call.name.clone()));
            }
        }))
        .await?;
        Ok((rook_store::format_session_id(session), outcome))
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
                let (id, remembered) = (fact.id.clone(), fact.text.clone());
                // Named, not merged: only the model knows whether this replaces
                // the older fact, narrows it, or contradicts it.
                let close = self.rook.similar_facts(&fact.text).unwrap_or_default();
                match self.rook.remember(fact, Some(format!("learned in turn {}", outcome.steps))) {
                    Ok(crate::memory::Learned::ScopedElsewhere(scope)) => format!(
                        "already remembered as [{id}], but scoped to {} — this workspace will \
                         not see it. Remember it with scope \"global\" to widen it.",
                        scope.label()
                    ),
                    Ok(crate::memory::Learned::Unchanged) => format!("already remembered as [{id}]"),
                    Ok(learned) => {
                        if learned == crate::memory::Learned::New {
                            outcome.facts_learned.push(remembered.clone());
                        }
                        let mut reply = format!("remembered as [{id}]");
                        for other in close {
                            reply.push_str(&format!(
                                "\nclose to [{}] {:?} — `forget` it if this replaces it",
                                other.id, other.text
                            ));
                        }
                        reply
                    }
                    Err(e) => format!("could not remember: {e}"),
                }
            }
            FORGET => match self.rook.forget(&string("id"), Some("forgotten by the agent".into())) {
                Ok(Some(fact)) => {
                    outcome.facts_forgotten.push(fact.text.clone());
                    format!("forgot [{}] {}", fact.id, fact.text)
                }
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

    /// Mark the end of the conversation as it stood before this turn, so each
    /// request reuses the whole prior prefix instead of only the system block.
    fn mark_stable_prefix(&self, messages: &mut [Message]) {
        if messages.len() >= 3 {
            let last_stable = messages.len() - 2;
            messages[last_stable].cache = true;
        }
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
        let risk = self.tools.get(&call.name)?.risk(&call.arguments);
        self.gate_risk(&call.name, &call.arguments, risk).await
    }

    /// The same decision for something the toolbox does not own. A pseudo-tool
    /// that changes the machine has to pass here too, or `readonly` means
    /// "readonly except for the tools the loop implements itself".
    async fn gate_risk(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        risk: rook_tools::policy::Risk,
    ) -> Option<String> {
        // The policy runs first so a hook cannot unlock what the deny list
        // forbids; everything else, a hook may override.
        let mut decision = self.policy.decide(&risk);
        if !matches!(decision, Decision::Deny(_)) && !self.hooks.is_empty() {
            let payload = self.payload(serde_json::json!({
                "tool": name,
                "input": arguments,
                "action": risk.describe(),
            }));
            let outcome = self.hooks.run(hooks::Event::PreTool, name, &payload).await;
            if let Some(hooked) = outcome.decision {
                decision = hooked;
            }
        }

        match decision {
            Decision::Allow => None,
            Decision::Deny(why) => Some(format!("refused: {why}")),
            Decision::Ask => match self.approver.ask(name, &risk).await {
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
    /// Returns what the model has to be told, which is nothing when the files
    /// were captured.
    ///
    /// A capture that fails takes the session's undo with it: `session rewind`
    /// restores from these, so a file edited without one is edited for good.
    /// That was a line in the log file, where neither the model nor the user was
    /// looking, and both believed the edit was recoverable.
    fn checkpoint_before(&self, call: &rook_llm::ToolCall) -> Option<String> {
        let tool = self.tools.get(&call.name)?;
        let paths: Vec<std::path::PathBuf> = tool
            .touched_paths(&call.arguments)
            .iter()
            .filter_map(|p| self.tool_ctx.resolve(p).ok())
            .collect();
        if paths.is_empty() {
            return None;
        }
        let failure = self
            .rook
            .checkpoint_paths(self.session, &call.name, &paths, &crate::CaptureLimits::for_skill())
            .err()?;
        let note = format!(
            "no checkpoint was taken first ({failure}), so `rook session rewind` cannot undo this one."
        );
        tracing::warn!("checkpoint before {}: {failure}", call.name);
        self.rook.log(self.session, EventKind::Error, "checkpoint", &note).ok();
        Some(note)
    }
}

/// Replace the earlier part of the session with a summary of it.
///
/// Summarised by the model rather than elided, because an agent that has
/// forgotten what it did twenty turns ago repeats it. If the summary cannot be
/// produced — a provider error, a span that will not fit — it falls back to a
/// marker, since a failed compaction must not wedge the turn.
impl AgentLoop<'_> {
    /// Public so a test can drive it: the alternative is filling a context
    /// window to make it happen, which measures the budget rather than this.
    pub async fn compact_now(&self) {
        self.compact().await
    }

    async fn compact(&self) {
        match self.summarise_span().await {
            Ok(note) => {
                self.rook.log(self.session, EventKind::Compaction, "auto", &note).ok();
            }
            // Not recorded as a compaction: one with no position in it frees no
            // context, so the next turn compacts again, and the one after that,
            // while the log grows an event each time and the recorded position
            // points at something nothing can read.
            Err(e) => {
                self.rook.log(self.session, EventKind::Error, "compaction", &e.to_string()).ok();
            }
        }
    }

    async fn summarise_span(&self) -> Result<String> {
        let (from_seq, previous) = self.rook.last_compaction(self.session)?;
        // Only what the model was actually shown. The log also holds checkpoint
        // manifests, asides and errors, and summarising those spends the budget
        // on bookkeeping and hands back a summary of things it never saw.
        let entries: Vec<_> = self
            .rook
            .transcript(self.session, from_seq, usize::MAX, 8_000)?
            .into_iter()
            .filter(|e| crate::context::kind_reaches_the_model(&e.kind))
            .collect();

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
        if split < 2 && previous.is_none() {
            return Err(CoreError::Other("not enough history to compact".into()));
        }

        let span = &entries[..split];
        let through_seq = span.last().map(|e| e.seq).unwrap_or(from_seq.saturating_sub(1));

        // The previous summary is folded into this one, and is never what gets
        // trimmed. Without it a second compaction covers only the span since
        // the first, and everything before that is simply gone — which is the
        // failure compaction exists to prevent.
        let carried = previous
            .map(|p| format!("A summary of everything before this point:\n\n{p}\n\n---\n\n"))
            .unwrap_or_default();
        let room = (self.budget.usable() / 2).saturating_sub(estimate_tokens(&carried));
        let material = format!("{carried}{}", render_span(span, room));

        // A summary that cannot be produced still leaves a compaction worth
        // recording: the span is dropped from the request either way, and the
        // events themselves are not deleted, so the note says where to read
        // them rather than pretending they are gone.
        let summary = match self.ask_for_summary(material).await {
            Ok(text) => text,
            Err(e) => format!(
                "The transcript before this point could not be summarised ({e}). It is still in \
                 the session log — `rook session show` reads it back — so ask before assuming \
                 what is in it."
            ),
        };

        Ok(serde_json::to_string(&serde_json::json!({
            "through_seq": through_seq,
            "dropped_events": span.len(),
            "summary": summary,
        }))?)
    }
}

impl AgentLoop<'_> {
    async fn ask_for_summary(&self, material: String) -> Result<String> {
        let mut request = Request::new(vec![Message::system(SUMMARY_INSTRUCTIONS), Message::user(material)]);
        // The same reason a sub-agent runs low: condensing a transcript is
        // mechanical, and a turn configured to think hard would otherwise spend
        // that thinking on writing its own summary.
        request.effort = Some(rook_llm::Effort::Low);
        let mut stream = self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
        let mut assembler = Assembler::default();
        while let Some(delta) = stream.next().await {
            assembler
                .push(delta.map_err(|e| CoreError::Other(e.to_string()))?)
                .map_err(|e| CoreError::Other(e.to_string()))?;
        }
        let summary = assembler.finish().message.content;
        match summary.trim().is_empty() {
            true => Err(CoreError::Other("the model returned an empty summary".into())),
            false => Ok(summary),
        }
    }
}

/// What a checker is told before the claim.
///
/// The shape of the answer is part of the instruction because a verdict that can
/// be hedged is one that will be: "looks reasonable" is what a model says when it
/// has read something and run nothing.
const VERDICT_INSTRUCTIONS: &str = "\
You are checking a claim somebody else made. You did not do the work and you have \
no stake in it being true.

Do not take the claim's word for anything. Where something can be run — a build, a \
test, a linter, a command that prints the value in question — run it, and let what \
it printed be the reason. Where it cannot, read the source and quote the part that \
decides it. You have no tools for writing files: you are judging this, not fixing \
it.

End with exactly one of these lines, and nothing after it:

VERDICT: holds
VERDICT: fails
VERDICT: unproven

`unproven` is the honest answer when nothing available settles it — say what \
would. Above that line, give the evidence: the command and its output, or the \
lines you read. Not a summary of your reasoning.";

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

/// One task, or several. Accepting both keeps a single delegation from having to
/// be phrased as a list.
/// Accepts a bare `task` as well as `tasks`, so a model that learnt the
/// single-task shape elsewhere is not refused over a detail of framing.
///
/// One or the other, not both. A live model filled both fields of every call
/// with the same instruction — differing only in whether the function name wore
/// backticks — so every sub-task ran twice, for twice the tokens and twice the
/// wait, and one of each pair was thrown away. Nobody was told.
///
/// Sameness is not judged by meaning. `memory::overlap` answers that question
/// for facts, and measured here it scores those two spellings 1.00 and two
/// genuinely different sub-tasks — `a.py` against `b.py` — 0.94, against a
/// threshold of 0.95. A hundredth of a point between "one task said twice" and
/// "two files to check" is not a distinction to spend real work on.
fn requested_tasks(args: &serde_json::Value) -> Vec<String> {
    let listed: Vec<&str> = args
        .get("tasks")
        .and_then(|t| t.as_array())
        .map(|items| items.iter().filter_map(|t| t.as_str()).collect())
        .unwrap_or_default();
    let single = args.get("task").and_then(|t| t.as_str());

    let mut tasks: Vec<String> = Vec::new();
    for task in listed.iter().copied().chain(single.filter(|_| listed.is_empty())) {
        let task = task.trim();
        if !task.is_empty() && !tasks.iter().any(|kept| kept == task) {
            tasks.push(task.to_string());
        }
    }
    tasks
}

/// A breakpoint below the minimum cacheable prefix only pays the write premium,
/// so a small system prompt is left unmarked.
fn cacheable(message: Message) -> Message {
    const MINIMUM_TOKENS: usize = 1024;
    if estimate_tokens(&message.content) >= MINIMUM_TOKENS { message.cacheable() } else { message }
}

fn measure(messages: &[Message]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content) + 4).sum()
}
