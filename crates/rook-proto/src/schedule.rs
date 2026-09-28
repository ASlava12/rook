//! Persisted schedules launch ordinary goal sessions.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub goal: String,
    pub workspace: String,
    /// once YYYY-MM-DD HH:MM, every Nm/Nh/Nd, daily HH:MM,
    /// weekdays HH:MM, or weekly mon..sun HH:MM.
    pub timing: String,
    pub timezone: String,
    pub stance: String,
    pub max_seconds: u64,
    pub max_tokens: u64,
    pub max_iterations: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// Stable across retries of the same submission.
    pub id: String,
    pub spec: Spec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Launch {
    pub session: String,
    pub at: u64,
    pub status: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub spec: Spec,
    pub enabled: bool,
    pub next_at: Option<u64>,
    pub note: String,
    /// Reserved before creating a session; replay always uses this same id.
    pub pending: Option<String>,
    pub history: Vec<Launch>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Enable,
    Disable,
    RunNow,
    CancelRun,
}
