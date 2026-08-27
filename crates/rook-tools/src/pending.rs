//! Correlating a question sent to a front end with the answer that comes back.
//!
//! Approvals and questions both need exactly this, and behaviour that differed
//! between them depending on which front end was attached would be a bug rather
//! than a feature.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, mpsc::UnboundedSender, oneshot};

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

    /// `build` is handed the id the answer must come back under.
    pub async fn ask(&self, build: impl FnOnce(String) -> Q) -> Result<A, Unanswered> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id.clone(), tx);

        if self.requests.send(build(id.clone())).is_err() {
            self.waiting.lock().await.remove(&id);
            return Err(Unanswered::NoListener);
        }
        match tokio::time::timeout(self.patience, rx).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(_)) => Err(Unanswered::Dropped),
            Err(_) => {
                self.waiting.lock().await.remove(&id);
                Err(Unanswered::Silence(self.patience))
            }
        }
    }

    /// Ignored when the id is unknown, which is what a late or duplicate answer
    /// looks like after the wait has already given up.
    pub fn answer(&self, id: &str, answer: A) {
        if let Ok(mut waiting) = self.waiting.try_lock()
            && let Some(tx) = waiting.remove(id)
        {
            let _ = tx.send(answer);
        }
    }
}
