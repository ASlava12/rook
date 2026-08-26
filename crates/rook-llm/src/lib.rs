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

pub mod openai;
pub mod types;

pub use types::*;

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("provider returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("could not parse the provider's response: {0}")]
    Decode(String),
    #[error("request exceeds the model's context window: {used} > {window} tokens")]
    ContextOverflow { used: usize, window: usize },
    #[error("no provider configured for {0:?}")]
    UnknownProvider(String),
    #[error("{0}")]
    Other(String),
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
}

/// Split a `provider/model` spec, e.g. `ollama/qwen3-coder:30b`.
pub fn split_spec(spec: &str) -> (&str, &str) {
    match spec.split_once('/') {
        Some((p, m)) => (p, m),
        None => ("openai-compatible", spec),
    }
}

/// Build a provider from a `provider/model` spec and the environment.
///
/// Endpoints and keys come from environment variables so a store or a config
/// file never has to hold a credential.
pub fn from_spec(spec: &str) -> Result<Box<dyn Provider>> {
    let (provider, model) = split_spec(spec);
    let cfg = match provider {
        "ollama" => openai::Config {
            base_url: env_or("OLLAMA_HOST", "http://127.0.0.1:11434") + "/v1",
            api_key: None,
            context_window: 32_768,
        },
        "lmstudio" => openai::Config {
            base_url: env_or("LMSTUDIO_HOST", "http://127.0.0.1:1234") + "/v1",
            api_key: None,
            context_window: 32_768,
        },
        "openai" => openai::Config {
            base_url: env_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            context_window: 128_000,
        },
        "openai-compatible" => openai::Config {
            base_url: std::env::var("ROOK_LLM_BASE_URL")
                .map_err(|_| LlmError::Other("set ROOK_LLM_BASE_URL for openai-compatible".into()))?,
            api_key: std::env::var("ROOK_LLM_API_KEY").ok(),
            context_window: 32_768,
        },
        other => return Err(LlmError::UnknownProvider(other.to_string())),
    };
    Ok(Box::new(openai::OpenAiCompatible::new(spec, model, cfg)?))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// Install the process-wide crypto provider for TLS.
///
/// `ring` rather than `aws-lc-rs`: it is pure Rust plus a small amount of
/// assembly and cross-compiles cleanly, which matters because FreeBSD is a
/// first-class target here and `aws-lc-rs` needs cmake and a working C
/// toolchain for it.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
