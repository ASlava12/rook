//! Asking again when the provider said "not now".
//!
//! A hosted endpoint answers 429 when the account is over its rate and 503 or
//! 529 when it is overloaded. Both mean *later*, not *no*. Without this a turn
//! that has been running for minutes ends on one of them and the work is gone —
//! which is the failure an autonomous agent can least afford, because nobody is
//! sitting there to ask again.
//!
//! Wrapped around the provider rather than written into each dialect: the same
//! statuses mean the same thing to all three, and three copies of a retry loop
//! is three places for the list of statuses to drift.
//!
//! Only the request is retried, never a stream that has started. Every dialect
//! checks the status before it returns the stream, so a failure that reaches
//! here has emitted nothing and there is no half-delivered reply to duplicate.
//!
//! The other kind of asking again is here for the same reason: a 400 that names
//! a field the agent added, rather than one the user wrote, is not a permanent
//! answer — the same request fails for ever, and a request without that field
//! may not.

use std::time::Duration;

use async_trait::async_trait;

use crate::{LlmError, ModelInfo, Provider, Request, Response, ResponseStream, Result};

/// Four tries over about seven seconds. Long enough to outlast the burst a rate
/// limiter is smoothing, short enough that someone watching a turn does not
/// conclude it has hung.
const ATTEMPTS: u32 = 4;
const FIRST_WAIT: Duration = Duration::from_secs(1);

/// Statuses that mean *later*.
///
/// A 400 or a 401 answers the same however many times it is asked, and retrying
/// one only delays the message that says what to fix.
fn worth_asking_again(error: &LlmError) -> bool {
    matches!(error, LlmError::Status { status, .. } if matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529))
}

/// Whether a refusal is about the effort this crate added.
///
/// Both directions happen: a route with no thinking rejects the field outright,
/// and a route where thinking is mandatory rejects it being turned down. Each
/// dialect spells it differently — `reasoning_effort`, `thinking`,
/// `thinkingBudget`, `output_config.effort` — so the words are matched rather
/// than the shape, and the request having an effort at all is what makes this
/// worth acting on.
fn names_the_effort(error: &LlmError) -> bool {
    let LlmError::Status { status, body, .. } = error else { return false };
    let said = body.to_ascii_lowercase();
    *status == 400 && ["reasoning", "thinking", "effort"].iter().any(|word| said.contains(word))
}

/// Never asked for less than this, whatever an endpoint says: a reply with no
/// room to write a tool call is the failure this is here to avoid, and an
/// endpoint refusing a thousand tokens is one nothing can be run against.
const LEAST_OUTPUT: u32 = 1024;

/// Whether a refusal is about how much output was asked for.
///
/// The ceiling on a reply is in no API: Anthropic refuses a `max_tokens` above
/// the model's own limit with a 400 that names it, and the number differs by
/// model and changes with each one. So it is learned from the refusal rather
/// than guessed from a name, which is the same reason the effort is.
fn names_the_output(error: &LlmError) -> bool {
    let LlmError::Status { status, body, .. } = error else { return false };
    let said = body.to_ascii_lowercase();
    *status == 400
        && ["max_tokens", "max_output_tokens", "maxtokens", "max output"].iter().any(|w| said.contains(w))
}

pub struct Retrying {
    inner: Box<dyn Provider>,
    /// Set by the first refusal. A model name is what decides whether the field
    /// is sent, and a gateway serving something else under that name is exactly
    /// where that guess is wrong — so the endpoint's own answer overrides it,
    /// once per process rather than once per step.
    effort_refused: std::sync::atomic::AtomicBool,
    /// The largest reply this endpoint has accepted, once one has been refused
    /// for being too large. Zero until then, and once set it holds for the life
    /// of the process — asking the same refusal every step is a step spent
    /// finding out what was already found out.
    output_ceiling: std::sync::atomic::AtomicU32,
}

impl Retrying {
    pub fn new(inner: Box<dyn Provider>) -> Self {
        Self {
            inner,
            effort_refused: std::sync::atomic::AtomicBool::new(false),
            output_ceiling: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// The request as this endpoint has already said it will take it.
    fn as_accepted(&self, mut request: Request) -> Request {
        if self.effort_refused.load(std::sync::atomic::Ordering::Relaxed) {
            request.effort = None;
        }
        let ceiling = self.output_ceiling.load(std::sync::atomic::Ordering::Relaxed);
        if ceiling > 0 {
            request.max_output_tokens = request.max_output_tokens.min(ceiling);
        }
        request
    }

    /// Whether this refusal is one to answer by asking for less and trying
    /// again. Halved rather than set to a guess: the endpoint says the number
    /// is too big and not what would fit, and halving finds a number that does
    /// in a few tries without a table of models to keep current.
    fn ask_for_less_output(&self, error: &LlmError, request: &mut Request) -> bool {
        if request.max_output_tokens <= LEAST_OUTPUT || !names_the_output(error) {
            return false;
        }
        let smaller = (request.max_output_tokens / 2).max(LEAST_OUTPUT);
        tracing::info!(
            provider = self.inner.id(),
            "the endpoint refused {} output tokens ({error}); asking for {smaller}",
            request.max_output_tokens
        );
        self.output_ceiling.store(smaller, std::sync::atomic::Ordering::Relaxed);
        request.max_output_tokens = smaller;
        true
    }

    /// Whether this refusal is one to answer by dropping the effort and asking
    /// again. Having an effort to drop is half the test, so a 400 that says
    /// "reasoning" for its own reasons cannot loop here.
    fn drop_the_effort(&self, error: &LlmError, request: &mut Request) -> bool {
        if request.effort.is_none() || !names_the_effort(error) {
            return false;
        }
        tracing::info!(
            provider = self.inner.id(),
            "the endpoint refused the effort field ({error}); asking again without it, and not sending it again"
        );
        self.effort_refused.store(true, std::sync::atomic::Ordering::Relaxed);
        request.effort = None;
        true
    }

    /// Waits before attempt `n`, doubling — or for as long as the provider
    /// asked, when it said. A rate limiter answering `Retry-After: 30` has
    /// given the only number worth waiting: doubling from a second spends
    /// every try inside the window it named and ends the turn on a refusal it
    /// had already explained how to avoid.
    async fn wait_before(&self, attempt: u32, error: &LlmError) -> bool {
        if attempt >= ATTEMPTS {
            return false;
        }
        let doubling = FIRST_WAIT * 2u32.pow(attempt - 1);
        let wait = match error {
            LlmError::Status { retry_after: Some(asked), .. } => doubling.max(*asked),
            _ => doubling,
        };
        tracing::debug!(provider = self.inner.id(), "{error}; trying again in {}s", wait.as_secs());
        tokio::time::sleep(wait).await;
        true
    }
}

#[async_trait]
impl Provider for Retrying {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn context_window(&self) -> usize {
        self.inner.context_window()
    }

    fn supports_tools(&self) -> bool {
        self.inner.supports_tools()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        self.inner.models().await
    }

    async fn reachable(&self) -> Result<()> {
        self.inner.reachable().await
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let mut request = self.as_accepted(request);
        let mut attempt = 1;
        loop {
            match self.inner.complete(request.clone()).await {
                Err(e) if worth_asking_again(&e) && self.wait_before(attempt, &e).await => attempt += 1,
                Err(e) if self.drop_the_effort(&e, &mut request) => continue,
                Err(e) if self.ask_for_less_output(&e, &mut request) => continue,
                answer => return answer,
            }
        }
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let mut request = self.as_accepted(request);
        let mut attempt = 1;
        loop {
            match self.inner.stream(request.clone()).await {
                Err(e) if worth_asking_again(&e) && self.wait_before(attempt, &e).await => attempt += 1,
                Err(e) if self.drop_the_effort(&e, &mut request) => continue,
                Err(e) if self.ask_for_less_output(&e, &mut request) => continue,
                answer => return answer,
            }
        }
    }
}
