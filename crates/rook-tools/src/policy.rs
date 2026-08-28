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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Anything not denied runs.
    Auto,
    /// Anything not explicitly allowed is confirmed first.
    #[default]
    Ask,
    /// Nothing that changes the machine runs at all.
    ReadOnly,
}

impl Mode {
    /// The spelling a user writes, in config and everywhere it is offered.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Ask => "ask",
            Mode::ReadOnly => "readonly",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Mode::Auto),
            "ask" => Some(Mode::Ask),
            "readonly" => Some(Mode::ReadOnly),
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
            Risk::External { name, .. } => Some(vec![name.clone()]),
        }
    }

    /// The text rules are matched against.
    pub fn subject(&self) -> String {
        match self {
            Risk::ReadOnly => String::new(),
            Risk::Write(paths) => paths.join(" "),
            Risk::Execute(command) => command.clone(),
            Risk::Network(url) => url.clone(),
            Risk::External { name, .. } => name.clone(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Risk::ReadOnly => "read".into(),
            Risk::Write(paths) => format!("write {}", paths.join(", ")),
            Risk::Execute(command) => format!("run `{command}`"),
            Risk::Network(url) => format!("fetch {url}"),
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
    mode: RwLock<Mode>,
    pub allow: Vec<Rule>,
    pub ask: Vec<Rule>,
    pub deny: Vec<Rule>,
    /// Deny patterns that would not compile, kept because a boundary the user
    /// asked for and did not get is the one failure this policy must not have
    /// quietly.
    broken_deny: Vec<String>,
    /// Approvals the user extended to the rest of the run.
    granted: Mutex<HashSet<String>>,
}

impl Policy {
    pub fn new(mode: Mode) -> Self {
        Self { mode: RwLock::new(mode), ..Default::default() }
    }

    pub fn mode(&self) -> Mode {
        *self.mode.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.write().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// Build from configured patterns, reporting the ones that would not compile
    /// rather than dropping them — a rule that silently never matches is worse
    /// than no rule.
    pub fn compile(mode: Mode, allow: &[String], ask: &[String], deny: &[String]) -> (Self, Vec<String>) {
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
            mode: RwLock::new(mode),
            allow: allow_rules,
            ask: ask_rules,
            deny: deny_rules,
            broken_deny,
            granted: Mutex::new(HashSet::new()),
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
        if self.mode() == Mode::ReadOnly {
            return Decision::Deny("read-only mode: nothing may change the machine".into());
        }
        if self.granted.lock().is_ok_and(|g| g.contains(&subject)) {
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
        match self.mode() {
            Mode::Auto => Decision::Allow,
            _ => Decision::Ask,
        }
    }

    pub fn grant_for_run(&self, subject: &str) {
        if let Ok(mut granted) = self.granted.lock() {
            granted.insert(subject.to_string());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    Once,
    /// Allow this exact subject for the rest of the run.
    ForRun,
    Deny(String),
}

impl Approval {
    /// What the user is shown after answering. `ForRun` is a Rust name, and the
    /// TUI was printing it back at them.
    pub fn describe(&self) -> String {
        match self {
            Approval::Once => "allowed once".into(),
            Approval::ForRun => "allowed for the rest of the run".into(),
            Approval::Deny(why) => format!("refused — {why}"),
        }
    }
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn ask(&self, tool: &str, risk: &Risk) -> Approval;
}

/// The approver used when nothing can prompt — a script, a cron run, the daemon.
///
/// Refusing rather than allowing: an unattended agent is exactly where an
/// unreviewed command does the most damage, and the message tells the model what
/// would make it possible.
pub struct Unattended;

#[async_trait]
impl Approver for Unattended {
    async fn ask(&self, _tool: &str, risk: &Risk) -> Approval {
        // Addressed to the model first, because the model is what reads it. The
        // remedies are all things only the person can do, and a refusal that
        // offers nothing actionable is one an agent works around: asked to edit
        // one line unattended, a real model spent nine steps and four minutes
        // trying other tools and then delegating the same task to a sub-agent,
        // which is refused for the same reason.
        Approval::Deny(format!(
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
    async fn ask(&self, tool: &str, risk: &Risk) -> Approval {
        let request = |id| ApprovalRequest { id, tool: tool.to_string(), action: risk.describe() };
        match self.0.ask(request).await {
            Ok(approval) => approval,
            Err(unanswered) => Approval::Deny(unanswered.to_string()),
        }
    }
}
