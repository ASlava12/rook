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

pub mod ask;
pub mod crates;
pub mod exec;
pub mod files;
pub mod mcp;
pub mod pending;
pub mod policy;
pub mod search;
pub mod web;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use rook_llm::ToolSpec;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool {name:?}{}", suggest(nearest))]
    Unknown { name: String, nearest: Vec<String> },
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

/// `content` is what the model sees. `full_bytes` is how much there was, which
/// is not the same thing: a tool that reads from disk can be asked again for the
/// rest, while a command's output is gone once it has been elided. That is why
/// truncation keeps both ends rather than one.
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

/// Where a text file is read from and written to.
///
/// The disk, unless a front end knows better. An editor holds unsaved buffers
/// the disk has never seen: an agent that reads around them sees the file as it
/// was before the user's last change, and edits it back.
#[async_trait]
pub trait Files: Send + Sync {
    async fn read(&self, path: &std::path::Path) -> Result<String>;
    async fn write(&self, path: &std::path::Path, contents: &str) -> Result<()>;
}

/// Where a command runs.
///
/// The agent's own process, unless a front end has somewhere better. An editor
/// has a terminal panel: running there lets the user watch a build instead of
/// waiting for the agent to report on one.
#[async_trait]
pub trait Terminals: Send + Sync {
    async fn run(&self, command: &str, cwd: &std::path::Path, output_limit: usize) -> Result<Ran>;
}

/// What a command did, however it was run.
pub struct Ran {
    pub output: String,
    pub exit_code: i32,
    /// The runner cut the output. Which end it kept is its own business — that
    /// is the price of running somewhere the user can see.
    pub truncated: bool,
    pub timed_out: bool,
}

/// The workspace root bounds every path a tool will touch, unless the caller
/// explicitly widens it.
#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub max_output_bytes: usize,
    pub command_timeout: std::time::Duration,
    /// When false, tools refuse paths outside the workspace.
    pub allow_outside_workspace: bool,
    /// Set by a front end whose environment owns the files — an editor, so far.
    pub files: Option<Arc<dyn Files>>,
    /// Set by a front end that has a terminal of its own to run in.
    pub terminals: Option<Arc<dyn Terminals>>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace", &self.workspace)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("command_timeout", &self.command_timeout)
            .field("allow_outside_workspace", &self.allow_outside_workspace)
            .field("files", &self.files.is_some())
            .field("terminals", &self.terminals.is_some())
            .finish()
    }
}

impl ToolContext {
    /// Read a text file through whatever owns it.
    pub async fn read_text(&self, path: &std::path::Path) -> Result<String> {
        match &self.files {
            Some(files) => files.read(path).await,
            None => tokio::fs::read_to_string(path)
                .await
                .map_err(|e| ToolError::Io { path: path.to_path_buf(), source: e }),
        }
    }

    pub async fn write_text(&self, path: &std::path::Path, contents: &str) -> Result<()> {
        match &self.files {
            Some(files) => files.write(path, contents).await,
            None => {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|e| ToolError::Io { path: parent.to_path_buf(), source: e })?;
                }
                tokio::fs::write(path, contents)
                    .await
                    .map_err(|e| ToolError::Io { path: path.to_path_buf(), source: e })
            }
        }
    }
}

impl ToolContext {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            max_output_bytes: 256 * 1024,
            command_timeout: std::time::Duration::from_secs(120),
            allow_outside_workspace: false,
            files: None,
            terminals: None,
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
        if self.allow_outside_workspace {
            return Ok(normalized);
        }

        let root = through_symlinks(&normalize(&self.workspace));
        let real = through_symlinks(&normalized);
        if !real.starts_with(&root) {
            let via = if real == normalized {
                String::new()
            } else {
                format!(" (it is {} through a symlink)", real.display())
            };
            return Err(ToolError::Denied(format!(
                "{} is outside the workspace {}{via} — run in a workspace that contains it \
                 (`rook -C`), or set sandbox.allow_outside_workspace",
                normalized.display(),
                root.display()
            )));
        }
        Ok(real)
    }
}

/// Where a path really leads. Lexical normalisation cannot see a symlink, and a
/// symlink inside the workspace pointing out of it is a way straight through
/// the wall: the file is read or written outside while every path involved
/// still looks contained.
///
/// Canonicalises the deepest ancestor that exists and re-appends the rest, so a
/// file that is about to be created still resolves.
pub fn through_symlinks(path: &std::path::Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    let mut rest = Vec::new();
    while let Some(name) = probe.file_name().map(|n| n.to_os_string()) {
        if let Ok(real) = probe.canonicalize() {
            return rest.iter().rev().fold(real, |p, part| p.join(part));
        }
        rest.push(name);
        probe.pop();
    }
    probe
        .canonicalize()
        .map(|real| rest.iter().rev().fold(real, |p, part| p.join(part)))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Keep both ends and drop the middle.
///
/// The tail carries the exit message and the last stack frame; the head carries
/// a compiler's first error, which is the one that caused the rest. Keeping only
/// the tail loses the reason and keeps the consequences.
pub fn elide_middle(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    // Weighted to the tail, which is where a run says how it ended.
    let head = boundary_at_or_before(text, budget / 3);
    let tail = boundary_at_or_after(text, text.len() - (budget - head));
    // What was dropped and how to get at it. A bare count tells the reader the
    // middle is gone and leaves them to guess whether it mattered; the head and
    // the tail are here because they are usually the answer, and when they are
    // not, narrowing the command is what works — the whole output was never
    // held in the first place, so there is nothing to page back to.
    format!(
        "{}\n[{} bytes elided from the middle — narrow the command if they matter, \
         e.g. filter it or ask for fewer lines]\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}

fn boundary_at_or_before(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn boundary_at_or_after(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i.min(text.len())
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
        tb.register(Arc::new(crates::CrateApi));
        tb
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    /// The same box with `dropped` taken out.
    ///
    /// The enforceable half of asking an agent not to do something: an
    /// instruction it may weigh against the rest of its prompt becomes a tool it
    /// cannot call, because it was never given one.
    pub fn without(&self, dropped: &[&str]) -> Self {
        Self { tools: self.tools.iter().filter(|t| !dropped.contains(&t.name())).cloned().collect() }
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
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown { name: name.to_string(), nearest: self.nearest(name) })?;
        tool.call(ctx, args).await
    }

    /// A model that typed `read_fil` has already spent a step, and "unknown
    /// tool" spends another. The names it might have meant cost nothing to say.
    fn nearest(&self, name: &str) -> Vec<String> {
        let mut scored: Vec<(usize, &str)> = self
            .names()
            .into_iter()
            .filter(|known| known.contains(name) || name.contains(known) || edits(name, known) <= 3)
            .map(|known| (edits(name, known), known))
            .collect();
        scored.sort();
        scored.into_iter().take(3).map(|(_, known)| known.to_string()).collect()
    }
}

fn suggest(nearest: &[String]) -> String {
    match nearest {
        [] => String::new(),
        near => format!(" — did you mean {}?", near.join(", ")),
    }
}

/// Levenshtein, over two short names: a dependency for this would cost more
/// than the ten lines it saves.
fn edits(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let cost = usize::from(x != y);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(diagonal + cost);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Past this a line is not source: a minified bundle, or a blob whose first
/// pages happened to hold no zero byte. Read and searched a line at a time, this
/// is what bounds the memory either costs.
pub(crate) const MAX_LINE: u64 = 1 << 20;

pub(crate) fn arg_str(args: &serde_json::Value, tool: &str, key: &str) -> Result<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| ToolError::Invalid {
        tool: tool.to_string(),
        message: format!("required argument {key:?} is missing or not a string"),
    })
}

pub(crate) fn arg_usize(args: &serde_json::Value, key: &str, default: usize) -> usize {
    args.get(key).and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(default)
}
