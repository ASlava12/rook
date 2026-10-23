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
//! A stream can report a transient refusal after HTTP 200. Retry only before
//! any response content or completion evidence has been delivered; transport
//! metadata alone is safe. A partial answer is never repeated automatically.
//!
//! The other kind of asking again is here for the same reason: a 400 that names
//! a field the agent added, rather than one the user wrote, is not a permanent
//! answer — the same request fails for ever, and a request without that field
//! may not.

use std::time::Duration;

use async_trait::async_trait;

use crate::{LlmError, ModelInfo, Provider, Request, Response, ResponseStream, Result};

/// Four tries with at most 8.75 seconds of ordinary backoff. Long enough to outlast the burst a rate
/// limiter is smoothing, short enough that someone watching a turn does not
/// conclude it has hung.
const ATTEMPTS: u32 = 4;
const FIRST_WAIT: Duration = Duration::from_secs(1);

fn random_jitter() -> u32 {
    fastrand::u32(..)
}

/// Positive jitter keeps concurrent callers from hitting the same rate window
/// together. Never undercut a provider's minimum or add time above 120 seconds.
/// A custom provider can supply a larger minimum; retain it without extra delay.
fn jittered_wait(floor: Duration, sample: u32) -> Duration {
    let headroom = Duration::from_secs(crate::MOST_PATIENCE_SECS).saturating_sub(floor);
    let band = (floor / 4).min(headroom);
    // The band is at most 30 seconds, so this product fits u128 and its
    // scaled nanoseconds fit u64. Integer scaling preserves both endpoints.
    let nanos = band.as_nanos() * u128::from(sample) / u128::from(u32::MAX);
    floor.saturating_add(Duration::from_nanos(nanos as u64))
}

/// Statuses that mean *later*.
///
/// A 400 or a 401 answers the same however many times it is asked, and retrying
/// one only delays the message that says what to fix. The status alone is not
/// the whole question: a gateway between here and the model answers 500 or 502
/// for its upstream's 400, and the same wrong request was then sent four times
/// with a backoff between them before the message that named it arrived.
fn worth_asking_again(error: &LlmError) -> bool {
    if let LlmError::IncompleteResponse { retryable, .. } = error {
        return *retryable;
    }
    matches!(error, LlmError::Status { status, .. } if matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529))
        && !names_a_wrong_request(error)
        && !out_of_credit(error)
}

/// Whether a refusal is an empty wallet.
///
/// Its own kind of permanent. Asking again in a second does not add money, so
/// there is nothing here for the backoff to wait out — and it is not the
/// request being wrong either, so a different endpoint may well answer it.
/// Both of those matter, which is why this is one question with one answer
/// rather than a condition written twice.
///
/// There is no single thing to look for, because providers do not agree.
/// OpenAI sends `insufficient_quota`, which is a code and is matched as
/// one. Anthropic sends `invalid_request_error` and puts the reason only in the
/// message, so that one is matched on the sentence. Prose, and knowingly:
/// matching a whole phrase that means exactly one thing is not the mistake of
/// reading the word "timeout" in a field named `timeout`, and here there is
/// nothing else to read.
pub(crate) fn out_of_credit(error: &LlmError) -> bool {
    let LlmError::Status { status, body, .. } = error else { return false };
    if *status == 402 {
        return true;
    }
    let said = body.to_ascii_lowercase();
    ["insufficient_quota", "insufficient_credits", "billing_not_active", "payment_required"]
        .iter()
        .any(|code| said.contains(code))
        || said.contains("credit balance is too low")
        || (*status == 429 && zai_empty_balance(body))
}

/// Z.ai calls exhausted balance a rate limit. Require its structured business
/// code and explicit message; rolling package limits also mention balance but
/// can reset without account changes. Bound JSON parsing for custom providers.
fn zai_empty_balance(body: &str) -> bool {
    if body.len() > 16 * 1024 {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else { return false };
    let Some(error) = value.get("error") else { return false };
    let code = error.get("code");
    let known_code = code.and_then(serde_json::Value::as_str) == Some("1113")
        || code.and_then(serde_json::Value::as_u64) == Some(1113);
    known_code
        && error.get("message").and_then(serde_json::Value::as_str).is_some_and(|message| {
            message.to_ascii_lowercase().contains("insufficient balance or no resource package")
        })
}

/// Whether a refusal carries a code that means *no* rather than *later*.
///
/// Codes rather than prose, and only the canonical ones: these are the strings
/// an API puts in a `type` or `code` field, so a body carrying one is stating a
/// deterministic verdict whatever status it arrived under. Matching prose here
/// would be the mistake openclaw made in the other direction — a message
/// containing the word "timeout" read as a transient failure when it was a
/// parameter named `timeout` being rejected.
pub(crate) fn names_a_wrong_request(error: &LlmError) -> bool {
    let LlmError::Status { body, .. } = error else { return false };
    let said = body.to_ascii_lowercase();
    [
        "invalid_request_error",
        "context_length_exceeded",
        "invalid_api_key",
        "authentication_error",
        "permission_error",
        "model_not_found",
    ]
    .iter()
    .any(|code| said.contains(code))
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

/// Whether a refusal is the prompt being longer than the model will take.
///
/// Every dialect words it differently — "context length exceeded", "maximum
/// context length is 8192 tokens", "prompt is too long", "input length exceeds"
/// — so the words are matched rather than a shape. Public because the answer
/// to this refusal is not another request: it is summarising the conversation,
/// which only the loop can do.
pub fn names_the_context(error: &LlmError) -> bool {
    let LlmError::Status { status, body, .. } = error else { return false };
    let said = body.to_ascii_lowercase();
    // The canonical code decides on its own, whatever status carried it: a
    // gateway wrapping its upstream's 400 in a 500 was answered here by four
    // retries and then a failed turn, when what it needed was a compaction.
    (*status == 400 || *status == 413 || said.contains("context_length_exceeded"))
        && ["context length", "context_length", "too long", "input length", "maximum context"]
            .iter()
            .any(|word| said.contains(word))
}

/// Never asked for less than this, whatever an endpoint says: a reply with no
/// room to write a tool call is the failure this is here to avoid, and an
/// endpoint refusing a thousand tokens is one nothing can be run against.
const LEAST_OUTPUT: u32 = 1024;

/// A credit balance is temporary, unlike a model's output ceiling. Only lower
/// this request, once, when a payment refusal states what it can afford.
fn use_credit_limit(error: &LlmError, request: &mut Request, retried: &mut bool) -> bool {
    let LlmError::Status { status: 402, body, .. } = error else { return false };
    if *retried {
        return false;
    }
    let body = body.to_ascii_lowercase();
    let Some((_, rest)) = body.split_once("can only afford ") else { return false };
    let rest = rest.trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != ',').unwrap_or(rest.len());
    let Some((number, tail)) = rest.get(..end).zip(rest.get(end..)) else { return false };
    if tail.starts_with(|c: char| !c.is_ascii_whitespace() && c != '.' && c != '"') {
        return false;
    }
    if tail
        .strip_prefix('.')
        .is_some_and(|tail| tail.starts_with(|c: char| !c.is_ascii_whitespace() && c != '"'))
    {
        return false;
    }
    let mut groups = number.split(',');
    let first = groups.next().unwrap_or_default();
    if first.is_empty() || (number.contains(',') && first.len() > 3) || groups.any(|group| group.len() != 3) {
        return false;
    }
    let Some(affordable) = number
        .bytes()
        .filter(|b| *b != b',')
        .try_fold(0u32, |n, digit| n.checked_mul(10)?.checked_add(u32::from(digit - b'0')))
    else {
        return false;
    };
    // Leave room for endpoint accounting differences without increasing any
    // caller allowance or remembering a balance across calls.
    let Some(smaller) = affordable.checked_sub(64) else { return false };
    if smaller < LEAST_OUTPUT || affordable >= request.max_output_tokens {
        return false;
    }
    request.max_output_tokens = smaller;
    *retried = true;
    true
}

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
        && [
            "max_tokens",
            "max_completion_tokens",
            "max_output_tokens",
            "maxtokens",
            "maxoutputtokens",
            "max output",
        ]
        .iter()
        .any(|w| said.contains(w))
}

#[derive(Clone)]
pub struct Retrying {
    inner: std::sync::Arc<dyn Provider>,
    // A sampling seam makes the actual retry paths testable without relying
    // on statistical spread or changing a process-wide random seed.
    sample_jitter: fn() -> u32,
    /// Set by the first refusal. A model name is what decides whether the field
    /// is sent, and a gateway serving something else under that name is exactly
    /// where that guess is wrong — so the endpoint's own answer overrides it,
    /// once per process rather than once per step.
    effort_refused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The largest reply this endpoint has accepted, once one has been refused
    /// for being too large. Zero until then, and once set it holds for the life
    /// of the process — asking the same refusal every step is a step spent
    /// finding out what was already found out.
    output_ceiling: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl Retrying {
    pub fn new(inner: Box<dyn Provider>) -> Self {
        Self {
            inner: inner.into(),
            sample_jitter: random_jitter,
            effort_refused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            output_ceiling: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
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
    async fn wait_before(
        &self,
        attempt: u32,
        error: &LlmError,
        observer: Option<&dyn crate::AttemptObserver>,
    ) -> bool {
        if attempt >= ATTEMPTS {
            return false;
        }
        let doubling = FIRST_WAIT * 2u32.pow(attempt.saturating_sub(1));
        let floor = match error {
            LlmError::Status { retry_after: Some(asked), .. } => doubling.max(*asked),
            _ => doubling,
        };
        let wait = jittered_wait(floor, (self.sample_jitter)());
        if let Some(observer) = observer {
            observer.waiting_to_retry(crate::RetryWait {
                delay: wait,
                next_attempt: attempt + 1,
                status: match error {
                    LlmError::Status { status, .. } => Some(*status),
                    _ => None,
                },
            });
        }
        tracing::debug!(provider = self.inner.id(), "{error}; trying again in {:.3}s", wait.as_secs_f64());
        tokio::time::sleep(wait).await;
        true
    }
}

#[async_trait]
impl Provider for Retrying {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn dispatch_identity(&self) -> Option<crate::Dispatch> {
        self.inner.dispatch_identity()
    }

    fn context_window(&self) -> usize {
        self.inner.context_window()
    }

    fn context_key(&self) -> Option<[u8; 32]> {
        self.inner.context_key()
    }

    fn context_is_explicit(&self) -> bool {
        self.inner.context_is_explicit()
    }

    async fn discover_context_window(&self, limits: crate::CatalogLimits) -> Result<Option<usize>> {
        self.inner.discover_context_window(limits).await
    }

    fn supports_tools(&self) -> bool {
        self.inner.supports_tools()
    }

    fn image_input_support(&self) -> Option<bool> {
        self.inner.image_input_support()
    }

    fn can_replay_reasoning(&self, messages: &[crate::Message]) -> bool {
        self.inner.can_replay_reasoning(messages)
    }

    fn can_resume_reasoning(&self, messages: &[crate::Message]) -> bool {
        self.inner.can_resume_reasoning(messages)
    }

    fn takes_effort(&self) -> bool {
        !self.effort_refused.load(std::sync::atomic::Ordering::Relaxed) && self.inner.takes_effort()
    }

    fn effort_use(&self, effort: crate::Effort) -> crate::EffortUse {
        if self.effort_refused.load(std::sync::atomic::Ordering::Relaxed) {
            crate::EffortUse::Omitted { reason: "endpoint refused the effort parameter" }
        } else {
            self.inner.effort_use(effort)
        }
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        self.inner.models().await
    }

    async fn models_with(&self, limits: crate::CatalogLimits) -> Result<Vec<ModelInfo>> {
        self.inner.models_with(limits).await
    }

    async fn reachable(&self) -> Result<()> {
        self.inner.reachable().await
    }

    async fn complete(&self, request: Request) -> Result<Response> {
        self.complete_with_metadata(request).await.map(|completed| completed.response)
    }

    async fn complete_with_metadata(&self, request: Request) -> Result<crate::Completion> {
        self.complete_attempts(request, None).await
    }

    async fn complete_observed(
        &self,
        request: Request,
        observer: std::sync::Arc<dyn crate::AttemptObserver>,
    ) -> Result<crate::Completion> {
        self.complete_attempts(request, Some(observer)).await
    }

    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        self.stream_attempts(request, None).await
    }

    async fn stream_observed(
        &self,
        request: Request,
        observer: std::sync::Arc<dyn crate::AttemptObserver>,
    ) -> Result<ResponseStream> {
        self.stream_attempts(request, Some(observer)).await
    }
}

impl Retrying {
    async fn complete_attempts(
        &self,
        request: Request,
        observer: Option<std::sync::Arc<dyn crate::AttemptObserver>>,
    ) -> Result<crate::Completion> {
        let mut request = self.as_accepted(request);
        let mut attempt = 1;
        let mut credit_retried = false;
        loop {
            let answer = match &observer {
                Some(observer) => self.inner.complete_observed(request.clone(), observer.clone()).await,
                None => self.inner.complete_with_metadata(request.clone()).await,
            };
            match answer {
                Err(e)
                    if worth_asking_again(&e) && self.wait_before(attempt, &e, observer.as_deref()).await =>
                {
                    attempt += 1
                }
                Err(e) if self.drop_the_effort(&e, &mut request) => continue,
                Err(e) if self.ask_for_less_output(&e, &mut request) => continue,
                Err(e) if use_credit_limit(&e, &mut request, &mut credit_retried) => continue,
                answer => return answer,
            }
        }
    }

    async fn stream_attempts(
        &self,
        request: Request,
        observer: Option<std::sync::Arc<dyn crate::AttemptObserver>>,
    ) -> Result<ResponseStream> {
        use futures_util::StreamExt;
        let requested = request.effort;
        let mut request = self.as_accepted(request);
        let mut attempt = 1;
        let mut credit_retried = false;
        let mut stream =
            self.open_stream(&mut request, requested, &observer, &mut attempt, &mut credit_retried).await?;
        let owner = self.clone();
        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let mut delivered = false;
                let mut refused = None;
                while let Some(delta) = stream.next().await {
                    match delta {
                        Ok(delta) => {
                            delivered |= !matches!(delta, crate::Delta::Dispatch(_) | crate::Delta::Effort(_));
                            yield delta;
                        }
                        Err(error) => {
                            if !delivered && use_credit_limit(&error, &mut request, &mut credit_retried) {
                                refused = Some((error, false));
                                break;
                            }
                            if !delivered && worth_asking_again(&error) {
                                refused = Some((error, true));
                                break;
                            }
                            Err(error)?;
                        }
                    }
                }
                let Some((error, wait)) = refused else { break };
                // Release the failed stream's endpoint permit and physical
                // attempt before waiting or opening its replacement.
                drop(stream);
                if wait {
                    if !owner.wait_before(attempt, &error, observer.as_deref()).await {
                        Err(error)?;
                    }
                    attempt += 1;
                }
                stream = owner.open_stream(&mut request, requested, &observer, &mut attempt, &mut credit_retried).await?;
            }
        }))
    }

    async fn open_stream(
        &self,
        request: &mut Request,
        requested: Option<crate::Effort>,
        observer: &Option<std::sync::Arc<dyn crate::AttemptObserver>>,
        attempt: &mut u32,
        credit_retried: &mut bool,
    ) -> Result<ResponseStream> {
        use futures_util::StreamExt;
        loop {
            let answer = match observer {
                Some(observer) => self.inner.stream_observed(request.clone(), observer.clone()).await,
                None => self.inner.stream(request.clone()).await,
            };
            match answer {
                Ok(stream) => {
                    let stream: ResponseStream = match self.inner.dispatch_identity() {
                        Some(identity) => Box::pin(
                            futures_util::stream::once(async { Ok(crate::Delta::Dispatch(identity)) })
                                .chain(stream),
                        ),
                        None => stream,
                    };
                    let Some(requested) = requested else { return Ok(stream) };
                    // Use this attempt's request, not shared refusal state: a
                    // concurrent call could have learned a different answer.
                    let applied = match request.effort {
                        Some(effort) => self.inner.effort_use_for(effort, request),
                        None => crate::EffortUse::Omitted { reason: "endpoint refused the effort parameter" },
                    };
                    let report = crate::EffortReport { provider: self.inner.id().into(), requested, applied };
                    return Ok(Box::pin(
                        futures_util::stream::once(async { Ok(crate::Delta::Effort(report)) }).chain(stream),
                    ));
                }
                Err(e)
                    if worth_asking_again(&e)
                        && self.wait_before(*attempt, &e, observer.as_deref()).await =>
                {
                    *attempt += 1
                }
                Err(e) if self.drop_the_effort(&e, request) => continue,
                Err(e) if self.ask_for_less_output(&e, request) => continue,
                Err(e) if use_credit_limit(&e, request, credit_retried) => continue,
                answer => return answer,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    #[test]
    fn zai_balance_requires_a_documented_structured_refusal() {
        for code in [serde_json::json!("1113"), serde_json::json!(1113)] {
            let body = serde_json::json!({"error":{"type":"rate_limit_error","code":code,
                "message":"[1113][Insufficient balance or no resource package. Please recharge.]"}})
            .to_string();
            let error = LlmError::Status { status: 429, body, retry_after: Some(Duration::from_secs(120)) };
            assert!(out_of_credit(&error));
            assert!(!worth_asking_again(&error), "a Retry-After cannot add balance");
        }
    }

    #[test]
    fn zai_temporary_windows_and_unproven_bodies_keep_transient_recovery() {
        let message = "Insufficient balance or no resource package. Please recharge.";
        let mut bodies = Vec::new();
        for code in ["1302", "1305", "1308", "1310", "1313", "1316", "1317", "1318", "1319", "1320", "1321"] {
            bodies.push(serde_json::json!({"error":{"code":code,"message":message}}).to_string());
        }
        bodies.extend([
            serde_json::json!({"error":{"code":"1113","message":"Rate limit reached; resets soon."}})
                .to_string(),
            serde_json::json!({"error":{"message":message}}).to_string(),
            serde_json::json!({"code":"1113","message":message}).to_string(),
            serde_json::json!({"error":{"code":"11130","message":message}}).to_string(),
            serde_json::json!({"error":{"code":"1113","message":message,"padding":"x".repeat(16*1024)}})
                .to_string(),
            format!("not JSON: 1113 {message}"),
        ]);
        for body in bodies {
            let error = LlmError::Status { status: 429, body, retry_after: None };
            assert!(!out_of_credit(&error), "{error}");
            assert!(worth_asking_again(&error), "unproven account failure must retain retry eligibility");
        }
        let body = serde_json::json!({"error":{"code":"1113","message":message}}).to_string();
        let other_status = LlmError::Status { status: 503, body, retry_after: None };
        assert!(!out_of_credit(&other_status));
        assert!(worth_asking_again(&other_status));
    }

    #[derive(Default)]
    struct WaitLog {
        waits: Mutex<Vec<crate::RetryWait>>,
        starts: std::sync::atomic::AtomicU32,
    }

    struct ObservedAttempt;
    impl crate::Attempt for ObservedAttempt {
        fn finish(&mut self, _: crate::AttemptStatus, _: &crate::AttemptFacts) -> Result<()> {
            Ok(())
        }
    }

    impl crate::AttemptObserver for WaitLog {
        fn waiting_to_retry(&self, wait: crate::RetryWait) {
            assert_eq!(self.starts.load(std::sync::atomic::Ordering::Relaxed), 1);
            self.waits.lock().unwrap().push(wait);
        }
        fn start(&self, _: Option<&crate::Dispatch>) -> Result<Box<dyn crate::Attempt>> {
            self.starts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Box::new(ObservedAttempt))
        }
    }

    #[test]
    fn positive_jitter_reaches_both_endpoints_without_undercutting_backoff() {
        for seconds in [1, 2, 4, 30] {
            let floor = Duration::from_secs(seconds);
            let ceiling = floor + floor / 4;
            assert_eq!(jittered_wait(floor, 0), floor);
            assert_eq!(jittered_wait(floor, u32::MAX), ceiling);
            let middle = jittered_wait(floor, u32::MAX / 2);
            assert!(middle > floor && middle < ceiling);
        }
        assert_eq!(jittered_wait(Duration::ZERO, u32::MAX), Duration::ZERO);
        assert_eq!(jittered_wait(Duration::from_nanos(1), u32::MAX), Duration::from_nanos(1));
    }

    #[test]
    fn jitter_never_crosses_the_advice_ceiling_or_shortens_a_larger_custom_minimum() {
        let cap = Duration::from_secs(120);
        let near = cap - Duration::from_millis(100);
        assert_eq!(jittered_wait(near, 0), near);
        assert_eq!(jittered_wait(near, u32::MAX), cap);
        for floor in [cap, cap + Duration::from_nanos(1), Duration::MAX] {
            for sample in [0, 1, u32::MAX / 2, u32::MAX] {
                assert_eq!(jittered_wait(floor, sample), floor);
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Complete,
        StreamOpen,
        FirstDelta,
        Partial,
        Completion,
        Terminal,
    }

    struct Timed {
        mode: Mode,
        calls: Arc<Mutex<Vec<Instant>>>,
    }

    impl Timed {
        fn called(&self) -> usize {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Instant::now());
            calls.len()
        }

        fn refusal(&self) -> LlmError {
            LlmError::Status {
                status: if matches!(self.mode, Mode::Terminal) { 400 } else { 503 },
                body: "owned refusal".into(),
                retry_after: None,
            }
        }
    }

    #[async_trait]
    impl Provider for Timed {
        fn id(&self) -> &str {
            "owned-jitter"
        }
        fn context_window(&self) -> usize {
            8192
        }

        async fn complete(&self, _: Request) -> Result<Response> {
            if self.called() == 1 {
                return Err(self.refusal());
            }
            Ok(Response {
                message: crate::Message::assistant("done"),
                stop_reason: crate::StopReason::EndTurn,
                usage: crate::Usage::default(),
                model: "owned".into(),
            })
        }

        async fn stream(&self, _: Request) -> Result<ResponseStream> {
            if self.called() == 1 {
                if matches!(self.mode, Mode::StreamOpen | Mode::Terminal) {
                    return Err(self.refusal());
                }
                let mut deltas = Vec::new();
                if matches!(self.mode, Mode::Partial) {
                    deltas.push(Ok(crate::Delta::Text("partial".into())));
                }
                if matches!(self.mode, Mode::Completion) {
                    deltas.push(Ok(crate::Delta::ResponseMetadata {
                        usage_reported: false,
                        completion_confirmed: true,
                    }));
                }
                deltas.push(Err(self.refusal()));
                return Ok(Box::pin(futures_util::stream::iter(deltas)));
            }
            Ok(Box::pin(futures_util::stream::iter([
                Ok(crate::Delta::Text("done".into())),
                Ok(crate::Delta::Done {
                    stop_reason: crate::StopReason::EndTurn,
                    usage: crate::Usage::default(),
                    model: "owned".into(),
                }),
            ])))
        }
    }

    fn fixture(mode: Mode) -> (Retrying, Arc<Mutex<Vec<Instant>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut retry = Retrying::new(Box::new(Timed { mode, calls: calls.clone() }));
        retry.sample_jitter = || u32::MAX;
        (retry, calls)
    }

    #[tokio::test]
    async fn real_retry_paths_apply_injected_jitter_before_the_next_physical_call() {
        for mode in [Mode::Complete, Mode::StreamOpen, Mode::FirstDelta] {
            let (retry, calls) = fixture(mode);
            let observer = Arc::new(WaitLog::default());
            if matches!(mode, Mode::Complete) {
                assert_eq!(
                    retry
                        .complete_observed(Request::new(Vec::new()), observer.clone())
                        .await
                        .unwrap()
                        .response
                        .message
                        .content,
                    "done"
                );
            } else {
                let mut stream =
                    retry.stream_observed(Request::new(Vec::new()), observer.clone()).await.unwrap();
                let mut text = String::new();
                while let Some(delta) = stream.next().await {
                    if let crate::Delta::Text(value) = delta.unwrap() {
                        text.push_str(&value);
                    }
                }
                assert_eq!(text, "done");
            }
            let calls = calls.lock().unwrap();
            assert_eq!(
                *observer.waits.lock().unwrap(),
                vec![crate::RetryWait {
                    delay: Duration::from_millis(1250),
                    next_attempt: 2,
                    status: Some(503),
                }]
            );
            assert_eq!(observer.starts.load(std::sync::atomic::Ordering::Relaxed), 2);
            assert_eq!(calls.len(), 2, "the actual provider must be refused and retried");
            assert!(
                calls[1].duration_since(calls[0]) >= Duration::from_millis(1250),
                "the first 1-second backoff must include its injected 250 ms jitter"
            );
        }
    }

    #[tokio::test]
    async fn terminal_errors_partial_answers_and_completion_evidence_never_sample_or_replay() {
        for mode in [Mode::Terminal, Mode::Partial, Mode::Completion] {
            let (mut retry, calls) = fixture(mode);
            let observer = Arc::new(WaitLog::default());
            retry.sample_jitter = || panic!("an ineligible retry must not sample randomness");
            if matches!(mode, Mode::Terminal) {
                assert!(retry.complete_observed(Request::new(Vec::new()), observer.clone()).await.is_err());
            } else {
                let mut stream =
                    retry.stream_observed(Request::new(Vec::new()), observer.clone()).await.unwrap();
                let first = stream.next().await.unwrap().unwrap();
                match mode {
                    Mode::Partial => assert!(matches!(first, crate::Delta::Text(text) if text == "partial")),
                    Mode::Completion => assert!(matches!(
                        first,
                        crate::Delta::ResponseMetadata { completion_confirmed: true, .. }
                    )),
                    _ => unreachable!(),
                }
                assert!(matches!(stream.next().await, Some(Err(LlmError::Status { status: 503, .. }))));
                assert!(stream.next().await.is_none());
            }
            assert_eq!(calls.lock().unwrap().len(), 1);
            assert!(observer.waits.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn exhausted_attempts_do_not_add_a_wait_or_sample() {
        let (mut retry, _) = fixture(Mode::Complete);
        retry.sample_jitter = || panic!("exhaustion must not sample randomness");
        let error = LlmError::Status { status: 503, body: "owned".into(), retry_after: None };
        let observer = WaitLog::default();
        assert!(!retry.wait_before(ATTEMPTS, &error, Some(&observer)).await);
        assert!(observer.waits.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dropping_an_announced_wait_does_not_admit_the_next_attempt() {
        let (retry, calls) = fixture(Mode::Complete);
        let observer = Arc::new(WaitLog::default());
        let mut pending = Box::pin(retry.complete_observed(Request::new(Vec::new()), observer.clone()));
        assert!(futures_util::poll!(&mut pending).is_pending());
        assert_eq!(observer.waits.lock().unwrap().len(), 1, "the future reached its real retry sleep");
        drop(pending);
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(observer.starts.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
