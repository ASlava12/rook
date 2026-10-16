//! Private values go straight to a bounded sink; only status crosses chat.
use crate::pending::{Limits, Pending, Unanswered};
use rook_proto::{SecretPrompt, SecretStatus};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc};

pub trait SecretSink: Send + Sync {
    /// Implementations must not return or log the value, including on failure.
    fn save(&self, value: &str) -> SecretStatus;
    /// Invalidate a request that has ended; an already admitted save may finish.
    fn close(&self);
}

pub struct PrivateInput {
    pending: Pending<SecretPrompt, SecretStatus>,
    sinks: Mutex<BTreeMap<String, Arc<dyn SecretSink>>>,
    capacity: Semaphore,
}

impl PrivateInput {
    pub fn new(sender: mpsc::UnboundedSender<SecretPrompt>, patience: Duration, limits: Limits) -> Self {
        let slots = limits.max_requests.clamp(1, 32);
        Self {
            pending: Pending::new(sender, patience, Limits { max_requests: slots, ..limits }),
            sinks: Mutex::new(BTreeMap::new()),
            capacity: Semaphore::new(slots),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.count()
    }

    pub fn current(&self) -> Vec<SecretPrompt> {
        self.pending.current()
    }

    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.pending.changes()
    }

    pub fn is_waiting(&self) -> bool {
        self.pending.is_waiting()
    }

    pub fn accepts(&self, id: &str, session: &str) -> bool {
        self.pending.accepts(id, |prompt| prompt.session == session)
    }

    pub async fn request(&self, prompt: SecretPrompt, sink: Arc<dyn SecretSink>) -> SecretStatus {
        if prompt.name.is_empty()
            || prompt.name.len() > 64
            || prompt.reason.is_empty()
            || prompt.reason.len() > 1024
            || prompt.session.len() > 64
        {
            sink.close();
            return SecretStatus::Failed;
        }
        let Ok(_permit) = self.capacity.try_acquire() else {
            sink.close();
            return SecretStatus::Busy;
        };
        let mut registration = Registration { owner: self, id: None, sink: sink.clone() };
        let answer = self
            .pending
            .ask(|id| {
                self.sinks.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), sink);
                registration.id = Some(id.clone());
                SecretPrompt { id, ..prompt }
            })
            .await;
        match answer {
            Ok(status) => status,
            Err(Unanswered::Silence(_)) => SecretStatus::Expired,
            Err(Unanswered::Dropped) => SecretStatus::Cancelled,
            Err(Unanswered::Full(_)) => SecretStatus::Busy,
            Err(Unanswered::NoListener) => SecretStatus::Unavailable,
        }
    }

    /// session and id are both required. The caller is the user's private
    /// transport, never a model-supplied answer or normal chat input.
    pub fn respond(
        &self,
        id: &str,
        session: &str,
        status: SecretStatus,
        value: Option<&str>,
    ) -> Option<SecretStatus> {
        if status == SecretStatus::Saved && value.is_none_or(|value| value.is_empty() || value.len() > 8192) {
            return None;
        }
        if status != SecretStatus::Saved
            && (value.is_some()
                || !matches!(
                    status,
                    SecretStatus::Declined | SecretStatus::Skipped | SecretStatus::Cancelled
                ))
        {
            return None;
        }
        let mut actual = None;
        let accepted = self.pending.answer_with(
            id,
            |prompt| prompt.session == session,
            |prompt| {
                if status != SecretStatus::Saved {
                    actual = Some(status);
                    return status;
                }
                let sink = self.sinks.lock().unwrap_or_else(|e| e.into_inner()).get(&prompt.id).cloned();
                let status = match (sink, value) {
                    (Some(sink), Some(value)) => sink.save(value),
                    _ => SecretStatus::Unavailable,
                };
                actual = Some(status);
                status
            },
        );
        if accepted { actual } else { None }
    }
}

struct Registration<'a> {
    owner: &'a PrivateInput,
    id: Option<String>,
    sink: Arc<dyn SecretSink>,
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            self.owner.sinks.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        }
        self.sink.close();
    }
}
