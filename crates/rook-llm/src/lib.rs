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
pub mod openai;
pub mod stream;
pub mod types;

pub use stream::{Assembler, Delta, ResponseStream};
pub use types::*;

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("cannot reach {endpoint}: {detail}\n{}", advice(.endpoint))]
    Unreachable { endpoint: String, detail: String },
    #[error("provider returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("could not parse the provider's response: {0}")]
    Decode(String),
    #[error("request exceeds the model's context window: {used} > {window} tokens")]
    ContextOverflow { used: usize, window: usize },
    #[error("no provider configured for {0:?}")]
    UnknownProvider(String),
    #[error("the model stopped sending for {secs}s; giving up on the stream")]
    Stalled { secs: u64 },
    #[error("{0}")]
    Other(String),
}

impl LlmError {
    pub fn unreachable(endpoint: &str, source: impl std::fmt::Display) -> Self {
        Self::Unreachable { endpoint: origin(endpoint), detail: source.to_string() }
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

/// Scheme and authority only: the path a request happened to use is noise, and
/// the same endpoint should read the same however it was reached.
fn origin(url: &str) -> String {
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

/// Split a `provider/model` spec, e.g. `ollama/qwen3-coder:30b`.
pub fn split_spec(spec: &str) -> (&str, &str) {
    match spec.split_once('/') {
        Some((p, m)) => (p, m),
        None => ("openai-compatible", spec),
    }
}

/// Endpoints and keys come from environment variables, so neither the store nor
/// the config file ever holds a credential.
pub fn from_spec(spec: &str, stream_idle: std::time::Duration) -> Result<Box<dyn Provider>> {
    from_spec_with(spec, stream_idle, None)
}

/// `context_window` overrides the provider's assumed default, which is guesswork
/// for anything self-hosted: a local model may serve 8k or a million.
pub fn from_spec_with(
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
            // Empty counts as unset: an exported-but-blank variable is the
            // usual way this goes wrong, and a 401 does not say which.
            let key = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty()).ok_or_else(
                || {
                    LlmError::Other(
                        "ANTHROPIC_API_KEY is not set. Export it, or set `[agent] model` to a \
                         local provider such as `ollama/…`."
                            .into(),
                    )
                },
            )?;
            let base = env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com");
            let mut config = anthropic::Config::new(base, key, model);
            config.stream_idle_timeout = stream_idle;
            if let Some(window) = context_window {
                config.context_window = window;
            }
            return Ok(Box::new(anthropic::Anthropic::new(spec, model, config)?));
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
        other => return Err(LlmError::UnknownProvider(other.to_string())),
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
