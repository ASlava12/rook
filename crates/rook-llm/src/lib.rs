//! Model providers.
//!
//! One trait, and one HTTP implementation that speaks the OpenAI chat-completions
//! dialect. That dialect is what Ollama, LM Studio, llama.cpp's server, vLLM,
//! OpenRouter, Together and OpenAI itself all accept, so a single implementation
//! covers local and hosted models alike. Providers with their own wire format
//! (Anthropic's Messages API, Google's generateContent) get their own impls of
//! the same trait.
//!
//! Everything here is provider-agnostic on purpose: the agent loop must never
//! contain a branch on which vendor is answering.

#![warn(clippy::string_slice)]
//
// Indexing a `&str` by a computed byte panics when the byte is inside a
// character, and under `panic = "abort"` that is the whole process. One did:
// `start byte index 8185 is not a char boundary; it is inside 'т'` ended a
// daemon and the half-hour turn it was holding. CLAUDE.md had the rule and
// nothing asked the compiler, which knows the types and can tell a `String`
// from a `Vec` where a guard reading the text never could.
//
// On here rather than for the whole workspace, because the workspace slices its
// own ASCII in eighty places and a warning allowed eighty times is decoration.
// This is where the text comes from outside — a model, a server, the web — and
// so where the characters wider than a byte actually arrive. A slice here
// either uses an index the code just found, and says so, or it is a crash
// waiting for somebody who does not write in English.
/// How long to wait for the first token of a reply, given how much was sent.
///
/// The configured patience is the right question for every chunk after the
/// first and the wrong one for the first: a model has to read the whole prompt
/// before it can say anything, and reading is work in proportion to the prompt.
/// A local model filling a two-hundred-thousand-token context is silent for
/// minutes by design — measured at about 290 tokens a second on the machine
/// this was written against — so ninety seconds of patience gave up on work
/// that would have finished, threw away the turn that had done it, and read
/// from outside as a hang.
///
/// The asymmetry decides the number. Giving up early destroys everything the
/// turn has done; waiting too long costs a wait that the window names to the
/// second and `^c` ends. So the floor is pessimistic — a third of what was
/// measured — and every step pays it, since a turn re-reads its whole context
/// after each tool call unless the provider cached it.
pub fn first_token_patience(idle: std::time::Duration, prompt_bytes: usize) -> std::time::Duration {
    /// Tokens a second, well under anything measured, because the cost of being
    /// wrong is not symmetric.
    const SLOWEST_PREFILL: u64 = 100;
    /// English prose and code both sit near this, and being out by half costs a
    /// wait rather than a failure.
    const BYTES_A_TOKEN: usize = 4;

    let tokens = (prompt_bytes / BYTES_A_TOKEN) as u64;
    idle.saturating_add(std::time::Duration::from_secs(tokens / SLOWEST_PREFILL))
}

/// Server-sent event frames, reassembled from transport chunks.
///
/// One of these rather than four. Each provider had its own copy of the same
/// dozen lines, so each carried the same two bugs, and fixing one would have
/// left the other two to be found the same way this one was: a daemon gone,
/// half an hour of work with it, and a session that "stopped working".
#[derive(Default)]
pub struct Frames {
    /// Whole characters, waiting for their frame to end.
    text: String,
    /// The tail of a chunk that is not yet a whole character.
    partial: Vec<u8>,
    /// How far into `text` the search for a separator has already looked.
    scanned: usize,
}

impl Frames {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one transport chunk.
    ///
    /// A character can straddle two chunks, and decoding each chunk on its own
    /// with `from_utf8_lossy` turned the two halves into two replacement
    /// characters — so a model streaming Russian lost a letter wherever the
    /// network happened to cut. Bytes are held until they are a whole
    /// character; at most three ever are, since a UTF-8 sequence is at most
    /// four long.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.partial.extend_from_slice(chunk);
        // Until the end, not once: a chunk can hold a bad byte and then a
        // kilobyte of perfectly good text, and stopping at the bad byte left
        // the rest of it sitting in `partial` where nothing would ever read it.
        // Each turn of this either finishes or drops at least one byte.
        loop {
            let e = match std::str::from_utf8(&self.partial) {
                Ok(all) => {
                    self.text.push_str(all);
                    self.partial.clear();
                    return;
                }
                Err(e) => e,
            };
            let whole = e.valid_up_to();
            self.text.push_str(&String::from_utf8_lossy(&self.partial[..whole]));
            match e.error_len() {
                // A truncated character: the next chunk finishes it.
                None => {
                    self.partial.drain(..whole);
                    return;
                }
                // Not UTF-8 at all. Held, it would stall the stream forever
                // waiting for a byte that cannot make it valid.
                Some(bad) => {
                    self.text.push(char::REPLACEMENT_CHARACTER);
                    self.partial.drain(..whole + bad);
                }
            }
        }
    }

    /// How much is being held, for the caller's frame cap.
    pub fn held(&self) -> usize {
        self.text.len()
    }

    /// The frames that are now complete, in order.
    ///
    /// The separator is searched for in bytes, because that is what it is. As
    /// text it panicked: the search resumes one byte back from the end of the
    /// last chunk, to catch a `\n\n` split across two of them, and one byte back
    /// from a Cyrillic letter is inside it — `start byte index 8185 is not a
    /// char boundary`. Release builds abort on a panic, so that took the whole
    /// daemon and the turn it was running.
    pub fn ready(&mut self) -> Vec<String> {
        let mut done = Vec::new();
        while let Some(offset) =
            self.text.as_bytes()[self.scanned..].windows(2).position(|pair| pair == b"\n\n")
        {
            let end = self.scanned + offset;
            self.scanned = 0;
            done.push(self.text.drain(..end + 2).collect());
        }
        self.scanned = self.text.len().saturating_sub(1);
        done
    }
}

pub mod anthropic;
pub mod google;
pub mod openai;
pub mod prompted;
pub mod retry;
pub mod stream;
pub mod types;

pub use stream::{Assembler, Delta, ResponseStream};
pub use types::*;

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("cannot reach {endpoint}: {detail}\n{}", advice(.endpoint, .detail))]
    Unreachable { endpoint: String, detail: String },
    #[error("provider returned {status}: {body}{}", what_to_try(*.status, .body))]
    Status {
        status: u16,
        body: String,
        /// What the provider said about when to come back, if it said. A rate
        /// limiter that answers `Retry-After: 30` has given the only number
        /// worth waiting: guessing a shorter one spends the tries and ends the
        /// turn on a refusal the server had already told us how to avoid.
        #[allow(dead_code)]
        retry_after: Option<std::time::Duration>,
    },
    #[error("could not parse the provider's response: {0}")]
    Decode(String),
    #[error(
        "this turn needs about {used} tokens and the model's window holds {window}.\n\
         Compacting cannot help when a single message is the problem — put the text in a file \
         and ask for it to be read, which is paged, or point `[agent] model` at a model with a \
         larger window."
    )]
    ContextOverflow { used: usize, window: usize },
    #[error(
        "no provider called {name:?}. `[agent] model` is `provider/model`, and provider is one of: {}",
        PROVIDERS.join(", ")
    )]
    UnknownProvider { name: String },
    #[error("the model stopped sending for {secs}s; giving up on the stream")]
    Stalled { secs: u64 },
    /// Told apart from `Stalled` because they are different questions. A stream
    /// that broke halfway is a broken stream; one that never began may be a
    /// model still reading, and the wait for it already allows for that — so
    /// the size of the context is the one explanation this rules out, and
    /// saying so is what stops the next hour going into it. A turn spent two
    /// more attempts and twenty minutes proving an environment was fine, on a
    /// message that did not say which of the two had happened.
    #[error(
        "the model sent nothing at all in {secs}s. The wait already allowed for reading a prompt \
         of about {tokens} tokens, so the size of the context is not the reason: the server is \
         loading a model, overloaded, or gone"
    )]
    NeverAnswered { secs: u64, tokens: usize },
    #[error("no model {model:?} on the server at {endpoint} — {}", offers(available))]
    NoSuchModel { model: String, endpoint: String, available: Vec<String> },
    #[error("{0}")]
    Other(String),
}

/// How long the provider asked us to wait, in seconds.
///
/// Only the delta-seconds form: the HTTP-date form is legal and nobody sends
/// it, and a date needs a clock and a parser to disagree about. A header that
/// is not a number is no answer, which is what `None` means here.
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let said = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let seconds: u64 = said.trim().parse().ok()?;
    // A provider that says to come back tomorrow is a provider to give up on
    // rather than to sleep through: a turn nobody is watching is still a turn
    // somebody is paying for.
    (seconds <= MOST_PATIENCE_SECS).then(|| std::time::Duration::from_secs(seconds))
}

/// Beyond this, the answer is not "later" in any useful sense.
const MOST_PATIENCE_SECS: u64 = 120;

/// What a person can do about a request this endpoint will not take.
///
/// A 400 is usually the agent's request being wrong, and one shape of wrong is a
/// field the agent added rather than the user: the tool definitions, which some
/// OpenAI-compatible servers refuse outright. Dropping them here would leave an
/// agent that cannot act and does not say why, so the setting that puts them in
/// the prompt instead is named and the choice is left where it belongs.
///
/// The other shape is not the request at all. A local runtime loads a model when
/// it is first asked for one, and a model it cannot load is refused with the same
/// 400 as a malformed request — `Failed to load model "…". Error: Failed to load
/// model.` was the whole of it, printed as provider JSON, with nothing to say
/// whether the name, the machine or the file was at fault. It was the file: a
/// 9B refused to load on a machine that had just loaded a 27B, and the reason
/// was in the server's log and nowhere else.
fn what_to_try(status: u16, body: &str) -> &'static str {
    let said = body.to_ascii_lowercase();
    // Checked before the request is blamed, and at any status: LM Studio says
    // this with a 400 and llama.cpp's server with a 500, and it means the same
    // either way.
    if said.contains("failed to load") {
        return "\nThe server lists this model but could not load it, so the fault is on that side \
                rather than in `[agent] model`: the reason is in the server's own log and not in \
                this reply. A quantisation its build has no kernels for and a download that did \
                not finish are the usual two.";
    }
    let about_tools = ["tool", "function"].iter().any(|word| said.contains(word));
    match status == 400 && about_tools {
        true => {
            "\nThis endpoint may not take tool definitions. `[agent] native_tools = false` \
                 describes them in the prompt and reads the model's answer back instead."
        }
        false => "",
    }
}

/// The answer to "which model, then" is on the same server that just refused, so
/// it is fetched and named rather than left for the user to go and look up.
fn offers(available: &[String]) -> String {
    match available {
        [] => "it has none. Pull one, or point `[agent] model` somewhere else.".into(),
        have => format!("it has {}. Set `[agent] model` to one of them, or pull it.", have.join(", ")),
    }
}

impl LlmError {
    pub fn unreachable(endpoint: &str, source: impl std::error::Error) -> Self {
        Self::Unreachable { endpoint: origin(endpoint), detail: root_cause(&source) }
    }
}

/// What to try, which depends on where the endpoint is *and* on what happened.
///
/// It depended only on the address once, and told the smoke job twice in one
/// run that nothing was listening on a server that was answering every other
/// request — it was busy with a long generation and the connection timed out.
/// Refused, timed out and a name that does not resolve are three different
/// fixes, which is why `root_cause` digs the reason out of the chain; throwing
/// it away here and guessing from the address is how the guess came to
/// contradict the line above it.
fn advice(endpoint: &str, detail: &str) -> String {
    // The whole 127/8 range, not just the usual address: a local server moved
    // off 127.0.0.1 is exactly the case where the wrong advice costs most.
    let local = ["://127.", "localhost", "[::1]", "://0.0.0.0"].iter().any(|h| endpoint.contains(h));
    let said = detail.to_ascii_lowercase();
    if said.contains("timed out") || said.contains("timeout") {
        return match local {
            // Something answered the address or the connection would have been
            // refused, so "start the server" is the opposite of the fix.
            true => "It is listening but did not answer in time — usually a model still \
                     loading, or a server working on another request. Give it a moment, or \
                     point `[agent] model` at something smaller."
                .to_string(),
            false => "It did not answer in time. Check the network — a provider that is \
                      overloaded looks the same from here."
                .to_string(),
        };
    }
    if ["dns", "failed to lookup", "name or service not known", "nodename nor servname"]
        .iter()
        .any(|d| said.contains(d))
    {
        return "That host does not resolve. Check the spelling of the endpoint, and DNS.".to_string();
    }
    match local {
        true => "Nothing is listening there. Start the server, or point `[agent] model` at one \
                 that is running — `rook models` lists what an endpoint offers."
            .to_string(),
        false => "Check the network, and that the provider's API key is set.".to_string(),
    }
}

/// The first of `names` that is set to something. Empty counts as unset: an
/// exported-but-blank variable is the usual way this goes wrong, and a 401 does
/// not say which.
fn required_key(names: &[&str]) -> Result<String> {
    names.iter().find_map(|name| std::env::var(name).ok().filter(|k| !k.trim().is_empty())).ok_or_else(|| {
        let (first, rest) = names.split_first().unwrap_or((&"", &[]));
        let alternatives = match rest {
            [] => String::new(),
            more => format!(" (or {})", more.join(", ")),
        };
        LlmError::Other(format!(
            "{first}{alternatives} is not set. Export it, or set `[agent] model` to a \
                 local provider such as `ollama/…`."
        ))
    })
}

/// What one reply may amount to, streamed or assembled in one piece.
///
/// A frame cap bounds a single SSE event and says nothing about how many of
/// them arrive; a provider that answers without stopping is held only by the
/// request timeout, which bounds the time and not the memory. Generous enough
/// that no real reply meets it — the largest context window in service is
/// smaller than this — so reaching it means the provider is broken.
pub(crate) const MOST_REPLY_BYTES: usize = 32 << 20;

/// How much of a failing endpoint's body is worth repeating back.
const MOST_QUOTED_BYTES: usize = 2000;

/// The whole body, refused as it arrives rather than measured once it is here.
///
/// `base_url` is configuration and the body is whatever is on the other end of
/// it, so "read it all and then look at the length" is a cap that has already
/// been paid by the time it is checked.
pub(crate) async fn whole_text(mut response: reqwest::Response, url: &str) -> Result<String> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(None) => return Ok(String::from_utf8_lossy(&body).into_owned()),
            Ok(Some(chunk)) => {
                body.extend_from_slice(&chunk);
                if body.len() > MOST_REPLY_BYTES {
                    return Err(LlmError::Decode(format!(
                        "{url} sent more than the {MOST_REPLY_BYTES} bytes one reply may be, and \
                         was still sending"
                    )));
                }
            }
            Err(e) => return Err(LlmError::unreachable(url, e)),
        }
    }
}

/// As much of a failed request's body as goes in the message, and no more: an
/// endpoint that answers a 500 with a megabyte of HTML should not cost a
/// megabyte to say so.
pub(crate) async fn quoted_text(mut response: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() <= MOST_QUOTED_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    truncate(&String::from_utf8_lossy(&body), MOST_QUOTED_BYTES)
}

/// A prefix of `text`, cut on a character boundary.
///
/// Shared with `rook-mcp`, which had its own copy. An audit called the two
/// unavoidable — `rook-mcp` was said to have no internal dependencies — and
/// that was already untrue: it depends on this crate, and the same mistaken
/// premise left the server-sent-event reassembly duplicated here too, where it
/// grew a panic that ended a daemon mid-turn.
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let cut = (0..=max).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    // The line above is the boundary search, which is the whole point of this
    // function: `cut` is a boundary because nothing else was accepted.
    #[allow(clippy::string_slice)]
    let head = &text[..cut];
    format!("{head}…")
}

/// The innermost cause. An HTTP client's own message is the url again and never
/// the reason; "Connection refused" and "dns error" are different problems with
/// different fixes, and only the bottom of the chain says which one happened.
fn root_cause(error: &dyn std::error::Error) -> String {
    let mut cause = error;
    while let Some(inner) = cause.source() {
        cause = inner;
    }
    cause.to_string()
}

/// Scheme and authority only: the path a request happened to use is noise, and
/// the same endpoint should read the same however it was reached.
pub(crate) fn origin(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority = rest.split('/').next().unwrap_or(rest);
    match scheme.is_empty() {
        true => authority.to_string(),
        false => format!("{scheme}://{authority}"),
    }
}

pub type Result<T> = std::result::Result<T, LlmError>;

#[async_trait]
pub trait Provider: Send + Sync {
    /// `provider/model`, as written in config.
    fn id(&self) -> &str;

    /// Total context window in tokens. Used for budgeting before a request is
    /// sent, rather than discovering the limit by being rejected.
    fn context_window(&self) -> usize;

    /// Whether the provider accepts tool definitions natively. When false the
    /// agent falls back to prompt-encoded tool calls.
    fn supports_tools(&self) -> bool {
        true
    }

    /// Whether `effort` reaches this model at all.
    ///
    /// Each dialect sends it only to the families documented to take it, which
    /// is right — an unknown field is rejected outright by a strict endpoint,
    /// and guessing fails every request rather than degrading. What was wrong
    /// is that the setting then did nothing and said nothing: a front end
    /// showing `assist/high` in its footer was reporting a knob connected to
    /// nothing. Asked here so a person can be told, rather than inferred twice
    /// by whoever writes the next front end.
    fn takes_effort(&self) -> bool {
        true
    }

    async fn complete(&self, request: Request) -> Result<Response>;

    fn supports_streaming(&self) -> bool {
        false
    }

    /// What this endpoint says it can serve. Empty when it does not say.
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }

    /// Cheapest possible proof that the endpoint is there and answering.
    async fn reachable(&self) -> Result<()> {
        self.models().await.map(|_| ())
    }

    /// Falls back to a one-shot `complete` so every provider is streamable and
    /// callers never branch on whether this one really streams.
    async fn stream(&self, request: Request) -> Result<ResponseStream> {
        let response = self.complete(request).await?;
        let mut deltas = vec![Ok(Delta::Text(response.message.content.clone()))];
        deltas.extend(response.message.tool_calls.iter().cloned().map(|c| Ok(Delta::ToolCall(c))));
        deltas.push(Ok(Delta::Done {
            stop_reason: response.stop_reason,
            usage: response.usage.clone(),
            model: response.model.clone(),
        }));
        Ok(Box::pin(futures_util::stream::iter(deltas)))
    }
}

/// Every dialect a spec can name, in the order they are tried. Beside the match
/// that dispatches on them, and checked against it by a test: a list that has
/// drifted from the code is worse than no list.
pub const PROVIDERS: &[&str] =
    &["anthropic", "claude", "google", "gemini", "openai", "openai-compatible", "ollama", "lmstudio"];

/// Split a `provider/model` spec, e.g. `ollama/qwen3-coder:30b`.
pub fn split_spec(spec: &str) -> (&str, &str) {
    match spec.split_once('/') {
        Some((p, m)) => (p, m),
        None => ("openai-compatible", spec),
    }
}

/// Endpoints and keys come from environment variables, so neither the store nor
/// the config file ever holds a credential.
/// `context_window` overrides the provider's assumed default, which is guesswork
/// for anything self-hosted: a local model may serve 8k or a million.
///
/// Everything built here is wrapped in [`retry::Retrying`], so a rate limit or an
/// overloaded endpoint is waited out rather than ending the turn. Wrapped in one
/// place because every front end goes through here, and a provider built past
/// this function would quietly be the one that gives up.
pub fn from_spec_with(
    spec: &str,
    stream_idle: std::time::Duration,
    context_window: Option<usize>,
) -> Result<Box<dyn Provider>> {
    Ok(Box::new(retry::Retrying::new(build(spec, stream_idle, context_window)?)))
}

/// A key sent over plain http to another machine is a key on the wire.
///
/// Every base here can be pointed elsewhere — `OPENAI_BASE_URL`,
/// `ANTHROPIC_BASE_URL`, `ROOK_LLM_BASE_URL` — and a gateway on plain http is
/// an ordinary thing to run on this machine and not an ordinary thing to send
/// a bearer token to across a network. Loopback is exempt, because that is
/// what every local runtime is; anything else with a key set is refused at
/// startup rather than on the first request, where the key has already gone.
///
/// Traced from goose's "require HTTPS for Snowflake" — see
/// [references/PORTED.md](../../../references/PORTED.md).
fn in_the_clear(base: &str, key: Option<&str>) -> Result<()> {
    if key.is_none() || !base.trim().to_ascii_lowercase().starts_with("http://") {
        return Ok(());
    }
    let host = host_of(base);
    let address = host.trim_matches(['[', ']']);
    let local = host == "localhost"
        || address.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
        || host.is_empty();
    if local {
        return Ok(());
    }
    Err(LlmError::Other(format!(
        "{base} is http and an API key is set, so the key would cross the network in clear text. \
         Use https, or unset the key if {host} does not need one."
    )))
}

/// The host an endpoint names. The brackets an IPv6 host is written in are not
/// part of the address, and `[::1]` parses as nothing at all with them left on,
/// so callers trim them.
fn host_of(base: &str) -> String {
    reqwest::Url::parse(base.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// Whether an endpoint is on this machine or on this network.
///
/// A proxy in the environment is a proxy to the internet — a VPN client's, a
/// company's — and a request to the next desk sent through one comes back as
/// whatever the tunnel makes of an address it cannot route to. Here that was an
/// empty 502 after eighty seconds from an `LMSTUDIO_HOST` two rooms away, which
/// rook reported as `cannot reach … operation timed out`: an answer that sends
/// you to look at the machine that was answering fine all along.
///
/// openclaw fixed this three times, each time wider — loopback, then localhost
/// by name, then "configured local origins" — see
/// [references/PORTED.md](../../../references/PORTED.md). A provider's base URL
/// *is* the configured origin: it is the only address this client ever talks to,
/// so there is no wider target to be careless about.
///
/// A different question from the one [`in_the_clear`] answers, and deliberately
/// broader: a key must not cross even a LAN in clear text, but a request to a
/// LAN has no business going through a proxy.
fn beside_us(base: &str) -> bool {
    let host = host_of(base);
    // Both resolve on this machine or this network, or not at all.
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        // `is_unique_local` and `is_unicast_link_local` are still unstable, and
        // the two prefixes they name are fixed: fc00::/7 and fe80::/10.
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.segments()[0] & 0xfe00 == 0xfc00 || ip.segments()[0] & 0xffc0 == 0xfe80
        }
        // A name that is not an address is reached however the machine reaches
        // names, proxy included.
        Err(_) => false,
    }
}

/// The HTTP client every provider in this crate talks through.
///
/// One function because all three have to answer the same two questions the
/// same way, and three copies of a builder is three places for the answers to
/// drift apart.
fn client_for(base: &str) -> Result<reqwest::Client> {
    init_tls();
    let mut client = reqwest::Client::builder()
        .user_agent(concat!("rook/", env!("CARGO_PKG_VERSION")))
        // A long-running agent turn can legitimately take minutes on a local
        // model; a short default timeout would look like a provider bug.
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(15));
    if beside_us(base) {
        client = client.no_proxy();
    }
    client.build().map_err(|e| LlmError::unreachable(base, e))
}

fn build(
    spec: &str,
    stream_idle: std::time::Duration,
    context_window: Option<usize>,
) -> Result<Box<dyn Provider>> {
    let (provider, model) = split_spec(spec);
    let mut cfg = match provider {
        "ollama" => {
            openai::Config::new(env_or("OLLAMA_HOST", "http://127.0.0.1:11434") + "/v1", None, 32_768)
        }
        "lmstudio" => {
            openai::Config::new(env_or("LMSTUDIO_HOST", "http://127.0.0.1:1234") + "/v1", None, 32_768)
        }
        "anthropic" | "claude" => {
            let key = required_key(&["ANTHROPIC_API_KEY"])?;
            let base = env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com");
            in_the_clear(&base, Some(&key))?;
            let mut config = anthropic::Config::new(base, key, model);
            config.stream_idle_timeout = stream_idle;
            if let Some(window) = context_window {
                config.context_window = window;
            }
            return Ok(Box::new(anthropic::Anthropic::new(spec, model, config)?));
        }
        "google" | "gemini" => {
            let key = required_key(&["GEMINI_API_KEY", "GOOGLE_API_KEY"])?;
            let base = env_or("GEMINI_BASE_URL", "https://generativelanguage.googleapis.com/v1beta");
            in_the_clear(&base, Some(&key))?;
            let mut config = google::Config::new(base, key, model);
            config.stream_idle_timeout = stream_idle;
            if let Some(window) = context_window {
                config.context_window = window;
            }
            return Ok(Box::new(google::Google::new(spec, model, config)?));
        }
        "openai" => {
            let base = env_or("OPENAI_BASE_URL", "https://api.openai.com/v1");
            let key = std::env::var("OPENAI_API_KEY").ok();
            in_the_clear(&base, key.as_deref())?;
            openai::Config::new(base, key, 128_000)
        }
        "openai-compatible" => {
            let base = std::env::var("ROOK_LLM_BASE_URL").ok().filter(|u| !u.trim().is_empty()).ok_or_else(
                || {
                    LlmError::Other(
                        "ROOK_LLM_BASE_URL is not set, and `openai-compatible` has no default \
                         endpoint to fall back to."
                            .into(),
                    )
                },
            )?;
            let key = std::env::var("ROOK_LLM_API_KEY").ok();
            in_the_clear(&base, key.as_deref())?;
            openai::Config::new(base, key, 32_768)
        }
        other => return Err(LlmError::UnknownProvider { name: other.to_string() }),
    };
    cfg.stream_idle_timeout = stream_idle;
    if let Some(window) = context_window {
        cfg.context_window = window;
    }
    Ok(Box::new(openai::OpenAiCompatible::new(spec, model, cfg)?))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// `ring` rather than rustls' default `aws-lc-rs`: the latter needs cmake and a
/// full C toolchain, which is the usual blocker for the FreeBSD target.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The smoke job said it twice in one run, against an ollama that was
    /// answering every other request and merely busy with a long one: `cannot
    /// reach http://127.0.0.1:11434: operation timed out` followed by "Nothing
    /// is listening there. Start the server." The line above it had the answer
    /// and the line below it contradicted it.
    #[test]
    fn what_went_wrong_decides_what_to_try_not_only_where_it_went_wrong() {
        let said = |detail: &str| {
            LlmError::Unreachable { endpoint: "http://127.0.0.1:11434".into(), detail: detail.into() }
                .to_string()
        };

        let busy = said("operation timed out");
        assert!(busy.contains("did not answer in time"), "a timeout is not silence: {busy}");
        assert!(!busy.contains("Nothing is listening"), "and it is running: {busy}");

        let refused = said("Connection refused (os error 61)");
        assert!(refused.contains("Nothing is listening"), "refused is the case that was right: {refused}");

        let remote = LlmError::Unreachable {
            endpoint: "https://api.example".into(),
            detail: "dns error: failed to lookup address information".into(),
        }
        .to_string();
        assert!(remote.contains("does not resolve"), "a name is a third fix again: {remote}");
        assert!(!remote.contains("API key"), "and not the one for a key: {remote}");
    }

    /// A gateway on plain http is an ordinary thing to run on this machine.
    /// Sending it a bearer token across a network is not, and the first
    /// request is too late to say so — by then the key has gone.
    #[test]
    fn a_key_is_refused_over_plain_http_to_another_machine() {
        assert!(in_the_clear("http://127.0.0.1:1234/v1", Some("sk-x")).is_ok(), "loopback is the local case");
        assert!(in_the_clear("http://localhost:8080/v1", Some("sk-x")).is_ok(), "by name as well");
        assert!(in_the_clear("http://[::1]:8080/v1", Some("sk-x")).is_ok(), "and in the other family");
        assert!(in_the_clear("https://gateway.example/v1", Some("sk-x")).is_ok(), "https is what to do");
        assert!(in_the_clear("http://gateway.example/v1", None).is_ok(), "no key, nothing to leak");

        let refused = in_the_clear("http://gateway.example/v1", Some("sk-x")).unwrap_err().to_string();
        assert!(refused.contains("clear text"), "it says what would happen: {refused}");
        assert!(refused.contains("https"), "and what to do instead: {refused}");
        assert!(refused.contains("gateway.example"), "and to whom: {refused}");
    }

    /// The address that started this was `192.168.1.46` — a model two rooms
    /// away, reached through a VPN's proxy and reported as unreachable.
    #[test]
    fn an_endpoint_on_this_network_is_told_from_one_on_the_internet() {
        for local in [
            "http://192.168.1.46:1234",
            "http://10.0.0.5:11434/v1",
            "http://172.16.3.9:8080",
            "http://172.31.255.254:8080",
            "http://127.0.0.1:11434/v1",
            "http://localhost:1234/v1",
            "http://desk.local:1234/v1",
            "http://[::1]:8080/v1",
            "http://[fd00::1]:8080/v1",
            "http://[fe80::1]:8080/v1",
            "http://169.254.7.7:80",
        ] {
            assert!(beside_us(local), "{local} is on this network");
        }

        for away in [
            "https://api.anthropic.com",
            "https://api.proxyapi.ru/v1",
            // Adjacent to the private block on either side, and not in it.
            "http://172.15.0.1:8080",
            "http://172.32.0.1:8080",
            "http://8.8.8.8",
            // A name is reached however the machine reaches names.
            "http://gateway.example/v1",
            "",
        ] {
            assert!(!beside_us(away), "{away} is not");
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn the_wait_a_provider_asks_for_is_read_when_it_is_a_number_it_means() {
        assert_eq!(retry_after(&headers(&[("retry-after", "30")])), Some(Duration::from_secs(30)));
        assert_eq!(retry_after(&headers(&[("retry-after", " 5 ")])), Some(Duration::from_secs(5)));
        assert_eq!(retry_after(&headers(&[])), None, "no header is no answer");

        // The date form is legal and nobody sends it; a clock and a parser
        // disagreeing about one is worse than falling back to doubling.
        assert_eq!(retry_after(&headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")])), None);

        // Past the ceiling is not "later" in any useful sense: a turn nobody
        // is watching is still one somebody is paying for.
        assert_eq!(retry_after(&headers(&[("retry-after", "86400")])), None);
        assert_eq!(
            retry_after(&headers(&[("retry-after", &MOST_PATIENCE_SECS.to_string())])),
            Some(Duration::from_secs(MOST_PATIENCE_SECS)),
            "the ceiling itself is still an answer"
        );
    }
}

#[cfg(test)]
mod frames {
    use super::Frames;

    /// A character the network cut in half is neither lost nor fatal.
    ///
    /// Both halves of this happened at once, in production, in one panic:
    /// `start byte index 8185 is not a char boundary; it is inside 'т'`. The
    /// scan for the next separator resumed one byte back from the end of the
    /// last chunk, which is inside a two-byte letter; release builds abort on a
    /// panic, so the daemon went and the half-hour turn it was running went
    /// with it. Underneath that, each chunk was decoded on its own, so a letter
    /// split between two of them became two replacement characters.
    #[test]
    fn a_character_split_across_chunks_is_neither_lost_nor_fatal() {
        let mut frames = Frames::new();
        frames.feed("data: прив".as_bytes());
        // `е` is 0xD0 0xB5, and the network stopped between them.
        frames.feed(&[0xD0]);
        assert!(frames.ready().is_empty(), "no separator has arrived yet");

        frames.feed(&[0xB5]);
        frames.feed("т\n\n".as_bytes());
        assert_eq!(
            frames.ready(),
            vec!["data: привет\n\n".to_string()],
            "the letter cut in half came back wrong, or the frame did not close"
        );
    }

    /// A separator split across two chunks still ends the frame.
    ///
    /// Which is why the scan resumes a byte back at all, and so why the panic
    /// above was reachable. Kept as a claim so a fix for one does not quietly
    /// undo the other.
    #[test]
    fn a_separator_split_across_two_chunks_still_ends_the_frame() {
        let mut frames = Frames::new();
        frames.feed(b"data: one\n");
        assert!(frames.ready().is_empty(), "half a separator is not one");
        frames.feed(b"\ndata: two\n\n");
        assert_eq!(frames.ready(), vec!["data: one\n\n".to_string(), "data: two\n\n".to_string()]);
    }

    /// Bytes that are not UTF-8 at all are replaced rather than held.
    ///
    /// Held, they would stall the stream for good, waiting for a byte that
    /// cannot make them valid.
    #[test]
    fn bytes_that_are_not_a_truncated_character_do_not_stall_the_stream() {
        let mut frames = Frames::new();
        frames.feed(&[0xFF, 0xFE]);
        frames.feed(b"data: ok\n\n");
        let ready = frames.ready();
        assert_eq!(ready.len(), 1, "the stream carried on: {ready:?}");
        assert!(ready[0].ends_with("data: ok\n\n"), "{ready:?}");
    }
}
