//! What the agent may do without being asked.
//!
//! An autonomous agent that runs shell commands needs an answer to "should this
//! one happen", and the answer cannot be a single global switch: `git status`
//! and `rm -rf` are not the same request. Rules are matched per command, the
//! default for anything unmatched is to ask, and a denial is never overridable
//! by an approval — those are the three properties that make the setting worth
//! having.

use std::collections::HashSet;
use std::sync::Mutex;

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
    pub mode: Mode,
    pub allow: Vec<Rule>,
    pub ask: Vec<Rule>,
    pub deny: Vec<Rule>,
    /// Approvals the user extended to the rest of the run.
    granted: Mutex<HashSet<String>>,
}

impl Policy {
    pub fn new(mode: Mode) -> Self {
        Self { mode, ..Default::default() }
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
            mode,
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
        if self.mode == Mode::ReadOnly {
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
        match self.mode {
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
///
/// The terminal UI and the websocket both need exactly this, and an approval
/// that behaves differently depending on which front end is attached would be a
/// bug rather than a feature.
pub struct ChannelApprover {
    requests: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
    waiting: tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<Approval>>>,
    next_id: std::sync::atomic::AtomicU64,
    patience: std::time::Duration,
}

impl ChannelApprover {
    /// `patience` bounds the wait: a closed tab or an abandoned terminal would
    /// otherwise leave the turn pending forever, holding its locks with it.
    pub fn new(
        requests: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
        patience: std::time::Duration,
    ) -> Self {
        Self {
            requests,
            waiting: Default::default(),
            next_id: std::sync::atomic::AtomicU64::new(1),
            patience,
        }
    }

    pub fn answer(&self, id: &str, approval: Approval) {
        if let Ok(mut waiting) = self.waiting.try_lock()
            && let Some(tx) = waiting.remove(id)
        {
            let _ = tx.send(approval);
        }
    }
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn ask(&self, tool: &str, risk: &Risk) -> Approval {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiting.lock().await.insert(id.clone(), tx);

        let request = ApprovalRequest { id: id.clone(), tool: tool.to_string(), action: risk.describe() };
        if self.requests.send(request).is_err() {
            return Approval::Deny("nothing is listening for approvals".into());
        }

        match tokio::time::timeout(self.patience, rx).await {
            Ok(Ok(approval)) => approval,
            Ok(Err(_)) => Approval::Deny("the approval was dropped".into()),
            Err(_) => {
                self.waiting.lock().await.remove(&id);
                Approval::Deny(format!("no answer within {}s", self.patience.as_secs()))
            }
        }
    }
}
