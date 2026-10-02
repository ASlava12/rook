//! Observing one physical generation without collecting its text or reasoning.
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::Stream;

use crate::{Completion, Delta, Dispatch, Provider, Request, ResponseStream, Result, Usage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptStatus {
    Completed,
    Failed,
    Incomplete,
    Interrupted,
}

/// Constant-space wire facts. An omitted counter never becomes a reported zero.
#[derive(Clone, Debug, Default)]
pub struct AttemptFacts {
    pub usage: Option<Usage>,
    pub usage_reported: bool,
    pub completion_confirmed: bool,
    pub reported_model: Option<String>,
}

/// The host persists admission before any generation request is sent.
pub trait AttemptObserver: Send + Sync {
    fn start(&self, dispatch: Option<&Dispatch>) -> Result<Box<dyn Attempt>>;
}

pub trait Attempt: Send {
    fn finish(&mut self, status: AttemptStatus, facts: &AttemptFacts) -> Result<()>;
}

struct Guard {
    sink: Option<Box<dyn Attempt>>,
    facts: AttemptFacts,
    metadata_seen: bool,
}

impl Guard {
    fn new(observer: &dyn AttemptObserver, dispatch: Option<&Dispatch>) -> Result<Self> {
        Ok(Self {
            sink: Some(observer.start(dispatch)?),
            facts: AttemptFacts::default(),
            metadata_seen: false,
        })
    }

    fn finish(&mut self, status: AttemptStatus) -> Result<()> {
        match self.sink.take() {
            Some(mut sink) => sink.finish(status, &self.facts),
            None => Ok(()),
        }
    }

    fn delta(&mut self, delta: &Delta) {
        match delta {
            Delta::ResponseMetadata { usage_reported, completion_confirmed } => {
                self.metadata_seen = true;
                self.facts.usage_reported = *usage_reported;
                self.facts.completion_confirmed = *completion_confirmed;
            }
            Delta::Done { usage, model, .. } => {
                self.facts.usage = Some(usage.clone());
                self.facts.reported_model = name(model);
                // Preserve the legacy Done contract without asserting native
                // counter presence. Native metadata remains authoritative.
                if !self.metadata_seen {
                    self.facts.completion_confirmed = true;
                }
            }
            _ => {}
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // A failed write leaves the admitted attempt open in durable history.
        // Drop cannot return an error; it must never turn this into a success.
        let _ = self.finish(AttemptStatus::Interrupted);
    }
}

fn name(value: &str) -> Option<String> {
    (value.len() <= 256 && !value.chars().any(char::is_control)).then(|| value.to_owned())
}

pub(crate) async fn complete<P: Provider + ?Sized>(
    provider: &P,
    request: Request,
    observer: Arc<dyn AttemptObserver>,
) -> Result<Completion> {
    let dispatch = provider.dispatch_identity();
    let mut guard = Guard::new(observer.as_ref(), dispatch.as_ref())?;
    match provider.complete_with_metadata(request).await {
        Ok(completed) => {
            guard.facts = AttemptFacts {
                usage: Some(completed.response.usage.clone()),
                usage_reported: completed.usage_reported,
                completion_confirmed: completed.completion_confirmed,
                reported_model: name(&completed.response.model),
            };
            guard.finish(if completed.completion_confirmed {
                AttemptStatus::Completed
            } else {
                AttemptStatus::Incomplete
            })?;
            Ok(completed)
        }
        Err(error) => {
            guard.finish(AttemptStatus::Failed)?;
            Err(error)
        }
    }
}

pub(crate) async fn stream<P: Provider + ?Sized>(
    provider: &P,
    request: Request,
    observer: Arc<dyn AttemptObserver>,
) -> Result<ResponseStream> {
    let dispatch = provider.dispatch_identity();
    let mut guard = Guard::new(observer.as_ref(), dispatch.as_ref())?;
    match provider.stream(request).await {
        Ok(stream) => Ok(Box::pin(Observed { stream, guard, ended: false })),
        Err(error) => {
            guard.finish(AttemptStatus::Failed)?;
            Err(error)
        }
    }
}

struct Observed {
    stream: ResponseStream,
    guard: Guard,
    ended: bool,
}

impl Stream for Observed {
    type Item = Result<Delta>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match self.stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(delta))) => {
                self.guard.delta(&delta);
                Poll::Ready(Some(Ok(delta)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.ended = true;
                let recorded = self.guard.finish(AttemptStatus::Failed);
                Poll::Ready(Some(Err(recorded.err().unwrap_or(error))))
            }
            Poll::Ready(None) => {
                self.ended = true;
                let status = if self.guard.facts.usage.is_some() && self.guard.facts.completion_confirmed {
                    AttemptStatus::Completed
                } else {
                    AttemptStatus::Incomplete
                };
                match self.guard.finish(status) {
                    Ok(()) => Poll::Ready(None),
                    Err(error) => Poll::Ready(Some(Err(error))),
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlmError, Message, Response, StopReason};
    use async_trait::async_trait;
    use futures_util::StreamExt;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Log {
        starts: AtomicUsize,
        ends: Mutex<Vec<(AttemptStatus, AttemptFacts)>>,
        reject: bool,
        reject_end: bool,
    }
    struct Sink(Arc<Log>);
    struct Recorder(Arc<Log>);
    impl AttemptObserver for Recorder {
        fn start(&self, dispatch: Option<&Dispatch>) -> Result<Box<dyn Attempt>> {
            if self.0.reject {
                return Err(LlmError::Other("admission could not be saved".into()));
            }
            assert_eq!(dispatch.unwrap().provider, "physical");
            assert!(self.0.starts.fetch_add(1, Ordering::SeqCst) < 32);
            Ok(Box::new(Sink(self.0.clone())))
        }
    }
    impl Attempt for Sink {
        fn finish(&mut self, status: AttemptStatus, facts: &AttemptFacts) -> Result<()> {
            let mut ends = self.0.ends.lock().unwrap();
            assert!(ends.len() < 32);
            ends.push((status, facts.clone()));
            if self.0.reject_end { Err(LlmError::Other("ending could not be saved".into())) } else { Ok(()) }
        }
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Complete,
        MissingMarker,
        Error,
        DelayedOpening,
        Partial,
    }
    struct Leaf {
        mode: Mode,
        log: Arc<Log>,
        calls: Arc<AtomicUsize>,
    }
    impl Leaf {
        fn entered(&self) {
            let count = self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(self.log.starts.load(Ordering::SeqCst) > count, "admission precedes the physical call");
        }
    }
    #[async_trait]
    impl Provider for Leaf {
        fn id(&self) -> &str {
            "physical"
        }
        fn context_window(&self) -> usize {
            65536
        }
        fn dispatch_identity(&self) -> Option<Dispatch> {
            Dispatch::bounded("physical", "model", true)
        }
        async fn complete(&self, _: Request) -> Result<Response> {
            self.entered();
            if matches!(self.mode, Mode::DelayedOpening) {
                futures_util::future::pending::<()>().await;
            }
            if matches!(self.mode, Mode::Error) {
                return Err(LlmError::Decode("private provider error".into()));
            }
            Ok(Response {
                message: Message::assistant("answer"),
                stop_reason: StopReason::EndTurn,
                usage: Usage { input_tokens: 0, output_tokens: 7, ..Default::default() },
                model: "echo".into(),
            })
        }
        async fn complete_with_metadata(&self, request: Request) -> Result<Completion> {
            Ok(Completion {
                response: self.complete(request).await?,
                dispatch: self.dispatch_identity(),
                usage_reported: true,
                completion_confirmed: !matches!(self.mode, Mode::MissingMarker),
            })
        }
        async fn stream(&self, _: Request) -> Result<ResponseStream> {
            self.entered();
            if matches!(self.mode, Mode::DelayedOpening) {
                futures_util::future::pending::<()>().await;
            }
            let signed =
                serde_json::json!({"thinking":"unchanged signed bytes","signature":"native-signature"});
            let complete = !matches!(self.mode, Mode::MissingMarker);
            let mut deltas = vec![
                Ok(Delta::ReasoningDone(signed)),
                Ok(Delta::Text("answer".into())),
                Ok(Delta::ResponseMetadata { usage_reported: complete, completion_confirmed: complete }),
                Ok(Delta::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage { input_tokens: 0, output_tokens: 7, ..Default::default() },
                    model: "echo".into(),
                }),
            ];
            if matches!(self.mode, Mode::Error) {
                deltas.push(Err(LlmError::Decode("private provider error".into())));
            }
            let stream = futures_util::stream::iter(deltas);
            if matches!(self.mode, Mode::Partial) {
                Ok(Box::pin(stream.chain(futures_util::stream::pending())))
            } else {
                Ok(Box::pin(stream))
            }
        }
    }

    fn setup(mode: Mode, reject: bool) -> (Leaf, Arc<dyn AttemptObserver>, Arc<Log>) {
        let log = Arc::new(Log { reject, ..Default::default() });
        let leaf = Leaf { mode, log: log.clone(), calls: Arc::new(AtomicUsize::new(0)) };
        (leaf, Arc::new(Recorder(log.clone())), log)
    }
    fn request() -> Request {
        Request::new(vec![Message::user("one request")])
    }

    #[tokio::test]
    async fn observations_preserve_deltas_zero_counter_presence_and_terminal_evidence() {
        for (mode, expected) in
            [(Mode::Complete, AttemptStatus::Completed), (Mode::MissingMarker, AttemptStatus::Incomplete)]
        {
            let (leaf, observer, log) = setup(mode, false);
            let mut stream = leaf.stream_observed(request(), observer).await.unwrap();
            let first = stream.next().await.unwrap().unwrap();
            let Delta::ReasoningDone(block) = first else { panic!("signed state was not preserved") };
            assert_eq!(
                block,
                serde_json::json!({"thinking":"unchanged signed bytes","signature":"native-signature"})
            );
            assert!(matches!(stream.next().await.unwrap().unwrap(), Delta::Text(text) if text == "answer"));
            while let Some(delta) = stream.next().await {
                delta.unwrap();
            }
            drop(stream);
            let ends = log.ends.lock().unwrap();
            assert_eq!(ends.len(), 1);
            assert_eq!(ends[0].0, expected);
            assert_eq!(ends[0].1.usage.as_ref().unwrap().input_tokens, 0);
            assert_eq!(ends[0].1.usage.as_ref().unwrap().output_tokens, 7);
            assert_eq!(ends[0].1.usage_reported, expected == AttemptStatus::Completed);
        }
    }

    #[tokio::test]
    async fn cancellation_records_one_interruption_during_opening_and_after_partial_usage() {
        for streamed in [false, true] {
            let (leaf, observer, log) = setup(Mode::DelayedOpening, false);
            let future = async {
                if streamed {
                    leaf.stream_observed(request(), observer).await.map(|_| ())
                } else {
                    leaf.complete_observed(request(), observer).await.map(|_| ())
                }
            };
            assert!(tokio::time::timeout(std::time::Duration::from_millis(20), future).await.is_err());
            let ends = log.ends.lock().unwrap();
            assert_eq!(ends.len(), 1);
            assert_eq!(ends[0].0, AttemptStatus::Interrupted);
            assert!(ends[0].1.usage.is_none());
        }
        let (leaf, observer, log) = setup(Mode::Partial, false);
        let mut stream = leaf.stream_observed(request(), observer).await.unwrap();
        for _ in 0..4 {
            stream.next().await.unwrap().unwrap();
        }
        drop(stream);
        let ends = log.ends.lock().unwrap();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].0, AttemptStatus::Interrupted);
        assert_eq!(ends[0].1.usage.as_ref().unwrap().output_tokens, 7);
    }

    #[tokio::test]
    async fn failures_end_once_and_an_unsaved_admission_never_calls_the_provider() {
        for streamed in [false, true] {
            let (leaf, observer, log) = setup(Mode::Error, false);
            if streamed {
                let mut stream = leaf.stream_observed(request(), observer).await.unwrap();
                while let Some(delta) = stream.next().await {
                    if delta.is_err() {
                        break;
                    }
                }
                drop(stream);
            } else {
                assert!(leaf.complete_observed(request(), observer).await.is_err());
            }
            let ends = log.ends.lock().unwrap();
            assert_eq!(ends.len(), 1);
            assert_eq!(ends[0].0, AttemptStatus::Failed);
        }
        let (leaf, observer, log) = setup(Mode::Complete, true);
        assert!(leaf.complete_observed(request(), observer.clone()).await.is_err());
        assert!(leaf.stream_observed(request(), observer).await.is_err());
        assert_eq!(leaf.calls.load(Ordering::SeqCst), 0);
        assert!(log.ends.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unsaved_ending_surfaces_once_without_drop_reclassifying_it_as_interrupted() {
        for streamed in [false, true] {
            let log = Arc::new(Log { reject_end: true, ..Default::default() });
            let leaf = Leaf { mode: Mode::Complete, log: log.clone(), calls: Arc::new(AtomicUsize::new(0)) };
            let observer: Arc<dyn AttemptObserver> = Arc::new(Recorder(log.clone()));
            if streamed {
                let mut stream = leaf.stream_observed(request(), observer).await.unwrap();
                for _ in 0..4 {
                    stream.next().await.unwrap().unwrap();
                }
                let error = stream.next().await.unwrap().unwrap_err();
                assert!(error.to_string().contains("ending could not be saved"));
                assert!(stream.next().await.is_none());
                drop(stream);
            } else {
                let error = leaf.complete_observed(request(), observer).await.unwrap_err();
                assert!(error.to_string().contains("ending could not be saved"));
            }
            let ends = log.ends.lock().unwrap();
            assert_eq!(ends.len(), 1, "a failed ending must leave its admission pending");
            assert_eq!(ends[0].0, AttemptStatus::Completed);
        }
    }
}
