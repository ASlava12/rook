//! Scoped runtime observations. They do not replace the durable execution journal.
use super::{Progress, TurnOutcome};
use crate::{CoreError, Result};
use std::sync::Arc;

pub trait Observer: Send + Sync {
    fn observe(&self, event: Event<'_>);
}

pub enum Event<'a> {
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
