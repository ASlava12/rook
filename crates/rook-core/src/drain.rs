//! Process-local admission fence and bounded restart handoff evidence.
use crate::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Start {
        id: String,
        timeout_secs: u64,
        #[serde(default)]
        restart: bool,
    },
    Cancel {
        id: String,
    },
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    pub turns: u32,
    pub managed: usize,
    pub jobs: usize,
    pub writers: usize,
    pub requests: usize,
    pub unknown: usize,
}
impl Counts {
    fn quiet(self) -> bool {
        self.turns == 0
            && self.managed == 0
            && self.jobs == 0
            && self.writers == 0
            && self.requests == 0
            && self.unknown == 0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct View {
    pub id: Option<String>,
    pub phase: String,
    pub restart: bool,
    pub elapsed_secs: u64,
    pub timeout_secs: u64,
    pub counts: Counts,
    pub reason: Option<String>,
    pub handoff: Option<RestartReceipt>,
}
struct State {
    id: Option<String>,
    phase: &'static str,
    restart: bool,
    started: std::time::Instant,
    timeout_secs: u64,
    allowed: BTreeSet<String>,
    counts: Counts,
    reason: Option<String>,
    requests: usize,
}
impl Default for State {
    fn default() -> Self {
        Self {
            id: None,
            phase: "idle",
            restart: false,
            started: std::time::Instant::now(),
            timeout_secs: 0,
            allowed: BTreeSet::new(),
            counts: Counts::default(),
            reason: None,
            requests: 0,
        }
    }
}
#[derive(Default)]
pub struct Control(Mutex<State>);
pub struct RequestPermit(Arc<Control>);
impl Drop for RequestPermit {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        state.requests = state.requests.saturating_sub(1);
    }
}
fn closed(state: &State) -> bool {
    !matches!(state.phase, "idle" | "cancelled")
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl Control {
    pub fn active(&self) -> bool {
        closed(&self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub fn allows_run(&self, id: &str) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        !closed(&state) || state.allowed.contains(id)
    }
    pub fn request(self: &Arc<Self>) -> Result<RequestPermit> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if closed(&state) {
            return Err(CoreError::Other(
                "daemon is draining; inspect its status or cancel the drain".into(),
            ));
        }
        state.requests += 1;
        Ok(RequestPermit(self.clone()))
    }
    /// Keep replies/stops reachable, but serialize them with restart handoff.
    pub fn interaction(self: &Arc<Self>) -> Result<RequestPermit> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase == "restarting" {
            return Err(CoreError::Other(
                "restart handoff has begun; reconnect to the replacement daemon".into(),
            ));
        }
        state.requests += 1;
        if state.phase == "ready" {
            state.phase = "draining";
        }
        Ok(RequestPermit(self.clone()))
    }

    pub fn change(&self, action: Action, allowed: BTreeSet<String>) -> Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match action {
            Action::Start { id, timeout_secs, restart } => {
                if !valid_id(&id) || !(1..=86400).contains(&timeout_secs) || allowed.len() > 512 {
                    return Err(CoreError::Other(
                        "drain needs a 1..64 byte request ID, timeout 1..86400 and at most 512 admitted runs"
                            .into(),
                    ));
                }
                if closed(&state) {
                    if state.id.as_ref() == Some(&id)
                        && state.timeout_secs == timeout_secs
                        && state.restart == restart
                    {
                        return Ok(());
                    }
                    return Err(CoreError::Other(
                        "another drain owns admission; inspect or cancel its exact ID".into(),
                    ));
                }
                if state.id.as_ref() == Some(&id) {
                    return Err(CoreError::Other("this drain ID has already ended; use a new ID".into()));
                }
                let requests = state.requests;
                *state = State {
                    id: Some(id),
                    phase: "draining",
                    restart,
                    timeout_secs,
                    allowed,
                    requests,
                    ..Default::default()
                };
            }
            Action::Cancel { id } => {
                if state.id.as_ref() != Some(&id) || state.phase == "restarting" {
                    return Err(CoreError::Other(
                        "drain identity changed or restart handoff already began".into(),
                    ));
                }
                state.phase = "cancelled";
                state.allowed.clear();
            }
        }
        Ok(())
    }
    pub fn observe(&self, mut counts: Counts) -> View {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        counts.requests = state.requests;
        state.counts = counts;
        if matches!(state.phase, "draining" | "ready") {
            if state.started.elapsed().as_secs() >= state.timeout_secs {
                state.phase = "timed-out";
                state.reason = Some(
                    "drain deadline expired; nothing was forced. Cancel the drain or explicitly stop work."
                        .into(),
                );
            } else {
                state.phase = if counts.quiet() { "ready" } else { "draining" };
            }
        }
        View {
            id: state.id.clone(),
            phase: state.phase.into(),
            restart: state.restart,
            elapsed_secs: if state.id.is_some() { state.started.elapsed().as_secs() } else { 0 },
            timeout_secs: state.timeout_secs,
            counts,
            reason: state.reason.clone(),
            handoff: None,
        }
    }
    pub fn handoff(&self, id: &str) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.id.as_deref() != Some(id) || state.phase != "ready" || !state.restart || state.requests != 0
        {
            return false;
        }
        state.phase = "restarting";
        true
    }
    pub fn failed(&self, reason: String) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = "failed";
        state.reason = Some(reason.chars().take(512).collect());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestartReceipt {
    pub version: u32,
    pub id: String,
    pub phase: String,
    pub reason: Option<String>,
}
impl RestartReceipt {
    pub fn save(&self) -> Result<()> {
        if self.version != 1
            || !valid_id(&self.id)
            || !matches!(self.phase.as_str(), "restarting" | "restarted" | "failed")
        {
            return Err(CoreError::Other("invalid restart receipt".into()));
        }
        let directory = crate::paths::home().join("run");
        std::fs::create_dir_all(&directory)
            .map_err(|source| CoreError::Io { path: directory.clone(), source })?;
        let bytes = crate::persistence::encode_with_limit(self, 4096)?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)
            .map_err(|source| CoreError::Io { path: directory.clone(), source })?;
        use std::io::Write;
        file.write_all(&bytes)
            .and_then(|_| file.as_file().sync_all())
            .map_err(|source| CoreError::Io { path: directory.clone(), source })?;
        file.persist(directory.join("drain-restart.json"))
            .map_err(|error| CoreError::Other(error.to_string()))?;
        Ok(())
    }
    pub fn read() -> Result<Option<Self>> {
        let directory = crate::paths::home().join("run");
        let text = match rook_contain::files::read_text(
            &directory,
            std::path::Path::new("drain-restart.json"),
            4096,
        ) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(CoreError::Io { path: directory.join("drain-restart.json"), source }),
        };
        let receipt: Self = serde_json::from_str(&text)?;
        if receipt.version != 1
            || !valid_id(&receipt.id)
            || !matches!(receipt.phase.as_str(), "restarting" | "restarted" | "failed")
        {
            return Err(CoreError::Other("unsupported restart receipt".into()));
        }
        Ok(Some(receipt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draining_counts_existing_mutations_and_retries_do_not_rearm_the_fence() {
        let control = Arc::new(Control::default());
        let request = control.request().unwrap();
        let action = Action::Start { id: "first".into(), timeout_secs: 300, restart: true };
        control.change(action.clone(), BTreeSet::from(["old-run".into()])).unwrap();
        assert!(control.request().is_err());
        assert!(control.allows_run("old-run"));
        assert!(!control.allows_run("new-run"));
        assert_eq!(control.observe(Counts::default()).phase, "draining");
        control.change(action, BTreeSet::new()).unwrap();
        assert!(control.allows_run("old-run"));
        drop(request);
        assert_eq!(control.observe(Counts::default()).phase, "ready");
        assert!(control.handoff("first"));
        assert!(!control.handoff("first"));
        assert!(control.change(Action::Cancel { id: "first".into() }, BTreeSet::new()).is_err());
    }
    #[test]
    fn timeout_never_forces_or_reopens_admission_and_old_cancel_cannot_cancel_a_new_plan() {
        let control = Arc::new(Control::default());
        control
            .change(Action::Start { id: "first".into(), timeout_secs: 1, restart: true }, BTreeSet::new())
            .unwrap();
        control.0.lock().unwrap().started -= std::time::Duration::from_secs(2);
        assert_eq!(control.observe(Counts::default()).phase, "timed-out");
        assert!(control.request().is_err());
        assert!(!control.handoff("first"));
        control.change(Action::Cancel { id: "first".into() }, BTreeSet::new()).unwrap();
        control
            .change(Action::Start { id: "second".into(), timeout_secs: 300, restart: false }, BTreeSet::new())
            .unwrap();
        assert!(control.change(Action::Cancel { id: "first".into() }, BTreeSet::new()).is_err());
    }
}
