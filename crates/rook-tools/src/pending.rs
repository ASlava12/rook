//! Correlating a question sent to a front end with the answer that comes back.
//!
//! Approvals and questions both need exactly this, and behaviour that differed
//! between them depending on which front end was attached would be a bug rather
//! than a feature.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use std::sync::Mutex;

use tokio::sync::{mpsc::UnboundedSender, oneshot};

#[derive(Debug)]
pub enum Unanswered {
    /// Nothing is attached to the other end of the channel.
    NoListener,
    /// The front end took the request and then went away.
    Dropped,
    Silence(Duration),
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoListener => write!(f, "nothing is listening"),
            Self::Dropped => write!(f, "the request was dropped"),
            Self::Silence(d) => write!(f, "no answer within {}s", d.as_secs()),
        }
    }
}

pub struct Pending<Q, A> {
    requests: UnboundedSender<Q>,
    /// A plain mutex, never held across an await: the answer arrives on a key
    /// press in the TUI, which is not async, and `try_lock` there dropped the
    /// answer on the floor whenever a second question was in flight — the person
    /// had answered and then waited out the whole timeout for nothing.
    waiting: Mutex<HashMap<String, oneshot::Sender<A>>>,
    next_id: AtomicU64,
    patience: Duration,
}

impl<Q, A> Pending<Q, A> {
    /// `patience` bounds the wait: a closed tab or an abandoned terminal would
    /// otherwise leave the turn pending forever, holding its locks with it.
    pub fn new(requests: UnboundedSender<Q>, patience: Duration) -> Self {
        Self { requests, waiting: Default::default(), next_id: AtomicU64::new(1), patience }
    }

    pub fn is_waiting(&self) -> bool {
        self.hold().values().any(|sender| !sender.is_closed())
    }

    /// `build` is handed the id the answer must come back under.
    pub async fn ask(&self, build: impl FnOnce(String) -> Q) -> Result<A, Unanswered> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (tx, rx) = oneshot::channel();
        self.hold().insert(id.clone(), tx);
        // Cancellation drops the future at its await, bypassing all ordinary
        // return paths. Keep removal tied to the future's lifetime instead.
        let _waiting = Waiting { pending: self, id: &id };

        if self.requests.send(build(id.clone())).is_err() {
            return Err(Unanswered::NoListener);
        }
        match tokio::time::timeout(self.patience, rx).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(_)) => Err(Unanswered::Dropped),
            Err(_) => Err(Unanswered::Silence(self.patience)),
        }
    }

    /// Ignored when the id is unknown, which is what a late or duplicate answer
    /// looks like after the wait has already given up.
    pub fn answer(&self, id: &str, answer: A) {
        if let Some(tx) = self.hold().remove(id) {
            let _ = tx.send(answer);
        }
    }

    /// A poisoned map is one a panicking asker left mid-insert; the entries are
    /// still sound, and refusing to answer anything afterwards is worse.
    fn hold(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<A>>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct Waiting<'a, Q, A> {
    pending: &'a Pending<Q, A>,
    id: &'a str,
}

impl<Q, A> Drop for Waiting<'_, Q, A> {
    fn drop(&mut self) {
        self.pending.hold().remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_a_question_removes_its_pending_entry() {
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let pending = Pending::<String, bool>::new(send, Duration::from_secs(60));
        for _ in 0..100 {
            let mut asking = Box::pin(pending.ask(|id| id));
            assert!(
                std::future::poll_fn(|cx| { std::task::Poll::Ready(asking.as_mut().poll(cx).is_pending()) })
                    .await
            );
            let id = requests.recv().await.unwrap();
            assert_eq!(pending.hold().len(), 1);
            assert!(pending.is_waiting());
            drop(asking);
            assert!(pending.hold().is_empty(), "cancelled request {id} left an entry");
            assert!(!pending.is_waiting());
            pending.answer(&id, true);
        }
    }

    #[tokio::test]
    async fn an_answer_removes_the_entry_before_resuming_its_question() {
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let pending = Pending::<String, bool>::new(send, Duration::from_secs(60));
        let mut asking = Box::pin(pending.ask(|id| id));
        assert!(
            std::future::poll_fn(|cx| { std::task::Poll::Ready(asking.as_mut().poll(cx).is_pending()) })
                .await
        );
        let id = requests.recv().await.unwrap();
        pending.answer(&id, true);
        assert!(pending.hold().is_empty());
        assert!(asking.await.unwrap());
        assert!(pending.hold().is_empty());
    }
}
