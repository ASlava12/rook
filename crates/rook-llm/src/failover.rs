//! Carrying on when an endpoint is not there.
//!
//! Home, work, and the machine at home being switched off are three different
//! sets of reachable endpoints, and the configuration is one file. A source
//! that cannot be reached is not a choice — but "cannot be reached" is
//! something the network says and not something a file can, so it is
//! discovered rather than declared.
//!
//! Discovered at the moment of the request, rather than by probing. A probe
//! before each turn costs a round trip per turn and per sub-agent to ask a
//! question the request is about to ask anyway, and a probe that passed a
//! second ago says nothing about a tunnel that has just dropped. So the first
//! candidate is asked, and where the answer is that there is nothing there, the
//! next one is.
//!
//! Only that answer. A 401 or a 400 is a configuration that is wrong, and
//! moving to another endpoint on one of those hides it — where the next
//! endpoint is somebody's paid gateway, expensively. `NeverAnswered` is left
//! out for the opposite reason: its own message says the server may be loading
//! a model, and a local server loading ninety gigabytes is the case this whole
//! feature is for, not a case to route around.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::{LlmError, ModelInfo, Provider, Request, Response, ResponseStream, Result};

/// How long an endpoint that could not be reached is left out.
///
/// Short on purpose. The case this exists for is a laptop moving between
/// networks and a tunnel coming back up, and somebody who has just reconnected
/// should not have to restart the agent to be served again by the machine in
/// front of them.
const RESTED: Duration = Duration::from_secs(60);

fn missing() -> &'static Mutex<HashMap<String, Instant>> {
    static MISSING: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    MISSING.get_or_init(Default::default)
}

/// Whether this endpoint failed to answer recently enough to skip.
fn is_missing(name: &str) -> bool {
    let held = missing().lock().unwrap_or_else(|e| e.into_inner());
    held.get(name).is_some_and(|since| since.elapsed() < RESTED)
}

/// Note that it did not answer, and say so once rather than once per request.
///
/// A turn is a great many requests, and a warning per request about a machine
/// that is switched off is a log nobody reads. The line that matters is the
/// transition — this is when it went away — so that is the only one written.
fn note_missing(name: &str, why: &str) {
    let mut held = missing().lock().unwrap_or_else(|e| e.into_inner());
    let fresh = held.get(name).is_none_or(|since| since.elapsed() >= RESTED);
    held.insert(name.to_string(), Instant::now());
    if fresh {
        tracing::warn!("{name} is not answering, so it is out of the rotation for now: {why}");
    }
}

/// Note that it answered, and say so if it had been out.
fn note_answering(name: &str) {
    let mut held = missing().lock().unwrap_or_else(|e| e.into_inner());
    if held.remove(name).is_some() {
        tracing::info!("{name} is answering again");
    }
}

/// Several endpoints in preference order, of which the first that answers is
/// used.
pub(crate) struct Failover {
    /// Never empty: the caller has one it could have used on its own, and this
    /// only exists because there is more than that.
    candidates: Vec<Box<dyn Provider>>,
}

impl Failover {
    pub(crate) fn new(candidates: Vec<Box<dyn Provider>>) -> Self {
        Self { candidates }
    }

    /// The ones worth asking, preferred first.
    ///
    /// Those not known to be missing, and if that is none of them, all of them
    /// anyway. Sixty seconds of refusing to try is the right answer for a
    /// second endpoint and the wrong one for the last: a blip would otherwise
    /// leave the agent with nothing to talk to while the machine was back.
    fn worth_asking(&self) -> Vec<&dyn Provider> {
        let all = || self.candidates.iter().map(Box::as_ref);
        let answering: Vec<&dyn Provider> = all().filter(|p| !is_missing(p.id())).collect();
        match answering.is_empty() {
            true => all().collect(),
            false => answering,
        }
    }
}

/// Runs `call` against each candidate until one answers, and collects what the
/// others said for the error if none does.
macro_rules! first_that_answers {
    ($self:expr, |$provider:ident| $call:expr) => {{
        let mut refused = Vec::new();
        for $provider in $self.worth_asking() {
            match $call.await {
                Ok(answer) => {
                    note_answering($provider.id());
                    return Ok(answer);
                }
                // The only failure that means "ask somebody else". Everything
                // else is this endpoint's answer, and it is the answer.
                Err(LlmError::Unreachable { endpoint, detail }) => {
                    note_missing($provider.id(), &detail);
                    refused.push(format!("{} ({endpoint}): {detail}", $provider.id()));
                }
                Err(other) => return Err(other),
            }
        }
        Err(LlmError::Other(format!(
            "none of the endpoints configured for this answered:\n  {}",
            refused.join("\n  ")
        )))
    }};
}

#[async_trait]
impl Provider for Failover {
    /// The preferred one's, which is what a person configured and what the
    /// warnings are written against. Which one actually served a request is in
    /// the log line for the ones that did not.
    fn id(&self) -> &str {
        self.candidates.first().map(|p| p.id()).unwrap_or("none")
    }

    /// The smallest of them.
    ///
    /// The budget is made before it is known which endpoint will serve the
    /// request, so it has to hold for any of them. The same reasoning each api
    /// uses for an unrecognised model: budgeting against a window the model
    /// does not have fails the request, and budgeting low only wastes some of
    /// it.
    fn context_window(&self) -> usize {
        self.candidates.iter().map(|p| p.context_window()).min().unwrap_or(32_768)
    }

    /// All of them, for the three below: the request is written once and may go
    /// to any of them, so a capability only some have is one that cannot be
    /// used. Sending tool definitions to an endpoint that refuses them fails
    /// the request outright.
    fn supports_tools(&self) -> bool {
        self.candidates.iter().all(|p| p.supports_tools())
    }

    fn takes_effort(&self) -> bool {
        self.candidates.iter().all(|p| p.takes_effort())
    }

    fn supports_streaming(&self) -> bool {
        self.candidates.iter().all(|p| p.supports_streaming())
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        first_that_answers!(self, |provider| provider.models())
    }

    async fn reachable(&self) -> Result<()> {
        first_that_answers!(self, |provider| provider.reachable())
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        first_that_answers!(self, |provider| provider.complete(request.clone()))
    }

    /// Failing over here is honest because every dialect checks the status
    /// before it returns the stream: a failure that reaches this point has
    /// emitted nothing, so there is no half-delivered reply to replace.
    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        first_that_answers!(self, |provider| provider.stream(request.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{Message, StopReason, Usage};

    enum Says {
        Answer,
        Unreachable,
        Refused,
    }

    struct Fake {
        id: String,
        says: Says,
        asked: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for Fake {
        fn id(&self) -> &str {
            &self.id
        }
        fn context_window(&self) -> usize {
            match self.says {
                Says::Answer => 128_000,
                _ => 8_192,
            }
        }
        async fn complete(&self, _request: Request) -> Result<Response> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            match self.says {
                Says::Answer => Ok(Response {
                    message: Message::assistant(""),
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                    model: self.id.clone(),
                }),
                Says::Unreachable => Err(LlmError::Unreachable {
                    endpoint: format!("http://{}", self.id),
                    detail: "connection refused".into(),
                }),
                Says::Refused => {
                    Err(LlmError::Status { status: 401, body: "invalid_api_key".into(), retry_after: None })
                }
            }
        }
    }

    /// Ids are unique per test because what is missing is remembered for the
    /// life of the process and keyed by id: two tests sharing one would have
    /// the second measuring the first.
    fn saying(id: &str, says: Says) -> (Box<dyn Provider>, Arc<AtomicUsize>) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let asked = Arc::new(AtomicUsize::new(0));
        let id = format!("{id}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        (Box::new(Fake { id, says, asked: asked.clone() }), asked)
    }

    fn asking(over: &Failover) -> Result<Response> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(over.complete(Request::new(vec![])))
    }

    #[test]
    fn an_endpoint_that_cannot_be_reached_hands_over_to_the_next() {
        let (gone, _) = saying("gone", Says::Unreachable);
        let (here, _) = saying("here", Says::Answer);
        let here_id = here.id().to_string();

        let answered = asking(&Failover::new(vec![gone, here])).expect("the second answered");

        assert_eq!(answered.model, here_id);
    }

    /// The one that matters. A wrong key is a configuration that is wrong, and
    /// moving to the next endpoint on it hides that — where the next endpoint
    /// is a paid gateway, expensively.
    #[test]
    fn a_key_that_is_wrong_is_the_answer_rather_than_a_reason_to_try_elsewhere() {
        let (refusing, _) = saying("refusing", Says::Refused);
        let (here, asked) = saying("here", Says::Answer);

        let why = asking(&Failover::new(vec![refusing, here])).unwrap_err();

        assert!(matches!(why, LlmError::Status { status: 401, .. }), "{why}");
        assert_eq!(asked.load(Ordering::Relaxed), 0, "the second endpoint was never asked");
    }

    #[test]
    fn with_nothing_answering_the_error_names_every_endpoint_tried() {
        let (first, _) = saying("gone", Says::Unreachable);
        let (second, _) = saying("also-gone", Says::Unreachable);
        let (names, _) = (vec![first.id().to_string(), second.id().to_string()], ());

        let why = asking(&Failover::new(vec![first, second])).unwrap_err().to_string();

        for name in names {
            assert!(why.contains(&name), "{name} is missing from: {why}");
        }
    }

    /// Out of the rotation means out of it: the second request does not pay the
    /// timeout of an endpoint that was not there a moment ago.
    #[test]
    fn an_endpoint_that_did_not_answer_is_not_asked_again_straight_away() {
        let (gone, tried) = saying("gone", Says::Unreachable);
        let (here, _) = saying("here", Says::Answer);
        let over = Failover::new(vec![gone, here]);

        asking(&over).expect("the second answered");
        asking(&over).expect("and again");

        assert_eq!(tried.load(Ordering::Relaxed), 1, "it was asked after being found missing");
    }

    /// The budget is made before it is known which endpoint will serve the
    /// request, so it has to hold for any of them.
    #[test]
    fn the_window_budgeted_against_is_the_smallest_on_offer() {
        let (small, _) = saying("small", Says::Unreachable);
        let (large, _) = saying("large", Says::Answer);

        assert_eq!(Failover::new(vec![large, small]).context_window(), 8_192);
    }
}
