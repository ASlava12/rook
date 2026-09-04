//! What the agent may do without being asked.
//!
//! An autonomous agent that runs shell commands needs an answer to "should this
//! one happen", and the answer cannot be a single global switch: `git status`
//! and `rm -rf` are not the same request. Rules are matched per command, the
//! default for anything unmatched is to ask, and a denial is never overridable
//! by an approval — those are the three properties that make the setting worth
//! having.

use std::collections::HashSet;
use std::sync::{Mutex, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// How much latitude the agent has: from watching every step to setting it a
/// task and standing back.
///
/// One idea rather than two. An approval mode and a level of autonomy are the
/// same question asked twice, and two ways to say one thing drift.
///
/// Ordered, and the order carries weight: a sub-agent inherits its parent's
/// stance and may be given less, never more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    /// Nothing that changes the machine runs at all.
    ReadOnly,
    /// Anything not explicitly allowed is confirmed first, and a fork in the
    /// work is put to the person rather than settled alone.
    #[default]
    #[serde(alias = "ask")]
    Assist,
    /// A task and its boundaries: anything not denied runs.
    #[serde(alias = "auto")]
    Autonomous,
    /// A goal and the freedom to choose how — including changing the machine
    /// outside the workspace, which no other stance does: installing a program
    /// the system's own way rather than under the state directory.
    Free,
}

impl Stance {
    /// The spelling a user writes, in config and everywhere it is offered.
    pub fn as_str(self) -> &'static str {
        match self {
            Stance::ReadOnly => "readonly",
            Stance::Assist => "assist",
            Stance::Autonomous => "autonomous",
            Stance::Free => "free",
        }
    }

    /// Every stance, least latitude first. One list, because an editor's menu
    /// and the policy answering it are the same question, and two lists of it
    /// drift — an ACP client was offered three names none of which was the one
    /// the policy reported as current.
    pub const ALL: [Stance; 4] = [Stance::ReadOnly, Stance::Assist, Stance::Autonomous, Stance::Free];

    /// The name a person reads.
    pub fn title(self) -> &'static str {
        match self {
            Stance::ReadOnly => "Read only",
            Stance::Assist => "Assist",
            Stance::Autonomous => "Autonomous",
            Stance::Free => "Free",
        }
    }

    /// What it means, in a sentence that fits beside the name.
    pub fn describe(self) -> &'static str {
        match self {
            Stance::ReadOnly => "Nothing that changes the machine runs at all.",
            Stance::Assist => "Ask before anything that changes the machine, and put a real choice to me.",
            Stance::Autonomous => "Run anything the deny list does not forbid, without asking.",
            Stance::Free => "Choose the means as well: may change the machine outside the workspace.",
        }
    }

    /// The old spellings are still read: a config written before the two ideas
    /// were one should keep working, and `as_str` answers with the name now.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "readonly" => Some(Stance::ReadOnly),
            "assist" | "ask" => Some(Stance::Assist),
            "autonomous" | "auto" => Some(Stance::Autonomous),
            "free" => Some(Stance::Free),
            _ => None,
        }
    }
}

/// What a tool call is about to do. Read-only calls never reach the policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Risk {
    ReadOnly,
    Write(Vec<String>),
    Execute(String),
    /// Reaching something that is not this machine.
    ///
    /// Its own kind rather than an `External`: what a rule wants to match here
    /// is where the request is going, so the subject is the url and an allow
    /// rule can name a host and mean it.
    Network(String),
    /// The agent asking to work with more latitude for the rest of the run.
    ///
    /// Through the policy like anything else, because that is what a person's
    /// approval is for: a deny rule can forbid it outright, and nobody being
    /// there leaves it unanswered rather than granted.
    Stance(Stance),
    /// A call into an MCP server's tool. Rook cannot see what one does, and the
    /// protocol's `readOnlyHint` is the claim of the very party whose behaviour
    /// is in question — so it goes through the policy like anything else that
    /// leaves the agent, and the claim is only repeated to the user.
    External {
        name: String,
        claims_read_only: bool,
    },
}

impl Risk {
    /// What each rule has to cover for this to be allowed without asking.
    ///
    /// One entry per command in a shell line and one per path in a write, so a
    /// rule that matches one of them does not carry the others through. `None`
    /// when it cannot be taken apart — a line with `$(…)` in it runs the
    /// commands inside too — and then the answer is to ask.
    pub fn parts(&self) -> Option<Vec<String>> {
        match self {
            Risk::ReadOnly => Some(Vec::new()),
            Risk::Write(paths) => Some(paths.clone()),
            Risk::Execute(line) => commands_in(line),
            Risk::Network(url) => Some(vec![url.clone()]),
            // Nothing to match an allow rule against: a raise is a person's to give.
            Risk::Stance(_) => None,
            Risk::External { name, .. } => Some(vec![name.clone()]),
        }
    }

    /// The family this belongs to, when there is one worth granting: the
    /// program a command runs, the directory a write lands in, the host a
    /// fetch reaches.
    ///
    /// Approving `cargo test -p rook-core` for the run does not cover `cargo
    /// test -p rook-cli`, so a person driving a build answers the same question
    /// with a different argument all afternoon. This is what they mean by "yes,
    /// and every one like it" — narrower than the stance, which allows
    /// everything, and wider than the line in front of them.
    ///
    /// `None` where there is no honest family: a stance is a person's to give,
    /// an MCP tool is already named by what it is, and a command line this
    /// cannot take apart is one nobody should be granting families from.
    pub fn kind(&self) -> Option<Vec<String>> {
        match self {
            Risk::Execute(line) => {
                let commands = commands_in(line)?;
                let programs: Vec<String> =
                    commands.iter().filter_map(|c| c.split_whitespace().next().map(str::to_string)).collect();
                (!programs.is_empty()).then_some(programs)
            }
            Risk::Write(paths) => {
                let dirs: Vec<String> = paths
                    .iter()
                    .map(|path| match path.rsplit_once(['/', '\\']) {
                        Some((dir, _)) => format!("{dir}/"),
                        None => "./".to_string(),
                    })
                    .collect();
                (!dirs.is_empty()).then_some(dirs)
            }
            Risk::Network(url) => {
                let host = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
                let host = host.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
                Some(vec![host.to_string()])
            }
            Risk::ReadOnly | Risk::Stance(_) | Risk::External { .. } => None,
        }
    }

    /// The text rules are matched against.
    pub fn subject(&self) -> String {
        match self {
            Risk::ReadOnly => String::new(),
            Risk::Write(paths) => paths.join(" "),
            Risk::Execute(command) => command.clone(),
            Risk::Network(url) => url.clone(),
            Risk::Stance(to) => format!("stance {}", to.as_str()),
            Risk::External { name, .. } => name.clone(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Risk::ReadOnly => "read".into(),
            Risk::Write(paths) => format!("write {}", paths.join(", ")),
            Risk::Execute(command) => format!("run `{command}`"),
            Risk::Network(url) => format!("fetch {url}"),
            Risk::Stance(to) => format!("work at `{}` for the rest of the run", to.as_str()),
            Risk::External { name, claims_read_only } => format!(
                "call the MCP tool `{name}`{}",
                if *claims_read_only { ", which its server calls read-only" } else { "" }
            ),
        }
    }
}

/// A pattern: a plain substring, or `/…/` for a regular expression.
#[derive(Clone, Debug)]
pub enum Rule {
    Contains(String),
    Matches(regex::Regex),
}

impl Rule {
    pub fn parse(pattern: &str) -> Result<Self, String> {
        match pattern.strip_prefix('/').and_then(|p| p.strip_suffix('/')) {
            Some(expression) => {
                regex::Regex::new(expression).map(Rule::Matches).map_err(|e| format!("{pattern}: {e}"))
            }
            None => Ok(Rule::Contains(pattern.to_string())),
        }
    }

    pub fn matches(&self, subject: &str) -> bool {
        match self {
            Rule::Contains(text) => subject.contains(text.as_str()),
            Rule::Matches(expression) => expression.is_match(subject),
        }
    }

    /// The same question of a path, where a plain rule has to line up with a
    /// directory boundary.
    ///
    /// `src/` means what is under `src`, not `notsrc/evil.rs` — which a
    /// substring match allows, and which no one writing that rule meant. A
    /// regular expression is left alone: someone who wrote one said what they
    /// meant.
    pub fn matches_path(&self, path: &str) -> bool {
        let Rule::Contains(text) = self else { return self.matches(path) };
        path.match_indices(text.as_str())
            .any(|(at, _)| at == 0 || path[..at].ends_with('/') || path[..at].ends_with('\\'))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny(String),
}

/// The commands a shell line runs, or nothing when that cannot be told.
///
/// Split on the separators rather than parsed: a quoted `;` splits a line that
/// a shell would not, which only ever means asking about something that could
/// have been allowed. Substitution is the case it cannot split at all — the
/// commands inside `$(…)` run too — so a line containing one is refused an
/// answer here and goes to the prompt.
fn commands_in(line: &str) -> Option<Vec<String>> {
    if line.contains("$(") || line.contains('`') {
        return None;
    }
    let parts: Vec<String> = line
        .split([';', '&', '|', '\n'])
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

#[derive(Default)]
pub struct Policy {
    /// Behind a lock so a front end can change it mid-run: the policy is shared
    /// and the editor offers the modes as a control.
    stance: RwLock<Stance>,
    pub allow: Vec<Rule>,
    pub ask: Vec<Rule>,
    pub deny: Vec<Rule>,
    /// Deny patterns that would not compile, kept because a boundary the user
    /// asked for and did not get is the one failure this policy must not have
    /// quietly.
    broken_deny: Vec<String>,
    /// Approvals the user extended to the rest of the run.
    granted: Mutex<HashSet<String>>,
    /// Families the user extended to the rest of the run — every `cargo`, every
    /// write under `src/`. Kept apart from `granted`, which holds whole
    /// subjects, because the two are matched differently: one is the thing
    /// itself, the other is what the thing belongs to.
    kinds: Mutex<HashSet<String>>,
}

impl Policy {
    pub fn new(stance: Stance) -> Self {
        Self { stance: RwLock::new(stance), ..Default::default() }
    }

    pub fn stance(&self) -> Stance {
        *self.stance.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_stance(&self, stance: Stance) {
        *self.stance.write().unwrap_or_else(|e| e.into_inner()) = stance;
    }

    /// Build from configured patterns, reporting the ones that would not compile
    /// rather than dropping them — a rule that silently never matches is worse
    /// than no rule.
    pub fn compile(stance: Stance, allow: &[String], ask: &[String], deny: &[String]) -> (Self, Vec<String>) {
        fn build(patterns: &[String]) -> (Vec<Rule>, Vec<String>) {
            let mut rules = Vec::new();
            let mut unusable = Vec::new();
            for pattern in patterns {
                match Rule::parse(pattern) {
                    Ok(rule) => rules.push(rule),
                    Err(e) => unusable.push(e),
                }
            }
            (rules, unusable)
        }

        let (allow_rules, mut errors) = build(allow);
        let (ask_rules, bad_ask) = build(ask);
        // A rule the user could not spell fails safe in two of the three lists:
        // dropping an `allow` only means being asked more often. Dropping a
        // `deny` means the boundary they asked for is not there, which is the
        // one thing this promises cannot happen — so it is kept, as the reason
        // nothing runs until it is fixed.
        let (deny_rules, broken_deny) = build(deny);
        errors.extend(bad_ask);
        errors.extend(broken_deny.iter().cloned());

        let policy = Self {
            stance: RwLock::new(stance),
            allow: allow_rules,
            ask: ask_rules,
            deny: deny_rules,
            broken_deny,
            granted: Mutex::new(HashSet::new()),
            kinds: Mutex::new(HashSet::new()),
        };
        (policy, errors)
    }

    pub fn decide(&self, risk: &Risk) -> Decision {
        // Reading still works, so the agent can open the file and say what is
        // wrong with it.
        if *risk == Risk::ReadOnly {
            return Decision::Allow;
        }
        if !self.broken_deny.is_empty() {
            return Decision::Deny(format!(
                "a deny rule in config.toml does not compile, so nothing that changes the \
                 machine runs until it is fixed: {}",
                self.broken_deny.join("; ")
            ));
        }
        let subject = risk.subject();

        // Denial first, and it is final: an approval prompt that can override
        // the deny list would make the deny list decorative.
        if let Some(rule) = self.deny.iter().find(|r| r.matches(&subject)) {
            return Decision::Deny(format!("matches the deny rule {rule:?}"));
        }
        // Asked even at read-only: asking for more is the one thing a stance
        // that changes nothing is still allowed to do.
        if let Risk::Stance(_) = risk {
            return Decision::Ask;
        }
        if self.stance() == Stance::ReadOnly {
            return Decision::Deny("read-only mode: nothing may change the machine".into());
        }
        if self.granted.lock().is_ok_and(|g| g.contains(&subject)) {
            return Decision::Allow;
        }
        // Every part again, and for the same reason: `cargo build && rm -rf ~`
        // is two families, and granting one of them is not granting the line.
        if let Some(kinds) = risk.kind()
            && self.kinds.lock().is_ok_and(|granted| kinds.iter().all(|k| granted.contains(k)))
        {
            return Decision::Allow;
        }
        // Every part, not just one: `ls && rm -rf ~` began with something
        // allowed, and a write touching `src/main.rs` and `/etc/passwd` had one
        // path that matched. Allowing either meant not asking about the rest.
        let covered = |part: &String| match risk {
            Risk::Write(_) => self.allow.iter().any(|r| r.matches_path(part)),
            _ => self.allow.iter().any(|r| r.matches(part)),
        };
        if let Some(parts) = risk.parts()
            && parts.iter().all(covered)
        {
            return Decision::Allow;
        }
        if self.ask.iter().any(|r| r.matches(&subject)) {
            return Decision::Ask;
        }
        match self.stance() {
            Stance::Autonomous | Stance::Free => Decision::Allow,
            _ => Decision::Ask,
        }
    }

    pub fn grant_for_run(&self, subject: &str) {
        if let Ok(mut granted) = self.granted.lock() {
            granted.insert(subject.to_string());
        }
    }

    /// Allow everything of the same family for the rest of the run. Silent when
    /// the risk has no family: the front end only offers it where there is one.
    pub fn grant_kind_for_run(&self, risk: &Risk) {
        if let (Some(kinds), Ok(mut granted)) = (risk.kind(), self.kinds.lock()) {
            granted.extend(kinds);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    Once,
    /// Allow this exact subject for the rest of the run.
    ForRun,
    /// Allow everything of the same family for the rest of the run: every
    /// `cargo` command, every write under this directory.
    KindForRun,
    /// A person said no. What they decided is on the record as a decision.
    Deny(String),
    /// Nobody was there to say anything. Told apart from a refusal because the
    /// two are different things to report: one is settled, the other is a
    /// question still waiting for whoever comes back.
    Unanswered(String),
}

impl Approval {
    /// A person said no. Worded so the model stops and asks rather than
    /// looking for a route around it: a refusal that reads like a failure gets
    /// retried through another tool, which is the one thing it was there to
    /// prevent.
    pub fn declined() -> Self {
        Approval::Deny(
            "the person driving this refused it deliberately — nothing failed, and no other \
             tool or sub-agent will be allowed the same thing. Ask them what they would rather \
             you did."
                .into(),
        )
    }

    /// What the user is shown after answering. `ForRun` is a Rust name, and the
    /// TUI was printing it back at them.
    pub fn describe(&self) -> String {
        match self {
            Approval::Once => "allowed once".into(),
            Approval::ForRun => "allowed for the rest of the run".into(),
            Approval::KindForRun => "every one like it allowed for the rest of the run".into(),
            Approval::Deny(why) => format!("refused — {why}"),
            Approval::Unanswered(why) => format!("unanswered — {why}"),
        }
    }
}

#[async_trait]
pub trait Approver: Send + Sync {
    /// `preview` is what the call would change, when the tool can say. Passed
    /// beside the risk rather than folded into it: the risk is what a rule
    /// matches on, and a diff is not that.
    async fn ask(&self, tool: &str, risk: &Risk, preview: Option<&str>) -> Approval;
}

/// The approver used when nothing can prompt — a script, a cron run, the daemon.
///
/// Refusing rather than allowing: an unattended agent is exactly where an
/// unreviewed command does the most damage, and the message tells the model what
/// would make it possible.
pub struct Unattended;

#[async_trait]
impl Approver for Unattended {
    async fn ask(&self, _tool: &str, risk: &Risk, _preview: Option<&str>) -> Approval {
        // Addressed to the model first, because the model is what reads it. The
        // remedies are all things only the person can do, and a refusal that
        // offers nothing actionable is one an agent works around: asked to edit
        // one line unattended, a real model spent nine steps and four minutes
        // trying other tools and then delegating the same task to a sub-agent,
        // which is refused for the same reason.
        Approval::Unanswered(format!(
            "`{}` needs someone to approve it and nobody is here. Stop and say what you \
             were about to do — no other tool, and no sub-agent, can get past this. For the user: \
             re-run interactively, pass --yes, or add a rule under [sandbox] allow in config.toml.",
            risk.describe()
        ))
    }
}

/// What a front end is being asked to decide.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub id: String,
    pub tool: String,
    pub action: String,
    /// What the call would change, when the tool can say.
    pub preview: Option<String>,
    /// The family a front end can offer to allow wholesale — `cargo`, `src/` —
    /// empty when this risk has none. Carried rather than recomputed, because
    /// the risk itself does not travel to a browser and every front end has to
    /// offer the same three answers.
    pub kind: Vec<String>,
}

/// An approver that hands the question to whatever is driving the UI and waits
/// for an answer to come back by id.
pub struct ChannelApprover(crate::pending::Pending<ApprovalRequest, Approval>);

impl ChannelApprover {
    pub fn new(
        requests: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
        patience: std::time::Duration,
    ) -> Self {
        Self(crate::pending::Pending::new(requests, patience))
    }

    pub fn answer(&self, id: &str, approval: Approval) {
        self.0.answer(id, approval);
    }
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn ask(&self, tool: &str, risk: &Risk, preview: Option<&str>) -> Approval {
        let request = |id| ApprovalRequest {
            id,
            tool: tool.to_string(),
            action: risk.describe(),
            preview: preview.map(str::to_string),
            kind: risk.kind().unwrap_or_default(),
        };
        match self.0.ask(request).await {
            Ok(approval) => approval,
            Err(unanswered) => Approval::Deny(unanswered.to_string()),
        }
    }
}
