//! Correlating a question sent to a front end with the answer that comes back.
//!
//! Approvals and questions both need exactly this, and behaviour that differed
//! between them depending on which front end was attached would be a bug rather
//! than a feature.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    Full(&'static str),
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoListener => write!(f, "nothing is listening"),
            Self::Dropped => write!(f, "the request was dropped"),
            Self::Silence(d) => write!(f, "no answer within {}s", d.as_secs()),
            Self::Full(limit) => write!(
                f,
                "the pending-input limit {limit} was reached; finish existing requests or raise that limit"
            ),
        }
    }
}

/// Per question/approval channel; shared by standalone and daemon front ends.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_requests: usize,
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_requests: 128, max_bytes: 1024 * 1024 }
    }
}

struct Entry<Q, A> {
    request: Q,
    answer: oneshot::Sender<A>,
    bytes: usize,
}

pub struct Pending<Q, A> {
    requests: UnboundedSender<Q>,
    /// A plain mutex, never held across an await: the answer arrives on a key
    /// press in the TUI, which is not async, and `try_lock` there dropped the
    /// answer on the floor whenever a second question was in flight — the person
    /// had answered and then waited out the whole timeout for nothing.
    waiting: Mutex<BTreeMap<String, Entry<Q, A>>>,
    next_id: AtomicU64,
    patience: Duration,
    limits: Limits,
    changed: tokio::sync::watch::Sender<u64>,
}

impl<Q, A> Pending<Q, A> {
    /// `patience` bounds the wait: a closed tab or an abandoned terminal would
    /// otherwise leave the turn pending forever, holding its locks with it.
    pub fn new(requests: UnboundedSender<Q>, patience: Duration, limits: Limits) -> Self {
        let limits = Limits {
            max_requests: limits.max_requests.clamp(1, 4096),
            max_bytes: limits.max_bytes.clamp(4096, 32 * 1024 * 1024),
        };
        let (changed, _) = tokio::sync::watch::channel(0);
        Self { requests, waiting: Default::default(), next_id: AtomicU64::new(1), patience, limits, changed }
    }

    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn changed(&self) {
        self.changed.send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub fn is_waiting(&self) -> bool {
        self.hold().values().any(|entry| !entry.answer.is_closed())
    }

    /// Only requests whose answers can still be delivered, never old replay.
    pub fn current(&self) -> Vec<Q>
    where
        Q: Clone,
    {
        self.hold()
            .values()
            .filter(|entry| !entry.answer.is_closed())
            .map(|entry| entry.request.clone())
            .collect()
    }

    /// `build` is handed the id the answer must come back under.
    pub async fn ask(&self, build: impl FnOnce(String) -> Q) -> Result<A, Unanswered>
    where
        Q: Clone + Serialize,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (tx, rx) = oneshot::channel();
        let request = build(id.clone());
        let mut counted = Count { bytes: 0, limit: self.limits.max_bytes };
        serde_json::to_writer(&mut counted, &request)
            .map_err(|_| Unanswered::Full("user_input.max_bytes"))?;
        {
            let mut waiting = self.hold();
            if waiting.len() >= self.limits.max_requests {
                return Err(Unanswered::Full("user_input.max_requests"));
            }
            let retained: usize = waiting.values().map(|entry| entry.bytes).sum();
            if retained > self.limits.max_bytes - counted.bytes {
                return Err(Unanswered::Full("user_input.max_bytes"));
            }
            // Admission precedes the extra copy retained for reconnecting views.
            waiting.insert(id.clone(), Entry { request: request.clone(), answer: tx, bytes: counted.bytes });
        }
        // Cancellation drops the future at its await, bypassing all ordinary
        // return paths. Keep removal tied to the future's lifetime instead.
        let _waiting = Waiting { pending: self, id: &id };
        self.changed();

        if self.requests.send(request).is_err() {
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
        if let Some(entry) = self.hold().remove(id) {
            self.changed();
            let _ = entry.answer.send(answer);
        }
    }

    /// A poisoned map is one a panicking asker left mid-insert; the entries are
    /// still sound, and refusing to answer anything afterwards is worse.
    fn hold(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry<Q, A>>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct Count {
    bytes: usize,
    limit: usize,
}

impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit - self.bytes {
            return Err(std::io::Error::other("pending request exceeds the byte limit"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Waiting<'a, Q, A> {
    pending: &'a Pending<Q, A>,
    id: &'a str,
}

impl<Q, A> Drop for Waiting<'_, Q, A> {
    fn drop(&mut self) {
        if self.pending.hold().remove(self.id).is_some() {
            self.pending.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_a_question_removes_its_pending_entry() {
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let pending = Pending::<String, bool>::new(send, Duration::from_secs(60), Limits::default());
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
        let pending = Pending::<String, bool>::new(send, Duration::from_secs(60), Limits::default());
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

    async fn start<A>(
        future: &mut std::pin::Pin<Box<impl std::future::Future<Output = Result<A, Unanswered>>>>,
    ) {
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx).is_pending())).await
        );
    }

    #[tokio::test]
    async fn current_inputs_exclude_answered_cancelled_and_unadmitted_requests() {
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let pending = Pending::<String, bool>::new(
            send,
            Duration::from_secs(60),
            Limits { max_requests: 2, max_bytes: 4096 },
        );
        let mut first = Box::pin(pending.ask(|id| id));
        let mut second = Box::pin(pending.ask(|id| id));
        start::<bool>(&mut first).await;
        start::<bool>(&mut second).await;
        let first_id = requests.recv().await.unwrap();
        let second_id = requests.recv().await.unwrap();
        assert_eq!(pending.current(), [first_id.clone(), second_id.clone()]);
        assert_eq!(pending.hold().len(), pending.limits.max_requests, "the request bound is reached");
        assert!(matches!(pending.ask(|id| id).await, Err(Unanswered::Full("user_input.max_requests"))));
        assert!(requests.try_recv().is_err(), "a rejected request never reaches a front end");
        pending.answer(&first_id, true);
        assert_eq!(pending.current(), [second_id]);
        assert!(first.await.unwrap());
        drop(second);
        assert!(pending.current().is_empty());
    }

    #[tokio::test]
    async fn retained_input_bytes_include_json_escaping_and_are_released_on_cancel() {
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let pending = Pending::<String, bool>::new(
            send,
            Duration::from_secs(60),
            Limits { max_requests: 128, max_bytes: 4096 },
        );
        let value = "\n".repeat(1100);
        let size = serde_json::to_vec(&value).unwrap().len();
        assert!(size < 4096 && size * 2 > 4096, "two retained requests exceed the byte limit");
        let mut first = Box::pin(pending.ask(|_| value.clone()));
        start::<bool>(&mut first).await;
        requests.recv().await.unwrap();
        assert!(matches!(
            pending.ask(|_| value.clone()).await,
            Err(Unanswered::Full("user_input.max_bytes"))
        ));
        assert_eq!(pending.current().as_slice(), std::slice::from_ref(&value));
        drop(first);
        let mut next = Box::pin(pending.ask(|_| value));
        start::<bool>(&mut next).await;
        requests.recv().await.unwrap();
        drop(next);
        assert!(pending.current().is_empty());
    }
}
