//! A Language Server Protocol client, for code intelligence the agent can use.
//!
//! Grep tells an agent where a name appears. A language server tells it where
//! the name is *defined*, what actually refers to it, and what the type checker
//! thinks — which is the difference between an edit that compiles and one that
//! looks right. This is the most-asked-for thing in the survey behind this
//! project (codex #8745, 564 reactions).
//!
//! Deliberately a subset: diagnostics, definition, references, workspace
//! symbols. No completion, no formatting, no code actions — an agent writes
//! whole edits rather than accepting completions.

pub mod protocol;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};

pub use protocol::{Diagnostic, Location, Position, Symbol};

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("could not start {command:?}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{server}: {0}", message)]
    Transport { server: String, message: String },
    #[error("{server}: {method} returned an error: {message}")]
    Rpc { server: String, method: String, message: String },
    #[error("{server}: {method} did not answer within {}s", timeout.as_secs())]
    Timeout { server: String, method: String, timeout: Duration },
    #[error("{server}: the language server exited{}", detail(complaint))]
    Closed { server: String, complaint: Option<String> },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{symbol:?} does not appear in {path}")]
    NoSuchSymbol { symbol: String, path: String },
    #[error("no language server configured for {0}")]
    NoServer(String),
}

pub type Result<T> = std::result::Result<T, LspError>;

/// A server that dies on startup usually says why on stderr, and that line is
/// the whole diagnosis — "the language server exited" alone sends the user
/// looking in the wrong place.
fn detail(complaint: &Option<String>) -> String {
    match complaint {
        Some(text) if !text.trim().is_empty() => format!(": {}", text.trim()),
        _ => String::new(),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    /// What this server is for, e.g. `rust`. Used in messages only.
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
    /// File extensions this server handles, without the dot.
    pub extensions: Vec<String>,
    /// The LSP language id sent on `didOpen`; defaults to `language`.
    pub language_id: Option<String>,
    pub startup_timeout_secs: u64,
    pub request_timeout_secs: u64,
    /// How long to wait for diagnostics after opening a file. They arrive as an
    /// unsolicited notification once the server has analysed it, so there is
    /// nothing to await other than time.
    pub diagnostics_wait_ms: u64,
    pub enabled: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            language: String::new(),
            command: String::new(),
            args: Vec::new(),
            extensions: Vec::new(),
            language_id: None,
            startup_timeout_secs: 60,
            request_timeout_secs: 30,
            diagnostics_wait_ms: 3_000,
            enabled: true,
        }
    }
}

impl ServerConfig {
    pub fn handles(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<protocol::Incoming>>>>;
type Complaint = Arc<Mutex<Option<String>>>;

/// What the server was last told a file contains.
struct Document {
    version: i64,
    text: String,
}
type Diagnostics = Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>;

pub struct Server {
    language: String,
    root: PathBuf,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    diagnostics: Diagnostics,
    opened: Mutex<HashMap<PathBuf, Document>>,
    next_id: AtomicU64,
    request_timeout: Duration,
    diagnostics_wait: Duration,
    complaint: Complaint,
    child: Mutex<Child>,
}

/// The program to start, looked up the way a shell would.
///
/// Windows searches `PATH` for `foo.exe` and consults `PATHEXT` only in a
/// shell, so a program installed as `npx.cmd` — which is how npm, uv and bun
/// install theirs — is "program not found" there while working everywhere
/// else. Everything a README tells someone to configure goes through this.
///
/// Copied rather than shared: `rook-mcp`, `rook-lsp` and `rook-skills` each
/// start a program somebody named in configuration, and the three sit on one
/// layer with nothing beneath them to hold it.
fn program(command: &str) -> std::path::PathBuf {
    match cfg!(windows) {
        true => resolved(
            command,
            &std::env::var_os("PATH").unwrap_or_default(),
            &std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into()),
        ),
        false => std::path::PathBuf::from(command),
    }
}

/// What Windows would start, given its two variables.
///
/// Apart from [`program`] so that all of it is reachable from a test: which
/// machine the test runs on is not what decides whether this is right.
fn resolved(command: &str, path: &std::ffi::OsStr, exts: &str) -> std::path::PathBuf {
    let named = std::path::Path::new(command);
    if named.extension().is_some() || named.parent() != Some(std::path::Path::new("")) {
        return named.to_path_buf();
    }
    let exts: Vec<&str> = exts.split(';').filter(|e| !e.is_empty()).collect();
    std::env::split_paths(path)
        .find_map(|dir| exts.iter().map(|ext| dir.join(format!("{command}{ext}"))).find(|c| c.is_file()))
        .unwrap_or_else(|| named.to_path_buf())
}

impl Server {
    pub async fn start(config: &ServerConfig, root: &Path) -> Result<Self> {
        let mut child = tokio::process::Command::new(program(&config.command))
            .args(&config.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| LspError::Spawn { command: config.command.clone(), source: e })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // Language servers are chatty on stderr; an undrained pipe fills and
        // blocks them mid-analysis.
        let complaint: Complaint = Default::default();
        let language = config.language.clone();
        let last = complaint.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(server = %language, "{line}");
                if !line.trim().is_empty() {
                    *last.lock().await = Some(line);
                }
            }
        });

        let pending: Pending = Default::default();
        let diagnostics: Diagnostics = Default::default();
        spawn_reader(config.language.clone(), stdout, pending.clone(), diagnostics.clone());

        let server = Self {
            language: config.language.clone(),
            root: root.to_path_buf(),
            stdin: Mutex::new(stdin),
            pending,
            diagnostics,
            opened: Default::default(),
            next_id: AtomicU64::new(1),
            request_timeout: Duration::from_secs(config.request_timeout_secs),
            diagnostics_wait: Duration::from_millis(config.diagnostics_wait_ms),
            complaint,
            child: Mutex::new(child),
        };

        server
            .request_with(
                "initialize",
                serde_json::json!({
                    "processId": std::process::id(),
                    "rootUri": protocol::to_uri(root),
                    "workspaceFolders": [{ "uri": protocol::to_uri(root), "name": "workspace" }],
                    "capabilities": {
                        "textDocument": {
                            "publishDiagnostics": {},
                            "definition": {},
                            "references": {},
                        },
                        "workspace": { "symbol": {} },
                    },
                }),
                Duration::from_secs(config.startup_timeout_secs),
            )
            .await?;
        server.notify("initialized", serde_json::json!({})).await?;
        Ok(server)
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// Tell the server what a file contains now, opening it the first time and
    /// sending a change after that.
    ///
    /// The agent's normal loop is edit, then check — so a server still holding
    /// the version it saw at open time reports diagnostics for code that no
    /// longer exists, which is worse than reporting none.
    /// Tell the server what this file says now, and hand back the same text.
    ///
    /// The text is returned rather than read again by the caller: a second read
    /// is a second answer, and a file that changed between them would leave the
    /// server holding one document while positions are computed against another.
    pub async fn sync(&self, path: &Path, language_id: &str) -> Result<String> {
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| LspError::Io { path: path.to_path_buf(), source: e })?;
        let uri = protocol::to_uri(path);

        let mut opened = self.opened.lock().await;
        let (method, params) = match opened.get_mut(path) {
            Some(document) if document.text == text => return Ok(text),
            Some(document) => {
                document.version += 1;
                document.text = text.clone();
                (
                    "textDocument/didChange",
                    serde_json::json!({
                        "textDocument": { "uri": uri, "version": document.version },
                        "contentChanges": [{ "text": text.clone() }],
                    }),
                )
            }
            None => {
                opened.insert(path.to_path_buf(), Document { version: 1, text: text.clone() });
                (
                    "textDocument/didOpen",
                    serde_json::json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id,
                            "version": 1,
                            "text": text.clone(),
                        }
                    }),
                )
            }
        };
        drop(opened);

        // Whatever the server said about the old text is now wrong, and leaving
        // it cached would let `diagnostics` return it before the new analysis
        // has arrived.
        self.diagnostics.lock().await.remove(&protocol::to_uri(path));
        self.notify(method, params).await?;
        Ok(text)
    }

    /// Diagnostics for a file, after giving the server time to produce them.
    pub async fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        let uri = protocol::to_uri(path);
        let deadline = std::time::Instant::now() + self.diagnostics_wait;
        loop {
            if let Some(found) = self.diagnostics.lock().await.get(&uri)
                && (!found.is_empty() || std::time::Instant::now() >= deadline)
            {
                return found.clone();
            }
            if std::time::Instant::now() >= deadline {
                return Vec::new();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn definition(&self, path: &Path, at: Position) -> Result<Vec<Location>> {
        let result = self.request("textDocument/definition", self.at(path, at)).await?;
        Ok(locations(result))
    }

    pub async fn references(&self, path: &Path, at: Position) -> Result<Vec<Location>> {
        let mut params = self.at(path, at);
        params["context"] = serde_json::json!({ "includeDeclaration": true });
        let result = self.request("textDocument/references", params).await?;
        Ok(locations(result))
    }

    pub async fn symbols(&self, query: &str) -> Result<Vec<Symbol>> {
        let result = self.request("workspace/symbol", serde_json::json!({ "query": query })).await?;
        Ok(result.as_array().map(|items| items.iter().filter_map(symbol).collect()).unwrap_or_default())
    }

    pub async fn shutdown(&self) {
        let _ = self.notify("exit", serde_json::json!({})).await;
        let _ = self.child.lock().await.kill().await;
    }

    fn at(&self, path: &Path, position: Position) -> serde_json::Value {
        serde_json::json!({
            "textDocument": { "uri": protocol::to_uri(path) },
            "position": position,
        })
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        self.request_with(method, params, self.request_timeout).await
    }

    async fn request_with(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let body = serde_json::to_string(&protocol::Request { jsonrpc: "2.0", id, method, params })
            .map_err(|e| self.transport(e.to_string()))?;
        if let Err(e) = self.write(&body).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        let message = match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(LspError::Timeout {
                    server: self.language.clone(),
                    method: method.into(),
                    timeout,
                });
            }
            Ok(Err(_)) => {
                return Err(LspError::Closed {
                    server: self.language.clone(),
                    complaint: self.complaint.lock().await.clone(),
                });
            }
            Ok(Ok(message)) => message,
        };

        if let Some(error) = message.error {
            return Err(LspError::Rpc {
                server: self.language.clone(),
                method: method.into(),
                message: error.message,
            });
        }
        Ok(message.result.unwrap_or(serde_json::Value::Null))
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let body = serde_json::to_string(&protocol::Notification { jsonrpc: "2.0", method, params })
            .map_err(|e| self.transport(e.to_string()))?;
        self.write(&body).await
    }

    async fn write(&self, body: &str) -> Result<()> {
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(framed.as_bytes()).await.map_err(|e| self.transport(e.to_string()))?;
        stdin.flush().await.map_err(|e| self.transport(e.to_string()))
    }

    fn transport(&self, message: String) -> LspError {
        LspError::Transport { server: self.language.clone(), message }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn spawn_reader(
    language: String,
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    diagnostics: Diagnostics,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let Some(length) = read_content_length(&mut reader).await else { break };
            let mut body = vec![0u8; length];
            if reader.read_exact(&mut body).await.is_err() {
                break;
            }
            let Ok(message) = serde_json::from_slice::<protocol::Incoming>(&body) else {
                tracing::debug!(server = %language, "unparsable message");
                continue;
            };

            match (message.id, message.method.as_deref()) {
                // A server request we do not implement; answering nothing is
                // fine for the handful they send during startup.
                (Some(_), Some(_)) => {}
                (Some(id), None) => {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(message);
                    }
                }
                (None, Some("textDocument/publishDiagnostics")) => {
                    if let Some(params) = message.params {
                        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
                        let found: Vec<Diagnostic> = params
                            .get("diagnostics")
                            .and_then(|d| serde_json::from_value(d.clone()).ok())
                            .unwrap_or_default();
                        diagnostics.lock().await.insert(uri, found);
                    }
                }
                _ => {}
            }
        }
        // The server is gone: waiters would otherwise hang until their timeout.
        pending.lock().await.clear();
    });
}

/// Read headers up to the blank line and return the body length.
async fn read_content_length<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Option<usize> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            return length;
        }
        if let Some(value) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().ok();
        }
    }
}

/// `definition` may answer with one location, a list, or link objects.
fn locations(result: serde_json::Value) -> Vec<Location> {
    fn one(value: &serde_json::Value) -> Option<Location> {
        let uri = value.get("uri").or_else(|| value.get("targetUri")).and_then(|u| u.as_str())?;
        let range = value.get("range").or_else(|| value.get("targetSelectionRange"))?;
        let start = range.get("start")?;
        Some(Location {
            path: protocol::from_uri(uri),
            line: start.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32 + 1,
            character: start.get("character").and_then(|c| c.as_u64()).unwrap_or(0) as u32 + 1,
        })
    }
    match result {
        serde_json::Value::Array(items) => items.iter().filter_map(one).collect(),
        value => one(&value).into_iter().collect(),
    }
}

fn symbol(value: &serde_json::Value) -> Option<Symbol> {
    let location = value.get("location")?;
    let uri = location.get("uri").and_then(|u| u.as_str())?;
    let line = location.pointer("/range/start/line").and_then(|l| l.as_u64()).unwrap_or(0);
    Some(Symbol {
        name: value.get("name")?.as_str()?.to_string(),
        kind: protocol::symbol_kind(value.get("kind").and_then(|k| k.as_u64()).unwrap_or(0)),
        path: protocol::from_uri(uri),
        line: line as u32 + 1,
        container: value
            .get("containerName")
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty())
            .map(str::to_string),
    })
}

/// Where a name first appears in a file, as a position a language server
/// accepts.
///
/// The alternative is asking the model for a line and column, which it does not
/// reliably know and cannot check — a name it can read off the source is the
/// thing it actually has.
pub fn locate(text: &str, symbol: &str) -> Option<Position> {
    for (line, content) in text.lines().enumerate() {
        let mut from = 0;
        while let Some(offset) = content[from..].find(symbol) {
            let at = from + offset;
            let before = content[..at].chars().next_back();
            let after = content[at + symbol.len()..].chars().next();
            let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if boundary(before) && boundary(after) {
                return Some(Position { line: line as u32, character: content[..at].chars().count() as u32 });
            }
            from = at + symbol.len();
        }
    }
    None
}
