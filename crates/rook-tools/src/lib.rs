//! The agent's built-in tools.
//!
//! Each tool here is shaped by a failure mode observed in shipping agents:
//!
//! * `read_file` pages by offset instead of refusing large files, because a hard
//!   size cap turns a legitimate read into an unrecoverable task.
//! * `run_command` caps captured output and always has a timeout, because an
//!   unbounded `cat` of a log file is how a transcript blows its context window.
//! * `edit_file` requires the old text to appear exactly once, because a
//!   silently-applied ambiguous edit is worse than a rejected one.
//! * Every tool returns structured [`ToolOutcome`], so a failure carries a reason
//!   the model can act on rather than a bare error string.

pub mod exec;
pub mod files;
pub mod mcp;
pub mod policy;
pub mod search;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use rook_llm::ToolSpec;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool {0:?}")]
    Unknown(String),
    #[error("{tool}: {message}")]
    Invalid { tool: String, message: String },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Denied(String),
}

pub type Result<T> = std::result::Result<T, ToolError>;

/// `content` is what the model sees; `full_bytes` is what actually happened, so
/// the store keeps the whole thing even when context gets only a window of it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub full_bytes: usize,
    /// Extra facts for the UIs: exit codes, byte ranges, match counts.
    #[serde(default)]
    pub meta: BTreeMap<String, serde_json::Value>,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        let content = content.into();
        let full_bytes = content.len();
        Self { content, is_error: false, truncated: false, full_bytes, meta: BTreeMap::new() }
    }

    pub fn error(content: impl Into<String>) -> Self {
        let content = content.into();
        let full_bytes = content.len();
        Self { content, is_error: true, truncated: false, full_bytes, meta: BTreeMap::new() }
    }

    pub fn with(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.meta.insert(key.to_string(), value.into());
        self
    }
}

/// The workspace root bounds every path a tool will touch, unless the caller
/// explicitly widens it.
#[derive(Clone, Debug)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub max_output_bytes: usize,
    pub command_timeout: std::time::Duration,
    /// When false, tools refuse paths outside the workspace.
    pub allow_outside_workspace: bool,
}

impl ToolContext {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            max_output_bytes: 256 * 1024,
            command_timeout: std::time::Duration::from_secs(120),
            allow_outside_workspace: false,
        }
    }

    /// Resolve a caller-supplied path against the workspace and refuse escapes.
    ///
    /// Done lexically after normalising `..`, so it works the same on Windows
    /// and on case-insensitive filesystems, and does not require the path to
    /// exist yet.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf> {
        let candidate = PathBuf::from(raw);
        let joined = if candidate.is_absolute() { candidate } else { self.workspace.join(candidate) };
        let normalized = normalize(&joined);
        if !self.allow_outside_workspace {
            let root = normalize(&self.workspace);
            if !normalized.starts_with(&root) {
                return Err(ToolError::Denied(format!(
                    "{} is outside the workspace {}",
                    normalized.display(),
                    root.display()
                )));
            }
        }
        Ok(normalized)
    }
}

/// Lexical normalisation: resolve `.` and `..` without touching the filesystem.
pub fn normalize(path: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn spec(&self) -> ToolSpec;
    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome>;

    /// Paths this call is about to modify, so the caller can checkpoint them
    /// first. Empty for read-only tools.
    fn touched_paths(&self, _args: &serde_json::Value) -> Vec<String> {
        Vec::new()
    }

    /// What this call would do to the machine, for the approval policy.
    fn risk(&self, args: &serde_json::Value) -> policy::Risk {
        match self.touched_paths(args) {
            paths if paths.is_empty() => policy::Risk::ReadOnly,
            paths => policy::Risk::Write(paths),
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolBox {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolBox {
    pub fn standard() -> Self {
        let mut tb = Self::default();
        tb.register(Arc::new(files::ReadFile));
        tb.register(Arc::new(files::WriteFile));
        tb.register(Arc::new(files::EditFile));
        tb.register(Arc::new(files::ListDir));
        tb.register(Arc::new(search::Search));
        tb.register(Arc::new(exec::RunCommand));
        tb
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Full schemas — what goes to the model when eager loading is on.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// Name and description only. With a few dozen tools this is the difference
    /// between ~400 and ~4,000 tokens on every single request, and on local
    /// models a tool-heavy prompt is far slower to process than a plain one.
    pub fn stubs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec().stub()).collect()
    }

    pub async fn call(&self, ctx: &ToolContext, name: &str, args: &serde_json::Value) -> Result<ToolOutcome> {
        let tool = self.get(name).ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        tool.call(ctx, args).await
    }
}

pub(crate) fn arg_str(args: &serde_json::Value, tool: &str, key: &str) -> Result<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| ToolError::Invalid {
        tool: tool.to_string(),
        message: format!("required argument {key:?} is missing or not a string"),
    })
}

pub(crate) fn arg_usize(args: &serde_json::Value, key: &str, default: usize) -> usize {
    args.get(key).and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(default)
}
