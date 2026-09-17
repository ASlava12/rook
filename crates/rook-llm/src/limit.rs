//! How many requests one endpoint is asked for at a time.
//!
//! A hosted API serves whatever it is sent and bills for it. A machine in the
//! next room does not: llama.cpp and LM Studio hold one model in memory and
//! serve a second request by interleaving it with the first, so two sub-agents
//! against one local server finish later than the same two run one after the
//! other — and a third is a server that has stopped answering. There is no
//! status for that. The request simply takes minutes, which reads as a hung
//! turn, and `stream_idle_timeout` eventually ends it as one.
//!
//! So a source says how many at once it is worth, and the default is one. It is
//! a property of the server rather than of the agent: `max_parallel_subagents`
//! bounds the errands one turn starts, and says nothing about the turn itself,
//! the compaction running beside it, or another window on the same machine
//! pointed at the same endpoint.
//!
//! Of the server, which is why the queue is named separately from the endpoint
//! waiting in it. One llama.cpp serving a small model and a large one is two
//! endpoints and one queue: what interleaves is requests at that process, not
//! requests for a model.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures_util::Stream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::stream::Delta;
use crate::{ModelInfo, Provider, Request, Response, ResponseStream, Result};

/// The permits for one server, shared by everything that talks to it.
///
/// Held for the life of the process and keyed by the queue's name, because a
/// provider is not: the loop builds a fresh one every turn, and each sub-agent
/// and each compaction builds its own. A count that lived on the provider would
/// be a new count per turn, which is no limit at all — the thing being counted
/// is requests in flight against one server, and that outlives all of them.
///
/// The first count wins if a later build asks for a different one. Changing it
/// takes a restart, which is worth saying because the alternative — resizing a
/// semaphore somebody is already waiting on — is a way to let more through than
/// either number allowed.
fn held_permits() -> &'static Mutex<HashMap<String, Arc<Semaphore>>> {
    static HELD: OnceLock<Mutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

fn permits(queue: &str, parallel: usize) -> Arc<Semaphore> {
    let mut held = held_permits().lock().unwrap_or_else(|e| e.into_inner());
    held.entry(queue.to_string()).or_insert_with(|| Arc::new(Semaphore::new(parallel))).clone()
}

/// A second name for a queue that already exists, pointing at the same permits.
///
/// [`free_at`] is asked about a provider, and a provider knows its own name
/// rather than the name of the queue it waits in. Without this, two models on
/// one server would be looked up under names the table has never seen — and
/// `None` there means "no limit", so an endpoint with no room left would read
/// as the freest one there is and every errand would be sent to it.
fn also_called(name: &str, permits: &Arc<Semaphore>) {
    let mut held = held_permits().lock().unwrap_or_else(|e| e.into_inner());
    held.entry(name.to_string()).or_insert_with(|| permits.clone());
}

/// How many requests this endpoint could take right now without waiting.
///
/// `None` where nothing has claimed a limit for it — an endpoint with no
/// `parallel` set, or one nothing has built a provider for yet — which is not
/// the same as busy and must not be read as it. The answer is a moment old by
/// the time it is used, and that is fine for what it is for: choosing which of
/// several endpoints to hand an errand to, where being wrong costs a wait and
/// not a failure.
pub(crate) fn free_at(name: &str) -> Option<usize> {
    let held = held_permits().lock().unwrap_or_else(|e| e.into_inner());
    held.get(name).map(|permits| permits.available_permits())
}

/// A provider that lets only so many requests through at a time.
pub(crate) struct Limited {
    inner: Box<dyn Provider>,
    permits: Arc<Semaphore>,
}

impl Limited {
    /// `queue` is the server, which is not always this endpoint: several models
    /// on one machine share one.
    pub(crate) fn new(inner: Box<dyn Provider>, queue: &str, parallel: usize) -> Self {
        let permits = permits(queue, parallel);
        if inner.id() != queue {
            also_called(inner.id(), &permits);
        }
        Self { inner, permits }
    }

    /// Waits for a turn. The error is the one a closed semaphore gives, which
    /// nothing here closes — so this is unreachable rather than a case.
    async fn a_turn(&self) -> Result<OwnedSemaphorePermit> {
        self.permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| crate::LlmError::Other(format!("waiting for a turn at {}: {e}", self.inner.id())))
    }
}

#[async_trait]
impl Provider for Limited {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn context_window(&self) -> usize {
        self.inner.context_window()
    }

    fn supports_tools(&self) -> bool {
        self.inner.supports_tools()
    }

    fn takes_effort(&self) -> bool {
        self.inner.takes_effort()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    /// Not held for: listing models is a question about the server rather than
    /// work for it, and `doctor` asking it must not queue behind a turn.
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        self.inner.models().await
    }

    /// The same, and more so: this is the question "is anything there", and an
    /// answer that waits for a busy endpoint to be free answers a different one.
    async fn reachable(&self) -> Result<()> {
        self.inner.reachable().await
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let _turn = self.a_turn().await?;
        self.inner.complete(request).await
    }

    /// The permit outlives this call. A stream is the request — the server is
    /// generating for as long as deltas are arriving — so releasing on return
    /// would let every waiter through at once and limit nothing.
    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let turn = self.a_turn().await?;
        let stream = self.inner.stream(request).await?;
        Ok(Box::pin(Holding { stream, _turn: turn }))
    }
}

/// A stream that keeps its turn until it is dropped.
struct Holding {
    stream: ResponseStream,
    _turn: OwnedSemaphorePermit,
}

impl Stream for Holding {
    type Item = Result<Delta>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.stream.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{Message, StopReason, Usage};

    /// Records when it is entered and when it is left, and yields in between so
    /// that anything able to run alongside it does.
    struct Noting {
        id: String,
        log: Arc<Mutex<String>>,
    }

    #[async_trait]
    impl Provider for Noting {
        fn id(&self) -> &str {
            &self.id
        }
        fn context_window(&self) -> usize {
            8_192
        }
        async fn complete(&self, _request: Request) -> Result<Response> {
            self.log.lock().unwrap_or_else(|e| e.into_inner()).push('+');
            // Scheduling points rather than a sleep. A sleep is a guess about
            // how fast the machine is; a yield is the runtime being offered the
            // chance to run somebody else, which is the whole of what "at the
            // same time" means here.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            self.log.lock().unwrap_or_else(|e| e.into_inner()).push('-');
            Ok(Response {
                message: Message::assistant(""),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: self.id.clone(),
            })
        }
    }

    /// A different name per test, because the permits are held for the life of
    /// the process and keyed by it — two tests sharing a name would share a
    /// count, and the second would be measuring the first's.
    fn three_requests(name: &'static str, parallel: usize) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let log = Arc::new(Mutex::new(String::new()));
        let limited =
            Arc::new(Limited::new(Box::new(Noting { id: name.clone(), log: log.clone() }), &name, parallel));

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let asked: Vec<_> = (0..3)
                .map(|_| {
                    let limited = limited.clone();
                    tokio::spawn(async move { limited.complete(Request::new(vec![])).await })
                })
                .collect();
            for one in asked {
                one.await.unwrap().unwrap();
            }
        });
        log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Three requests at an endpoint that serves one: each is over before the
    /// next begins, however many chances the runtime is given to interleave
    /// them.
    #[test]
    fn a_source_that_serves_one_at_a_time_is_never_asked_for_two() {
        assert_eq!(three_requests("one", 1), "+-+-+-");
    }

    /// The precondition for the test above, and it is not optional: if the
    /// yields never let another request in, `+-+-+-` is what an unlimited
    /// endpoint produces too and the first test proves nothing at all.
    #[test]
    fn the_same_three_do_overlap_when_the_endpoint_allows_it() {
        let overlapped = three_requests("three", 3);
        assert_ne!(overlapped, "+-+-+-", "nothing ran alongside anything");
        assert!(overlapped.starts_with("+++"), "all three were in flight together: {overlapped}");
    }

    /// A stream keeps its place in the queue until it is dropped, and the
    /// model having stopped talking is not the same moment.
    ///
    /// That gap deadlocked a turn for four and a half hours. An endpoint
    /// allowing one request at a time — which is what every configured
    /// endpoint allows unless it says otherwise — finished its answer, took a
    /// goal check, and the checker it spawned waited for the place its own
    /// parent was still holding. The parent was waiting for the checker. The
    /// fix is a `drop` in `agent.rs` at the moment the stream is exhausted;
    /// this is the half of it that belongs here, so the next edit knows the
    /// permit really does outlive the last delta.
    #[test]
    fn a_finished_stream_still_holds_its_place_until_it_is_dropped() {
        let queue = "desk-stream";
        let log = Arc::new(Mutex::new(String::new()));
        let limited = Limited::new(Box::new(Noting { id: "one".into(), log: log.clone() }), queue, 1);

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let stream = limited.stream(Request::new(vec![])).await.expect("a stream");
            // Read to the end: the model has said everything it is going to.
            let mut stream = stream;
            while stream.next().await.is_some() {}
            assert_eq!(free_at(queue), Some(0), "and the place is still taken");

            drop(stream);
            assert_eq!(free_at(queue), Some(1), "only dropping it gives the place back");
        });
    }

    /// Two models on one server are two endpoints and one queue: what a local
    /// runtime interleaves is requests at the process, not requests for a
    /// model, so asking the small one while the large one is answering is the
    /// same mistake as asking the large one twice.
    #[test]
    fn two_models_on_one_server_wait_in_the_same_queue() {
        let queue = "desk";
        let log = Arc::new(Mutex::new(String::new()));
        let small: Arc<dyn Provider> =
            Arc::new(Limited::new(Box::new(Noting { id: "desk-small".into(), log: log.clone() }), queue, 1));
        let large: Arc<dyn Provider> =
            Arc::new(Limited::new(Box::new(Noting { id: "desk-large".into(), log: log.clone() }), queue, 1));

        // Under each endpoint's own name, which is what `free_at` is given:
        // one queue with one permit, reachable by either name. Asserted before
        // the requests, because a lookup that missed would answer `None` — no
        // limit — and send every errand to whichever was busiest.
        assert_eq!(free_at("desk-small"), Some(1), "the small model's name finds the queue");
        assert_eq!(free_at("desk-large"), Some(1), "and so does the large one's");

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let asked: Vec<_> = [small, large]
                .into_iter()
                .map(|provider| tokio::spawn(async move { provider.complete(Request::new(vec![])).await }))
                .collect();
            for one in asked {
                one.await.unwrap().unwrap();
            }
        });

        let ran = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(ran, "+-+-", "one finished before the other started: {ran}");
    }
}
