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
    /// Optimistic revision for edits; accepted messages are immutable.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub withdrawn_at: Option<u64>,
    /// Submission retries compare the original content even after an edit.
    #[serde(default)]
    pub submitted_hash: Option<String>,
    #[serde(default)]
    pub follow_up: Option<FollowUp>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FollowUp {
    /// Opaque completion boundary, resolved once at submission.
    pub after: String,
    pub goal: Option<RunIdentity>,
    #[serde(default)]
    pub ready: bool,
    pub reserved: Option<String>,
    pub blocked: Option<String>,
}

impl Steering {
    pub fn queued(&self) -> bool {
        self.applied_at.is_none() && self.withdrawn_at.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditInstruction {
    pub revision: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WithdrawInstruction {
    pub revision: u64,
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
    /// Distinguishes successive goals in the same conversation.
    #[serde(default)]
    pub generation: String,
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

/// A running consumer must not adopt a replacement goal with the same ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunIdentity {
    pub id: String,
    pub generation: String,
}

impl Run {
    pub fn identity(&self) -> RunIdentity {
        RunIdentity {
            id: self.id.clone(),
            generation: if self.generation.is_empty() {
                format!("legacy-{}", self.created_at)
            } else {
                self.generation.clone()
            },
        }
    }
}
