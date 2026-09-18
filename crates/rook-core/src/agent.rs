//! The agent loop.
//!
//! This module owns the public turn API and step ordering. Private modules own
//! admission and closing (`lifecycle`), provider streaming (`stream`), context
//! construction (`prompt`, `history`, `budget`, `compaction`), tool execution
//! (`tools`, `effects`, `tool_catalog`), delegation, checks, setup and output.
//! All front ends still drive the same loop; splitting its responsibilities
//! does not introduce another execution path.
//!
//! Deliberately small, across all of them. Everything that varies — the model,
//! the tools, the skills — is behind a trait or a data structure, so the loop
//! itself stays something a person can read in one sitting and reason about.
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

mod budget;
mod checks;
mod compaction;
mod delegation;
mod effects;
mod history;
mod lifecycle;
mod output;
mod prompt;
mod setup;
mod stream;
mod tool_catalog;
mod tools;

use budget::{cacheable, images_in, measure, measured};
use compaction::TOO_MUCH_COMPACTION;
use delegation::{Nursery, drain_uncollected};
use effects::CHANGES_FILES;
use history::with_thinking;
use setup::Fetching;

use futures_util::StreamExt;
use rook_llm::{Assembler, Delta, Message, Provider, Request, Role};
use rook_store::EventKind;
use rook_tools::policy::{Approver, Policy, Stance, Unattended};
use rook_tools::{ToolBox, ToolContext};
use serde::{Deserialize, Serialize};

use std::path::Path;

use crate::config::Config;
use crate::context::ContextBudget;
use crate::error::{CoreError, Result};
use crate::hooks::Hooks;
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
    ctx.isolate = sandbox.isolate;
    ctx.isolation.network = sandbox.network;
    // Every project's transcripts, checkpoints and memory live under here, so
    // a command run for one project has no business reading another's — and
    // with the network on, reading is the whole of what an exfiltration needs.
    ctx.isolation.unreadable = vec![crate::paths::home()];
    ctx.isolation.scratch.extend(sandbox.writable.iter().map(|dir| match dir.strip_prefix("~/") {
        Some(rest) => crate::paths::user_home().join(rest),
        None => std::path::PathBuf::from(dir),
    }));
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
pub fn finished(stopped: &str) -> bool {
    matches!(stopped, "end_turn" | "stop")
}

/// What to tell a person about a turn that stopped rather than finished, and
/// nothing when it finished.
///
/// One sentence for every front end: `rook run` said it and the windows said
/// only how many steps had run, which reads exactly like a turn that decided
/// it was done.
/// What a turn stopped at a limit is asked when somebody says to carry on.
///
/// A limit is not a verdict on the task, and everything the turn did is in the
/// session — so the useful thing is not to start again but to go on. Said
/// explicitly, because a model handed a fresh prompt in a session it has
/// already worked in will redo the reading it did in the first ten steps: what
/// it is told here is where it stands, not what to do.
/// Whether what was typed means "carry on" rather than being a prompt.
///
/// One function because three front ends ask it and a fourth spelling known to
/// only two of them is a command that works in some windows.
pub fn carrying_on(typed: &str) -> bool {
    matches!(typed.trim(), "/continue" | "/go on" | "/carry on")
}

pub const CARRY_ON: &str = "The previous turn stopped at a limit rather than because the work was \
    done. Everything it read, ran and changed is above, in this same session. Carry on from where \
    it stopped: do not start again, and do not redo what has already been done. If you are stopped \
    again, end by saying plainly what is left, so the next turn starts from there.";

pub fn why_it_stopped(stopped: &str) -> Option<String> {
    if finished(stopped) {
        return None;
    }
    Some(match stopped {
        // Every limit says the same third thing, because it is the answer more
        // often than either of the others: what ran out is a turn, not the
        // task, and what the turn did is still in the session.
        "max_steps" => carry_on("the step limit", "`[agent] max_steps`"),
        "budget" => carry_on("the spend limit", "`[agent] max_turn_tokens`"),
        "time" => carry_on("the time limit", "`[agent] max_turn_secs`"),
        "incomplete" => {
            "the model kept promising further work without performing it; `/continue` resumes the task".into()
        }
        "completion_unchecked" => {
            "could not determine whether the model finished; `/continue` resumes the task".into()
        }
        "recovery" => "an interrupted operation has an unknown result; inspect `/recovery` and record what you checked before changes resume".into(),
        "blocked" => "the model reported a refusal or blocker; the task remains incomplete".into(),
        other => format!("the turn ended as {other:?} rather than finishing"),
    })
}

fn carry_on(limit: &str, knob: &str) -> String {
    format!("stopped at {limit} — `/continue` carries on with a fresh one, or raise {knob}")
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

    pub fn changed_note(&self) -> Option<String> {
        changed_note(&self.files_changed)
    }
}

/// What was written, for a person reading the end of a turn.
///
/// Bounded in the saying, not in the record: a refactor across forty files has
/// forty of them in `files_changed`, and one line about it here. A free
/// function as well as a method because a window watching a turn the daemon is
/// running has the list and not the outcome, and two renderings of one thing
/// are two answers waiting to differ.
pub fn changed_note(files: &[String]) -> Option<String> {
    const NAMED: usize = 5;
    let named: Vec<&str> = files.iter().take(NAMED).map(String::as_str).collect();
    let more = files.len().saturating_sub(NAMED);
    match (files.len(), more) {
        (0, _) => None,
        (1, _) => Some(format!("wrote {}", named[0])),
        (n, 0) => Some(format!("wrote {n} files: {}", named.join(", "))),
        (n, more) => Some(format!("wrote {n} files: {}, and {more} more", named.join(", "))),
    }
}

/// The loop's own tools that change something, and are therefore not offered to
/// a checker. `delegate` is here because a checker that can start an agent with
/// the writing tools has not been stopped from writing, only from doing it
/// itself.
const CHANGES_THINGS: &[&str] =
    &[WRITE_SKILL, FIND_SKILL, REMEMBER, FORGET, DELEGATE, SUBAGENTS, STANCE, VERIFY, crate::worktrees::TOOL];

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
    /// A sub-task called a tool. Several run at once, so which one is said
    /// alongside: without this a delegation that takes minutes shows a counter
    /// that does not move, which reads the same as a hang.
    ///
    /// A number rather than the task, and a phrase rather than a tool's name.
    /// It used to be `    {task}: {tool}`, with the task cut to forty-eight
    /// characters — `Find regressions and new defects in the veil-nod:
    /// write_file`, which reads as a sentence that has gone wrong rather than
    /// as a thing being done. The task each number stands for is in the
    /// transcript and in the calls pane; what a person watching four of these
    /// wants from this line is that they are moving, and on what.
    Delegating {
        at: usize,
        doing: &'a str,
    },
    /// A call that is taking a while, saying what is happening while it does.
    ///
    /// A long command and a wedged one are the same await from outside and drew
    /// the same unchanging line. The tool is the only thing that knows which —
    /// how long it has been running, and how long since it printed anything —
    /// and this is where it says so.
    Working {
        call: &'a str,
        said: &'a str,
    },
    /// A step of the turn, as it begins. What a person watching wants to know
    /// is not only that it is still going but how much of the budget is left:
    /// a turn at step 190 of 200 is about to stop whatever it is in the middle
    /// of, and that is worth knowing before it does.
    Step {
        at: u32,
        of: u32,
    },
    /// What the turn has spent, after each reply from the model. A turn that
    /// runs for minutes across a dozen steps otherwise shows no cost at all
    /// until it is over and the number can no longer change a decision.
    Spent {
        input: u32,
        output: u32,
        cached: u32,
    },
    /// Something the person said while the turn ran, at the moment the turn
    /// takes it up.
    ///
    /// Said once it is in the request rather than when it was typed, because
    /// those are different moments and only the second one is news. A window
    /// that says "the turn will see this" and never says more leaves the person
    /// watching a queue they cannot see the end of — and a model's request is
    /// already sent when they type, so the wait is real and worth marking the
    /// end of.
    Heard {
        text: &'a str,
    },
    /// The model has been asked and has not begun to answer.
    ///
    /// The last silence in a turn with no account of itself. A tool that takes
    /// a while says so, a sub-agent says so, a step says which it is — and then
    /// the request goes out and nothing is heard until the first token, which
    /// on a local model reading a full context is a quarter of an hour by
    /// design. Watching that, a working agent and a dead tunnel are the same
    /// blank screen, and it was read as the second for weeks.
    ///
    /// `patience` is how long this wait may last, so the line answers the
    /// question that follows it: whether to keep waiting or go and look.
    Waiting {
        secs: u64,
        patience: u64,
    },
}

/// How long a wait for the model may go unmentioned. An ordinary answer begins
/// before this and a line about it would be noise.
const SAY_IT_WAITS_AFTER: std::time::Duration = std::time::Duration::from_secs(20);
/// And how often after that — often enough to be a sign of life, rare enough
/// that a quarter of an hour of prefill is thirty lines and not seven hundred.
const SAY_IT_STILL_WAITS_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait for something, saying how long it has been waiting while it waits.
///
/// The same shape a long tool call already uses, for the one wait that had no
/// voice: the model's. First after twenty seconds, because an ordinary answer
/// begins before that and a line about it would be noise; then every half
/// minute, which is often enough to be a sign of life and rare enough to read.
async fn saying_it_waits<T>(
    working: impl std::future::Future<Output = T>,
    patience: std::time::Duration,
    mut on_progress: impl FnMut(Progress<'_>),
) -> T {
    // The runtime's clock rather than the system's: it is the one the timer
    // beside it runs on, so the two cannot disagree — and a test can hold it
    // still rather than waiting out half a minute to watch this work.
    let started = tokio::time::Instant::now();
    tokio::pin!(working);
    let mut saying =
        tokio::time::interval_at(tokio::time::Instant::now() + SAY_IT_WAITS_AFTER, SAY_IT_STILL_WAITS_EVERY);
    loop {
        tokio::select! {
            // So a wait that ends on the tick is reported as over rather than
            // as still going.
            biased;
            done = &mut working => return done,
            _ = saying.tick() => on_progress(Progress::Waiting {
                secs: started.elapsed().as_secs(),
                patience: patience.as_secs(),
            }),
        }
    }
}

/// How many times a turn may be told it is asking the same thing again before
/// the turn ends. Three: the first is a slip, the second is a habit, and the
/// third is the rest of the step budget.
const STUCK_ON_ONE_CALL: usize = 3;

/// Pseudo-tools: implemented by the loop rather than the toolbox, because they
/// need the agent's own state.
pub const LOAD_SKILL: &str = "load_skill";
pub const WRITE_SKILL: &str = "write_skill";
pub const FIND_SKILL: &str = "find_skill";
pub const REMEMBER: &str = "remember";
pub const FORGET: &str = "forget";
pub const RECALL: &str = "recall";
pub const DELEGATE: &str = "delegate";
/// Only under `[agent] todo_tool`, which is off: the default is a line in the
/// prompt and no bookkeeping (ADR-0010).
pub const PLAN: &str = "plan";
pub const STANCE: &str = "stance";
pub const SUBAGENTS: &str = "subagents";
pub const VERIFY: &str = "verify";
pub const DOCS: &str = "docs";

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
    /// What this turn wrote, as workspace-relative paths. A turn that says it
    /// has done the work and a turn that has done it read the same from the
    /// outside — which is a question a person asked here, two and a half hours
    /// into one that had written nothing.
    #[serde(default)]
    pub files_changed: Vec<String>,
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

/// What a turn has to tell the person at the end, by what it is.
enum Reported {
    Decision(String),
    Open(String),
}

pub struct AgentLoop<'a> {
    execution: Option<std::sync::Weak<crate::execution::Journal>>,
    launched_job: std::sync::Mutex<Option<String>>,
    pub options: rook_proto::TurnOptions,
    effective_options: Option<rook_proto::TurnOptions>,
    recipe_skill: Option<String>,
    recipe_output: bool,
    pub rook: &'a Rook,
    /// Shared rather than owned so a delegated child can reuse the connection
    /// instead of building a second HTTP client per sub-task.
    pub provider: std::sync::Arc<dyn Provider>,
    pub tools: ToolBox,
    pub tool_ctx: ToolContext,
    pub session: u128,
    pub policy: std::sync::Arc<Policy>,
    /// The secrets this machine holds, and what has been handed to a tool this
    /// turn. Per turn on purpose: a value resolved for one turn is not in
    /// memory for the next, and the loop is rebuilt for every turn.
    vault: std::sync::Arc<crate::secrets::Vault>,
    pub hooks: std::sync::Arc<Hooks>,
    pub servers: std::sync::Arc<crate::lsp::Servers>,
    /// Who condenses a span when the context fills. `None` builds it from
    /// `[agent] compaction_model`, or uses the provider doing the work when
    /// that is empty — a front end or a test can hand one in instead.
    pub summariser: Option<std::sync::Arc<dyn Provider>>,
    /// What the `session_start` hooks contributed, computed once.
    session_context: std::sync::Mutex<Option<String>>,
    /// Where this turn's events begin, so what the goal check is shown is this
    /// turn and not the session. Set when the prompt is logged; zero until
    /// then, which is a whole session and is what a loop that has not run yet
    /// should say.
    began_at_seq: u64,
    /// A language server being fetched while the turn runs. It is a minute of
    /// npm or a release download, it serves from the next session on, and it
    /// used to be paid for before the first request — a person's first turn in
    /// a new project sat silent while it downloaded something that turn could
    /// not use.
    installing: std::sync::Mutex<Option<Fetching>>,
    /// Every file this turn has written, workspace-relative. Collected here
    /// because the writing happens inside a call whose only answer is the
    /// text the model sees.
    wrote_paths: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Claims this turn has already checked that failed, and what it had
    /// written when they did.
    ///
    /// So that a claim which fails, and then holds once the turn has rewritten
    /// the thing it was about, is not reported as verified. The instruction not
    /// to do that is already on a failing result and a three-billion-parameter
    /// model read it and did it anyway — twice, in the same recorded run — so
    /// what is needed here is a fact the loop holds rather than a sentence the
    /// model weighs.
    failed_claims: std::sync::Mutex<std::collections::HashMap<String, std::collections::BTreeSet<String>>>,
    /// What a language server said about a file before this turn last wrote
    /// to it. A write is answered with what it broke; everything already wrong
    /// in somebody's file is not this call's news, and on every write it is
    /// noise.
    problems_before: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Vec<String>>>,
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
    /// What is left of the turn's allowance, in tokens, its sub-agents
    /// included. A child is given the remainder rather than a fresh one, which
    /// is the whole point: `max_steps` is inherited whole, so nine errands are
    /// nine times the bound, and this is the bound that cannot be multiplied.
    /// 0 lifts it.
    pub max_turn_tokens: u64,
    /// How long this turn may run. 0 lifts it.
    pub max_turn_secs: u64,
    /// The moment this turn stops, set when it starts and inherited by every
    /// sub-agent unchanged.
    ///
    /// An instant rather than a duration, because sub-agents run at the same
    /// time: they finish by the moment the turn does, and a duration each would
    /// be the multiplication that made the step budget useless.
    by: Option<std::time::Instant>,
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
        let mut tool_ctx = tool_context(&rook.config, &rook.workspace, &rook.output_dir);

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
            let through = rook.config.proxy.for_web();
            match rook_tools::web::Fetch::new(patience, &through) {
                Ok(fetch) => tools.register(std::sync::Arc::new(fetch)),
                Err(e) => tracing::warn!("web is enabled but unusable: {e}"),
            }
            // Only when an engine is named and usable. Offering a search that
            // fails on its first call teaches the model to stop asking, which is
            // worse than never having offered it.
            let engine = rook_tools::web::Engine::named(&rook.config.web.search, &rook.config.web.search_url);
            if let Some(engine) = engine
                && let Ok(search) = rook_tools::web::Search::new(engine, patience, &through)
            {
                tools.register(std::sync::Arc::new(search));
            }
        }

        // Read once per turn. A machine with no secrets file gets an empty one,
        // which answers every name with "no such secret" and costs nothing.
        let vault = std::sync::Arc::new(crate::secrets::Vault::load().unwrap_or_else(|e| {
            tracing::warn!("secrets are unreadable, so none are offered: {e}");
            crate::secrets::Vault::empty()
        }));
        tool_ctx.secrets = Some(vault.clone());

        let (hooks, bad_hooks) = Hooks::compile(&rook.config.hooks);
        for error in bad_hooks {
            tracing::warn!("ignoring unusable hook matcher: {error}");
        }

        let window = rook.window_to_budget(provider.context_window());
        let budget = ContextBudget::new(window, rook.config.agent.compact_at);
        Self {
            execution: None,
            launched_job: Default::default(),
            options: Default::default(),
            effective_options: None,
            recipe_skill: None,
            recipe_output: false,
            rook,
            provider,
            tools,
            tool_ctx,
            session,
            policy: policy_for(&rook.config),
            vault,
            hooks: std::sync::Arc::new(hooks),
            servers,
            summariser: None,
            session_context: std::sync::Mutex::new(None),
            began_at_seq: 0,
            problems_before: Default::default(),
            installing: Default::default(),
            wrote_paths: Default::default(),
            failed_claims: Default::default(),
            reported: Default::default(),
            asker: None,
            approver: std::sync::Arc::new(Unattended),
            interjections: Default::default(),
            depth: 0,
            max_steps: rook.config.agent.max_steps,
            max_turn_tokens: rook.config.agent.max_turn_tokens,
            max_turn_secs: rook.config.agent.max_turn_secs,
            by: None,
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
        let sources = self.source_context();
        if !sources.is_empty() {
            messages.push(cacheable(Message::user(sources)));
        }
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

    fn turn_options(&self) -> &rook_proto::TurnOptions {
        self.effective_options.as_ref().unwrap_or(&self.options)
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
        let _workspace = crate::worktrees::Lease::acquire(&self.rook.workspace, false)?;
        let prepared_prompt = self.prepare_recipe(prompt)?;
        let prompt = prepared_prompt.as_deref().unwrap_or(prompt);
        crate::attachments::prepare(prompt, &self.turn_options().attachments)?;
        let contract = crate::output::Contract::compile(self.turn_options(), &self.rook.workspace)?;
        if let Some(path) = &contract.path {
            let risk = rook_tools::policy::Risk::Write(vec![
                self.rook.workspace.join(path).to_string_lossy().into_owned(),
            ]);
            if let rook_tools::policy::Decision::Deny(why) = self.policy.decide(&risk) {
                return Err(CoreError::Other(format!("output write refused: {why}")));
            }
        }
        // Keep the top-level turn marked through final validation and file I/O too.
        let journal =
            crate::execution::Journal::start(self.rook, self.session, self.tool_ctx.jobs.as_deref())?;
        self.execution = Some(std::sync::Arc::downgrade(&journal));
        // From here until the turn ends, this session is marked as having one in
        // flight. Only the turn a person asked for: a sub-agent's session ends
        // with its parent's, and two explanations of one death read as two.
        let _running = (self.depth == 0).then(|| crate::service::Running::marked(self.session));
        let mut outcome = match self.run_inner(prompt, &mut on_progress).await {
            Ok(outcome) => outcome,
            Err(error) => {
                journal.finish("failed", self.tool_ctx.jobs.as_deref())?;
                return Err(error);
            }
        };
        let finalised = self.apply_output(&contract, &mut outcome, &mut on_progress).await;
        if let Err(error) = &finalised {
            outcome.stopped = "output_error".into();
            outcome.open_questions.push(error.to_string());
            self.rook.log(self.session, EventKind::Error, "output", &error.to_string()).ok();
        }
        let closing = self.end_of_turn(&mut outcome).await;
        if finalised.is_ok() && closing.is_ok() {
            journal.record_outcome(&outcome)?;
        }
        let needs_review = journal.finish(
            if finalised.is_ok() && closing.is_ok() { &outcome.stopped } else { "failed" },
            self.tool_ctx.jobs.as_deref(),
        )?;
        finalised?;
        closing?;
        if needs_review || self.rook.recovery_block(self.session)?.is_some() {
            outcome.stopped = "recovery".into();
            outcome.open_questions.push("An interrupted operation has an unknown result. Inspect `/recovery` before acknowledging it; no side effects will be retried automatically.".into());
        }
        Ok(outcome)
    }

    async fn run_inner<F: FnMut(Progress<'_>)>(
        &mut self,
        prompt: &str,
        mut on_progress: F,
    ) -> Result<TurnOutcome> {
        let loaded_recipe_skill = self.begin_turn(prompt, &mut on_progress).await?;
        let mut messages = self.request_messages(prompt)?;
        let mut outcome = TurnOutcome {
            steps: 0,
            stopped: "end_turn".into(),
            reply: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            tools_called: Vec::new(),
            skills_loaded: loaded_recipe_skill.into_iter().collect(),
            skills_written: Vec::new(),
            facts_learned: Vec::new(),
            facts_forgotten: Vec::new(),
            files_changed: Vec::new(),
            delegated: Vec::new(),
            compactions: 0,
            decisions: Vec::new(),
            open_questions: Vec::new(),
        };

        // Built once, before the loop borrows `self` mutably: a child's future
        // takes the crew rather than the parent, which is what lets the parent
        // go on stepping while it runs. The allowance it carries is what the
        // turn had at the start, since the crew outlives every step: a child
        // started late is bounded by its share of that rather than by nothing.
        let crew = self.crew(self.max_turn_tokens);
        let (mut nursery, mut nursery_steps) = Nursery::new(self.rook.config.agent.max_parallel_subagents);
        let mut carrying = tokio::time::interval(std::time::Duration::from_millis(200));

        let mut asked_for_one_script = false;
        let mut asked_to_say = false;
        let mut asked_to_go_on = false;
        let mut handed_left = false;
        let mut repeated: std::collections::BTreeMap<(String, String), (String, u32)> =
            std::collections::BTreeMap::new();
        // How many calls this turn were refused as a repeat of one already
        // answered, and whether that was what ended it.
        let mut looping = 0usize;
        let mut stuck = false;
        let mut checked_goal = false;
        let mut completion_retries = 0;
        let mut worth_compacting = true;
        // Once per turn: an endpoint that refuses the length twice is not
        // refusing an assumption, and summarising again would spend a call to
        // meet the same wall.
        let mut shrunk = false;
        // What the provider last said the request cost, and how many messages
        // that covered. See `measured`.
        let mut anchor: Option<(usize, usize)> = None;
        while outcome.steps < self.max_steps {
            // Before the request, because the request is what costs. A turn
            // that has reached its allowance has stopped converging, and the
            // step count says nothing about that: a step is worth whatever the
            // context happened to be.
            if self.overspent(&outcome) {
                let said = self.spend_note();
                self.rook.log(self.session, EventKind::Note, "budget", &said).ok();
                self.report(Reported::Open(said));
                outcome.stopped = "budget".into();
                break;
            }
            // And the other ceiling, which on a local model is the only one
            // that costs: tokens are free there and an afternoon is not.
            if self.out_of_time() {
                let said = self.time_note();
                self.rook.log(self.session, EventKind::Note, "time", &said).ok();
                self.report(Reported::Open(said));
                outcome.stopped = "time".into();
                break;
            }
            outcome.steps += 1;
            on_progress(Progress::Step { at: outcome.steps, of: self.max_steps });

            // Before the request rather than after the tool results: this is the
            // one place a user message may go, and it is what makes a turn
            // steerable instead of only stoppable.
            for said in self.interjections.take() {
                self.rook.log(self.session, EventKind::UserMessage, "while running", &said).ok();
                on_progress(Progress::Heard { text: &said });
                messages.push(Message::user(&said));
            }

            if crate::results::prune(self.rook, self.session, &mut messages)? > 0 {
                anchor = None;
                worth_compacting = true;
            }

            // Once per turn that it achieves something. A span too small to
            // summarise leaves the context where it was, so the next step would
            // ask again, and the step after that — spending a summarisation
            // call each time to stay exactly as full as it already is.
            if worth_compacting
                && (self.budget.needs_compaction(measured(&messages, anchor))
                    || images_in(&messages) > crate::attachments::MAX_ATTACHMENTS)
            {
                // Before summarising anything: the window is a guess for
                // anything self-hosted, and this is the first moment it
                // matters. Asked here rather than at the start of every turn,
                // where it is a round trip most turns never need — and where
                // it changed the shape of every conversation a fixture had to
                // answer, which is how the cost became visible.
                self.ask_the_window().await;
            }
            if worth_compacting
                && (self.budget.needs_compaction(measured(&messages, anchor))
                    || images_in(&messages) > crate::attachments::MAX_ATTACHMENTS)
            {
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
                // Said once, when it stops being incidental. Each compaction is
                // a summarisation call, so a turn compacting every few steps is
                // spending most of itself on bookkeeping — which is visible as
                // an hour of nothing and, until this, was reported only in the
                // count at the end.
                if outcome.compactions == TOO_MUCH_COMPACTION {
                    let said = format!(
                        "compacted {} times in {} steps: the context window ({} tokens) is small \
                         for this work, and most of the turn is going into summarising it. \
                         `[agent] context_window` sets it; a model with a larger one costs less \
                         than this.",
                        outcome.compactions, outcome.steps, self.budget.window
                    );
                    self.rook.log(self.session, EventKind::Note, "compaction", &said).ok();
                    self.report(Reported::Open(said));
                }
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

            if images_in(&messages) > crate::attachments::MAX_ATTACHMENTS {
                return Err(CoreError::Other("too many images remain in context after compaction; start a new session or compact older turns".into()));
            }
            let sent = messages.len();
            let mut request = Request::new(messages.clone());
            if self.native_tools() {
                request.tools = self.tool_specs();
            }
            request.effort = Some(self.effort);
            request.max_output_tokens = self.room_for_output(used);
            request.cache_ttl = self.rook.config.agent.cache_ttl();
            // How long this endpoint may be silent before it is given up on —
            // the configured patience plus what reading this prompt should
            // take. Carried into the waiting line so that it answers the
            // question it raises: whether to keep waiting or go and look.
            let patience =
                rook_llm::first_token_patience(self.rook.config.agent.stream_idle(), request.prompt_bytes());
            let answering =
                saying_it_waits(self.provider.stream(request.clone()), patience, &mut on_progress);
            let asked = match answering.await {
                Ok(stream) => Ok(stream),
                // The window was an assumption and the endpoint has just
                // disagreed with it. Believing the refusal costs a
                // summarisation; not believing it ends the turn on a number
                // nobody chose, which is what a wrong guess used to do.
                Err(e) if rook_llm::retry::names_the_context(&e) && !shrunk => {
                    shrunk = true;
                    let assumed = self.budget.window;
                    let smaller = (used * 3 / 4).max(4096);
                    self.rook.learn_window(smaller);
                    self.budget = ContextBudget::new(smaller, self.rook.config.agent.compact_at);
                    let said = format!(
                        "the endpoint refused {used} tokens as too long, so its window is smaller \
                         than the {assumed} assumed — budgeting {smaller} and summarising to fit. \
                         `[agent] context_window` sets it outright."
                    );
                    self.rook.log(self.session, EventKind::Note, "window", &said).ok();
                    self.report(Reported::Open(said));
                    outcome.compactions += 1;
                    self.compact().await;
                    messages = self.request_messages(prompt)?;
                    anchor = None;
                    continue;
                }
                Err(e @ rook_llm::LlmError::Status { status: 400 | 422, .. }) if images_in(&messages) > 0 => {
                    Err(CoreError::Other(format!(
                        "The model rejected a request containing images. Check that the selected model supports image input and these file formats. Images were not removed: {e}"
                    )))
                }
                Err(e) => Err(CoreError::Other(e.to_string())),
            };
            let assembler = self
                .receive(asked?, &mut nursery, &mut nursery_steps, &mut carrying, patience, &mut on_progress)
                .await?;
            let thinking = assembler.reasoning().to_string();
            if !thinking.is_empty() {
                self.rook.log(self.session, EventKind::Reasoning, "", &thinking).ok();
            }
            let mut response = assembler.finish();
            // Read back either way. Without native tools the object is the
            // only way a call arrives; with them, a small model still writes
            // one as text some of the time, and the turn ended with nothing
            // called. Only a tool that was offered is taken as called.
            let offered = |name: &str| self.tool_specs().iter().any(|t| t.name == name);
            rook_llm::prompted::adopt(&mut response, offered);
            // And in the thinking, where one arrived: a model that writes its
            // call in `reasoning_content` and nothing in `content` ended the
            // turn mid-task, with the call written out in the transcript.
            rook_llm::prompted::adopt_thought(&mut response, &thinking, offered);

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

            // What goes back to the model at the next step, thinking and all.
            // A provider with a place of its own for it — Anthropic signs
            // blocks and they ride on the message already — needs no second
            // copy as text; every other one hands the model back its answer
            // and its calls and nothing it worked out, so it works it out
            // again, every step.
            let carried = match response.message.reasoning.is_empty() {
                false => response.message.clone(),
                true => Message {
                    content: with_thinking(
                        Some(crate::context::shorten_thinking(
                            &thinking,
                            self.rook.config.agent.max_reasoning_tokens,
                        ))
                        .filter(|kept| !kept.is_empty()),
                        &response.message.content,
                    ),
                    ..response.message.clone()
                },
            };

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
                    // Handed to the model once, so it can answer from them: a
                    // parent that started three readers and ended the turn was
                    // asked what it found and made a number up, with the
                    // readers' answers appended below where it never looked.
                    // A second time they go to the reply, or a model that
                    // keeps starting readers keeps the turn open.
                    if let Some(left) = drain_uncollected(&mut nursery, &mut outcome).await {
                        if !handed_left {
                            handed_left = true;
                            self.rook.log(self.session, EventKind::Note, "sub-agents", &left).ok();
                            messages.push(carried.clone());
                            messages.push(Message::user(crate::sources::data(
                                "subagent_report",
                                "collected child turns",
                                &left,
                            )));
                            continue;
                        }
                        outcome.reply.push_str(&format!("\n\n{left}"));
                    }
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
                            messages.push(carried.clone());
                            messages.push(Message::user(&note));
                            continue;
                        }
                    }
                    // Once. A reply cut at the output limit is not an answer
                    // and not a call — a delegation ended inside a fenced JSON
                    // object, with nothing called. Asked to go on, a call gets
                    // written whole and an answer gets finished.
                    //
                    // The stop reason alone was not enough to tell: Ollama says
                    // `stop` for a reply it truncated, so a call cut in half
                    // arrived looking like an ordinary answer, and the model
                    // wrote its previous call again. A half-written object
                    // naming a tool that was offered is the same cut, read off
                    // the text.
                    let cut = response.stop_reason == rook_llm::StopReason::MaxTokens
                        || rook_llm::prompted::cut_off_call(&response.message.content, |name| {
                            self.tool_specs().iter().any(|t| t.name == name)
                        });
                    if cut && !asked_to_go_on {
                        asked_to_go_on = true;
                        self.rook.log(self.session, EventKind::Note, "cut off", GO_ON).ok();
                        messages.push(carried.clone());
                        messages.push(Message::user(GO_ON));
                        continue;
                    }
                    let did_something = !outcome.tools_called.is_empty() || !outcome.delegated.is_empty();
                    // Once. A small model does the work and stops without a
                    // word — read the file, found the number, said nothing —
                    // and every front end renders that as a hang. Asked, it
                    // says what it found; asked twice, it had nothing to say.
                    if did_something && response.message.content.trim().is_empty() && !asked_to_say {
                        asked_to_say = true;
                        self.rook.log(self.session, EventKind::Note, "say it", SAY_IT).ok();
                        messages.push(carried.clone());
                        messages.push(Message::user(SAY_IT));
                        continue;
                    }
                    // Autonomy is a task and its boundaries, and this is the
                    // boundary being held: before the turn ends, a checker asks
                    // whether the goal is met and whether anything forbidden was
                    // done. Once, and only for a turn that did something.
                    // The front end's turn only: a checker checking itself
                    // against the claim it was handed, or a sub-task checked
                    // against its errand, is a checker per step at every depth.
                    if self.policy.stance() == Stance::Autonomous
                        && self.depth == 0
                        && !self.checking
                        && !checked_goal
                        && did_something
                    {
                        checked_goal = true;
                        // Autonomy is a task and its boundaries; with no goal
                        // set for the session, the task is what this turn was
                        // asked. Without that, `rook run` at autonomous checked
                        // nothing, which is the stance with nobody else to.
                        let goal =
                            self.rook.goal(self.session).ok().flatten().unwrap_or_else(|| prompt.to_string());
                        let (report, verdict) = self.goal_check(&goal, &mut outcome, &mut on_progress).await;
                        self.rook.log(self.session, EventKind::Note, "goal check", &report).ok();
                        match verdict {
                            Some("fails") => {
                                // "Put it right" was the whole of this, and it
                                // presumes the goal is a state the workspace
                                // can be put into. Asked to *check* a claim, a
                                // turn reported truthfully that the claim was
                                // false, was told the check failed and to put
                                // it right, and edited the very function it had
                                // been asked to judge — writing in its own
                                // reasoning that the instruction had changed
                                // and it would follow the newer one. Two models
                                // did it, the larger one with its eyes open.
                                // A checker is a second opinion and not an
                                // order, so disagreeing with it is a move the
                                // turn is allowed to have.
                                let told = format!(
                                    "Checked against the goal before finishing, and the check \
                                     fails:\n\n{}\n\nEither put it right and say what was \
                                     wrong, or say why the check is mistaken — if what you were \
                                     asked for was a finding, the finding standing is the work, \
                                     and making it come out otherwise would not be.",
                                    crate::sources::data("checker_report", "goal verification", &report)
                                );
                                messages.push(carried.clone());
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
                    // EndTurn closes a model message, including a progress-only
                    // "Let me delegate three sweeps". Check its intent before
                    // treating it as the end of the user's task. Checkers already
                    // have their own verdict protocol and must not check themselves.
                    if !self.checking
                        && !cut
                        && response.stop_reason == rook_llm::StopReason::EndTurn
                        && !response.message.content.trim().is_empty()
                    {
                        let decision = self
                            .completion_check(
                                prompt,
                                messages
                                    .iter()
                                    .rev()
                                    .find(|m| m.role == Role::User)
                                    .map(|m| m.content.as_str())
                                    .unwrap_or(prompt),
                                &response.message.content,
                                &mut outcome,
                                &mut on_progress,
                            )
                            .await;
                        // A new instruction can arrive during the check too.
                        let said = self.interjections.take();
                        if !said.is_empty() {
                            messages.push(carried.clone());
                            for text in said {
                                self.rook.log(
                                    self.session,
                                    EventKind::UserMessage,
                                    "while running",
                                    &text,
                                )?;
                                on_progress(Progress::Heard { text: &text });
                                messages.push(Message::user(&text));
                            }
                            continue;
                        }
                        match decision {
                            Ok(crate::completion::Action::Finish) => {}
                            Ok(crate::completion::Action::Continue) if completion_retries < 2 => {
                                completion_retries += 1;
                                self.rook.log(
                                    self.session,
                                    EventKind::Note,
                                    "completion",
                                    crate::completion::CONTINUE,
                                )?;
                                messages.push(carried.clone());
                                messages.push(Message::user(crate::completion::CONTINUE));
                                continue;
                            }
                            decision => {
                                let (stopped, note) = match decision {
                                    Ok(crate::completion::Action::Blocked) => (
                                        "blocked",
                                        "the model reported a refusal or blocker; requested work remains unchecked or incomplete".to_owned(),
                                    ),
                                    Ok(_) => (
                                        "incomplete",
                                        "the model repeatedly announced further work without performing it"
                                            .to_owned(),
                                    ),
                                    Err(note) => (
                                        if self.out_of_time() {
                                            "time"
                                        } else if self.overspent(&outcome) {
                                            "budget"
                                        } else {
                                            "completion_unchecked"
                                        },
                                        note,
                                    ),
                                };
                                outcome.stopped = stopped.into();
                                self.rook.log(self.session, EventKind::Note, stopped, &note)?;
                                self.report(Reported::Open(note));
                                return Ok(outcome);
                            }
                        }
                    }
                    outcome.stopped = response.stop_reason.as_str().into();
                    // Ending on the output limit is the model having no room to
                    // finish, not a turn that finished: asked once to go on it
                    // was cut again, and a reply cut mid-call writes nothing.
                    // Three hours of reading and no edit was this, and nothing
                    // said which number to change.
                    if outcome.stopped == "max_tokens" {
                        self.report(Reported::Open(format!(
                            "the model's reply was cut at {} tokens twice, so what it was writing \
                             never arrived — raise `[agent] max_output_tokens`, which a model that \
                             reasons out loud spends on reasoning",
                            self.rook.config.agent.max_output_tokens
                        )));
                    }
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
                    return Ok(outcome);
                }
                messages.push(carried.clone());
                for text in said {
                    self.rook.log(self.session, EventKind::UserMessage, "while running", &text).ok();
                    on_progress(Progress::Heard { text: &text });
                    messages.push(Message::user(&text));
                }
                continue;
            }

            completion_retries = 0;
            // Two calls given one id would be replayed as two results carrying
            // it, which every dialect rejects — so the model's mistake would
            // come back as an opaque error from the provider, after the work had
            // been done twice.
            let mut asked = carried.clone();
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
                    Some((_, times)) if *times >= 2 => {
                        let said = format!(
                            "`{}` with these same arguments was made {times} times this turn and \
                             answered the same each time; the answer is above — act on it, or ask \
                             something different",
                            call.name
                        );
                        // Logged, not only answered. The refusal is written
                        // here rather than by `dispatch`, so a turn spent in
                        // one was a transcript of nothing but the model's own
                        // messages: a hundred and seventy-five identical
                        // replies with no visible cause, and the one thing
                        // that would have explained them never recorded.
                        self.rook.log(self.session, EventKind::ToolResult, &call.name, &said).ok();
                        looping += 1;
                        (said, true)
                    }
                    _ => {
                        let done = self
                            .dispatch_recorded(call, &mut outcome, &mut on_progress, &crew, &mut nursery)
                            .await?;
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
                let recorded_body = rook_store::ObjectId::of(result.as_bytes());
                on_progress(Progress::ToolDone { name: &call.name, failed });
                for (_, name) in dropped.iter().filter(|(id, _)| *id == call.id) {
                    result.push_str(&format!(
                        "\n\n[`{name}` came with this same call id and was not made — one id per \
                         call, and it can be asked for again]"
                    ));
                }
                let shown = if call.name == LOAD_SKILL && !failed {
                    result
                } else {
                    let recorded = self
                        .rook
                        .store
                        .get_session(self.session)
                        .ok()
                        .flatten()
                        .and_then(|m| m.next_seq.checked_sub(1))
                        .and_then(|seq| self.rook.store.events(self.session, seq, 1).ok())
                        .and_then(|events| events.into_iter().next())
                        .filter(|e| {
                            e.record.kind == EventKind::ToolResult
                                && e.record.label == call.name
                                && e.record.body == recorded_body
                        });
                    match recorded {
                        Some(event) => crate::results::fresh(&event, &result),
                        None => crate::sources::tool_result(&call.name, &result),
                    }
                };
                messages.push(Message::tool_result(&call.id, shown));
            }

            // Told three times that it is asking the same thing again, and
            // asking it again. The refusal was written for a model that would
            // then move on; one that does not spends the whole step budget on
            // it — a hundred and ninety-four steps and six hundred thousand
            // tokens to arrive at "stopped at the step limit", which says
            // nothing about what went wrong. Ending here says it.
            if looping >= STUCK_ON_ONE_CALL {
                let said = "the same call was made over and over and answered the same way each \
                            time, so the turn was ended rather than spending the rest of its \
                            steps on it";
                self.rook.log(self.session, EventKind::Note, "looping", said).ok();
                self.report(Reported::Open(said.to_string()));
                stuck = true;
                // Out through the same door as the step limit, rather than
                // returning here: a turn that looped has usually already found
                // the answer — the live one had four passages of the
                // documentation it was asked for — and ending on the loop
                // leaves the person with the loop instead of the answer.
                break;
            }
        }

        // The two ceilings name themselves on the way out; the other two are
        // told apart here. All four leave through the same door below, which is
        // what asks the model for what it found rather than ending on a limit.
        if !matches!(outcome.stopped.as_str(), "budget" | "time") {
            outcome.stopped = if stuck { "looping" } else { "max_steps" }.into();
        }
        // The limit is the model's, not the children's: what they were still
        // doing is waited for here as it is at the end of a turn that finished.
        let left = drain_uncollected(&mut nursery, &mut outcome).await;
        // A turn that ran out of steps with a call as its last word has done
        // work nobody was told about. One more call, with nothing to reach
        // for, so the turn ends on what it found rather than on the limit —
        // and with what its sub-agents brought back in front of it.
        // A looping turn asks even with a reply already in hand: what it has is
        // the sentence it kept repeating on the way into the loop — "I will
        // answer, first let me check" — which is an announcement and not an
        // answer, and it is what the person would otherwise be handed.
        if (stuck || outcome.reply.trim().is_empty()) && !outcome.tools_called.is_empty() {
            if let Some(left) = &left {
                self.rook.log(self.session, EventKind::Note, "sub-agents", left).ok();
                messages.push(Message::user(crate::sources::data(
                    "subagent_report",
                    "collected child turns",
                    left,
                )));
            }
            let told = if stuck { STOP_ASKING } else { OUT_OF_STEPS };
            messages.push(Message::user(told));
            let why = if stuck { "looping" } else { "out of steps" };
            self.rook.log(self.session, EventKind::Note, why, told).ok();
            let used = measured(&messages, anchor);
            let mut request = Request::new(messages);
            request.effort = Some(self.effort);
            request.max_output_tokens = self.room_for_output(used);
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
        } else if let Some(left) = &left {
            outcome.reply.push_str(&format!("\n\n{left}"));
        }
        // For a front end, which renders silence as a hang. A child's silence
        // is the parent's to report, by what the child called.
        if self.depth == 0 && outcome.reply.is_empty() {
            outcome.reply =
                format!("(the model ended the turn without saying anything — {})", outcome.stopped);
        }
        Ok(outcome)
    }

    /// Skip every prompt: run whatever the deny list does not forbid.
    pub fn allow_everything_not_denied(&mut self) {
        let sandbox = &self.rook.config.sandbox;
        let (policy, _) =
            Policy::compile(rook_tools::policy::Stance::Autonomous, &sandbox.allow, &[], &sandbox.deny);
        self.policy = std::sync::Arc::new(policy);
    }

    fn report(&self, what: Reported) {
        if let Ok(mut list) = self.reported.lock() {
            list.push(what);
        }
    }

    /// Into the outcome, which is what every front end reads at the end.
    fn settle_reports(&self, outcome: &mut TurnOutcome) {
        if let Ok(mut wrote) = self.wrote_paths.lock() {
            outcome.files_changed = std::mem::take(&mut *wrote).into_iter().collect();
        }
        let Ok(mut list) = self.reported.lock() else { return };
        for what in list.drain(..) {
            match what {
                Reported::Decision(text) => outcome.decisions.push(text),
                Reported::Open(text) => outcome.open_questions.push(text),
            }
        }
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

/// The same, for a turn that spent itself on one call rather than on all of
/// them. It has the answer already — the live one had four passages of the
/// documentation it was asked for — and asking again is what it was doing.
const STOP_ASKING: &str = "\
You have asked the same thing several times and been answered the same way each time; the \
answer is above. Say now, in words and without calling anything: what you found, and what is \
still missing.";

const GO_ON: &str = "\
Your reply was cut off at the output limit. Go on from where it stopped, briefly: a call you \
were writing, make it whole; an answer, finish it.";

/// The label on the note that says what a command wrote. Read by `changes`,
/// which is the other half of this: one function writes it and one reads it,
/// and a third spelling of the word would be a third answer.
pub const WROTE: &str = "wrote";

/// Whether a note is prose somebody reads, rather than a record a program does.
///
/// Almost all of them are prose, and they are where a session says what
/// happened to it: that the process running a turn died before the turn did,
/// why a turn stopped, what the goal check made of it, that the model was asked
/// to answer in words. The window showed none of them — it drew a recalled
/// session from `user`, `assistant` and `tool-call` and dropped the rest — so a
/// session that died read as a session that simply stopped, in the one place a
/// person would go to ask. The browser had been showing them all along, which
/// makes it the rule about three front ends and one engine as well.
///
/// `WROTE` is the exception and the reason this is a question rather than a
/// constant: it is JSON for `changes` to read, and it belongs on a screen no
/// more than a row of a database does.
pub fn note_is_for_a_person(label: &str) -> bool {
    label != WROTE
}

const SAY_IT: &str = "\
You ended the turn without saying anything. Answer now, in words: what you found, or what \
you did and what is left.";

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
