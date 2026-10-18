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
pub mod environments;
pub mod exec;
pub mod files;
pub mod isolate;
pub mod jobs;
pub mod mcp;
pub mod network;
pub mod pdf;
pub mod pending;
pub mod policy;
pub mod private_input;
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
    #[error("{}", io_at(path, source))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Denied(String),
    #[error("execution result is uncertain: {0}")]
    ExecutionUnknown(String),
}

/// What went wrong at a path, in the words of what was asked for rather than
/// the kernel's.
///
/// `io error at /tmp/x/status.txt: No such file or directory (os error 2)` says
/// one fact three times and names nothing to do about it. A file that is not
/// there is not an I/O failure — it is the ordinary answer to asking for
/// something that does not exist, and the half worth having is which directory
/// was looked in, because the caller passed a relative name and the tool
/// resolved it somewhere the caller cannot see.
///
/// `Is a directory` is the same shape and is the one already written down: a
/// tool handed a directory answered with the operating system's `os error 21`,
/// and the checkpoint taken before it failed on the same path, so an ordinary
/// bad argument was reported as work that could not be undone.
///
/// Everything else keeps the kernel's sentence, which is the right one when
/// nobody here has a better.
fn io_at(path: &std::path::Path, source: &std::io::Error) -> String {
    use std::io::ErrorKind;
    let named = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let directory = path.parent().filter(|p| !p.as_os_str().is_empty());
    match (source.kind(), named, directory) {
        (ErrorKind::NotFound, Some(name), Some(dir)) => {
            format!("there is no {name:?} in {} — `list_dir` there says what is", dir.display())
        }
        (ErrorKind::NotFound, _, _) => format!("{} is not there", path.display()),
        (ErrorKind::IsADirectory, _, _) => {
            format!("{} is a directory, not a file", path.display())
        }
        (ErrorKind::PermissionDenied, _, _) => {
            format!("{} cannot be read or written: permission denied", path.display())
        }
        _ => format!("io error at {}: {source}", path.display()),
    }
}

pub type Result<T> = std::result::Result<T, ToolError>;

/// `content` is what the model sees. `full_bytes` is how much there was, which
/// is not the same thing: a tool that reads from disk can be asked again for the
/// rest, while a command's output is gone once it has been elided. That is why
/// truncation keeps both ends rather than one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub content: String,
    /// Validated inline tool data, kept separate from text and metadata so
    /// hooks and transcript rendering cannot accidentally print base64.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<rook_llm::Image>,
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
        Self {
            content,
            images: Vec::new(),
            is_error: false,
            truncated: false,
            full_bytes,
            meta: BTreeMap::new(),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        let content = content.into();
        let full_bytes = content.len();
        Self {
            content,
            images: Vec::new(),
            is_error: true,
            truncated: false,
            full_bytes,
            meta: BTreeMap::new(),
        }
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
    /// A negotiated editor bridge can address child work without changing its
    /// filesystem authority. Other frontends keep sharing the original bridge.
    fn for_session(&self, _session: &str) -> Option<Arc<dyn Files>> {
        None
    }
    async fn read(&self, path: &std::path::Path) -> Result<String>;
    /// A raw protocol response needs an admission bound before its owned copy.
    /// Bridges must opt in rather than silently falling back to on-disk content.
    async fn read_limited(&self, _path: &std::path::Path, _maximum: usize) -> Result<String> {
        Err(ToolError::Denied("The selected file bridge does not support bounded raw reads; update the bridge rather than reading around its buffers".into()))
    }
    async fn write(&self, path: &std::path::Path, contents: &str) -> Result<()>;
}

/// Where a command runs.
///
/// The agent's own process, unless a front end has somewhere better. An editor
/// has a terminal panel: running there lets the user watch a build instead of
/// waiting for the agent to report on one.
#[async_trait]
pub trait Terminals: Send + Sync {
    fn for_session(&self, _session: &str) -> Option<Arc<dyn Terminals>> {
        None
    }
    async fn run(&self, command: &str, cwd: &std::path::Path, output_limit: usize) -> Result<Ran>;
}

/// Where a tool gets a secret's value, when a call names one.
///
/// A trait rather than a map, and asked per call rather than filled in per
/// turn: the value is fetched when it is about to be used and the thing that
/// answers is the thing that later takes it back out of what comes back.
pub trait Secrets: Send + Sync {
    fn value(&self, name: &str) -> Option<String>;
    /// Values already resolved this turn, for redacting persisted output.
    fn redactions(&self) -> Vec<String> {
        Vec::new()
    }
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
    pub documents: pdf::Settings,
    pub session: Option<u128>,
    pub read_only: bool,
    pub workspace: PathBuf,
    pub trace: Option<rook_llm::TraceContext>,
    pub propagate_trace: bool,
    pub elicitor: Option<Arc<dyn rook_mcp::elicitation::Elicitor>>,
    /// True in a sub-agent. Branch-changing commands remain forbidden even
    /// in an isolated worktree; changes are reviewed and transferred by its parent.
    pub delegated: bool,
    pub max_output_bytes: usize,
    pub command_timeout: std::time::Duration,
    /// A file-effects receipt needs a quiet owned process group before its
    /// final filesystem observation, including children the shell abandoned.
    pub require_command_cleanup: bool,
    /// When false, tools refuse paths outside the workspace.
    pub allow_outside_workspace: bool,
    /// What a command may write and reach, and whether it is held to it: the
    /// platform's containment where there is some, none, or a refusal to run
    /// without it.
    pub isolation: isolate::Isolation,
    pub isolate: isolate::Mode,
    /// Set by a front end whose environment owns the files — an editor, so far.
    pub files: Option<Arc<dyn Files>>,
    /// Set by a front end that has a terminal of its own to run in.
    pub terminals: Option<Arc<dyn Terminals>>,
    pub execution: Option<Arc<environments::Context>>,
    /// Commands left running. Built once by the front end, because a registry
    /// built per turn would kill everything in it between one turn and the next.
    pub jobs: Option<Arc<jobs::Jobs>>,
    /// Where the whole of a runaway command's output is kept. `None` discards
    /// the middle, which is what happened before there was anywhere to put it.
    pub spill_dir: Option<PathBuf>,
    /// Ceiling on one such file. Zero keeps none.
    pub max_spill_bytes: u64,
    /// Files `search` may look at before giving up. The hits are capped by the
    /// call; this caps the looking, which is what turns a walk of somebody's
    /// home directory into a hang.
    pub max_files_searched: usize,
    /// Where a named secret's value comes from, when a call names one. `None`
    /// where nothing set it, and then a call naming a secret is refused rather
    /// than run without it.
    pub secrets: Option<Arc<dyn Secrets>>,
    #[doc(hidden)]
    pub command_transport: Option<Arc<environments::Transport>>,
    /// Where a call that takes a while says so, while it takes it.
    ///
    /// A tool call is one await from outside, and a long one is indisputable
    /// from a wedged one: the screen shows the same line either way. What
    /// separates them is whether anything is happening, which only the tool
    /// knows. A sender rather than a callback because the loop's own progress
    /// is `FnMut` and this crosses into a spawned future; set per call by the
    /// loop, and `None` anywhere nobody is listening.
    pub watching: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// Per-call cumulative pipe bytes. Readers update fixed-size state; the
    /// owner samples it instead of queueing every output chunk.
    pub output_activity: Option<Arc<std::sync::atomic::AtomicU64>>,
}

impl ToolContext {
    /// Say what is happening, to whoever is watching this call. Nothing when
    /// nobody is.
    pub fn watching(&self, said: impl Into<String>) {
        if let Some(watching) = &self.watching {
            let _ = watching.send(said.into());
        }
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace", &self.workspace)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("command_timeout", &self.command_timeout)
            .field("allow_outside_workspace", &self.allow_outside_workspace)
            .field("spill_dir", &self.spill_dir)
            .field("max_spill_bytes", &self.max_spill_bytes)
            .field("max_files_searched", &self.max_files_searched)
            .field("jobs", &self.jobs.is_some())
            .field("files", &self.files.is_some())
            .field("terminals", &self.terminals.is_some())
            .finish()
    }
}

impl ToolContext {
    pub(crate) fn disk_path(&self, path: &std::path::Path) -> Result<(PathBuf, PathBuf)> {
        if !path.is_absolute() {
            return Err(ToolError::Invalid {
                tool: "file I/O".into(),
                message: "expected an absolute path resolved against the workspace".into(),
            });
        }
        let root = if self.allow_outside_workspace {
            path.ancestors().last().unwrap_or(&self.workspace).to_path_buf()
        } else {
            through_symlinks(&normalize(&self.workspace))
        };
        let relative = path
            .strip_prefix(&root)
            .or_else(|_| path.strip_prefix(&self.workspace))
            .map_err(|_| ToolError::Denied("path is outside the workspace".into()))?
            .to_path_buf();
        Ok((root, relative))
    }

    /// Read a text file through whatever owns it.
    pub async fn read_text(&self, path: &std::path::Path) -> Result<String> {
        match &self.files {
            Some(files) => files.read(path).await,
            None => {
                let ctx = self.clone();
                let path = path.to_path_buf();
                tokio::task::spawn_blocking(move || {
                    let (root, relative) = ctx.disk_path(&path)?;
                    rook_contain::files::read_text(&root, &relative, 16 * 1024 * 1024)
                        .map_err(|source| ToolError::Io { path, source })
                })
                .await
                .map_err(|e| ToolError::Denied(format!("file reader failed: {e}")))?
            }
        }
    }

    pub async fn read_text_limited(&self, path: &std::path::Path, maximum: usize) -> Result<String> {
        if let Some(files) = &self.files {
            return files.read_limited(path, maximum).await;
        }
        let ctx = self.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let (root, relative) = ctx.disk_path(&path)?;
            let file = rook_contain::files::open(&root, &relative)
                .map_err(|source|ToolError::Io {path:path.clone(),source})?;
            let metadata = file.metadata().map_err(|source|ToolError::Io {path:path.clone(),source})?;
            if !metadata.is_file() || metadata.len()>maximum as u64 {
                return Err(ToolError::Denied(format!("Raw text reads require a regular file within the {maximum}-byte file limit; request a smaller source or raise acp_executor.file_bytes")));
            }
            use std::io::Read;
            let mut text = String::new();
            file.take(maximum.saturating_add(1) as u64).read_to_string(&mut text)
                .map_err(|source|ToolError::Io {path,source})?;
            if text.len()>maximum { return Err(ToolError::Denied("File grew past the raw read byte limit; no partial content was returned".into())); }
            Ok(text)
        }).await.map_err(|error|ToolError::Denied(format!("Raw file reader failed: {error}")))?
    }

    pub async fn write_text(&self, path: &std::path::Path, contents: &str) -> Result<()> {
        match &self.files {
            Some(files) => files.write(path, contents).await,
            None => {
                let ctx = self.clone();
                let path = path.to_path_buf();
                let contents = contents.to_owned();
                tokio::task::spawn_blocking(move || {
                    let (root, relative) = ctx.disk_path(&path)?;
                    rook_contain::files::write(&root, &relative, contents.as_bytes())
                        .map_err(|source| ToolError::Io { path, source })
                })
                .await
                .map_err(|e| ToolError::Denied(format!("file writer failed: {e}")))?
            }
        }
    }
}

impl ToolContext {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            documents: Default::default(),
            session: None,
            read_only: false,
            isolation: isolate::Isolation::for_workspace(&workspace),
            isolate: isolate::Mode::Off,
            workspace,
            delegated: false,
            trace: None,
            propagate_trace: false,
            elicitor: None,
            max_output_bytes: 256 * 1024,
            command_timeout: std::time::Duration::from_secs(120),
            require_command_cleanup: false,
            allow_outside_workspace: false,
            files: None,
            terminals: None,
            execution: None,
            jobs: None,
            spill_dir: None,
            max_spill_bytes: 0,
            max_files_searched: 20_000,
            secrets: None,
            command_transport: None,
            watching: None,
            output_activity: None,
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
            return Ok(through_symlinks(&normalized));
        }

        for component in normalized.ancestors() {
            if std::fs::symlink_metadata(component).is_ok_and(|m| m.file_type().is_symlink())
                && component.canonicalize().is_err()
            {
                return Err(ToolError::Denied(format!("{} is an unresolved symlink", component.display())));
            }
        }
        let root = through_symlinks(&normalize(&self.workspace));
        let real = through_symlinks(&normalized);
        if !real.starts_with(&root) {
            let via = if real == normalized {
                String::new()
            } else {
                format!(" (it is {} through a symlink)", real.display())
            };
            // A leading slash is not an attempt to leave the workspace, and the
            // reader of this message is the model. `read_file {"path":
            // "/app/mover.py"}` in a workspace holding `app/mover.py` was
            // answered with `rook -C` and a sandbox setting — two things a
            // person does and a model cannot — so it went to `run_command`,
            // was refused there for the same reason, and read the file on the
            // third try with the whole absolute path. Three calls and eleven
            // thousand tokens to read one file, and the one word that would
            // have fixed it was never said.
            if let Some(meant) = self.same_path_within(raw) {
                return Err(ToolError::Denied(format!(
                    "{} is outside the workspace {}, but {} is in it — paths here are relative to \
                     the workspace, so drop the leading slash",
                    normalized.display(),
                    root.display(),
                    meant.display()
                )));
            }
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

impl ToolContext {
    /// The path the caller probably meant: the same one, taken as relative to
    /// the workspace, when something is actually there.
    ///
    /// Only when it exists. Offering it for any absolute path would answer a
    /// genuine escape — `/etc/passwd` — with a suggestion to try
    /// `<workspace>/etc/passwd`, which is a worse message than the plain
    /// refusal and reads as a hint about how to get through.
    ///
    /// The root is dropped by components rather than by trimming a `/`, because
    /// what a rooted path begins with differs by platform: `C:\\app` has a
    /// prefix as well as a root, and neither is a separator to strip.
    fn same_path_within(&self, raw: &str) -> Option<PathBuf> {
        let asked = PathBuf::from(raw);
        // `has_root`, not `is_absolute`: on Windows an absolute path needs a
        // drive as well as a root, so `/app/mover.py` — the very shape this is
        // here to catch — is not absolute there, and the Windows runner found
        // this message never firing at all. Rooted is the question being asked.
        if !asked.has_root() {
            return None;
        }
        let relative: PathBuf = asked
            .components()
            .skip_while(|c| matches!(c, std::path::Component::Prefix(_) | std::path::Component::RootDir))
            .collect();
        if relative.as_os_str().is_empty() {
            return None;
        }
        let root = normalize(&self.workspace);
        let within = normalize(&root.join(&relative));
        (within.starts_with(&root) && within.exists()).then_some(relative)
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

pub(crate) fn boundary_at_or_after(text: &str, mut i: usize) -> usize {
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

    /// The model-facing form; external schemas must retain their constraints.
    fn advertisement(&self, lazy: bool) -> ToolSpec {
        let spec = self.spec();
        if lazy { spec.stub() } else { spec }
    }
    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome>;

    /// Concrete operation for approvals and hooks when a tool forwards a call.
    fn invocation<'a>(&'a self, args: &'a serde_json::Value) -> (&'a str, &'a serde_json::Value) {
        (self.name(), args)
    }

    /// Configured MCP identity, independent of text or metadata returned by a server.
    fn mcp_source(&self, _args: &serde_json::Value) -> Option<(&str, &str)> {
        None
    }

    /// Paths this call is about to modify, so the caller can checkpoint them
    /// first. Empty for read-only tools.
    fn touched_paths(&self, _args: &serde_json::Value) -> Vec<String> {
        Vec::new()
    }

    /// Paths this call reads, so a caller can tell that this session has seen
    /// what is there. Empty for anything that does not read a named file.
    fn observed_paths(&self, _args: &serde_json::Value) -> Vec<String> {
        Vec::new()
    }

    /// Whether this replaces a file wholesale rather than changing part of it.
    ///
    /// An edit names the text it is replacing and fails on its own when somebody
    /// else has changed that text. An overwrite has nothing to fail against.
    fn overwrites(&self) -> bool {
        false
    }

    /// What the call would change, for whoever is asked to approve it.
    ///
    /// A person shown only a path is approving something they cannot see, which
    /// is most of the value of asking. `None` where there is nothing to show —
    /// a command's effect is the command, and it is already in the question.
    async fn preview(&self, _ctx: &ToolContext, _args: &serde_json::Value) -> Option<String> {
        None
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
    mcp_catalog: mcp::CatalogSummary,
    mcp_sources: Arc<Vec<(Arc<rook_mcp::Server>, Vec<rook_mcp::ToolDescriptor>)>>,
    mcp_names: Arc<std::collections::BTreeSet<String>>,
    mcp_versions: Arc<Vec<u64>>,
    mcp_dropped: Arc<std::collections::BTreeSet<String>>,
    mcp_limits: mcp::CatalogLimits,
}

impl ToolBox {
    pub fn standard() -> Self {
        let mut tb = Self::default();
        tb.register(Arc::new(files::ReadFile));
        tb.register(Arc::new(pdf::ReadPdf));
        tb.register(Arc::new(files::WriteFile));
        tb.register(Arc::new(files::EditFile));
        tb.register(Arc::new(files::DeleteFile));
        tb.register(Arc::new(files::MoveFile));
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
        let mut limited = Self {
            tools: self.tools.iter().filter(|t| !dropped.contains(&t.name())).cloned().collect(),
            mcp_catalog: self.mcp_catalog.clone(),
            mcp_sources: self.mcp_sources.clone(),
            mcp_names: self.mcp_names.clone(),
            mcp_versions: self.mcp_versions.clone(),
            mcp_dropped: Arc::new(
                self.mcp_dropped.iter().cloned().chain(dropped.iter().map(|s| (*s).to_owned())).collect(),
            ),
            mcp_limits: self.mcp_limits,
        };
        // Deferred routes must obey removal immediately, not only after a
        // later notification rebuilds their discovery/call helpers.
        if !self.mcp_sources.is_empty() {
            limited.register_mcp_catalog(self.mcp_sources.iter().cloned(), self.mcp_limits);
            limited.mcp_versions = self.mcp_versions.clone();
        }
        limited
    }

    pub fn register_mcp_catalog(
        &mut self,
        servers: impl IntoIterator<Item = (Arc<rook_mcp::Server>, Vec<rook_mcp::ToolDescriptor>)>,
        limits: mcp::CatalogLimits,
    ) {
        self.tools.retain(|tool| !self.mcp_names.contains(tool.name()));
        let sources: Vec<_> = servers.into_iter().collect();
        self.mcp_limits = limits;
        let mut catalog = Self { mcp_dropped: self.mcp_dropped.clone(), ..Default::default() };
        catalog.install_mcp_catalog(sources.iter().cloned(), limits);
        catalog.tools.retain(|tool| !self.mcp_dropped.contains(tool.name()));
        self.mcp_names = Arc::new(catalog.tools.iter().map(|tool| tool.name().to_owned()).collect());
        self.mcp_catalog = catalog.mcp_catalog;
        for tool in catalog.tools {
            self.register(tool);
        }
        // The caller supplied descriptors without a version. Reconcile enabled
        // sources once before using them, instead of tagging old bytes with a
        // concurrently published generation.
        self.mcp_versions = Arc::new(
            sources
                .iter()
                .map(|(server, _)| {
                    let status = server.catalog_status();
                    if status.configured { 0 } else { status.generation }
                })
                .collect(),
        );
        self.mcp_sources = Arc::new(sources);
    }
    pub fn mcp_catalog_changed(&self) -> bool {
        self.mcp_sources.iter().zip(self.mcp_versions.iter()).any(|((server, _), version)| {
            let status = server.catalog_status();
            status.configured
                && ((status.stale && status.retry_after_secs.is_none()) || status.generation != *version)
        })
    }

    /// Refresh only the connections admitted to this box, retaining the Arc
    /// bindings of previously cloned boxes and already admitted calls.
    pub async fn refresh_mcp_catalog(
        &mut self,
        limits: mcp::CatalogLimits,
        timeout: std::time::Duration,
    ) -> bool {
        use futures_util::{StreamExt, stream};
        let sources = self.mcp_sources.clone();
        let servers: Vec<_> = sources.iter().map(|(server, _)| server.clone()).collect();
        let refresh = stream::iter(
            servers.into_iter().map(|server| async move { server.refresh_tool_catalog().await }),
        )
        .buffer_unordered(4);
        let _ = tokio::time::timeout(
            timeout.min(std::time::Duration::from_secs(300)),
            refresh.collect::<Vec<_>>(),
        )
        .await;
        let changed = sources.iter().zip(self.mcp_versions.iter()).any(|((server, _), version)| {
            let status = server.catalog_status();
            status.configured && status.generation != *version
        });
        if changed {
            let mut versions = Vec::new();
            let current = sources
                .iter()
                .zip(self.mcp_versions.iter())
                .map(|((server, old), version)| {
                    let snapshot =
                        server.catalog_status().configured.then(|| server.cached_tools()).flatten();
                    let (generation, tools) = snapshot.unwrap_or_else(|| (*version, old.clone()));
                    versions.push(generation);
                    (server.clone(), tools)
                })
                .collect::<Vec<_>>();
            self.register_mcp_catalog(current, limits);
            self.mcp_versions = Arc::new(versions);
        }
        changed
    }
    pub fn restrict_mcp(&mut self, allowed: &std::collections::BTreeSet<String>, limits: mcp::CatalogLimits) {
        let sources: Vec<_> =
            self.mcp_sources.iter().filter(|(server, _)| allowed.contains(server.name())).cloned().collect();
        self.register_mcp_catalog(sources, limits);
    }
    pub fn mcp_server_names(&self) -> std::collections::BTreeSet<String> {
        self.mcp_sources.iter().map(|(server, _)| server.name().to_owned()).collect()
    }

    pub fn mcp_catalog_summary(&self) -> &mcp::CatalogSummary {
        &self.mcp_catalog
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
        self.tools.iter().map(|t| t.advertisement(true)).collect()
    }

    pub async fn call(&self, ctx: &ToolContext, name: &str, args: &serde_json::Value) -> Result<ToolOutcome> {
        if ctx.isolation.network_policy.enabled && !crate::network::local_tool(name) {
            return Err(ToolError::Denied(format!(
                "{name} has no domain-contained network path; use a local foreground run_command through the configured broker"
            )));
        }

        if ctx.execution.as_ref().is_some_and(|e| e.settings.native_workspace)
            && [
                "read_file",
                "write_file",
                "edit_file",
                "delete_file",
                "move_file",
                "list_dir",
                "search",
                "crate_api",
                "read_pdf",
            ]
            .contains(&name)
        {
            return Err(ToolError::Denied("selected SSH-native workspace has remote filesystem authority; use ssh_workspace instead of local file tools".into()));
        }
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown { name: name.to_string(), nearest: self.nearest(name) })?;
        if let Some(unusable) = unusable_arguments(name, args) {
            return Err(unusable);
        }
        let read_again = as_the_schema_reads_them(&tool.spec().parameters, args);
        tool.call(ctx, read_again.as_ref().unwrap_or(args)).await
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

/// Why the call cannot be made at all, before anyone is asked to approve it.
///
/// `Null` is what a dialect hands back for arguments it could not parse — a call
/// cut off at the output limit produces it every time. Without this the tool
/// reports the argument that is missing as a result, so a model told `path` is
/// missing sends the same truncated call again; and the policy is asked to weigh
/// a command that is the empty string.
pub fn unusable_arguments(tool: &str, args: &serde_json::Value) -> Option<ToolError> {
    args.is_null().then(|| ToolError::Invalid {
        tool: tool.to_string(),
        message: "the arguments were not valid JSON — send the call again, as one complete \
                  JSON object"
            .into(),
    })
}

/// Arguments a model wrote as a string of JSON, read as the JSON they are.
///
/// `{"edits": "[{\"old\": …}]"}` — the field is a string and inside it is
/// exactly the array the tool asked for. Deserializing refuses it, and a
/// refusal is answered by writing the same thing again, which is the whole
/// class of retry this repository keeps meeting. It cost four steps of a real
/// turn.
///
/// Against the tool's own schema, never blindly: `write_file` takes a file's
/// contents as a string, and a JSON document on its way to disk must arrive as
/// the text it is.
///
/// `None` when nothing needed re-reading, so the ordinary call is not copied.
pub fn as_the_schema_reads_them(
    schema: &serde_json::Value,
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    let properties = schema.get("properties")?.as_object()?;
    let given = args.as_object()?;
    let mut read_again = given.clone();
    let mut changed = false;
    for (key, value) in given {
        let (Some(text), Some(wanted)) =
            (value.as_str(), properties.get(key).and_then(|p| p.get("type")).and_then(|t| t.as_str()))
        else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text.trim()) else { continue };
        if (wanted == "array" && parsed.is_array()) || (wanted == "object" && parsed.is_object()) {
            read_again.insert(key.clone(), parsed);
            changed = true;
        }
    }
    changed.then_some(serde_json::Value::Object(read_again))
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
