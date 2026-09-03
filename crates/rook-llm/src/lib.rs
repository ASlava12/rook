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
    #[error("cannot reach {endpoint}: {detail}\n{}", advice(.endpoint))]
    Unreachable { endpoint: String, detail: String },
    #[error("provider returned {status}: {body}{}", what_to_try(*.status, .body))]
    Status { status: u16, body: String },
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
    #[error("no model {model:?} on the server at {endpoint} — {}", offers(available))]
    NoSuchModel { model: String, endpoint: String, available: Vec<String> },
    #[error("{0}")]
    Other(String),
}

/// What a person can do about a request this endpoint will not take.
///
/// A 400 is the agent's request being wrong, and one shape of wrong is a field
/// the agent added rather than the user: the tool definitions, which some
/// OpenAI-compatible servers refuse outright. Dropping them here would leave an
/// agent that cannot act and does not say why, so the setting that puts them in
/// the prompt instead is named and the choice is left where it belongs.
fn what_to_try(status: u16, body: &str) -> &'static str {
    let said = body.to_ascii_lowercase();
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

/// What to try, which depends only on where the endpoint is.
///
/// A local one that answers nothing means the server is not running; a remote
/// one usually means the network or a missing key. Naming the wrong one wastes
/// the user's time, so this says both only when it cannot tell.
fn advice(endpoint: &str) -> String {
    // The whole 127/8 range, not just the usual address: a local server moved
    // off 127.0.0.1 is exactly the case where the wrong advice costs most.
    let local = ["://127.", "localhost", "[::1]", "://0.0.0.0"].iter().any(|h| endpoint.contains(h));
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
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let cut = (0..=max).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    format!("{}…", &text[..cut])
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
            let mut config = google::Config::new(base, key, model);
            config.stream_idle_timeout = stream_idle;
            if let Some(window) = context_window {
                config.context_window = window;
            }
            return Ok(Box::new(google::Google::new(spec, model, config)?));
        }
        "openai" => openai::Config::new(
            env_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
            std::env::var("OPENAI_API_KEY").ok(),
            128_000,
        ),
        "openai-compatible" => openai::Config::new(
            std::env::var("ROOK_LLM_BASE_URL").ok().filter(|u| !u.trim().is_empty()).ok_or_else(|| {
                LlmError::Other(
                    "ROOK_LLM_BASE_URL is not set, and `openai-compatible` has no default \
                         endpoint to fall back to."
                        .into(),
                )
            })?,
            std::env::var("ROOK_LLM_API_KEY").ok(),
            32_768,
        ),
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
