//! Scoped runtime observations. They do not replace the durable execution journal.
use super::{Progress, TurnOutcome};
use crate::{CoreError, Result};
use std::sync::Arc;

pub(super) struct RetryStatus(pub tokio::sync::watch::Sender<Option<rook_proto::activity::ProviderWait>>);
impl Drop for RetryStatus {
    fn drop(&mut self) {
        self.0.send_if_modified(|wait| wait.take().is_some());
    }
}

pub trait Observer: Send + Sync {
    fn observe(&self, event: Event<'_>);
}

pub enum Event<'a> {
    ModelQueued {
        session: u128,
    },
    ModelStarted {
        session: u128,
    },
    ModelRetry {
        session: u128,
        wait: rook_proto::activity::ProviderWait,
        delay: std::time::Duration,
    },
    /// Fresh pipe data, sampled at a bounded rate; never elapsed-time prose.
    ToolOutput {
        session: u128,
    },
    ChildStarted {
        parent: u128,
        child: u128,
        task: &'a str,
    },
    ChildEnded {
        child: u128,
        result: Option<std::result::Result<&'a str, &'a CoreError>>,
    },
    Progress {
        session: u128,
        workspace: &'a std::path::Path,
        progress: &'a Progress<'a>,
    },
    CompactionStarted {
        session: u128,
        id: u128,
    },
    CompactionEnded {
        session: u128,
        id: u128,
        result: Option<std::result::Result<(u64, &'a str), &'a CoreError>>,
    },
}

pub(super) fn model(
    inner: Arc<dyn rook_llm::AttemptObserver>,
    observer: Option<Arc<dyn Observer>>,
    session: u128,
    retry_status: tokio::sync::watch::Sender<Option<rook_proto::activity::ProviderWait>>,
) -> Arc<dyn rook_llm::AttemptObserver> {
    Arc::new(Model { inner, observer, session, retry_status })
}

struct Model {
    inner: Arc<dyn rook_llm::AttemptObserver>,
    observer: Option<Arc<dyn Observer>>,
    session: u128,
    retry_status: tokio::sync::watch::Sender<Option<rook_proto::activity::ProviderWait>>,
}

impl rook_llm::AttemptObserver for Model {
    fn waiting_for_capacity(&self) {
        self.inner.waiting_for_capacity();
        self.retry_status.send_if_modified(|wait| wait.take().is_some());
        if let Some(observer) = &self.observer {
            observer.observe(Event::ModelQueued { session: self.session });
        }
    }

    fn waiting_to_retry(&self, retry: rook_llm::RetryWait) {
        self.inner.waiting_to_retry(retry);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let until_ms = u64::try_from(now.saturating_add(retry.delay).as_millis()).unwrap_or(u64::MAX);
        let wait = rook_proto::activity::ProviderWait {
            until_ms,
            next_attempt: retry.next_attempt,
            status: retry.status,
        };
        self.retry_status.send_replace(Some(wait));
        if let Some(observer) = &self.observer {
            observer.observe(Event::ModelRetry { session: self.session, wait, delay: retry.delay });
        }
    }

    fn start(&self, dispatch: Option<&rook_llm::Dispatch>) -> rook_llm::Result<Box<dyn rook_llm::Attempt>> {
        let attempt = self.inner.start(dispatch)?;
        self.retry_status.send_if_modified(|wait| wait.take().is_some());
        if let Some(observer) = &self.observer {
            observer.observe(Event::ModelStarted { session: self.session });
        }
        Ok(attempt)
    }
}

pub(super) struct Child {
    observer: Option<Arc<dyn Observer>>,
    session: u128,
}
impl Child {
    pub fn start(observer: Option<Arc<dyn Observer>>, parent: u128, session: u128, task: &str) -> Self {
        if let Some(observer) = &observer {
            observer.observe(Event::ChildStarted { parent, child: session, task });
        }
        Self { observer, session }
    }
    pub fn finish(&mut self, result: &Result<(String, TurnOutcome)>) {
        if let Some(observer) = self.observer.take() {
            observer.observe(Event::ChildEnded {
                child: self.session,
                result: Some(result.as_ref().map(|(_, outcome)| outcome.stopped.as_str())),
            });
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if let Some(observer) = self.observer.take() {
            // Dropping an in-flight future loses observation; it does not prove
            // that an external command finished or cancelled.
            observer.observe(Event::ChildEnded { child: self.session, result: None });
        }
    }
}

pub(super) struct Compaction {
    observer: Option<Arc<dyn Observer>>,
    session: u128,
    pub id: u128,
}
impl Compaction {
    pub fn start(observer: Option<Arc<dyn Observer>>, session: u128) -> Self {
        let id = rook_store::new_session_id();
        if let Some(observer) = &observer {
            observer.observe(Event::CompactionStarted { session, id });
        }
        Self { observer, session, id }
    }
    pub fn finish(&mut self, result: std::result::Result<(u64, &str), &CoreError>) {
        if let Some(observer) = self.observer.take() {
            observer.observe(Event::CompactionEnded {
                session: self.session,
                id: self.id,
                result: Some(result),
            });
        }
    }
}
impl Drop for Compaction {
    fn drop(&mut self) {
        if let Some(observer) = self.observer.take() {
            observer.observe(Event::CompactionEnded { session: self.session, id: self.id, result: None });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Receipts {
        waits: Mutex<Vec<rook_llm::RetryWait>>,
    }
    struct Attempt;
    impl rook_llm::Attempt for Attempt {
        fn finish(&mut self, _: rook_llm::AttemptStatus, _: &rook_llm::AttemptFacts) -> rook_llm::Result<()> {
            Ok(())
        }
    }
    impl rook_llm::AttemptObserver for Receipts {
        fn waiting_to_retry(&self, wait: rook_llm::RetryWait) {
            self.waits.lock().unwrap().push(wait);
        }
        fn start(&self, _: Option<&rook_llm::Dispatch>) -> rook_llm::Result<Box<dyn rook_llm::Attempt>> {
            Ok(Box::new(Attempt))
        }
    }
    #[derive(Default)]
    struct Watch {
        sessions: Mutex<Vec<u128>>,
    }
    impl Observer for Watch {
        fn observe(&self, event: Event<'_>) {
            if let Event::ModelRetry { session, .. } = event {
                self.sessions.lock().unwrap().push(session);
            }
        }
    }

    #[test]
    fn provider_status_reaches_local_views_and_the_scoped_observer_without_extra_admission() {
        let receipts = Arc::new(Receipts::default());
        let events = Arc::new(Watch::default());
        let (root, mut view) = tokio::sync::watch::channel(None);
        let (child, child_view) = tokio::sync::watch::channel(None);
        let main = model(receipts.clone(), Some(events.clone()), 1, root.clone());
        let delegated = model(receipts.clone(), Some(events.clone()), 2, child);
        main.start(None).unwrap();
        assert!(!view.has_changed().unwrap(), "an ordinary admission must not invent a retry status");
        let retry = rook_llm::RetryWait {
            delay: std::time::Duration::from_secs(45),
            next_attempt: 2,
            status: Some(429),
        };
        main.waiting_to_retry(retry);
        assert_eq!(view.borrow_and_update().unwrap().status, Some(429));
        assert!(child_view.borrow().is_none());
        delegated.waiting_to_retry(retry);
        assert_eq!(*events.sessions.lock().unwrap(), vec![1, 2]);
        assert_eq!(
            receipts.waits.lock().unwrap().len(),
            2,
            "wrappers forward the notice to the inner observer"
        );
        main.start(None).unwrap();
        assert!(view.borrow().is_none());
        assert!(child_view.borrow().is_some(), "a parent admission cannot clear a child wait");
        let local = model(receipts, None, 1, root.clone());
        local.waiting_to_retry(retry);
        assert!(
            view.borrow().is_some(),
            "local frontends also receive provider status without a runtime observer"
        );
        drop(RetryStatus(root));
        assert!(view.borrow().is_none(), "dropping the owner clears advisory status");
    }
}
