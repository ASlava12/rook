use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] rook_store::StoreError),
    #[error(transparent)]
    Skill(#[from] rook_skills::SkillError),
    #[error("capture failed: {0}")]
    Capture(String),
    #[error("refusing to capture {what}: exceeds {limit}. Narrow the paths or raise the limit in config.")]
    CaptureTooBig { what: String, limit: String },
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no session {0}")]
    NoSession(String),
    #[error(transparent)]
    Llm(#[from] rook_llm::LlmError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
