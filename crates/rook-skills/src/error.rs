use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: missing YAML frontmatter (a SKILL.md must start with a `---` block)")]
    NoFrontmatter { path: PathBuf },
    #[error("{path}: invalid frontmatter: {reason}")]
    BadFrontmatter { path: PathBuf, reason: String },
    #[error("{path}: field `{field}` is required")]
    MissingField { path: PathBuf, field: &'static str },
    #[error("{path}: `{field}` is not a valid semver requirement ({value:?}): {reason}")]
    BadVersionReq { path: PathBuf, field: String, value: String, reason: String },
    #[error("skill {0:?} not found")]
    NotFound(String),
    #[error("skill {name} has no version compatible with this environment; {detail}")]
    NoCompatibleVersion { name: String, detail: String },
}

pub type Result<T> = std::result::Result<T, SkillError>;

impl SkillError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}
