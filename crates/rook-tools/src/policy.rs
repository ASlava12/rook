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
}

impl Risk {
    /// The text rules are matched against.
    pub fn subject(&self) -> String {
        match self {
            Risk::ReadOnly => String::new(),
            Risk::Write(paths) => paths.join(" "),
            Risk::Execute(command) => command.clone(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Risk::ReadOnly => "read".into(),
            Risk::Write(paths) => format!("write {}", paths.join(", ")),
            Risk::Execute(command) => format!("run `{command}`"),
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny(String),
}

#[derive(Default)]
pub struct Policy {
    /// Behind a lock so a front end can change it mid-run: the policy is shared
    /// and the editor offers the modes as a control.
    mode: RwLock<Mode>,
    pub allow: Vec<Rule>,
    pub ask: Vec<Rule>,
    pub deny: Vec<Rule>,
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
        let mut errors = Vec::new();
        let mut build = |patterns: &[String]| {
            patterns
                .iter()
                .filter_map(|p| match Rule::parse(p) {
                    Ok(rule) => Some(rule),
                    Err(e) => {
                        errors.push(e);
                        None
                    }
                })
                .collect()
        };
        let policy = Self {
            mode: RwLock::new(mode),
            allow: build(allow),
            ask: build(ask),
            deny: build(deny),
            granted: Mutex::new(HashSet::new()),
        };
        (policy, errors)
    }

    pub fn decide(&self, risk: &Risk) -> Decision {
        if *risk == Risk::ReadOnly {
            return Decision::Allow;
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
        if self.allow.iter().any(|r| r.matches(&subject)) {
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
        Approval::Deny(format!(
            "nothing can approve `{}` here. Re-run interactively, pass --yes, or add a rule under \
             [sandbox] allow in config.toml",
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
