//! A fixed-size progress tracker independent of model and tool futures.
use rook_proto::{
    ChatEvent,
    activity::{Snapshot, Stage},
};
use rook_store::Store;
use tokio::time::Instant;

use crate::{CoreError, Result};

const RECORD_BYTES: usize = 2048;

pub struct Tracker {
    state: Snapshot,
    last_progress: Instant,
    active_stage: Stage,
    waiting: bool,
    retry_due: Option<Instant>,
}

impl Tracker {
    pub fn new(session: u128, threshold_secs: u64) -> Self {
        let now = rook_store::now_unix();
        Self {
            state: Snapshot {
                version: 1,
                session: rook_store::format_session_id(session),
                generation: rook_store::format_session_id(rook_store::new_session_id()),
                stage: Stage::Queued,
                operation: None,
                retry_at: None,
                last_progress_at: now,
                observed_at: now,
                idle_secs: 0,
                threshold_secs: if threshold_secs == 0 { 0 } else { threshold_secs.clamp(30, 86400) },
                stalled: false,
                notifications: 0,
                owner_live: Some(true),
            },
            last_progress: Instant::now(),
            active_stage: Stage::Queued,
            waiting: false,
            retry_due: None,
        }
    }

    fn progressed(&mut self, stage: Stage) {
        self.state.retry_at = None;
        self.retry_due = None;
        self.last_progress = Instant::now();
        self.state.last_progress_at = rook_store::now_unix();
        self.state.stalled = false;
        self.active_stage = stage;
        self.state.stage = stage;
        if stage != Stage::Tool {
            self.state.operation = None;
        }
    }

    /// Typed physical-request and pipe observations supply stages independently
    /// of when an attached view receives its display events.
    pub fn observe_phase(&mut self, stage: Stage) {
        self.progressed(stage);
    }

    /// Scheduling is status, not fresh progress. Repeated observation of the
    /// same deadline must not postpone overdue notices or reset idle time.
    pub fn retry_wait(&mut self, at: u64) {
        if self.state.retry_at == Some(at) {
            return;
        }
        let now = u64::try_from(rook_store::now_unix()).unwrap_or(0);
        self.retry_due = Instant::now().checked_add(std::time::Duration::from_secs(at.saturating_sub(now)));
        self.state.retry_at = Some(at);
        self.state.stage = Stage::Queued;
        self.active_stage = Stage::Queued;
        self.state.operation = None;
        self.state.stalled = false;
    }

    pub fn clear_retry(&mut self) -> bool {
        let changed = self.state.retry_at.take().is_some();
        self.retry_due = None;
        changed
    }

    /// Periodic waiting lines, elapsed-time tool updates and control receipts
    /// are status, not evidence that the work itself is progressing.
    pub fn observe(&mut self, event: &ChatEvent) {
        match event {
            ChatEvent::Started { .. }
            | ChatEvent::Turn { .. }
            | ChatEvent::FollowUp { .. }
            | ChatEvent::ModelRequest { .. }
            | ChatEvent::Step { .. } => self.progressed(Stage::Model),
            ChatEvent::Text { text } | ChatEvent::Reasoning { text } if !text.is_empty() => {
                self.progressed(Stage::Model);
            }
            ChatEvent::Tool { name, .. } => {
                self.progressed(Stage::Tool);
                self.state.operation = Some(
                    if name.len() <= 64
                        && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-:.".contains(&c))
                    {
                        name.clone()
                    } else {
                        "tool".into()
                    },
                );
            }
            ChatEvent::ToolDone { .. } => self.progressed(Stage::Model),
            ChatEvent::Agent { receipt: None, admission: None, extension_ui: None, text }
                if !text.is_empty() && self.state.retry_at.is_none() =>
            {
                self.progressed(Stage::Tool)
            }
            ChatEvent::Done { .. } | ChatEvent::Failed { .. } | ChatEvent::Cancelled => {
                self.progressed(Stage::Finished);
                self.state.owner_live = Some(false);
            }
            _ => {}
        }
    }

    /// Returns true exactly when a new inactivity episode needs a notice.
    /// An hour spent waiting for a person starts a fresh clock on their answer.
    pub fn check(&mut self, approval: bool, question: bool) -> bool {
        let waiting = approval || question;
        if self.waiting && !waiting {
            self.progressed(self.active_stage);
        }
        self.waiting = waiting;
        self.state.stage = if question {
            Stage::Question
        } else if approval {
            Stage::Approval
        } else {
            self.active_stage
        };
        self.state.observed_at = rook_store::now_unix();
        self.state.idle_secs = self.last_progress.elapsed().as_secs();
        if waiting {
            self.state.stalled = false;
            return false;
        }
        let unexpected_idle = self
            .retry_due
            .map_or(self.state.idle_secs, |due| Instant::now().saturating_duration_since(due).as_secs());
        if self.state.stage != Stage::Finished
            && self.state.threshold_secs > 0
            && unexpected_idle >= self.state.threshold_secs
            && !self.state.stalled
        {
            self.state.stalled = true;
            self.state.notifications = self.state.notifications.saturating_add(1);
            return true;
        }
        false
    }

    pub fn snapshot(&self, owner_live: bool) -> Snapshot {
        let mut snapshot = self.state.clone();
        snapshot.idle_secs = self.last_progress.elapsed().as_secs();
        snapshot.observed_at = rook_store::now_unix();
        snapshot.owner_live = Some(owner_live && snapshot.stage != Stage::Finished);
        snapshot
    }
}

fn key(session: u128) -> String {
    format!("activity/{session:032x}")
}

/// A single bounded companion is replaced, so alerts cannot grow another log.
pub fn save(store: &Store, session: u128, state: &Snapshot) -> Result<()> {
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() > RECORD_BYTES {
        return Err(CoreError::Other("activity observation exceeds 2048 bytes".into()));
    }
    // A cancelled watchdog may finish after the next owner has started. It
    // must not replace that owner's observation with its own stopped state.
    store.kv_update_session(session, &key(session), RECORD_BYTES, None, |old| {
        if let Some(old) = old {
            let previous: Snapshot = serde_json::from_slice(old)
                .map_err(|error| rook_store::StoreError::Encoding(error.to_string()))?;
            if previous.generation != state.generation {
                return Ok(old.to_vec());
            }
        }
        Ok(bytes)
    })?;
    Ok(())
}

/// Installs a new owner's generation before any of its watchers can write.
pub fn begin(store: &Store, session: u128, state: &Snapshot) -> Result<()> {
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() > RECORD_BYTES {
        return Err(CoreError::Other("activity observation exceeds 2048 bytes".into()));
    }
    store.kv_update_session_values(session, &[(&key(session), &bytes)])?;
    Ok(())
}

pub(crate) fn load(store: &Store, session: u128) -> Result<Option<Snapshot>> {
    let Some(bytes) = store.kv_get_limited(&key(session), RECORD_BYTES)? else { return Ok(None) };
    let mut state: Snapshot = serde_json::from_slice(&bytes)?;
    if state.version != 1
        || state.session != rook_store::format_session_id(session)
        || rook_store::parse_session_id(&state.generation).is_none()
        || state.operation.as_ref().is_some_and(|s| {
            s.len() > 64 || !s.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-:.".contains(&c))
        })
    {
        return Err(CoreError::Other("invalid activity observation".into()));
    }
    state.owner_live = None;
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn scheduled_retry_waits_preserve_idle_evidence_and_only_notice_unexpected_delay() {
        let mut tracker = Tracker::new(1, 30);
        tracker.observe_phase(Stage::Model);
        tokio::time::advance(Duration::from_secs(31)).await;
        let at = u64::try_from(rook_store::now_unix()).unwrap() + 1800;
        tracker.retry_wait(at);
        assert_eq!(tracker.snapshot(true).stage, Stage::Queued);
        assert_eq!(tracker.snapshot(true).retry_at, Some(at));
        assert_eq!(tracker.snapshot(true).idle_secs, 31, "a scheduler wait is not progress");
        assert!(tracker.snapshot(true).describe().contains("waiting to retry"));
        tracker.observe(&ChatEvent::Agent {
            receipt: None,
            admission: None,
            extension_ui: None,
            text: "Goal saved; retry in 1800s".into(),
        });
        assert_eq!(tracker.snapshot(true).retry_at, Some(at));
        assert_eq!(tracker.snapshot(true).idle_secs, 31, "the retry announcement is status only");
        assert!(!tracker.check(false, false));
        tokio::time::advance(Duration::from_secs(1800)).await;
        assert!(!tracker.check(false, false), "the configured wait is excluded from inactivity");
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(tracker.check(false, false), "an overdue scheduler still needs a notice");
        tracker.retry_wait(at);
        assert!(tracker.snapshot(true).stalled, "the same deadline cannot rearm inactivity");
        tracker.observe_phase(Stage::Model);
        assert!(tracker.snapshot(true).retry_at.is_none());
        assert_eq!(tracker.snapshot(true).idle_secs, 0);
        assert!(!tracker.check(false, false));
        let mut legacy = serde_json::to_value(tracker.snapshot(true)).unwrap();
        legacy.as_object_mut().unwrap().remove("retry_at");
        let legacy: Snapshot = serde_json::from_value(legacy).unwrap();
        assert!(legacy.retry_at.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_heartbeats_do_not_hide_a_stall_and_real_tokens_rearm_one_notice() {
        let mut tracker = Tracker::new(1, 30);
        tracker.observe(&ChatEvent::ModelRequest {
            model: "private-model".into(),
            requested_effort: "high".into(),
            effort: "high".into(),
        });
        tokio::time::advance(Duration::from_secs(31)).await;
        tracker.observe(&ChatEvent::ToolWorking { name: "model".into(), said: "still waiting".into() });
        assert!(tracker.snapshot(true).idle_secs > 30, "the inactivity limit was exceeded");
        assert!(tracker.check(false, false));
        assert!(!tracker.check(false, false), "one notice per inactivity episode");
        tracker.observe(&ChatEvent::Text { text: "new token".into() });
        assert!(!tracker.check(false, false));
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(tracker.check(false, false));
        assert_eq!(tracker.snapshot(true).notifications, 2);
        tracker.observe(&ChatEvent::Cancelled);
        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(!tracker.check(false, false), "a finished turn cannot stall");
    }

    #[tokio::test(start_paused = true)]
    async fn user_decisions_are_excluded_and_a_returning_answer_starts_a_new_clock() {
        let mut tracker = Tracker::new(1, 30);
        tracker.observe(&ChatEvent::Tool { name: "run_command".into(), doing: "PRIVATE_COMMAND".into() });
        assert!(!tracker.check(true, false));
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(!tracker.check(true, false));
        assert_eq!(tracker.snapshot(true).stage, Stage::Approval);
        assert!(!tracker.check(false, true));
        assert_eq!(tracker.snapshot(true).stage, Stage::Question);
        assert!(!tracker.check(false, false));
        assert_eq!(tracker.snapshot(true).idle_secs, 0);
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(tracker.check(false, false));
        let json = serde_json::to_string(&tracker.snapshot(true)).unwrap();
        assert!(!json.contains("PRIVATE_COMMAND"));
        assert!(json.contains("run_command"));
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_notices_still_observe_activity_and_tool_names_cannot_expand_state() {
        let mut tracker = Tracker::new(1, 0);
        let oversized = "x".repeat(2049);
        assert!(oversized.len() > RECORD_BYTES);
        tracker.observe(&ChatEvent::Tool { name: oversized, doing: "".into() });
        tokio::time::advance(Duration::from_secs(90000)).await;
        assert!(!tracker.check(false, false));
        let state = tracker.snapshot(true);
        assert_eq!(state.operation.as_deref(), Some("tool"));
        assert!(serde_json::to_vec(&state).unwrap().len() < RECORD_BYTES);
    }

    #[test]
    fn saved_observations_never_claim_a_live_owner_and_disappear_with_the_session() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.create_session(&rook_store::SessionMeta::new(1, "task", "workspace", 1)).unwrap();
        let tracker = Tracker::new(1, 30);
        begin(&store, 1, &tracker.snapshot(true)).unwrap();
        assert!(load(&store, 1).unwrap().unwrap().owner_live.is_none());
        let too_big = vec![b' '; RECORD_BYTES + 1];
        assert!(too_big.len() > RECORD_BYTES);
        store.kv_set(&key(1), &too_big).unwrap();
        assert!(load(&store, 1).is_err(), "reject before decoding an oversized companion");
        begin(&store, 1, &tracker.snapshot(true)).unwrap();
        store.delete_session(1).unwrap();
        assert!(load(&store, 1).unwrap().is_none());
        assert!(save(&store, 1, &tracker.snapshot(true)).is_err(), "watchdog cannot resurrect deleted state");
    }

    #[test]
    fn an_old_watchdogs_late_cleanup_cannot_replace_its_successor() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.create_session(&rook_store::SessionMeta::new(1, "task", "workspace", 1)).unwrap();
        let old = Tracker::new(1, 30);
        begin(&store, 1, &old.snapshot(true)).unwrap();
        let new = Tracker::new(1, 30);
        assert_ne!(old.snapshot(true).generation, new.snapshot(true).generation);
        begin(&store, 1, &new.snapshot(true)).unwrap();
        save(&store, 1, &old.snapshot(false)).unwrap();
        assert_eq!(load(&store, 1).unwrap().unwrap().generation, new.snapshot(true).generation);
        save(&store, 1, &new.snapshot(false)).unwrap();
        assert_eq!(load(&store, 1).unwrap().unwrap().generation, new.snapshot(true).generation);
    }
}
