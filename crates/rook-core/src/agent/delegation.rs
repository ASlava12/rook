//! Subagent execution, isolation, steering and result collection.

use super::SUBAGENTS;
use super::effects::Shown;
use super::{
    AgentLoop, DELEGATE, Interjections, Progress, Reported, TurnOutcome, finished, jobs_for, servers_for,
};
use crate::error::{CoreError, Result};
use crate::hooks::Hooks;
use crate::service::Rook;
use futures_util::StreamExt;
use rook_llm::{Delta, Provider};
use rook_store::EventKind;
use rook_tools::policy::{Approver, Policy, Stance};
use rook_tools::{ToolBox, ToolContext};

/// What an errand may spend: steps, and its share of the turn's allowance.
///
/// Together because they are one question — how far this may go — and apart
/// they were two arguments among eight, which is where a caller starts passing
/// them in the wrong order.
#[derive(Clone)]
struct Bounds {
    isolated: bool,
    steps: Option<u32>,
    /// The turn's deadline, passed down unchanged: every sub-agent of a turn
    /// finishes by the moment the turn does.
    by: Option<std::time::Instant>,
    /// A share rather than the whole remainder: the errands of one call run at
    /// the same time, and each taking the remainder is the multiplication the
    /// ceiling exists to stop.
    tokens: u64,
    /// What this errand runs on, and how hard it thinks.
    ///
    /// Not a bound, and here because this is what a child is handed: a second
    /// struct beside this one would be two things to keep in step at the same
    /// four call sites. `None` is the configured default either way.
    provider: Option<std::sync::Arc<dyn Provider>>,
    effort: Option<rook_llm::Effort>,
}

pub(super) struct Crew<'a> {
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
    /// What the turn had left to spend when the crew was assembled, shared out
    /// among the errands it is given. 0 lifts the ceiling, as everywhere else.
    left_to_spend: u64,
    /// The turn's deadline, which every child of it shares rather than divides.
    by: Option<std::time::Instant>,
}

/// Sub-tasks a turn has started and not yet collected.
///
/// Held across the parent's steps, which is the whole difference between this
/// and `delegate`: a parent blocked on its children can neither look at them,
/// nor say anything to them, nor do anything else while they run.
pub(super) struct Nursery<'f> {
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
    parallel: usize,
    doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
}

/// What a sub-task came back with: the session it ran in and what it did, or
/// why it could not.
type Landed = Result<(String, TurnOutcome)>;

/// One sub-task, running. Boxed because this is the turn calling itself.
type Child<'f> = std::pin::Pin<Box<dyn std::future::Future<Output = (usize, Landed)> + Send + 'f>>;

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

/// A turn does not end with its work still out. What the model never collected
/// is waited for and handed back rather than dropped at the door: the children
/// have already spent the tokens, and their answers are the reason they ran.
/// The report, if there was anything to collect.
pub(super) async fn drain_uncollected(
    nursery: &mut Nursery<'_>,
    outcome: &mut TurnOutcome,
) -> Option<String> {
    if !nursery.busy() && nursery.taken.iter().all(|taken| *taken) {
        return None;
    }
    while nursery.collect_next().await.is_some() {}
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
    (!left.is_empty())
        .then(|| format!("(sub-agents this turn started and did not collect)\n\n{}", left.join("\n\n")))
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

/// The head of a sub-task, for a progress line. A task is a whole instruction —
/// a live one ran to two hundred characters — and repeating it on every step
/// buries what the step actually was.
pub(super) fn short(task: &str) -> &str {
    let line = task.lines().next().unwrap_or(task);
    match line.char_indices().nth(48) {
        Some((cut, _)) => &line[..cut],
        None => line,
    }
}

/// The fewest steps a sub-task is given whatever the model asked for: a call,
/// a look at what came back, and an answer.
const SUBTASK_STEPS_FLOOR: u32 = 3;

impl Crew<'_> {
    async fn run_subtask(
        &self,
        task: &str,
        inherited: Option<&str>,
        bounds: Bounds,
        doing: tokio::sync::mpsc::UnboundedSender<(usize, String)>,
        index: usize,
        said: std::sync::Arc<Interjections>,
    ) -> Result<(String, TurnOutcome)> {
        let session = self.rook.fork_for_subtask(self.parent, task)?;
        let mut tree =
            if bounds.isolated { Some(crate::worktrees::create(self.rook, session).await?) } else { None };
        let _finished = tree.as_ref().map(|_| crate::worktrees::Finished(self.rook, session));
        let isolated_rook = tree.as_ref().map(|tree| self.rook.for_workspace(tree.path.clone()));
        let rook = isolated_rook.as_ref().unwrap_or(self.rook);
        if let Some(context) = inherited {
            self.rook.log(session, EventKind::Note, "inherited", context).ok();
        }

        // What the call asked for, or what the turn is using.
        let chosen = bounds.provider.clone().unwrap_or_else(|| self.provider.clone());
        let mut child = AgentLoop::new(rook, chosen, session);
        child.depth = self.depth + 1;
        if tree.is_none() {
            child.tools = self.tools.clone();
            child.tool_ctx = self.tool_ctx.clone();
            child.servers = self.servers.clone();
        } else {
            // MCP servers and editor bridges may be rooted in the parent. Local
            // tools, language servers and jobs must be constructed for this tree.
            child.tool_ctx.delegated = true;
            child.tool_ctx.allow_outside_workspace = false;
            child.servers = servers_for(&rook.config, &rook.workspace);
            crate::lsp::register(&mut child.tools, child.servers.clone());
            child.tool_ctx.jobs = Some(jobs_for(&rook.config));
            child.tools.register(std::sync::Arc::new(rook_tools::jobs::JobTool));
        }
        child.policy = self.policy.clone();
        child.approver = self.approver.clone();
        // Deliberately not `ask_via`: a subagent the user did not start should
        // not interrupt them, and its parent is the one holding the context to
        // judge the answer.
        child.hooks = self.hooks.clone();
        child.spawned = self.spawned.clone();
        // Its own queue, not the parent's: what the user says while several of
        // these run has to reach all of them, and taking from one queue would
        // give it to whichever child stepped first.
        child.interjections = said;
        // A sub-task is a bounded errand, and lower effort means fewer and more
        // consolidated tool calls rather than a worse answer.
        child.effort = bounds.effort.unwrap_or(rook_llm::Effort::Low);
        child.max_steps = bounds.steps.unwrap_or(self.max_steps);
        child.max_turn_tokens = bounds.tokens;
        child.by = bounds.by;

        // Boxed because this is `run` calling itself through a tool call. The
        // channel carries only tool names, so it holds at most one short string
        // per step the children are already bounded to.
        let where_it_runs = rook.workspace.clone();
        let result = Box::pin(child.run_with(task, move |progress| {
            if let Progress::Delta(Delta::ToolCall(call)) = progress {
                let _ = doing
                    .send((index, crate::calls::doing(&call.name, Some(&call.arguments), &where_it_runs)));
            }
        }))
        .await;
        let mut report = None;
        if let Some(tree) = tree.as_mut() {
            tree.finished = true;
            tree.save(self.rook, session)?;
            report = Some(tree.report(session));
        }
        let mut outcome =
            result.map_err(|why| CoreError::Other(format!("{why}\n{}", report.as_deref().unwrap_or(""))))?;
        if let Some(report) = report {
            outcome.reply.push_str(&format!("\n\n{report}"));
        }
        Ok((rook_store::format_session_id(session), outcome))
    }
}

impl<'f> Nursery<'f> {
    pub(super) fn relay(&self, from: &Interjections, carried: &mut Vec<String>) {
        relay(from, &self.said, carried);
    }

    /// Polling remains cancellation-safe: a finished child's receipt is kept
    /// before yielding control back to the parent's select loop.
    pub(super) async fn collect_next(&mut self) -> Option<()> {
        let (at, result) = self.running.next().await?;
        self.landed[at] = Some(result);
        Some(())
    }

    pub(super) fn new(parallel: usize) -> (Self, tokio::sync::mpsc::UnboundedReceiver<(usize, String)>) {
        let (doing, steps) = tokio::sync::mpsc::unbounded_channel();
        let nursery = Self {
            running: Default::default(),
            tasks: Vec::new(),
            said: Vec::new(),
            landed: Vec::new(),
            taken: Vec::new(),
            limit: std::sync::Arc::new(tokio::sync::Semaphore::new(parallel.max(1))),
            // How many can be running at once, which is how the turn's
            // remaining allowance is shared among errands started one by one.
            parallel: parallel.max(1),
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
        mut bounds: Bounds,
    ) -> String {
        let at = self.tasks.len();
        let said: std::sync::Arc<Interjections> = Default::default();
        self.tasks.push(task.to_string());
        self.said.push(said.clone());
        self.landed.push(None);
        self.taken.push(false);
        let task = task.to_string();
        // The most that can be running at once is what the remainder is shared
        // among: started one at a time, each taking the whole of it would be
        // the multiplication the ceiling exists to stop.
        let share = match crew.left_to_spend {
            0 => 0,
            left => (left / self.parallel as u64).max(1),
        };
        let (limit, doing) = (self.limit.clone(), self.doing.clone());
        // Read before the move: every child of a turn shares its deadline.
        bounds.by = crew.by;
        bounds.tokens = share;
        self.running.push(Box::pin(async move {
            let _permit = limit.acquire().await;
            (at, crew.run_subtask(&task, inherited.as_deref(), bounds, doing, at, said).await)
        }));
        name_of(at)
    }

    pub(super) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        (0..self.tasks.len()).find(|at| name_of(*at) == name)
    }

    /// Whether there is anything left to wait for: one named child, or all.
    fn all_in(&self, at: Option<usize>) -> bool {
        match at {
            Some(at) => self.landed[at].is_some(),
            None => self.landed.iter().all(Option::is_some),
        }
    }

    pub(super) fn names(&self) -> Vec<String> {
        (0..self.tasks.len()).map(name_of).collect()
    }

    /// Where a child got to, as far as anyone here can see: a running one is
    /// known by its task and nothing else until it lands.
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

impl<'a> AgentLoop<'a> {
    /// Run a sub-task in its own session and return only what it concluded.
    ///
    /// The child's full transcript stays in the store, linked to this session by
    /// its parent, so the detail is recoverable without ever entering this
    /// conversation's context — which is the entire point.
    pub(super) async fn delegate<'f>(
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
        let isolated = match args.get("isolation") {
            None => false,
            Some(value) if value.as_str() == Some("shared") => false,
            Some(value) if value.as_str() == Some("worktree") => true,
            _ => return "isolation must be shared or worktree".into(),
        };
        if isolated {
            if self.tool_ctx.files.is_some() || self.tool_ctx.terminals.is_some() {
                return "worktree isolation requires local disk tools; editor-owned files/terminals are not supported".into();
            }
            let paths = match crate::worktrees::allocation_paths(self.rook).await {
                Ok(paths) => paths,
                Err(why) => return why.to_string(),
            };
            let risk = rook_tools::policy::Risk::Write(paths);
            if let Some(refusal) = self.gate_risk(DELEGATE, args, risk, Shown::Text("Create detached Git worktrees for these tasks; keep their edits separately for review.")).await { return refusal; }
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
        // decides how long its own sub-agents may run. And never below what a
        // task needs — a call, a look at what came back, an answer: a model
        // wrote `max_steps: 1`, and its sub-agent read the file and had no
        // step left to say what it read.
        // What this call asked to run on, and how hard. The configuration sets
        // the default and a call may ask upward: `careful` is the turn's own
        // model, which is the way round that fails safely — a call that asks
        // for nothing gets what the operator chose, and a bad guess by the
        // model costs speed rather than the answer.
        let careful = args.get("care").and_then(|m| m.as_str()).map(str::trim) == Some("careful");
        // An endpoint the call named, where the configuration offers a choice.
        // `careful` still wins: it asks for the turn's own model, which is a
        // statement about how much judgement the task needs rather than about
        // which machine is free.
        let named = args.get("model").and_then(|m| m.as_str()).map(str::trim).filter(|m| !m.is_empty());
        let provider = match (careful, named) {
            (true, _) => None,
            (false, None) => Some(self.errand_provider()),
            (false, Some(named)) => match self.errand_on(named).await {
                Ok(chosen) => Some(chosen),
                Err(why) => return why,
            },
        };
        let effort = careful.then_some(self.effort);
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
            let names: Vec<String> = tasks
                .iter()
                .map(|task| {
                    nursery.start(
                        crew,
                        task,
                        inherited.clone(),
                        Bounds {
                            steps: max_steps,
                            by: crew.by,
                            tokens: 0,
                            provider: provider.clone(),
                            effort,
                            isolated,
                        },
                    )
                })
                .collect();
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
        let crew = self.crew(0);
        let crew = &crew;
        // Shared out rather than handed to each: errands of one call run at the
        // same time, and each taking the whole remainder is the multiplication
        // the ceiling exists to stop.
        let each = match self.left_to_spend(outcome) {
            0 => 0,
            left => (left / total.max(1) as u64).max(1),
        };
        let running: futures_util::stream::FuturesUnordered<_> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let limit = limit.clone();
                let inherited = inherited.clone();
                let doing = doing.clone();
                let said = relayed[i].clone();
                let provider = provider.clone();
                async move {
                    let _permit = limit.acquire().await;
                    let bounds =
                        Bounds { steps: max_steps, by: self.by, tokens: each, provider, effort, isolated };
                    (i, crew.run_subtask(task, inherited.as_deref(), bounds, doing, i, said).await)
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
                Some((i, doing)) = steps.recv() => {
                    on_progress(Progress::Delegating { at: i, doing: &doing });
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
        while let Ok((i, doing)) = steps.try_recv() {
            on_progress(Progress::Delegating { at: i, doing: &doing });
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
    pub(super) async fn subagents(
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
                let Ok(Some(())) = tokio::time::timeout_at(deadline, nursery.collect_next()).await else {
                    break;
                };
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

    /// What a sub-task needs from the turn that started it, owned.
    ///
    /// Taken out of the loop rather than read from it so a child's future
    /// borrows the engine and not the parent: the parent has to keep stepping
    /// while they run, and a future holding `&self` freezes it.
    pub(super) fn crew(&self, left_to_spend: u64) -> Crew<'a> {
        Crew {
            rook: self.rook,
            provider: self.provider.clone(),
            tools: self.tools.clone(),
            // What tells a child from the turn that started it, everywhere a
            // tool can see. The policy is shared on purpose — an approval given
            // for the run is given for the run — so this is where "and it is a
            // sub-agent" has to live.
            tool_ctx: rook_tools::ToolContext { delegated: true, ..self.tool_ctx.clone() },
            policy: self.policy.clone(),
            approver: self.approver.clone(),
            hooks: self.hooks.clone(),
            servers: self.servers.clone(),
            spawned: self.spawned.clone(),
            parent: self.session,
            depth: self.depth,
            max_steps: self.max_steps,
            left_to_spend,
            by: self.by,
        }
    }

    /// A sub-task on the endpoint the call asked for by name.
    ///
    /// Refused rather than quietly given another: a model that named an
    /// endpoint and silently got a different one has been told its choice was
    /// honoured when it was not, and the answer it comes back with is about a
    /// model nobody thinks it used.
    ///
    /// Who decides is the stance, and the line it draws here is the one it
    /// draws everywhere else. Up to `assist` the person is asked, because
    /// running this workspace's work through a particular endpoint is a
    /// decision with a cost — somebody's tokens, or content leaving for a host
    /// they did not pick for this. Past it the agent decides alone and says
    /// which and why, rather than proceeding quietly.
    async fn errand_on(&self, named: &str) -> std::result::Result<std::sync::Arc<dyn Provider>, String> {
        let config = &self.rook.config;
        if !config.models.contains_key(named) {
            return Err(match config.models.is_empty() {
                true => format!(
                    "{named:?} is not an endpoint: nothing is configured under `[models]`, so \
                     there is nothing to choose between. Leave `model` out."
                ),
                false => format!(
                    "{named:?} is not one of the configured endpoints. There is: {}.",
                    config.models.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        // The address rather than the name, because that is what a rule in the
        // policy can match and what a person reading the question needs: the
        // name says nothing about where the work would go.
        let going_to = crate::models::endpoint_for(config, &self.vault, named)
            .ok()
            .flatten()
            .map(|endpoint| endpoint.url)
            .unwrap_or_else(|| named.to_string());

        if self.policy.stance() <= Stance::Assist {
            let risk = rook_tools::policy::Risk::Network(going_to.clone());
            let asking = format!("a sub-task would run on {named}, at {going_to}");
            match self.approver.ask(DELEGATE, &risk, Some(&asking)).await {
                rook_tools::policy::Approval::Once => {}
                rook_tools::policy::Approval::ForRun => self.policy.grant_for_run(&risk.subject()),
                rook_tools::policy::Approval::KindForRun => self.policy.grant_kind_for_run(&risk),
                rook_tools::policy::Approval::Deny(why) => return Err(format!("refused: {why}")),
                rook_tools::policy::Approval::Unanswered(why) => {
                    return Err(rook_tools::policy::no_one_answered(&why));
                }
            }
        } else {
            self.report(Reported::Decision(format!(
                "a sub-task on {named}, at {going_to} — asked for by the call"
            )));
        }

        match crate::models::errand_provider_for(config, &self.vault, named) {
            Ok(chosen) => Ok(std::sync::Arc::from(chosen)),
            Err(why) => Err(format!("{named:?} cannot be used: {why}")),
        }
    }

    /// What a delegated errand runs on.
    ///
    /// The same shape as [`Self::summariser`], and the same reasoning: an
    /// errand is bounded work to get through rather than the judgement the turn
    /// was asked for, and a smaller model on the same endpoint gets through it
    /// faster. A call that says `careful` is given the turn's own instead —
    /// the configuration sets the default and the model may ask upward, which
    /// is the way round that fails safely.
    fn errand_provider(&self) -> std::sync::Arc<dyn Provider> {
        let config = &self.rook.config.agent;
        let spec = config.errand_model.trim();
        if spec.is_empty() || spec == config.model {
            return self.provider.clone();
        }
        match crate::models::errand_provider_for(&self.rook.config, &self.vault, spec) {
            Ok(provider) => std::sync::Arc::from(provider),
            Err(e) => {
                tracing::warn!(
                    "`[agent] errand_model` {spec:?} could not be built ({e}); using {}",
                    config.model
                );
                self.provider.clone()
            }
        }
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
