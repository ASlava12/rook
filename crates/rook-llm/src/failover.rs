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
//! What counts is one thing said four ways: this endpoint cannot serve this
//! request. It could not be reached; it has no money left; it is overloaded or
//! broken and has already spent its retries; or it took the connection and
//! then said nothing at all, which is what a hung API looks like from here.
//!
//! And not a request that is wrong. A 400, a 401, a model that is not there —
//! those answer the same from every endpoint, and moving to the next hides the
//! message that says what to fix. Where the next one is a paid gateway, it
//! hides it expensively.

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
        // With the time it comes back. "For now" leaves whoever reads it
        // wondering whether to wait or to go and fix something, which is the
        // one thing the line is there to answer.
        tracing::warn!(
            "{name} is not answering, so it is out of the rotation for the next {}s: {why}",
            RESTED.as_secs()
        );
    }
}

/// Put an endpoint back in the rotation now, or all of them.
///
/// The waiting answers the case where a laptop moved networks and nobody
/// noticed. This answers the other one: somebody has just topped up an account
/// or started a server, and wants to know whether it worked — not in a minute,
/// now. Waiting out a timer to find out is the same as not being told.
pub fn answering_again(name: Option<&str>) {
    let mut held = missing().lock().unwrap_or_else(|e| e.into_inner());
    match name {
        Some(one) => {
            held.remove(one);
        }
        None => held.clear(),
    }
}

/// Which endpoints are out, and how long each has left.
///
/// For a person asking what the agent is doing, and for the command that puts
/// them back — a listing that says only "some are out" is one nobody can act
/// on.
pub fn not_answering() -> Vec<(String, Duration)> {
    let held = missing().lock().unwrap_or_else(|e| e.into_inner());
    held.iter()
        .filter_map(|(name, since)| RESTED.checked_sub(since.elapsed()).map(|left| (name.clone(), left)))
        .collect()
}

/// Note that it answered, and say so if it had been out.
fn note_answering(name: &str) {
    let mut held = missing().lock().unwrap_or_else(|e| e.into_inner());
    if held.remove(name).is_some() {
        tracing::info!("{name} is answering again");
    }
}

/// Whether this answer means to ask somebody else.
///
/// The statuses are the same list the retry wrapper waits out, and that is not
/// a coincidence: each candidate carries its own retries, so an error reaching
/// here has already been given its chances on the endpoint that gave it. An
/// empty wallet skips the waiting entirely — there is nothing for a backoff to
/// wait for — which is why it is asked about before the status is looked at,
/// and why it has to be: Anthropic sends it as a 400 that every other rule
/// would read as a request that is wrong.
fn somewhere_else(error: &LlmError) -> bool {
    match error {
        LlmError::Unreachable { .. } => true,
        // It accepted the connection and then said nothing for the whole
        // patience, which already allows for the size of the prompt. A local
        // server loading a large model looks like this too and is put out for
        // only a minute, by which time it has loaded.
        LlmError::NeverAnswered { .. } => true,
        _ if crate::retry::out_of_credit(error) => true,
        LlmError::Status { status, .. } => {
            matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529)
                && !crate::retry::names_a_wrong_request(error)
        }
        _ => false,
    }
}

/// Which of several endpoints to ask first, when more than one would answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    /// The order the configuration gave.
    ///
    /// What a turn wants. It was pointed at a model, and the rest are there for
    /// when that one is not — moving a turn between endpoints part-way through
    /// throws away the cached prefix of every request it has sent, and hands
    /// the next step to a model that did not write the last one.
    AsConfigured,
    /// Whichever has room right now, and the configured order between equals.
    ///
    /// What an errand wants. It is bounded work to get through, it has no
    /// conversation worth caching, and there is nothing to be said for queueing
    /// behind a turn on one endpoint while another sits idle.
    WhicheverIsFree,
}

/// Several endpoints, of which the first that answers is used.
pub(crate) struct Failover {
    /// Never empty: the caller has one it could have used on its own, and this
    /// only exists because there is more than that.
    candidates: Vec<Box<dyn Provider>>,
    prefer: Prefer,
}

impl Failover {
    pub(crate) fn new(candidates: Vec<Box<dyn Provider>>, prefer: Prefer) -> Self {
        Self { candidates, prefer }
    }

    /// The ones worth asking, preferred first.
    ///
    /// Those not known to be missing, and if that is none of them, all of them
    /// anyway. Sixty seconds of refusing to try is the right answer for a
    /// second endpoint and the wrong one for the last: a blip would otherwise
    /// leave the agent with nothing to talk to while the machine was back.
    fn worth_asking(&self) -> Vec<&dyn Provider> {
        let all = || self.candidates.iter().map(Box::as_ref);
        let mut answering: Vec<&dyn Provider> = all().filter(|p| !is_missing(p.id())).collect();
        if answering.is_empty() {
            answering = all().collect();
        }
        if self.prefer == Prefer::WhicheverIsFree {
            // Stable, so the configured order survives as the tiebreak: two
            // endpoints with the same room are still asked in the order
            // somebody wrote them in.
            //
            // An endpoint nothing has claimed a limit for sorts with the
            // freest, because that is what it is — no limit is not busy, and
            // reading it as busy would send every errand to the one server that
            // had bothered to say how much it could take.
            answering.sort_by_key(|p| std::cmp::Reverse(crate::limit::free_at(p.id()).unwrap_or(usize::MAX)));
        }
        answering
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
                Err(why) if somewhere_else(&why) => {
                    note_missing($provider.id(), &why.to_string());
                    refused.push(format!("{}: {why}", $provider.id()));
                }
                // This endpoint's answer, and it is the answer.
                Err(theirs) => return Err(theirs),
            }
        }
        Err(LlmError::Other(format!(
            "none of the endpoints configured for this answered:\n  {}\n\
             They are tried again in {}s, or at once with `rook models --recheck`.",
            refused.join("\n  "),
            RESTED.as_secs()
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

    /// The shapes of refusal that matter, spelled as the providers spell them.
    enum Says {
        Answer,
        Unreachable,
        Status(u16, &'static str),
        Silent,
    }

    const REFUSED: Says = Says::Status(401, r#"{"error":{"type":"invalid_api_key"}}"#);
    /// OpenAI's empty wallet: a code, and a status the retry wrapper would
    /// otherwise wait out.
    const NO_QUOTA: Says = Says::Status(429, r#"{"error":{"code":"insufficient_quota"}}"#);
    /// Anthropic's, which every other rule here reads as a request that is
    /// wrong: the type is `invalid_request_error` and the reason is prose.
    const NO_CREDIT: Says = Says::Status(
        400,
        r#"{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}"#,
    );
    const OVERLOADED: Says = Says::Status(503, r#"{"type":"overloaded_error"}"#);
    /// A request that is wrong, in the same envelope as the empty wallet above,
    /// so the two are told apart by what they say and not by their shape.
    const TOO_LARGE: Says =
        Says::Status(400, r#"{"type":"invalid_request_error","message":"max_tokens is too large"}"#);

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
                Says::Status(status, body) => {
                    Err(LlmError::Status { status, body: body.into(), retry_after: None })
                }
                Says::Silent => Err(LlmError::NeverAnswered { secs: 90, tokens: 4_000 }),
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

    /// Whether the first endpoint's answer sent the request on to the second.
    fn handed_over(first: Says) -> bool {
        let (refusing, _) = saying("first", first);
        let (second, reached) = saying("second", Says::Answer);
        let _ = asking(&Failover::new(vec![refusing, second], Prefer::AsConfigured));
        reached.load(Ordering::Relaxed) == 1
    }

    #[test]
    fn an_endpoint_that_cannot_be_reached_hands_over_to_the_next() {
        assert!(handed_over(Says::Unreachable));
    }

    /// Both spellings of it. Asking again does not add money, and another
    /// endpoint may have some — but Anthropic says so in a 400 whose type is
    /// `invalid_request_error`, which every other rule here reads as a request
    /// that is wrong and refuses to move on from.
    #[test]
    fn an_endpoint_with_no_money_left_hands_over_however_it_says_so() {
        assert!(handed_over(NO_QUOTA), "the code OpenAI sends");
        assert!(handed_over(NO_CREDIT), "the sentence Anthropic sends");
    }

    #[test]
    fn an_overloaded_endpoint_hands_over_once_it_has_spent_its_attempts() {
        assert!(handed_over(OVERLOADED));
    }

    /// It took the connection and then said nothing for the whole patience,
    /// which already allowed for the size of the prompt. That is a hung API.
    #[test]
    fn an_endpoint_that_answers_with_silence_hands_over() {
        assert!(handed_over(Says::Silent));
    }

    /// The one that matters most. A wrong key is a configuration that is wrong,
    /// and moving to the next endpoint on it hides that — where the next
    /// endpoint is a paid gateway, expensively.
    #[test]
    fn a_key_that_is_wrong_is_the_answer_rather_than_a_reason_to_try_elsewhere() {
        let (refusing, _) = saying("refusing", REFUSED);
        let (here, asked) = saying("here", Says::Answer);

        let why = asking(&Failover::new(vec![refusing, here], Prefer::AsConfigured)).unwrap_err();

        assert!(matches!(why, LlmError::Status { status: 401, .. }), "{why}");
        assert_eq!(asked.load(Ordering::Relaxed), 0, "the second endpoint was never asked");
    }

    /// And the same envelope as an empty wallet, to show the two are told apart
    /// by what they say rather than by their shape.
    #[test]
    fn a_request_that_is_wrong_is_wrong_at_every_endpoint() {
        assert!(!handed_over(TOO_LARGE));
    }

    #[test]
    fn with_nothing_answering_the_error_names_every_endpoint_tried() {
        let (first, _) = saying("gone", Says::Unreachable);
        let (second, _) = saying("also-gone", Says::Unreachable);
        let names = [first.id().to_string(), second.id().to_string()];

        let why = asking(&Failover::new(vec![first, second], Prefer::AsConfigured)).unwrap_err().to_string();

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
        let over = Failover::new(vec![gone, here], Prefer::AsConfigured);

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

        assert_eq!(Failover::new(vec![large, small], Prefer::AsConfigured).context_window(), 8_192);
    }

    /// An errand has no conversation to keep, so there is nothing to be said
    /// for queueing it behind a turn on one endpoint while another sits idle.
    ///
    /// Asserted on the order rather than by sending a request, because a
    /// candidate with no room left would block rather than answer — a test that
    /// proved the point by hanging would prove it once and cost the suite for
    /// ever after.
    #[test]
    fn an_errand_prefers_whichever_endpoint_has_room() {
        let (full, _) = saying("full", Says::Answer);
        let (room, _) = saying("room", Says::Answer);
        let (full_id, room_id) = (full.id().to_string(), room.id().to_string());
        // Claiming a limit is what puts a name in the register at all, and none
        // left is the plainest way to be busy without holding anything open.
        let full: Box<dyn Provider> = Box::new(crate::limit::Limited::new(full, &full_id, 0));
        let room: Box<dyn Provider> = Box::new(crate::limit::Limited::new(room, &room_id, 4));

        let asked = Failover::new(vec![full, room], Prefer::WhicheverIsFree);
        let order: Vec<&str> = asked.worth_asking().iter().map(|p| p.id()).collect();

        assert_eq!(order, [room_id.as_str(), full_id.as_str()], "the one with room goes first");
    }

    /// And the precondition for the test above: without the preference the
    /// order is the one the file gave, so what moved it was the room and not
    /// something else about these two.
    #[test]
    fn a_turn_stays_in_the_order_it_was_configured_in_however_busy() {
        let (full, _) = saying("full", Says::Answer);
        let (room, _) = saying("room", Says::Answer);
        let (full_id, room_id) = (full.id().to_string(), room.id().to_string());
        let full: Box<dyn Provider> = Box::new(crate::limit::Limited::new(full, &full_id, 0));
        let room: Box<dyn Provider> = Box::new(crate::limit::Limited::new(room, &room_id, 4));

        let asked = Failover::new(vec![full, room], Prefer::AsConfigured);
        let order: Vec<&str> = asked.worth_asking().iter().map(|p| p.id()).collect();

        assert_eq!(order, [full_id.as_str(), room_id.as_str()], "moving a turn costs its cache");
    }

    /// The waiting answers a laptop that moved networks and nobody noticed.
    /// This answers the other case: the server has just been started, and the
    /// person wants to know now rather than after a timer they cannot see.
    #[test]
    fn an_endpoint_put_back_by_hand_is_asked_again_at_once() {
        let (gone, tried) = saying("gone", Says::Unreachable);
        let (here, _) = saying("here", Says::Answer);
        let name = gone.id().to_string();
        let over = Failover::new(vec![gone, here], Prefer::AsConfigured);

        asking(&over).expect("the second answered");
        asking(&over).expect("and again");
        assert_eq!(tried.load(Ordering::Relaxed), 1, "it was out of the rotation");
        assert!(
            crate::not_answering().iter().any(|(out, left)| *out == name && !left.is_zero()),
            "and says so, with the time it has left"
        );

        crate::answering_again(None);
        asking(&over).expect("still answered");

        assert_eq!(tried.load(Ordering::Relaxed), 2, "and is asked again once put back");
    }
}
