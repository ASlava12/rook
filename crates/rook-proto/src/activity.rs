//! Payload-free observations; a stalled observation never declares work failed.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Queued,
    Model,
    Tool,
    Approval,
    Question,
    Finished,
}

/// Advisory provider backoff, separate from the durable work scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderWait {
    pub until_ms: u64,
    pub next_attempt: u32,
    pub status: Option<u16>,
}

impl ProviderWait {
    pub fn describe(&self) -> String {
        let refusal =
            self.status.map_or("temporary provider failure".into(), |status| format!("HTTP {status}"));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        if self.until_ms > now_ms {
            format!(
                "provider retry in {}s after {refusal} (attempt {})",
                self.until_ms.saturating_sub(now_ms).div_ceil(1000),
                self.next_attempt
            )
        } else {
            format!(
                "provider retry overdue by {}s after {refusal} (attempt {})",
                now_ms.saturating_sub(self.until_ms) / 1000,
                self.next_attempt
            )
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub session: String,
    pub generation: String,
    pub stage: Stage,
    pub operation: Option<String>,
    /// Expected scheduler retry, not evidence of a physical model request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_wait: Option<ProviderWait>,
    pub last_progress_at: i64,
    pub observed_at: i64,
    pub idle_secs: u64,
    pub threshold_secs: u64,
    pub stalled: bool,
    pub notifications: u64,
    /// Only a live owner can answer this. Saved observations do not establish
    /// that the same turn is still running after a daemon restart.
    pub owner_live: Option<bool>,
}

impl Snapshot {
    pub fn describe(&self) -> String {
        let owner = match self.owner_live {
            Some(true) => "running",
            Some(false) => "owner stopped",
            None => "saved observation; owner unknown",
        };
        let stage = match self.stage {
            Stage::Queued => "queued",
            Stage::Model => "model",
            Stage::Tool => "tool",
            Stage::Approval => "waiting for approval",
            Stage::Question => "waiting for an answer",
            Stage::Finished => "finished",
        };
        let stage = if let Some(wait) = self.provider_wait {
            if self.owner_live == Some(true) { wait.describe() } else { format!("saved {}", wait.describe()) }
        } else if let Some(at) = self.retry_at.filter(|_| self.stage == Stage::Queued) {
            let observed = u64::try_from(self.observed_at).unwrap_or(0);
            if at > observed {
                format!("waiting to retry in {}s (at {at})", at - observed)
            } else {
                format!("retry overdue by {}s (at {at})", observed - at)
            }
        } else {
            stage.into()
        };
        format!(
            "{}: {owner}; {stage}{}; no observed progress for {}s (notice after {}s); last progress at {}{}",
            self.session,
            self.operation.as_ref().map(|name| format!(" · {name}")).unwrap_or_default(),
            self.idle_secs,
            self.threshold_secs,
            self.last_progress_at,
            if self.stalled { "; inactivity threshold reached" } else { "" },
        )
    }
}
