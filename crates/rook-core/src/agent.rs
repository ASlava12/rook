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
use rook_llm::{Assembler, Delta, Message, Provider, Request, Role, ToolSpec};
use rook_store::EventKind;
use rook_tools::policy::{Approver, Decision, Policy, Stance, Unattended};
use rook_tools::{ToolBox, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::path::Path;

use crate::config::Config;
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
    jobs: std::sync::Arc<rook_tools::jobs::Jobs>,
) {
    agent.servers = servers.clone();
    crate::lsp::register(&mut agent.tools, servers);
    for (server, tools) in &mcp.servers {
        agent.tools.register_server(server.clone(), tools.clone());
    }
    // Registered only where there is a registry behind it, the way `ask` is
    // registered only where somebody can answer.
    agent.tools.register(std::sync::Arc::new(rook_tools::jobs::JobTool));
    agent.tool_ctx.jobs = Some(jobs);
}

/// Build the registry of commands left running.
///
/// Exposed for the same reason as [`policy_for`]: it belongs to the front end,
/// and one built per turn would kill everything in it between one turn and the
/// next — which is every background command there is.
pub fn jobs_for(config: &Config) -> std::sync::Arc<rook_tools::jobs::Jobs> {
    std::sync::Arc::new(rook_tools::jobs::Jobs::new(
        config.sandbox.max_background_jobs,
        config.sandbox.max_output_bytes,
    ))
}

/// Build the language-server pool from configuration.
///
/// Exposed for the same reason as [`policy_for`], and more urgently: a pool
/// dropped at the end of a turn takes its running servers with it, and
/// rust-analyzer spends seconds indexing the workspace every time it starts.
pub fn servers_for(config: &Config, workspace: &Path) -> std::sync::Arc<crate::lsp::Servers> {
    crate::lsp::Servers::new(crate::lsp::for_workspace(config, workspace), workspace)
}

/// What the file and command tools are bounded by, from configuration.
///
/// Exposed for the same reason as [`policy_for`]: a turn is not the only thing
/// that runs a tool. `rook mcp serve` runs them for somebody else's client, and
/// two places deciding separately what a tool may write to is how one of them
/// ends up with a boundary the other does not have.
///
/// Configuration and a directory rather than the engine, because that is all it
/// ever read — and `mcp serve` has the first two without opening the store,
/// which is what lets it run beside a daemon that is holding it.
pub fn tool_context(config: &Config, workspace: &Path, output_dir: &Path) -> ToolContext {
    let sandbox = &config.sandbox;
    let mut ctx = ToolContext::new(workspace.to_path_buf());
    ctx.max_output_bytes = sandbox.max_output_bytes;
    ctx.command_timeout = std::time::Duration::from_secs(sandbox.command_timeout_secs);
    ctx.allow_outside_workspace = sandbox.allow_outside_workspace;
    // Outside the workspace on purpose: it is the agent's record of a command,
    // not a file of the project's, and a checkpoint should not capture it.
    ctx.spill_dir = Some(output_dir.to_path_buf());
    ctx.max_spill_bytes = sandbox.max_spill_bytes;
    ctx.max_files_searched = sandbox.max_files_searched;
    ctx
}

/// What the user said while a turn was running.
///
/// A turn is not a wall: somebody watching one go the wrong way should be able
/// to say so without killing it and starting over. What they type is carried
/// here and given to the model at the next step, which is the one place it can
/// go — between an assistant's tool call and its result, no dialect accepts a
/// user message.
#[derive(Default)]
pub struct Interjections(std::sync::Mutex<Vec<String>>);

/// Hand what the user said to every sub-task, and keep it for whoever is
/// waiting on them.
///
/// Said to the conversation while its work is out with the children: it reaches
/// each of them at their next step, and the parent still sees it afterwards —
/// otherwise the one place it lands is the sub-tasks, and the turn that started
/// them never learns anybody spoke. `carried` is what to give back, so a message
/// is broadcast once rather than at every poll.
fn relay(from: &Interjections, to: &[std::sync::Arc<Interjections>], carried: &mut Vec<String>) {
    for text in from.take() {
        for child in to {
            child.say(&text);
        }
        carried.push(text);
    }
}

impl Interjections {
    pub fn say(&self, text: &str) {
        if !text.trim().is_empty() {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(text.trim().to_string());
        }
    }

    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Whether a turn ended because it was done.
///
/// `max_steps` and `max_tokens` are a turn that stopped, not one that finished,
/// and the difference is the whole of what a parent needs to know about a
/// sub-task it did not watch.
fn finished(stopped: &str) -> bool {
    matches!(stopped, "end_turn" | "stop")
}

/// What to show whoever is asked to approve a call.
///
/// A variant rather than a string because building it costs a file read and a
/// diff, and most calls are never asked about.
enum Shown<'a> {
    Nothing,
    Text(&'a str),
    Tool(&'a std::sync::Arc<dyn rook_tools::Tool>),
}

impl Shown<'_> {
    async fn build(&self, ctx: &rook_tools::ToolContext, args: &serde_json::Value) -> Option<String> {
        match self {
            Shown::Nothing => None,
            Shown::Text(text) => Some((*text).to_string()),
            Shown::Tool(tool) => tool.preview(ctx, args).await,
        }
    }
}

/// Build the approval policy from configuration.
///
/// Exposed because "allow this for the rest of the run" has to outlive a single
/// turn: an interactive front end builds one policy for the session and hands it
/// to every loop, or the user is asked again the moment they said not to be.
pub fn policy_for(config: &Config) -> std::sync::Arc<Policy> {
    let sandbox = &config.sandbox;
    let (policy, unusable) = Policy::compile(sandbox.stance, &sandbox.allow, &sandbox.ask, &sandbox.deny);
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

/// What [`AgentLoop::checkpoint_before`] hands back: the claim to hold for the
/// duration of the call, and whatever the model has to be told.
type ClaimedResult<'a> = std::result::Result<(Option<crate::service::Writing<'a>>, Option<String>), String>;

/// The loop's own tools that change something, and are therefore not offered to
/// a checker. `delegate` is here because a checker that can start an agent with
/// the writing tools has not been stopped from writing, only from doing it
/// itself.
const CHANGES_THINGS: &[&str] =
    &[WRITE_SKILL, FIND_SKILL, REMEMBER, FORGET, DELEGATE, SUBAGENTS, STANCE, VERIFY];

/// The same for the toolbox. `run_command` is deliberately absent — verifying a
/// claim means running things — so this stops a checker editing the work it is
/// judging, and is not a sandbox.
const CHANGES_FILES: &[&str] = &["write_file", "edit_file", "delete_file"];

fn system_risk(recipe: &crate::install::Recipe) -> (serde_json::Value, rook_tools::policy::Risk) {
    let command = recipe.system_command().unwrap_or_default();
    (serde_json::json!({ "command": command }), rook_tools::policy::Risk::Execute(command))
}

/// The verdict a checker ended with, if it ended with one of the three.
///
/// Tolerant of how a model dresses the line — bold, a bullet, a different case,
/// a full stop — because the whole mechanism turns on finding it, and a check
/// that ran the build and read the code is not "unchecked" for having written
/// `**VERDICT: holds**`.
///
/// Not tolerant of a fourth word. `VERDICT: probably` is a hedge, and reporting
/// a hedge as a verdict is what asking for one of three exists to prevent.
fn verdict_in(reply: &str) -> Option<&'static str> {
    reply.lines().rev().find_map(verdict_line)
}

fn verdict_line(line: &str) -> Option<&'static str> {
    const DRESSING: [char; 6] = ['*', '_', '#', '-', '>', '`'];
    let line = line.trim().trim_start_matches(|c: char| DRESSING.contains(&c) || c == ' ');
    let (head, rest) = line.split_at_checked("VERDICT:".len())?;
    if !head.eq_ignore_ascii_case("VERDICT:") {
        return None;
    }
    let word = rest
        .trim_start_matches(|c: char| DRESSING.contains(&c) || c == ' ')
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_lowercase();
    ["holds", "fails", "unproven"].into_iter().find(|known| *known == word)
}

/// The reply with its verdict line taken out, for a report that overrules it.
/// A small model reads the last line, and a discounted `holds` left at the
/// bottom of the quotation was read as the answer.
fn without_verdict(reply: &str) -> String {
    let mut lines: Vec<&str> = reply.lines().collect();
    if let Some(at) = lines.iter().rposition(|line| verdict_line(line).is_some()) {
        lines.remove(at);
    }
    lines.join("\n").trim_end().to_string()
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
/// How long a pause was, when it was long enough to matter.
///
/// An hour is the shortest gap worth a line: below it a conversation reads as
/// one sitting, and above it the answer to "have you already done that" starts
/// to depend on when.
fn gap_before(last: i64, now: i64) -> Option<String> {
    const HOUR: i64 = 3600;
    let seconds = now.saturating_sub(last);
    if last == 0 || seconds < HOUR {
        return None;
    }
    Some(match seconds / HOUR {
        hours @ ..24 => format!("{hours} hour{}", plural(hours)),
        hours => {
            let days = hours / 24;
            format!("{days} day{}", plural(days))
        }
    })
}

fn plural(n: i64) -> &'static str {
    match n {
        1 => "",
        _ => "s",
    }
}

/// A turn does not end with its work still out. What the model never collected
/// is waited for and appended rather than dropped at the door: the children
/// have already spent the tokens, and their answers are the reason they ran.
async fn drain_uncollected(nursery: &mut Nursery<'_>, outcome: &mut TurnOutcome) {
    if !nursery.busy() && nursery.taken.iter().all(|taken| *taken) {
        return;
    }
    while let Some((landed, result)) = nursery.running.next().await {
        nursery.landed[landed] = Some(result);
    }
    let mut left = Vec::new();
    for at in 0..nursery.tasks.len() {
        if nursery.taken[at] {
            continue;
        }
        nursery.taken[at] = true;
        if let Some(result) = &nursery.landed[at] {
            left.push(collected(&nursery.tasks[at], result, outcome));
        }
    }
    if !left.is_empty() {
        outcome.reply.push_str(&format!(
            "\n\n(sub-agents this turn started and did not collect)\n\n{}",
            left.join("\n\n")
        ));
    }
}

fn close_open_call(messages: &mut Vec<Message>, open: &mut Option<String>) {
    if let Some(id) = open.take() {
        messages.push(Message::tool_result(id, "no result was recorded: the turn did not finish"));
    }
}

/// The fewest steps a sub-task is given whatever the model asked for: a call,
/// a look at what came back, and an answer.
const SUBTASK_STEPS_FLOOR: u32 = 3;

/// Pseudo-tools: implemented by the loop rather than the toolbox, because they
/// need the agent's own state.
pub const LOAD_SKILL: &str = "load_skill";
pub const WRITE_SKILL: &str = "write_skill";
pub const FIND_SKILL: &str = "find_skill";
pub const REMEMBER: &str = "remember";
pub const FORGET: &str = "forget";
pub const RECALL: &str = "recall";
pub const DELEGATE: &str = "delegate";
pub const STANCE: &str = "stance";
pub const SUBAGENTS: &str = "subagents";
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
    /// Settled by somebody during the turn — a refusal, a stance granted.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// Waiting for somebody: what nobody was there to approve, a goal check that
    /// could not settle. Told apart from decisions because they are different
    /// things to read at the end of a run one was not watching.
    #[serde(default)]
    pub open_questions: Vec<String>,
}

/// Where a language server goes when the agent installs one.
enum How {
    System,
    Fetch,
}

/// What a turn has to tell the person at the end, by what it is.
enum Reported {
    Decision(String),
    Open(String),
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
    /// Collected where refusals happen, which has no `outcome` in hand, and
    /// moved into it when the turn ends.
    reported: std::sync::Mutex<Vec<Reported>>,
    /// Whoever can answer a question, when somebody can. Kept as well as
    /// registered as a tool, because the loop has questions of its own.
    asker: Option<std::sync::Arc<dyn rook_tools::ask::Asker>>,
    /// Consulted whenever the policy says to ask. Refuses by default, so an
    /// unattended run cannot silently do something nobody reviewed.
    pub approver: std::sync::Arc<dyn Approver>,
    /// What the user said while the turn was running, if a front end can take
    /// it. Shared rather than owned because a loop is built per turn and this
    /// has to outlive one.
    pub interjections: std::sync::Arc<Interjections>,
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
        let tool_ctx = tool_context(&rook.config, &rook.workspace, &rook.output_dir);

        // No language servers until a front end hands them over with `equip`.
        // A loop is rebuilt for every turn, so a pool built here is rebuilt with
        // it — and worse, the tools registered from it hold that pool, so what
        // `equip` set afterwards was never what answered. A workspace with no
        // Rust in it was offered rust-analyzer for exactly this reason.
        let servers = crate::lsp::Servers::new(Vec::new(), &rook.workspace);
        let mut tools = ToolBox::standard();
        // Registered rather than gated at the call: a tool the agent is never
        // shown is one it cannot decide to try, and off is the default because
        // this agent's point is that it runs here.
        if rook.config.web.enabled {
            let patience = std::time::Duration::from_secs(rook.config.web.timeout_secs);
            match rook_tools::web::Fetch::new(patience) {
                Ok(fetch) => tools.register(std::sync::Arc::new(fetch)),
                Err(e) => tracing::warn!("web is enabled but unusable: {e}"),
            }
            // Only when an engine is named and usable. Offering a search that
            // fails on its first call teaches the model to stop asking, which is
            // worse than never having offered it.
            let engine = rook_tools::web::Engine::named(&rook.config.web.search, &rook.config.web.search_url);
            if let Some(engine) = engine
                && let Ok(search) = rook_tools::web::Search::new(engine, patience)
            {
                tools.register(std::sync::Arc::new(search));
            }
        }

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
            policy: policy_for(&rook.config),
            hooks: std::sync::Arc::new(hooks),
            servers,
            session_context: std::sync::Mutex::new(None),
            reported: Default::default(),
            asker: None,
            approver: std::sync::Arc::new(Unattended),
            interjections: Default::default(),
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
        self.tools.register(std::sync::Arc::new(rook_tools::ask::AskUser(asker.clone())));
        self.asker = Some(asker);
    }

    /// A language with files here and no server for it, and what the stance
    /// says to do about that: ask, fetch, or use the machine's own installer.
    ///
    /// Once per session. What is installed serves the next session: the pool
    /// of servers is built by the front end before the first turn, which is
    /// what keeps rust-analyzer from re-indexing every turn, and the same fact
    /// means one added now is not in it yet. The report says so.
    async fn offer_language_server(&self) {
        let config = &self.rook.config.agent;
        if !config.install_servers || self.depth > 0 || self.rook.offered_server(self.session).unwrap_or(true)
        {
            return;
        }
        let missing = crate::lsp::missing_here(&self.rook.config, &self.rook.workspace);
        let Some((language, recipe)) = missing
            .iter()
            .find_map(|(language, command)| crate::install::recipe_for(command).map(|r| (*language, r)))
        else {
            return;
        };
        self.rook.note_offered_server(self.session).ok();

        let local = format!("fetch into {}", crate::paths::servers_dir().display());
        let system = recipe.system_command().map(|c| format!("run `{c}`"));
        let how = match self.policy.stance() {
            // Nothing may change the machine, so there is nothing to ask: a
            // question whose every answer the policy then refuses is a wasted
            // one. Said once, for whoever reads the outcome.
            Stance::ReadOnly => {
                self.report(Reported::Open(format!(
                    "this workspace has {language} files and no {} — read-only, so nothing was \
                     installed; `rook lsp install {}` does it by hand",
                    recipe.command, recipe.command
                )));
                return;
            }
            // A person chooses where it goes. Without one there is nobody to
            // choose, and the question waits for whoever reads the outcome.
            Stance::Assist => {
                let Some(asker) = &self.asker else {
                    self.report(Reported::Open(format!(
                        "this workspace has {language} files and no {} — `rook lsp install {}` fetches \
                         one, or run `{}` yourself",
                        recipe.command,
                        recipe.command,
                        system.as_deref().unwrap_or("the system's installer")
                    )));
                    return;
                };
                let mut choices = vec![local.clone()];
                choices.extend(system.clone());
                choices.push("not now".into());
                let question =
                    format!("There are {language} files here and no {}. Install it?", recipe.command);
                let asked = rook_tools::ask::Question { question, choices, multi: false };
                let answer =
                    asker.ask(&[asked]).await.into_iter().next().and_then(|a| a.chosen.into_iter().next());
                match answer.as_deref() {
                    Some(chosen) if chosen == local => {
                        self.answered(&self.fetch_risk(recipe).1);
                        How::Fetch
                    }
                    Some(chosen) if Some(chosen) == system.as_deref() => {
                        self.answered(&system_risk(recipe).1);
                        How::System
                    }
                    _ => {
                        self.report(Reported::Decision(format!(
                            "{} not installed — declined",
                            recipe.command
                        )));
                        return;
                    }
                }
            }
            Stance::Autonomous => How::Fetch,
            Stance::Free => match system {
                Some(_) => How::System,
                None => How::Fetch,
            },
        };

        let done = match how {
            How::System => match self.install_with_system(recipe).await {
                Ok(said) => Ok(said),
                // The machine's way failed; the state directory is the fallback,
                // and the report names both.
                Err(first) => self
                    .install_by_fetching(recipe)
                    .await
                    .map(|done| done.describe())
                    .map_err(|second| format!("{first}; then {second}")),
            },
            How::Fetch => self.install_by_fetching(recipe).await.map(|done| done.describe()),
        };
        match done {
            Ok(said) => {
                let said = format!("{said} — it serves from the next session on");
                self.rook.log(self.session, EventKind::Note, "lsp install", &said).ok();
                self.report(Reported::Decision(said));
            }
            Err(why) => {
                let said = format!(
                    "could not install {}: {why} — `rook lsp install {}` by hand, or say how",
                    recipe.command, recipe.command
                );
                self.rook.log(self.session, EventKind::Note, "lsp install", &said).ok();
                self.report(Reported::Open(said));
            }
        }
    }

    /// A person who chose an install through the asker has answered; the
    /// approver asking about the command or the download it takes would be
    /// the same question twice. Granted for the run rather than once, which
    /// is the grant the policy has, and a deny rule still comes first.
    fn answered(&self, risk: &rook_tools::policy::Risk) {
        self.policy.grant_for_run(&risk.subject());
    }

    async fn install_with_system(
        &self,
        recipe: &crate::install::Recipe,
    ) -> std::result::Result<String, String> {
        let (args, risk) = system_risk(recipe);
        let command = recipe.system_command().ok_or("no system installer for it")?;
        if let Some(refusal) = self.gate_risk("run_command", &args, risk, Shown::Nothing).await {
            return Err(refusal);
        }
        let out = self.tools.call(&self.tool_ctx, "run_command", &args).await.map_err(|e| e.to_string())?;
        match out.is_error {
            false => Ok(format!("installed {} with `{command}`", recipe.command)),
            true => Err(format!("`{command}` failed: {}", short(&out.content))),
        }
    }

    async fn install_by_fetching(
        &self,
        recipe: &crate::install::Recipe,
    ) -> std::result::Result<crate::install::Installed, String> {
        let (args, risk) = self.fetch_risk(recipe);
        if let Some(refusal) = self.gate_risk("lsp install", &args, risk, Shown::Nothing).await {
            return Err(refusal);
        }
        let installer = crate::install::Installer::new(crate::paths::servers_dir())?;
        installer.install(recipe, self.rook.env()).await
    }

    /// What fetching a server is, for the policy: a command for the sources
    /// that are one, a request to the release host for the one that is a
    /// download.
    fn fetch_risk(&self, recipe: &crate::install::Recipe) -> (serde_json::Value, rook_tools::policy::Risk) {
        let into = crate::paths::servers_dir().join(recipe.command).join("current");
        match recipe.command_into(&into) {
            Some((command, _)) => {
                (serde_json::json!({ "command": command }), rook_tools::policy::Risk::Execute(command))
            }
            None => {
                let api = "https://api.github.com";
                (serde_json::json!({ "url": api }), rook_tools::policy::Risk::Network(api.into()))
            }
        }
    }

    /// A server fetched once is one somebody has to remember to update. Past
    /// the configured age, once per session: an autonomous turn fetches again,
    /// one with a person asks, and one with nobody to ask leaves it for whoever
    /// reads the outcome.
    async fn offer_server_update(&self) {
        let config = &self.rook.config.agent;
        let after = config.server_update_after_days;
        if !config.install_servers
            || after == 0
            || self.depth > 0
            || self.rook.offered_update(self.session).unwrap_or(true)
        {
            return;
        }
        let stale = crate::install::stale(
            &crate::paths::servers_dir(),
            std::time::Duration::from_secs(after.saturating_mul(86_400)),
        );
        if stale.is_empty() {
            return;
        }
        self.rook.note_offered_update(self.session).ok();
        let named = stale
            .iter()
            .map(|(r, tag, days)| format!("{} ({tag}, {days} days ago)", r.command))
            .collect::<Vec<_>>();
        let named = named.join(", ");

        let fetch = match (self.policy.stance(), &self.asker) {
            (Stance::Autonomous | Stance::Free, _) => true,
            (Stance::Assist, Some(asker)) => {
                let question = format!("Fetched more than {after} days ago: {named}. Update now?");
                let choices = vec!["update now".to_string(), "not now".to_string()];
                let asked = rook_tools::ask::Question { question, choices, multi: false };
                let answer =
                    asker.ask(&[asked]).await.into_iter().next().and_then(|a| a.chosen.into_iter().next());
                let chosen = answer.as_deref() == Some("update now");
                if chosen {
                    for (recipe, ..) in &stale {
                        self.answered(&self.fetch_risk(recipe).1);
                    }
                }
                chosen
            }
            (Stance::Assist, None) | (Stance::ReadOnly, _) => {
                self.report(Reported::Open(format!(
                    "fetched more than {after} days ago: {named} — `rook lsp update` fetches them again"
                )));
                return;
            }
        };
        if !fetch {
            self.report(Reported::Decision(format!("not updated — declined: {named}")));
            return;
        }
        for (recipe, before, _) in stale {
            let said = match self.install_by_fetching(recipe).await {
                Ok(done) if done.tag == before => Ok(format!("{} already at {before}", recipe.command)),
                Ok(done) => Ok(format!(
                    "{} {before} → {} — it serves from the next session on",
                    recipe.command, done.tag
                )),
                Err(why) => {
                    Err(format!("could not update {}: {why} — `rook lsp update` by hand", recipe.command))
                }
            };
            match said {
                Ok(said) => {
                    self.rook.log(self.session, EventKind::Note, "lsp update", &said).ok();
                    self.report(Reported::Decision(said));
                }
                Err(said) => {
                    self.rook.log(self.session, EventKind::Note, "lsp update", &said).ok();
                    self.report(Reported::Open(said));
                }
            }
        }
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
    /// Whether this turn sends its tools with the request or describes them in
    /// the prompt. Both the provider and the user get a say: the provider knows
    /// its dialect, and only the user knows what the endpoint behind a
    /// `base_url` will accept.
    fn native_tools(&self) -> bool {
        self.rook.config.agent.native_tools && self.provider.supports_tools()
    }

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
        // What the stance means for the model, rather than only for the policy:
        // being refused a call teaches it what it may do, one refusal at a
        // time, and says nothing about whether to decide or to ask.
        s.push_str(match self.policy.stance() {
            Stance::ReadOnly => {
                "Nothing you do may change this machine. Read, run what only reads, and say what \
                 you would change.\n"
            }
            Stance::Assist => {
                "At a fork with more than one defensible answer — a library, a shape, an order of \
                 work — put it to the person with `ask` rather than settling it alone.\n"
            }
            Stance::Autonomous => {
                "Work to the task and the boundaries you were given without asking, and say what \
                 you did.\n"
            }
            Stance::Free => {
                "You were given a goal, and the means are yours to choose. Say what you chose and \
                 why.\n"
            }
        });
        if let Ok(Some(goal)) = self.rook.goal(self.session) {
            s.push_str(&format!("\nThe user's goal for this session: {goal}\n"));
        }
        s.push('\n');
        s.push_str(&format!(
            "## Environment\nos: {} ({} userland)\narch: {}\nshell: {}\nworkspace: {}\n",
            env.os,
            env.userland,
            env.arch,
            // Named for the same reason the userland is: a model that is not
            // told which shell it has writes the one it saw most in training.
            // `;` does not chain commands in `cmd.exe`, `$(…)` is not
            // substitution there, and neither fails loudly — the line runs as
            // something else. Stable per machine, so it costs no cache.
            crate::SHELL,
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

        for standing in crate::instructions::applying_in(
            &self.rook.workspace,
            self.rook.config.agent.max_instructions_bytes,
        ) {
            s.push_str(&format!("\n## {}\n{}\n", standing.from.display(), standing.text.trim_end()));
            // Said rather than silently cut: instructions that stop mid-sentence
            // read as instructions that end there.
            if standing.elided > 0 {
                s.push_str(&format!(
                    "[{} more bytes not shown — past `[agent] max_instructions_bytes`]\n",
                    standing.elided
                ));
            }
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
        if !self.native_tools() {
            s.push_str(&rook_llm::prompted::describe(&self.tool_specs()));
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
        // A replayed conversation reads as continuous however long the gaps
        // were, so a session picked up a week later looks like one paused for a
        // moment — and "did you already run the tests?" has a different answer
        // depending on which it was. Marked rather than stamped per message: a
        // timestamp on every line costs tokens on every request to answer a
        // question nobody asks except across a gap.
        let mut last_at = 0i64;

        for event in events {
            let body = match self.rook.store.get(&event.record.body) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(_) => continue,
            };
            if let Some(gap) = gap_before(last_at, event.record.ts) {
                messages.push(Message::user(format!("[{gap} later]")));
            }
            last_at = event.record.ts;

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
            description: "Write down a repeatable procedure so a later session does not work it \
                          out again. For what took real effort — a build incantation, a platform \
                          quirk — not for what this conversation already says. A script the body \
                          runs goes in `files`, with the tools it needs; `requires` scopes it to \
                          where it holds."
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
                            "description": "What it starts with. `recent` is the last few \
                                            exchanges; anything else is passed verbatim, which is \
                                            where a file it would otherwise go and read belongs."
                        },
                        "max_steps": { "type": "integer" },
                        "wait": {
                            "type": "boolean",
                            "default": true,
                            "description": "False answers at once and leaves them running; \
                                            `subagents` reads and steers them."
                        }
                    }
                }),
            });
            // Only where there is more to ask for.
            if self.policy.stance() < Stance::Free {
                push(ToolSpec {
                    name: STANCE.into(),
                    description: "Ask for more latitude for the rest of this run. A person decides.".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "to": { "type": "string", "enum": ["assist", "autonomous", "free"] },
                            "why": { "type": "string" }
                        },
                        "required": ["to"]
                    }),
                });
            }
            push(ToolSpec {
                name: SUBAGENTS.into(),
                description:
                    "Where sub-agents left running got to, and their results. No id answers for all.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "say": { "type": "string", "description": "A remark it sees at its next step." },
                        "wait_secs": { "type": "integer", "description": "Answer when it lands, or after this." }
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

    /// Everything the model is sent, in order.
    ///
    /// One function because it is asked in two places — at the top of a turn and
    /// again after a compaction — and the two drifted: the second built the
    /// prefix and the history and stopped, which dropped what belongs beside the
    /// prompt exactly when context was tightest.
    ///
    /// The prompt itself is not appended: it was logged before this, so
    /// replaying the session already ends with it, and the log is the only
    /// source of truth for what was said.
    fn request_messages(&self, prompt: &str) -> Result<Vec<Message>> {
        let mut messages = vec![cacheable(Message::system(self.system_prompt()))];
        messages.extend(self.history()?);
        self.mark_stable_prefix(&mut messages);

        // Beside the newest turn rather than in the system block, which must not
        // vary: a date is the example that rule names. A model with a training
        // cutoff otherwise guesses what "now" is, and guesses low.
        let today = format!("Today is {}.", rook_store::today());
        let volatile = match self.recalled(prompt) {
            Some(memory) => format!("{today}\n\n{memory}"),
            None => today,
        };
        messages.insert(messages.len().saturating_sub(1), Message::user(volatile));
        Ok(messages)
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
        self.offer_language_server().await;
        self.offer_server_update().await;
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

        let mut messages = self.request_messages(prompt)?;
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
            decisions: Vec::new(),
            open_questions: Vec::new(),
        };

        // Built once, before the loop borrows `self` mutably: a child's future
        // takes the crew rather than the parent, which is what lets the parent
        // go on stepping while it runs.
        let crew = self.crew();
        let (mut nursery, mut nursery_steps) = Nursery::new(self.rook.config.agent.max_parallel_subagents);
        let mut carrying = tokio::time::interval(std::time::Duration::from_millis(200));

        let mut asked_for_one_script = false;
        let mut asked_to_say = false;
        let mut repeated: std::collections::BTreeMap<(String, String), (String, u32)> =
            std::collections::BTreeMap::new();
        let mut checked_goal = false;
        let mut worth_compacting = true;
        // What the provider last said the request cost, and how many messages
        // that covered. See `measured`.
        let mut anchor: Option<(usize, usize)> = None;
        while outcome.steps < self.max_steps {
            outcome.steps += 1;

            // Before the request rather than after the tool results: this is the
            // one place a user message may go, and it is what makes a turn
            // steerable instead of only stoppable.
            for said in self.interjections.take() {
                self.rook.log(self.session, EventKind::UserMessage, "while running", &said).ok();
                messages.push(Message::user(&said));
            }

            // Once per turn that it achieves something. A span too small to
            // summarise leaves the context where it was, so the next step would
            // ask again, and the step after that — spending a summarisation
            // call each time to stay exactly as full as it already is.
            if worth_compacting && self.budget.needs_compaction(measured(&messages, anchor)) {
                let before = measured(&messages, anchor);
                outcome.compactions += 1;
                self.compact().await;
                // Rebuilt the same way it was built, not a shorter way: this
                // used to assemble the prefix and the history and stop there,
                // so what sits beside the prompt — the date, and whatever was
                // recalled — vanished at the first compaction. It also made the
                // guard below believe the summary had shrunk something.
                messages = self.request_messages(prompt)?;
                // The anchor counted messages that are no longer there.
                anchor = None;
                worth_compacting = measured(&messages, anchor) < before;
            }

            // Compaction summarises history; it cannot make one message smaller.
            // A pasted build log larger than the window would otherwise be sent
            // whole and come back as a provider error about a limit the user
            // never saw.
            let used = measured(&messages, anchor);
            if used > self.budget.usable() {
                return Err(CoreError::Llm(rook_llm::LlmError::ContextOverflow {
                    used,
                    window: self.budget.usable(),
                }));
            }

            let sent = messages.len();
            let mut request = Request::new(messages.clone());
            if self.native_tools() {
                request.tools = self.tool_specs();
            }
            request.effort = Some(self.effort);
            request.cache_ttl = self.rook.config.agent.cache_ttl();
            let mut stream =
                self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
            let mut assembler = Assembler::default();
            // The model call is the long wait in a step, so it is where started
            // sub-agents get to run. Without this they would only advance while
            // the parent was blocked on them, which is the thing being undone.
            let mut carried: Vec<String> = Vec::new();
            loop {
                tokio::select! {
                    biased;
                    Some((at, tool)) = nursery_steps.recv() => {
                        on_progress(Progress::Delegating { task: short(&nursery.tasks[at]), tool: &tool });
                    }
                    _ = carrying.tick() => relay(&self.interjections, &nursery.said, &mut carried),
                    Some((at, result)) = nursery.running.next(), if nursery.busy() => {
                        nursery.landed[at] = Some(result);
                    }
                    delta = stream.next() => {
                        let Some(delta) = delta else { break };
                        let delta = delta.map_err(|e| CoreError::Other(e.to_string()))?;
                        on_progress(Progress::Delta(&delta));
                        assembler.push(delta).map_err(|e| CoreError::Other(e.to_string()))?;
                    }
                }
            }
            for text in carried {
                self.interjections.say(&text);
            }
            if !assembler.reasoning().is_empty() {
                self.rook.log(self.session, EventKind::Reasoning, "", assembler.reasoning()).ok();
            }
            let mut response = assembler.finish();
            // Read back either way. Without native tools the object is the
            // only way a call arrives; with them, a small model still writes
            // one as text some of the time, and the turn ended with nothing
            // called. Only a tool that was offered is taken as called.
            rook_llm::prompted::adopt(&mut response, |name| self.tool_specs().iter().any(|t| t.name == name));

            // Only when it is at least what the text plainly weighs. A provider
            // reporting less than that is not counting what this needs counted —
            // several local servers report a constant — and under-counting is
            // the direction that ends a turn with a limit error.
            let reported = response.usage.input_tokens as usize;
            if reported >= measure(&messages[..sent.min(messages.len())]) {
                anchor = Some((sent, reported));
            }

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

            // What the message carries decides, not what the provider said
            // about it: a dialect that reported `stop` beside two calls had
            // them logged as text and never run.
            if response.message.tool_calls.is_empty() {
                // Said while this was answering, and the answer is now in front
                // of it: the turn is not over, whatever the model thinks. Left
                // in the queue it would reach the next prompt instead, folded
                // into it, which is not where the person put it.
                let said = self.interjections.take();
                if said.is_empty() {
                    drain_uncollected(&mut nursery, &mut outcome).await;
                    // Once. A model that slips twice is one that cannot write
                    // the answer any other way, and a second ask spends a turn
                    // to be told so again.
                    if self.rook.config.agent.one_script && !asked_for_one_script {
                        let known: std::collections::BTreeSet<_> =
                            messages.iter().flat_map(|m| crate::script::scripts(&m.content)).collect();
                        let mine = crate::script::scripts(&response.message.content);
                        if let Some(slip) = crate::script::slipped(&mine, &known) {
                            asked_for_one_script = true;
                            let note = crate::script::say_again(slip, &known);
                            self.rook.log(self.session, EventKind::Note, "one script", &note).ok();
                            messages.push(response.message.clone());
                            messages.push(Message::user(&note));
                            continue;
                        }
                    }
                    let did_something = !outcome.tools_called.is_empty() || !outcome.delegated.is_empty();
                    // Once. A small model does the work and stops without a
                    // word — read the file, found the number, said nothing —
                    // and every front end renders that as a hang. Asked, it
                    // says what it found; asked twice, it had nothing to say.
                    if did_something && response.message.content.trim().is_empty() && !asked_to_say {
                        asked_to_say = true;
                        self.rook.log(self.session, EventKind::Note, "say it", SAY_IT).ok();
                        messages.push(response.message.clone());
                        messages.push(Message::user(SAY_IT));
                        continue;
                    }
                    // Autonomy is a task and its boundaries, and this is the
                    // boundary being held: before the turn ends, a checker asks
                    // whether the goal is met and whether anything forbidden was
                    // done. Once, and only for a turn that did something.
                    if self.policy.stance() == Stance::Autonomous
                        && !checked_goal
                        && did_something
                        && let Ok(Some(goal)) = self.rook.goal(self.session)
                    {
                        checked_goal = true;
                        let (report, verdict) = self.goal_check(&goal, &mut outcome, &mut on_progress).await;
                        self.rook.log(self.session, EventKind::Note, "goal check", &report).ok();
                        match verdict {
                            Some("fails") => {
                                let told = format!(
                                    "Checked against the goal before finishing, and the check \
                                     fails:\n\n{report}\n\nPut it right, and say what was wrong."
                                );
                                messages.push(response.message.clone());
                                messages.push(Message::user(&told));
                                continue;
                            }
                            Some("holds") => {}
                            // Not a pass and not a fail: the one thing this exists
                            // to make visible to the person, rather than to bury.
                            _ => self.report(Reported::Open(format!(
                                "whether the goal was met could not be settled: {report}"
                            ))),
                        }
                    }
                    outcome.stopped = response.stop_reason.as_str().into();
                    // A turn that said nothing at any step ends in silence every
                    // front end renders as a hang. Set here rather than logged:
                    // the transcript records what the model said, and it said
                    // nothing.
                    if outcome.reply.is_empty() {
                        outcome.reply = format!(
                            "(the model ended the turn without saying anything — {})",
                            outcome.stopped
                        );
                    }
                    self.settle_reports(&mut outcome);
                    self.finish(&outcome).await;
                    return Ok(outcome);
                }
                messages.push(response.message.clone());
                for text in said {
                    self.rook.log(self.session, EventKind::UserMessage, "while running", &text).ok();
                    messages.push(Message::user(&text));
                }
                continue;
            }

            // Two calls given one id would be replayed as two results carrying
            // it, which every dialect rejects — so the model's mistake would
            // come back as an opaque error from the provider, after the work had
            // been done twice.
            let mut asked = response.message.clone();
            let mut dropped: Vec<(String, String)> = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            asked.tool_calls.retain(|call| {
                let first = seen.insert(call.id.clone());
                if !first {
                    dropped.push((call.id.clone(), call.name.clone()));
                }
                first
            });
            messages.push(asked.clone());

            for call in &asked.tool_calls {
                // The same call answered the same way twice is a loop, not a
                // question: a model verified one claim five times over, told
                // `fails` each time, until the sub-agent ceiling ended it. The
                // third is refused and pointed at the answer it has. Same
                // result is the test, so a command run again after an edit is
                // not caught by it.
                let key = (call.name.clone(), call.arguments.to_string());
                let (mut result, failed) = match repeated.get(&key) {
                    Some((_, times)) if *times >= 2 => (
                        format!(
                            "`{}` with these same arguments was made {times} times this turn and \
                             answered the same each time; the answer is above — act on it, or ask \
                             something different",
                            call.name
                        ),
                        true,
                    ),
                    _ => {
                        let done =
                            self.dispatch(call, &mut outcome, &mut on_progress, &crew, &mut nursery).await;
                        // A call that changed the workspace makes every earlier
                        // answer stale: the file read twice reads differently
                        // after the edit, and the count starts over. Not the
                        // loop's own tools — a claim verified twice to the same
                        // verdict is the loop this exists for.
                        if CHANGES_FILES.contains(&call.name.as_str()) || call.name == "run_command" {
                            repeated.clear();
                        }
                        repeated
                            .entry(key)
                            .and_modify(|(last, times)| {
                                *times = if *last == done.0 { *times + 1 } else { 1 };
                                *last = done.0.clone();
                            })
                            .or_insert((done.0.clone(), 1));
                        done
                    }
                };
                on_progress(Progress::ToolDone { name: &call.name, failed });
                for (_, name) in dropped.iter().filter(|(id, _)| *id == call.id) {
                    result.push_str(&format!(
                        "\n\n[`{name}` came with this same call id and was not made — one id per \
                         call, and it can be asked for again]"
                    ));
                }
                messages.push(Message::tool_result(&call.id, result));
            }
        }

        outcome.stopped = "max_steps".into();
        // The limit is the model's, not the children's: what they were still
        // doing is waited for here as it is at the end of a turn that finished.
        drain_uncollected(&mut nursery, &mut outcome).await;
        // A turn that ran out of steps with a call as its last word has done
        // work nobody was told about. One more call, with nothing to reach
        // for, so the turn ends on what it found rather than on the limit.
        if outcome.reply.trim().is_empty() && !outcome.tools_called.is_empty() {
            messages.push(Message::user(OUT_OF_STEPS));
            self.rook.log(self.session, EventKind::Note, "out of steps", OUT_OF_STEPS).ok();
            let mut request = Request::new(messages);
            request.effort = Some(self.effort);
            request.cache_ttl = self.rook.config.agent.cache_ttl();
            let mut stream =
                self.provider.stream(request).await.map_err(|e| CoreError::Other(e.to_string()))?;
            let mut assembler = Assembler::default();
            while let Some(delta) = stream.next().await {
                let delta = delta.map_err(|e| CoreError::Other(e.to_string()))?;
                on_progress(Progress::Delta(&delta));
                assembler.push(delta).map_err(|e| CoreError::Other(e.to_string()))?;
            }
            let response = assembler.finish();
            outcome.input_tokens += response.usage.input_tokens;
            outcome.output_tokens += response.usage.output_tokens;
            outcome.cached_tokens += response.usage.cache_read_tokens;
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
                outcome.reply = response.message.content;
            }
        }
        // For a front end, which renders silence as a hang. A child's silence
        // is the parent's to report, by what the child called.
        if self.depth == 0 && outcome.reply.is_empty() {
            outcome.reply =
                format!("(the model ended the turn without saying anything — {})", outcome.stopped);
        }
        self.settle_reports(&mut outcome);
        self.finish(&outcome).await;
        Ok(outcome)
    }

    /// The text the model sees, and whether the call failed — which the outcome
    /// knows and the text only hints at.
    async fn dispatch<'f>(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
        crew: &'f Crew<'a>,
        nursery: &mut Nursery<'f>,
    ) -> (String, bool)
    where
        'a: 'f,
    {
        self.rook.log(self.session, EventKind::ToolCall, &call.name, &call.arguments.to_string()).ok();

        // Refused before it is recorded, and the order is the point: a verdict
        // from a checker that called nothing is reported as unproven, and a
        // reach for a tool it was never given is not a call it made. Counting it
        // would let a check reach for `write_skill`, be refused, and have its
        // recollection stand as evidence.
        if self.checking && CHANGES_THINGS.contains(&call.name.as_str()) {
            let refusal = format!("{}: a check may not change anything", call.name);
            return (refusal, true);
        }

        // Before the gate, not after: a call whose arguments did not parse has
        // no risk worth weighing, and asking somebody to approve running the
        // empty string is a question with no answer.
        if let Some(unusable) = rook_tools::unusable_arguments(&call.name, &call.arguments) {
            let refusal = unusable.to_string();
            self.rook.log(self.session, EventKind::Error, &call.name, &refusal).ok();
            return (refusal, true);
        }

        outcome.tools_called.push(call.name.clone());

        if call.name == VERIFY {
            let text = self.verify(&call.arguments, outcome, on_progress).await;
            self.rook.log(self.session, EventKind::ToolResult, VERIFY, &text).ok();
            return (text, false);
        }

        if call.name == DELEGATE {
            let text = self.delegate(&call.arguments, outcome, on_progress, crew, nursery).await;
            self.rook.log(self.session, EventKind::ToolResult, DELEGATE, &text).ok();
            return (text, false);
        }

        if call.name == STANCE {
            let text = self.request_stance(&call.arguments).await;
            self.rook.log(self.session, EventKind::ToolResult, STANCE, &text).ok();
            return (text, false);
        }

        if call.name == SUBAGENTS {
            let text = self.subagents(&call.arguments, outcome, nursery).await;
            self.rook.log(self.session, EventKind::ToolResult, SUBAGENTS, &text).ok();
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
            if let Some(refusal) = self.gate_risk(FIND_SKILL, &call.arguments, risk, Shown::Nothing).await {
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
            // The body is the whole of what it would write, so the body is the
            // preview: there is nothing on disk to diff it against.
            let body = call.arguments.get("body").and_then(|b| b.as_str());
            let shown = body.map(Shown::Text).unwrap_or(Shown::Nothing);
            if let Some(refusal) = self.gate_risk(WRITE_SKILL, &call.arguments, risk, shown).await {
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

        // `_writing` is held across the call and dropped when it returns: the
        // window it protects is the one between the checkpoint and the write.
        let (_writing, unprotected) = match self.checkpoint_before(call) {
            Ok(pair) => pair,
            Err(refusal) => {
                self.rook.log(self.session, EventKind::ToolResult, &call.name, &refusal).ok();
                return (refusal, true);
            }
        };
        let outcome = match self.tools.call(&self.tool_ctx, &call.name, &call.arguments).await {
            Ok(o) => o,
            Err(e) => rook_tools::ToolOutcome::error(format!("tool error: {e}")),
        };
        // A checker's reading is not recorded. The registry holds one holder
        // per path, so a look from any session makes every other session's
        // overwrite stale until it looks again — right for a sub-task, which
        // may have changed the file, and a false alarm from a loop that has no
        // writing tools: the goal check read a file and the turn it was
        // checking was then refused the fix the check had just asked for.
        if !outcome.is_error
            && !self.checking
            && let Some(tool) = self.tools.get(&call.name)
        {
            let seen: Vec<std::path::PathBuf> = tool
                .observed_paths(&call.arguments)
                .iter()
                .filter_map(|p| self.tool_ctx.resolve(p).ok())
                .collect();
            self.rook.touched(self.session, &seen);
        }
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
    async fn delegate<'f>(
        &self,
        args: &serde_json::Value,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
        crew: &'f Crew<'a>,
        nursery: &mut Nursery<'f>,
    ) -> String
    where
        'a: 'f,
    {
        let tasks = match requested_tasks(args) {
            Ok(tasks) => tasks,
            Err(why) => return why,
        };

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
        // decides how long its own sub-agents may run. And never below what a
        // task needs — a call, a look at what came back, an answer: a model
        // wrote `max_steps: 1`, and its sub-agent read the file and had no
        // step left to say what it read.
        let max_steps = args
            .get("max_steps")
            .and_then(|s| s.as_u64())
            .map(|s| (s as u32).max(SUBTASK_STEPS_FLOOR).min(self.max_steps));

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

        // Started and left to run: the turn goes on, and `subagents` is how the
        // parent looks at them, redirects one, and takes their results.
        if !args.get("wait").and_then(|w| w.as_bool()).unwrap_or(true) {
            let names: Vec<String> =
                tasks.iter().map(|task| nursery.start(crew, task, inherited.clone(), max_steps)).collect();
            return format!(
                "started: {}. `{SUBAGENTS}` says where they got to, passes one a remark, and \
                 hands back what they answer.",
                names.join(", ")
            );
        }

        // Bounded rather than unbounded: the sub-tasks share one token budget and
        // one provider, and a model asked to check twenty things will ask for
        // twenty at once.
        let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(
            self.rook.config.agent.max_parallel_subagents.max(1),
        ));
        let total = tasks.len();
        // One queue each, filled from the parent's while they run.
        let relayed: Vec<std::sync::Arc<Interjections>> = (0..total).map(|_| Default::default()).collect();
        let (doing, mut steps) = tokio::sync::mpsc::unbounded_channel::<(usize, String)>();
        let crew = self.crew();
        let crew = &crew;
        let running: futures_util::stream::FuturesUnordered<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let limit = limit.clone();
                let inherited = inherited.clone();
                let doing = doing.clone();
                let said = relayed[i].clone();
                async move {
                    let _permit = limit.acquire().await;
                    (i, crew.run_subtask(task, inherited.as_deref(), max_steps, doing, i, said).await)
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
        let mut carried: Vec<String> = Vec::new();
        let mut carrying = tokio::time::interval(std::time::Duration::from_millis(200));
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
                // Said to the conversation while its work is out with the
                // children. It reaches each of them at their next step, and is
                // kept for the parent too — otherwise the one place it lands is
                // the sub-tasks, and the turn that started them never learns
                // anybody spoke.
                _ = carrying.tick() => relay(&self.interjections, &relayed, &mut carried),
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
        for text in carried {
            self.interjections.say(&text);
        }

        let report: Vec<String> = tasks
            .iter()
            .zip(results.into_iter().flatten())
            .map(|(task, result)| collected(task, &result, outcome))
            .collect();
        report.join("\n\n")
    }

    /// Reading, steering and collecting the sub-agents this turn started.
    ///
    /// `delegate` waits, so by the time the parent could speak its children
    /// have finished. This is the other half: what they are doing, a remark to
    /// one of them, and their results when it wants them.
    async fn subagents(
        &self,
        args: &serde_json::Value,
        outcome: &mut TurnOutcome,
        nursery: &mut Nursery<'_>,
    ) -> String {
        if nursery.tasks.is_empty() {
            return format!(
                "nothing was started this turn. `{DELEGATE}` with `wait: false` starts one and \
                 answers with its name."
            );
        }
        let at = match args.get("id").and_then(|i| i.as_str()) {
            None => None,
            Some(name) => match nursery.index_of(name) {
                Some(at) => Some(at),
                None => {
                    return format!("no sub-agent {name}. Started: {}", nursery.names().join(", "));
                }
            },
        };

        if let Some(text) = args.get("say").and_then(|s| s.as_str()).filter(|t| !t.trim().is_empty()) {
            let Some(at) = at else {
                return "say needs the id of the one to say it to".into();
            };
            if nursery.landed[at].is_some() {
                return format!("{} has finished; nothing is listening.", name_of(at));
            }
            nursery.said[at].say(text);
            return format!("{} sees it at its next step.", name_of(at));
        }

        // Bounded by what a command in the foreground would have been given,
        // for the same reason `job` is: a wait the model wrote is a wait the
        // model decides the length of.
        if let Some(secs) = args.get("wait_secs").and_then(|w| w.as_u64()) {
            let patience = std::time::Duration::from_secs(secs).min(self.tool_ctx.command_timeout);
            let deadline = tokio::time::Instant::now() + patience;
            while nursery.busy() && !nursery.all_in(at) {
                let Ok(Some((landed, result))) =
                    tokio::time::timeout_at(deadline, nursery.running.next()).await
                else {
                    break;
                };
                nursery.landed[landed] = Some(result);
            }
        }

        let wanted: Vec<usize> = match at {
            Some(at) => vec![at],
            None => (0..nursery.tasks.len()).collect(),
        };
        let mut lines = Vec::with_capacity(wanted.len());
        for at in wanted {
            match (&nursery.landed[at], nursery.taken[at]) {
                (Some(result), false) => {
                    lines.push(collected(&nursery.tasks[at], result, outcome));
                    nursery.taken[at] = true;
                }
                _ => lines.push(nursery.how_it_is_going(at)),
            }
        }
        lines.join("\n\n")
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
        self.check(claim, settles, outcome, on_progress).await.0
    }

    /// The report and the verdict it carries. The report is what a model reads;
    /// the verdict is what the loop acts on when it asked the question itself.
    async fn check(
        &self,
        claim: &str,
        settles: &str,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> (String, Option<&'static str>) {
        let ceiling = self.rook.config.agent.max_subagents_per_turn;
        let claimed = self.spawned.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |started| (started < ceiling).then_some(started + 1),
        );
        if claimed.is_err() {
            let refused = format!(
                "this turn has already started {ceiling} sub-agents, which is the limit \
                 (`[agent] max_subagents_per_turn`)."
            );
            return (refused, None);
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
                    // A verdict from a checker that ran nothing and read nothing
                    // is the model's memory with a label on it, which is exactly
                    // what asking a second agent was supposed to get past. It is
                    // reported as unproven whatever it said.
                    Some(verdict) if verdict != "unproven" && child.tools_called.is_empty() => (
                        format!(
                            "checked by {id}, which reached for nothing — no command, no file, \
                             no page — so its `{verdict}` is recollection rather than a check:\n{}\n\n\
                             VERDICT: unproven — nothing was run or read to settle it",
                            without_verdict(&child.reply)
                        ),
                        Some("unproven"),
                    ),
                    Some(verdict) => (format!("checked by {id}:\n{}", child.reply), Some(verdict)),
                    // Not treated as passing: a check that would not commit is
                    // the outcome this exists to make visible.
                    None => (
                        format!(
                            "checked by {id}, and it did not answer with a verdict:\n{}\n\n\
                             The claim is unchecked — neither held nor failed",
                            child.reply
                        ),
                        None,
                    ),
                }
            }
            Err(e) => (format!("could not check {claim:?}: {e}"), None),
        }
    }

    /// Whether the goal is met, asked of a checker before an autonomous turn
    /// ends — and whether anything the person said not to do was done anyway.
    ///
    /// A turn is the unit because it is the last moment the agent can still
    /// act: told afterwards, it can only apologise. Asked of a checker rather
    /// than of the turn itself, because the author is the worst judge of its
    /// own work.
    async fn goal_check(
        &self,
        goal: &str,
        outcome: &mut TurnOutcome,
        on_progress: &mut impl FnMut(Progress<'_>),
    ) -> (String, Option<&'static str>) {
        let claim = format!(
            "The person set this goal for the session, and the agent has just finished a turn \
             towards it:\n\n{goal}\n\nTwo questions, both answered from what is on disk and what \
             runs rather than from the agent's own account: has the goal been met, and was anything \
             the person asked not to do done anyway? `holds` means both are as they should be. \
             `fails` means the goal is not met, or something the person forbade was done — say \
             which, and what would put it right."
        );
        self.check(&claim, "", outcome, on_progress).await
    }

    async fn run_checker(
        &self,
        instruction: &str,
        doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
    ) -> Result<(String, TurnOutcome)> {
        let session = self.rook.fork_for_subtask(self.session, instruction)?;
        let mut child = AgentLoop::new(self.rook, self.provider.clone(), session);
        child.depth = self.depth + 1;
        child.tools = self.tools.without(CHANGES_FILES);
        child.tool_ctx = self.tool_ctx.clone();
        child.policy = self.policy.clone();
        child.approver = self.approver.clone();
        child.hooks = self.hooks.clone();
        child.servers = self.servers.clone();
        child.spawned = self.spawned.clone();
        // No relay of what the user says mid-turn, unlike a sub-task: a checker
        // is asked to be the one party with no stake in the answer, and a remark
        // from the person whose work is being checked is a stake.
        child.checking = true;
        // Not lowered the way a delegated errand is: an errand is bounded work
        // to get through, and a check is the judgement the parent could not make
        // for itself.
        child.effort = self.effort;

        let mut relay = |progress: Progress<'_>| {
            if let Progress::Delta(Delta::ToolCall(call)) = progress {
                let _ = doing.send((0, call.name.clone()));
            }
        };
        let mut outcome = Box::pin(child.run_with(instruction, &mut relay)).await?;
        // A small model narrates what it would run and stops, or reasons its
        // way to the end and forgets the line. Asked once, in the same session,
        // it usually does what it said; a second silence is reported as one.
        if verdict_in(&outcome.reply).is_none() {
            let finished = Box::pin(child.run_with(VERDICT_NUDGE, &mut relay)).await?;
            outcome.reply = finished.reply;
            outcome.stopped = finished.stopped;
            outcome.steps += finished.steps;
            outcome.input_tokens += finished.input_tokens;
            outcome.output_tokens += finished.output_tokens;
            outcome.cached_tokens += finished.cached_tokens;
            outcome.tools_called.extend(finished.tools_called);
        }
        Ok((rook_store::format_session_id(session), outcome))
    }

    /// What a sub-task needs from the turn that started it, owned.
    ///
    /// Taken out of the loop rather than read from it so a child's future
    /// borrows the engine and not the parent: the parent has to keep stepping
    /// while they run, and a future holding `&self` freezes it.
    fn crew(&self) -> Crew<'a> {
        Crew {
            rook: self.rook,
            provider: self.provider.clone(),
            tools: self.tools.clone(),
            tool_ctx: self.tool_ctx.clone(),
            policy: self.policy.clone(),
            approver: self.approver.clone(),
            hooks: self.hooks.clone(),
            servers: self.servers.clone(),
            spawned: self.spawned.clone(),
            parent: self.session,
            depth: self.depth,
            max_steps: self.max_steps,
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
    #[doc(hidden)]
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
        let (policy, _) =
            Policy::compile(rook_tools::policy::Stance::Autonomous, &sandbox.allow, &[], &sandbox.deny);
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
        self.gate_risk(&call.name, &call.arguments, risk, Shown::Tool(tool)).await
    }

    /// The same decision for something the toolbox does not own. A pseudo-tool
    /// that changes the machine has to pass here too, or `readonly` means
    /// "readonly except for the tools the loop implements itself".
    async fn gate_risk(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        risk: rook_tools::policy::Risk,
        shown: Shown<'_>,
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
            // Built here rather than earlier because a hook may turn an allowed
            // call into one somebody is asked about, and a diff of a call nobody
            // is asked about is work nobody reads.
            Decision::Ask => match self
                .approver
                .ask(name, &risk, shown.build(&self.tool_ctx, arguments).await.as_deref())
                .await
            {
                rook_tools::policy::Approval::Once => None,
                rook_tools::policy::Approval::ForRun => {
                    self.policy.grant_for_run(&risk.subject());
                    None
                }
                rook_tools::policy::Approval::Deny(why) => {
                    self.report(Reported::Decision(format!("{name}: {} — declined", risk.describe())));
                    Some(format!("refused: {why}"))
                }
                rook_tools::policy::Approval::Unanswered(why) => {
                    self.report(Reported::Open(format!(
                        "{name} wanted to {}, and nobody was here to say",
                        risk.describe()
                    )));
                    Some(format!("refused: {why}"))
                }
            },
        }
    }

    fn report(&self, what: Reported) {
        if let Ok(mut list) = self.reported.lock() {
            list.push(what);
        }
    }

    /// Into the outcome, which is what every front end reads at the end.
    fn settle_reports(&self, outcome: &mut TurnOutcome) {
        let Ok(mut list) = self.reported.lock() else { return };
        for what in list.drain(..) {
            match what {
                Reported::Decision(text) => outcome.decisions.push(text),
                Reported::Open(text) => outcome.open_questions.push(text),
            }
        }
    }

    /// The agent asking for more latitude. A person grants it or nobody can;
    /// the agent never raises its own stance.
    async fn request_stance(&self, args: &serde_json::Value) -> String {
        let Some(to) = args.get("to").and_then(|t| t.as_str()).and_then(Stance::parse) else {
            let names: Vec<&str> = Stance::ALL.iter().map(|s| s.as_str()).collect();
            return format!("stance needs `to`: one of {}", names.join(", "));
        };
        let now = self.policy.stance();
        if to <= now {
            return format!("already at `{}`; a stance is only ever asked up", now.as_str());
        }
        let why = args.get("why").and_then(|w| w.as_str()).unwrap_or("").trim().to_string();
        let shown = match why.is_empty() {
            true => Shown::Nothing,
            false => Shown::Text(&why),
        };
        match self.gate_risk(STANCE, args, rook_tools::policy::Risk::Stance(to), shown).await {
            Some(refusal) => refusal,
            None => {
                self.policy.set_stance(to);
                let note = format!("stance raised to `{}` for the rest of the run", to.as_str());
                self.rook.log(self.session, EventKind::Note, "stance", &note).ok();
                self.report(Reported::Decision(note.clone()));
                note
            }
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
    fn checkpoint_before(&self, call: &rook_llm::ToolCall) -> ClaimedResult<'_> {
        // Not a tool of the toolbox — the loop's own, which write through their
        // own paths and take their own checkpoints.
        let Some(tool) = self.tools.get(&call.name) else { return Ok((None, None)) };
        let paths: Vec<std::path::PathBuf> = tool
            .touched_paths(&call.arguments)
            .iter()
            .filter_map(|p| self.tool_ctx.resolve(p).ok())
            .collect();
        if paths.is_empty() {
            return Ok((None, None));
        }
        if tool.overwrites()
            && let Some(unseen) = self.rook.overwriting_unseen(self.session, &paths)
        {
            return Err(unseen);
        }
        // The paths a checkpoint is about to capture are exactly the ones
        // another turn in this project must not be writing, so the claim is
        // asked for here, where they are already known.
        let held = self.rook.writing(self.session, &paths).map_err(|e| e.to_string())?;
        // Writing it makes this turn the one that has seen it.
        self.rook.touched(self.session, &paths);

        let Some(failure) = self
            .rook
            .checkpoint_paths(self.session, &call.name, &paths, &crate::CaptureLimits::for_skill())
            .err()
        else {
            return Ok((Some(held), None));
        };
        let note = format!(
            "no checkpoint was taken first ({failure}), so `rook session rewind` cannot undo this one."
        );
        tracing::warn!("checkpoint before {}: {failure}", call.name);
        self.rook.log(self.session, EventKind::Error, "checkpoint", &note).ok();
        Ok((Some(held), Some(note)))
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
    #[doc(hidden)]
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
        // A model that wrote the summary into its reasoning channel and left
        // `content` empty has still written one, and discarding it for the note
        // that says the span could not be summarised throws away a transcript
        // that exists.
        let thought = assembler.reasoning().trim().to_string();
        let said = assembler.finish().message.content;
        let summary = if said.trim().is_empty() { thought } else { said };
        match summary.trim().is_empty() {
            true => Err(CoreError::Other("the model returned an empty summary".into())),
            false => Ok(summary),
        }
    }
}

struct Crew<'a> {
    rook: &'a Rook,
    provider: std::sync::Arc<dyn Provider>,
    tools: ToolBox,
    tool_ctx: ToolContext,
    policy: std::sync::Arc<Policy>,
    approver: std::sync::Arc<dyn Approver>,
    hooks: std::sync::Arc<Hooks>,
    servers: std::sync::Arc<crate::lsp::Servers>,
    spawned: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    parent: u128,
    depth: u32,
    max_steps: u32,
}

impl Crew<'_> {
    async fn run_subtask(
        &self,
        task: &str,
        inherited: Option<&str>,
        max_steps: Option<u32>,
        doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
        index: usize,
        said: std::sync::Arc<Interjections>,
    ) -> Result<(String, TurnOutcome)> {
        let session = self.rook.fork_for_subtask(self.parent, task)?;
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
        // Its own queue, not the parent's: what the user says while several of
        // these run has to reach all of them, and taking from one queue would
        // give it to whichever child stepped first.
        child.interjections = said;
        // A sub-task is a bounded errand, and lower effort means fewer and more
        // consolidated tool calls rather than a worse answer.
        child.effort = rook_llm::Effort::Low;
        child.max_steps = max_steps.unwrap_or(self.max_steps);

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
}

/// Sub-tasks a turn has started and not yet collected.
///
/// Held across the parent's steps, which is the whole difference between this
/// and `delegate`: a parent blocked on its children can neither look at them,
/// nor say anything to them, nor do anything else while they run.
/// What a sub-task came back with: the session it ran in and what it did, or
/// why it could not.
type Landed = Result<(String, TurnOutcome)>;

/// One sub-task, running. Boxed because this is the turn calling itself.
type Child<'f> = std::pin::Pin<Box<dyn std::future::Future<Output = (usize, Landed)> + Send + 'f>>;

struct Nursery<'f> {
    running: futures_util::stream::FuturesUnordered<Child<'f>>,
    tasks: Vec<String>,
    /// One queue each, so a remark reaches every child rather than whichever
    /// stepped first.
    said: Vec<std::sync::Arc<Interjections>>,
    landed: Vec<Option<Landed>>,
    /// Reported to the model already. A result handed over twice is a turn
    /// charged twice for the same tokens.
    taken: Vec<bool>,
    /// Shared with the blocking path for the same reason it has one: the
    /// sub-tasks share a provider and a token budget.
    limit: std::sync::Arc<tokio::sync::Semaphore>,
    doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
}

impl<'f> Nursery<'f> {
    fn new(parallel: usize) -> (Self, tokio::sync::mpsc::UnboundedReceiver<(usize, String)>) {
        let (doing, steps) = tokio::sync::mpsc::unbounded_channel();
        let nursery = Self {
            running: Default::default(),
            tasks: Vec::new(),
            said: Vec::new(),
            landed: Vec::new(),
            taken: Vec::new(),
            limit: std::sync::Arc::new(tokio::sync::Semaphore::new(parallel.max(1))),
            doing,
        };
        (nursery, steps)
    }

    /// Starts one, and answers with the name the model will call it by.
    fn start<'a: 'f>(
        &mut self,
        crew: &'f Crew<'a>,
        task: &str,
        inherited: Option<String>,
        max_steps: Option<u32>,
    ) -> String {
        let at = self.tasks.len();
        let said: std::sync::Arc<Interjections> = Default::default();
        self.tasks.push(task.to_string());
        self.said.push(said.clone());
        self.landed.push(None);
        self.taken.push(false);
        let task = task.to_string();
        let (limit, doing) = (self.limit.clone(), self.doing.clone());
        self.running.push(Box::pin(async move {
            let _permit = limit.acquire().await;
            (at, crew.run_subtask(&task, inherited.as_deref(), max_steps, doing, at, said).await)
        }));
        name_of(at)
    }

    fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        (0..self.tasks.len()).find(|at| name_of(*at) == name)
    }

    /// Where a child got to, as far as anyone here can see: a running one is
    /// known by its task and nothing else until it lands.
    /// Whether there is anything left to wait for: one named child, or all.
    fn all_in(&self, at: Option<usize>) -> bool {
        match at {
            Some(at) => self.landed[at].is_some(),
            None => self.landed.iter().all(Option::is_some),
        }
    }

    fn names(&self) -> Vec<String> {
        (0..self.tasks.len()).map(name_of).collect()
    }

    fn how_it_is_going(&self, at: usize) -> String {
        if self.taken[at] {
            return format!("{}: already reported", name_of(at));
        }
        match &self.landed[at] {
            None => format!("{}: still running — {}", name_of(at), short(&self.tasks[at])),
            Some(Ok((id, child))) => {
                format!("{}: {} after {} steps — {}", name_of(at), child.stopped, child.steps, id)
            }
            Some(Err(e)) => format!("{}: failed — {e}", name_of(at)),
        }
    }
}

/// What the model calls a sub-agent it started. Positional rather than the
/// session id, which is thirty characters of no meaning to it.
fn name_of(at: usize) -> String {
    format!("task{:02}", at + 1)
}

/// One sub-agent's block in a report, with its cost folded into the turn.
///
/// A child that ran out of steps, or was cut off mid-answer, used to read like
/// one that finished: the stop reason was in the line, and uniform blocks are
/// read uniformly. What it managed still follows, because it is usually most of
/// the work.
fn collected(task: &str, result: &Landed, outcome: &mut TurnOutcome) -> String {
    match result {
        Ok((id, child)) => {
            outcome.delegated.push(id.clone());
            outcome.input_tokens += child.input_tokens;
            outcome.output_tokens += child.output_tokens;
            // Or the turn reports the children's input against only its own
            // cache, and the ratio a person reads is wrong.
            outcome.cached_tokens += child.cached_tokens;
            let how = match finished(&child.stopped) {
                true => format!("sub-agent {id}, {} steps ({}):", child.steps, child.stopped),
                false => format!(
                    "sub-agent {id} did not finish — {} after {} steps. What it had done:",
                    child.stopped, child.steps
                ),
            };
            // A child whose last step was a call has no reply to show, and
            // "what it had done" followed by nothing reads as nothing done.
            let done = match child.reply.trim().is_empty() && !child.tools_called.is_empty() {
                true => format!(
                    "called {}, and the budget ended before it could answer",
                    child.tools_called.join(", ")
                ),
                false => child.reply.clone(),
            };
            format!("### {task}\n{how}\n{done}")
        }
        Err(e) => format!("### {task}\nfailed: {e}"),
    }
}

/// What a checker is told before the claim.
///
/// The shape of the answer is part of the instruction because a verdict that can
/// be hedged is one that will be: "looks reasonable" is what a model says when it
/// has read something and run nothing.
const OUT_OF_STEPS: &str = "\
You are out of steps for this turn. Say now, in words and without calling anything: what \
you found, what you did, and what is left.";

const SAY_IT: &str = "\
You ended the turn without saying anything. Answer now, in words: what you found, or what \
you did and what is left.";

const VERDICT_NUDGE: &str = "\
You stopped without a verdict. If something is still to be run or read, do it now \
with the tools rather than describing it; then end with exactly one of \
`VERDICT: holds`, `VERDICT: fails`, `VERDICT: unproven`.";

const VERDICT_INSTRUCTIONS: &str = "\
You are checking a claim somebody else made. You did not do the work and you have \
no stake in it being true.

Do not take the claim's word for anything, and do not answer from memory: a \
verdict reached without reaching for something is a recollection, and is reported \
as unproven however sure it sounded.

Where something can be run — a build, a test, a linter, a command that prints the \
value in question — run it, and let what it printed be the reason. Where it is \
about this code, read it and quote the lines that decide it. Where it is about \
the world, find where it is said and quote that, with the address it came from; \
if the tools for reaching the web are not here, that is a claim you cannot settle \
and should say so.

Separate what a source states from what it argues. `The figure was 400` is \
something a page asserts and can be attributed; `the figure was disappointing` is \
its writer, and belongs in your answer only as theirs. Two sources that copy one \
another are one source.

You have no tools for writing files: you are judging this, not fixing it.

End with exactly one of these lines, and nothing after it:

VERDICT: holds
VERDICT: fails
VERDICT: unproven

`unproven` is the honest answer when nothing available settles it — say what \
would. Above that line, give the evidence: the command and its output, the lines \
you read, or the quotation and where it is from. Not a summary of your \
reasoning.";

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
/// What the model asked to delegate. A task is words — a sentence of what to
/// do — one as `task`, several as `tasks`. An entry that is an object with the
/// sentence under `task` is read for it; one that is anything else, a tool
/// call say, is refused by its shape: a child is handed a task it decides how
/// to do, not a call somebody else decided on.
fn requested_tasks(args: &serde_json::Value) -> std::result::Result<Vec<String>, String> {
    let mut listed: Vec<&str> = Vec::new();
    for item in args.get("tasks").and_then(|t| t.as_array()).into_iter().flatten() {
        let text = item.as_str().or_else(|| {
            ["task", "goal", "prompt", "description"].iter().find_map(|key| item.get(key)?.as_str())
        });
        match (text, item.as_object()) {
            (Some(text), _) => listed.push(text),
            (None, Some(object)) => {
                let keys: Vec<&str> = object.keys().map(String::as_str).collect();
                return Err(format!(
                    "each task is a sentence of what to do — words, not an object with {} — as in \
                     `tasks: [\"read notes/port.txt and report the port it names\"]`",
                    keys.join(", ")
                ));
            }
            (None, None) => return Err("each task is a sentence of what to do, as a string".into()),
        }
    }
    let single = args.get("task").and_then(|t| t.as_str());

    let mut tasks: Vec<String> = Vec::new();
    for task in listed.iter().copied().chain(single.filter(|_| listed.is_empty())) {
        let task = task.trim();
        if !task.is_empty() && !tasks.iter().any(|kept| kept == task) {
            tasks.push(task.to_string());
        }
    }
    if tasks.is_empty() {
        return Err("delegate needs a task, or a list of tasks".into());
    }
    Ok(tasks)
}

/// A breakpoint below the minimum cacheable prefix only pays the write premium,
/// so a small system prompt is left unmarked.
fn cacheable(message: Message) -> Message {
    const MINIMUM_TOKENS: usize = 1024;
    if estimate_tokens(&message.content) >= MINIMUM_TOKENS { message.cacheable() } else { message }
}

/// What the request costs, anchored on what the provider last said it cost.
///
/// Estimation is `len / 4` and it is wrong in the direction that hurts: it counts
/// the messages and not the tool schemas, which are ~750 tokens of every request
/// by default and more when they are sent in full. Both the compaction threshold
/// and the overflow check turn on this number, so under-counting means a request
/// that comes back as a limit error the user never saw coming.
///
/// The provider counted what it received exactly. Everything up to that point
/// therefore needs no estimating, and the error shrinks to whatever has been
/// appended since.
fn measured(messages: &[Message], anchor: Option<(usize, usize)>) -> usize {
    match anchor {
        Some((counted, reported)) if counted <= messages.len() => reported + measure(&messages[counted..]),
        _ => measure(messages),
    }
}

fn measure(messages: &[Message]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content) + 4).sum()
}

#[cfg(test)]
mod verdict_tests {
    use super::verdict_in;

    /// The whole mechanism turns on finding this line, and a check that ran the
    /// build is not "unchecked" for having written it in bold.
    #[test]
    fn a_verdict_is_read_however_the_model_dressed_it() {
        assert_eq!(verdict_in("evidence\n\nVERDICT: holds"), Some("holds"));
        assert_eq!(verdict_in("**VERDICT: fails**"), Some("fails"));
        assert_eq!(verdict_in("- verdict: Unproven."), Some("unproven"));
        assert_eq!(verdict_in("> `VERDICT:` holds"), Some("holds"));
        assert_eq!(verdict_in("VERDICT: holds\nVERDICT: fails"), Some("fails"), "the last one is the one");
    }

    /// Asking for one of three is what stops a hedge being reported as a check.
    #[test]
    fn anything_but_the_three_words_is_not_a_verdict() {
        assert_eq!(verdict_in("VERDICT: probably holds"), None);
        assert_eq!(verdict_in("I would say it holds"), None);
        assert_eq!(verdict_in("VERDICT:"), None);
    }
}

#[cfg(test)]
mod relay_tests {
    use super::{Interjections, relay};
    use std::sync::Arc;

    /// One queue each, because taking from one shared queue would give the
    /// message to whichever child stepped first and to none of the others.
    #[test]
    fn what_is_said_mid_delegation_reaches_every_child_and_is_kept_for_the_parent() {
        let parent = Interjections::default();
        let children: Vec<Arc<Interjections>> = (0..3).map(|_| Default::default()).collect();
        let mut carried = Vec::new();

        parent.say("use serde, not a hand-rolled parser");
        relay(&parent, &children, &mut carried);

        for child in &children {
            assert_eq!(child.take(), ["use serde, not a hand-rolled parser"]);
        }
        assert_eq!(carried, ["use serde, not a hand-rolled parser"], "and the parent still hears it");

        // Polled many times a second: a message must go out once.
        relay(&parent, &children, &mut carried);
        assert!(children[0].take().is_empty());
        assert_eq!(carried.len(), 1);
    }
}

#[cfg(test)]
mod gap_tests {
    use super::gap_before;

    /// A conversation replayed without them reads as one sitting, and "have you
    /// already run the tests?" has a different answer if the last exchange was
    /// last week.
    #[test]
    fn only_a_pause_long_enough_to_change_an_answer_is_marked() {
        const HOUR: i64 = 3600;
        assert_eq!(gap_before(0, 10 * HOUR), None, "nothing precedes the first event");
        assert_eq!(gap_before(100, 100 + HOUR - 1), None, "a conversation is one sitting");
        assert_eq!(gap_before(100, 100 + HOUR).as_deref(), Some("1 hour"));
        assert_eq!(gap_before(100, 100 + 5 * HOUR).as_deref(), Some("5 hours"));
        assert_eq!(gap_before(100, 100 + 24 * HOUR).as_deref(), Some("1 day"));
        assert_eq!(gap_before(100, 100 + 90 * HOUR).as_deref(), Some("3 days"));
        // A clock that went backwards is not a gap.
        assert_eq!(gap_before(10 * HOUR, HOUR), None);
    }
}

#[cfg(test)]
mod checker_tests {
    use super::CHANGES_FILES;

    /// Both lists a checker is held to are names, so a tool added later is
    /// handed to it by default — which is how `delete_file` was, for a while,
    /// something a read-only checker could call. A new tool fails this until
    /// somebody has put it on one side or the other.
    #[test]
    fn a_tool_a_checker_may_call_is_one_somebody_decided_it_may_call() {
        let checker = rook_tools::ToolBox::standard().without(CHANGES_FILES);
        let mut allowed = checker.names();
        allowed.sort_unstable();
        assert_eq!(
            allowed,
            ["crate_api", "list_dir", "read_file", "run_command", "search"],
            "a new toolbox tool reaches a checker unweighed; add it here or to CHANGES_FILES"
        );
    }
}
