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

pub struct Retrying(Box<dyn Provider>);

impl Retrying {
    pub fn new(inner: Box<dyn Provider>) -> Self {
        Self(inner)
    }

    /// Waits before attempt `n`, doubling. Returns whether there is another try.
    async fn wait_before(&self, attempt: u32, error: &LlmError) -> bool {
        if attempt >= ATTEMPTS {
            return false;
        }
        let wait = FIRST_WAIT * 2u32.pow(attempt - 1);
        tracing::debug!(provider = self.0.id(), "{error}; trying again in {}s", wait.as_secs());
        tokio::time::sleep(wait).await;
        true
    }
}

#[async_trait]
impl Provider for Retrying {
    fn id(&self) -> &str {
        self.0.id()
    }

    fn context_window(&self) -> usize {
        self.0.context_window()
    }

    fn supports_tools(&self) -> bool {
        self.0.supports_tools()
    }

    fn supports_streaming(&self) -> bool {
        self.0.supports_streaming()
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        self.0.models().await
    }

    async fn reachable(&self) -> Result<()> {
        self.0.reachable().await
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        let mut attempt = 1;
        loop {
            match self.0.complete(request.clone()).await {
                Err(e) if worth_asking_again(&e) && self.wait_before(attempt, &e).await => attempt += 1,
                answer => return answer,
            }
        }
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let mut attempt = 1;
        loop {
            match self.0.stream(request.clone()).await {
                Err(e) if worth_asking_again(&e) && self.wait_before(attempt, &e).await => attempt += 1,
                answer => return answer,
            }
        }
    }
}
