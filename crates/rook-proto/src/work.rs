//! Durable work and steering receipts, shared by every front end.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Running,
    RetryWait,
    Paused,
    Blocked,
    Completed,
    Cancelled,
    Limited,
}

impl Status {
    pub fn runnable(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::RetryWait)
    }

    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Steering {
    /// Client-generated id makes retrying an interrupted submission idempotent.
    pub id: String,
    pub text: String,
    pub submitted_at: u64,
    /// Set only when the agent takes the message into its context, not on receipt.
    pub applied_at: Option<u64>,
    pub session: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub goal: String,
    pub workspace: Option<String>,
    /// Continue one existing conversation, including its transcript and settings.
    #[serde(default)]
    pub conversation: Option<Conversation>,
    #[serde(default)]
    pub autonomous: bool,
    /// None uses the configured default. Zero removes this particular ceiling.
    pub max_iterations: Option<u32>,
    pub max_tokens: Option<u64>,
    pub max_seconds: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub session: String,
    pub model: Option<String>,
    pub effort: String,
    pub stance: String,
    #[serde(default)]
    pub options: crate::TurnOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Steer {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Iteration {
    pub number: u32,
    pub session: String,
    pub stopped: String,
    pub changed: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    #[serde(default)]
    pub conversation: Option<Conversation>,
    pub workspace: String,
    pub goal: String,
    pub status: Status,
    pub reason: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub next_attempt_at: Option<u64>,
    pub autonomous: bool,
    pub max_iterations: u32,
    pub max_tokens: u64,
    pub max_seconds: u64,
    pub iterations: u32,
    pub tokens: u64,
    pub consecutive_failures: u32,
    pub session: Option<String>,
    pub instructions: Vec<Steering>,
    pub recent: Vec<Iteration>,
    pub reply: String,
    pub verification: String,
}
