//! Code intelligence as tools the agent can call.
//!
//! Servers start lazily and one per language: rust-analyzer takes seconds to
//! index, and paying that on every `rook` invocation to answer a question that
//! never comes would make the whole tool feel slow.
//!
//! The requested feature was "auto-detect and auto-install". Detection is here —
//! a known server on `PATH` with matching files in the workspace is used without
//! configuration. Installation is not: downloading and running a binary on the
//! user's behalf is a different decision from using one they already have.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use rook_llm::ToolSpec;
use rook_lsp::{Server, ServerConfig};
use rook_tools::{Result as ToolResult, Tool, ToolBox, ToolContext, ToolOutcome};

/// Servers worth trying without being asked, when their binary is present.
pub fn detected() -> Vec<ServerConfig> {
    // The node-based servers multiplex several transports and need to be told
    // which one; the rest speak stdio unprompted.
    const KNOWN: [(&str, &str, &[&str], &[&str]); 5] = [
        ("rust", "rust-analyzer", &[], &["rs"]),
        ("typescript", "typescript-language-server", &["--stdio"], &["ts", "tsx", "js", "jsx"]),
        ("python", "pyright-langserver", &["--stdio"], &["py", "pyi"]),
        ("go", "gopls", &[], &["go"]),
        ("c", "clangd", &[], &["c", "h", "cpp", "hpp", "cc"]),
    ];
    KNOWN
        .iter()
        .filter(|(_, command, _, _)| on_path(command))
        .map(|(language, command, args, extensions)| ServerConfig {
            language: (*language).into(),
            command: (*command).into(),
            args: args.iter().map(|a| (*a).into()).collect(),
            extensions: extensions.iter().map(|e| (*e).into()).collect(),
            ..Default::default()
        })
        .collect()
}

fn on_path(command: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(command);
        candidate.is_file() || candidate.with_extension("exe").is_file()
    })
}

/// The language servers available in one workspace.
pub struct Servers {
    configs: Vec<ServerConfig>,
    root: PathBuf,
    running: Mutex<HashMap<String, Arc<Server>>>,
}

impl Servers {
    pub fn new(configs: Vec<ServerConfig>, root: &Path) -> Arc<Self> {
        Arc::new(Self {
            configs: configs.into_iter().filter(|c| c.enabled).collect(),
            // Canonical, because a server answers in whatever spelling it
            // resolved to: on macOS `/tmp` and `/private/tmp` are the same
            // directory and results arrive under both.
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            running: Mutex::new(HashMap::new()),
        })
    }

    /// Paths as the model should read them: relative to the workspace, since it
    /// asked in those terms and will act in them.
    pub fn shorten(&self, path: &str) -> String {
        Path::new(path)
            .strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.to_string())
    }

    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    pub fn languages(&self) -> Vec<&str> {
        self.configs.iter().map(|c| c.language.as_str()).collect()
    }

    /// The server for a file, started if this is the first file to need it.
    pub async fn for_path(&self, path: &Path) -> rook_lsp::Result<(Arc<Server>, String)> {
        let config = self
            .configs
            .iter()
            .find(|c| c.handles(path))
            .ok_or_else(|| rook_lsp::LspError::NoServer(path.display().to_string()))?;
        let language_id = config.language_id.clone().unwrap_or_else(|| config.language.clone());
        Ok((self.start(config).await?, language_id))
    }

    /// Any running or startable server, for questions that are not about one
    /// file — a workspace symbol search has to pick one.
    pub async fn any(&self) -> rook_lsp::Result<Arc<Server>> {
        let config =
            self.configs.first().ok_or_else(|| rook_lsp::LspError::NoServer("this workspace".into()))?;
        self.start(config).await
    }

    async fn start(&self, config: &ServerConfig) -> rook_lsp::Result<Arc<Server>> {
        let mut running = self.running.lock().await;
        if let Some(server) = running.get(&config.language) {
            return Ok(server.clone());
        }
        let server = Arc::new(Server::start(config, &self.root).await?);
        running.insert(config.language.clone(), server.clone());
        Ok(server)
    }

    pub async fn shutdown(&self) {
        for server in self.running.lock().await.values() {
            server.shutdown().await;
        }
    }

    /// Open a file and return its text and the exact path the server was told
    /// about — every later query must name the same one, or the server answers
    /// that it has no such document.
    async fn prepare(&self, path: &Path) -> rook_lsp::Result<Opened> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let (server, language_id) = self.for_path(&path).await?;
        server.sync(&path, &language_id).await?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| rook_lsp::LspError::Io { path: path.clone(), source: e })?;
        Ok(Opened { server, text, path })
    }
}

pub struct Opened {
    pub server: Arc<Server>,
    pub text: String,
    pub path: PathBuf,
}

/// Register the code-intelligence tools, if any server is available.
pub fn register(tools: &mut ToolBox, servers: Arc<Servers>) {
    if servers.is_empty() {
        return;
    }
    tools.register(Arc::new(Diagnostics(servers.clone())));
    tools.register(Arc::new(Definition(servers.clone())));
    tools.register(Arc::new(References(servers.clone())));
    tools.register(Arc::new(FindSymbol(servers)));
}

fn path_and_symbol(args: &serde_json::Value) -> (String, String) {
    let get = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    (get("path"), get("symbol"))
}

pub struct Diagnostics(Arc<Servers>);

#[async_trait]
impl Tool for Diagnostics {
    fn name(&self) -> &str {
        "diagnostics"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "diagnostics".into(),
            description: "What the language server thinks is wrong with a file — type errors and \
                          warnings, without running a build."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> ToolResult<ToolOutcome> {
        let path = ctx.resolve(&path_and_symbol(args).0)?;
        let opened = match self.0.prepare(&path).await {
            Ok(opened) => opened,
            Err(e) => return Ok(ToolOutcome::error(e.to_string())),
        };
        let found = opened.server.diagnostics(&opened.path).await;
        if found.is_empty() {
            return Ok(ToolOutcome::ok(format!(
                "no diagnostics for {}",
                self.0.shorten(&opened.path.display().to_string())
            )));
        }
        let lines: Vec<String> = found
            .iter()
            .map(|d| {
                format!(
                    "{}:{}:{}: {}: {}",
                    self.0.shorten(&opened.path.display().to_string()),
                    d.range.start.line + 1,
                    d.range.start.character + 1,
                    d.severity_name(),
                    d.message
                )
            })
            .collect();
        Ok(ToolOutcome::ok(lines.join("\n")).with("count", found.len() as u64))
    }
}

pub struct Definition(Arc<Servers>);

#[async_trait]
impl Tool for Definition {
    fn name(&self) -> &str {
        "definition"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "definition".into(),
            description: "Where a name used in a file is defined. Give the name as it appears in \
                          the source; no line numbers needed."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file the name is used in." },
                    "symbol": { "type": "string", "description": "The name, exactly as written." }
                },
                "required": ["path", "symbol"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> ToolResult<ToolOutcome> {
        let (raw_path, symbol) = path_and_symbol(args);
        let path = ctx.resolve(&raw_path)?;
        let opened = match self.0.prepare(&path).await {
            Ok(opened) => opened,
            Err(e) => return Ok(ToolOutcome::error(e.to_string())),
        };
        let Some(at) = rook_lsp::locate(&opened.text, &symbol) else {
            return Ok(ToolOutcome::error(
                rook_lsp::LspError::NoSuchSymbol { symbol: symbol.clone(), path: raw_path.clone() }
                    .to_string(),
            ));
        };
        match opened.server.definition(&opened.path, at).await {
            Ok(found) if found.is_empty() => {
                Ok(ToolOutcome::ok(format!("no definition found for {symbol:?}")))
            }
            Ok(found) => Ok(ToolOutcome::ok(
                found
                    .iter()
                    .map(|l| format!("{}:{}:{}", self.0.shorten(&l.path), l.line, l.character))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            Err(e) => Ok(ToolOutcome::error(e.to_string())),
        }
    }
}

pub struct References(Arc<Servers>);

#[async_trait]
impl Tool for References {
    fn name(&self) -> &str {
        "references"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "references".into(),
            description: "Everything that actually refers to a name, as the type checker sees it \
                          — not every place the text happens to appear."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "symbol": { "type": "string" }
                },
                "required": ["path", "symbol"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> ToolResult<ToolOutcome> {
        let (raw_path, symbol) = path_and_symbol(args);
        let path = ctx.resolve(&raw_path)?;
        let opened = match self.0.prepare(&path).await {
            Ok(opened) => opened,
            Err(e) => return Ok(ToolOutcome::error(e.to_string())),
        };
        let Some(at) = rook_lsp::locate(&opened.text, &symbol) else {
            return Ok(ToolOutcome::error(
                rook_lsp::LspError::NoSuchSymbol { symbol: symbol.clone(), path: raw_path.clone() }
                    .to_string(),
            ));
        };
        match opened.server.references(&opened.path, at).await {
            Ok(found) if found.is_empty() => Ok(ToolOutcome::ok(format!("nothing refers to {symbol:?}"))),
            Ok(found) => {
                let mut lines: Vec<String> = found
                    .iter()
                    .map(|l| format!("{}:{}:{}", self.0.shorten(&l.path), l.line, l.character))
                    .collect();
                // A server that indexed the same file under two spellings of its
                // path reports each use twice; shortening makes them identical.
                lines.sort();
                lines.dedup();
                let count = lines.len();
                lines.truncate(ctx.max_output_bytes / 64);
                Ok(ToolOutcome::ok(lines.join("\n")).with("count", count as u64))
            }
            Err(e) => Ok(ToolOutcome::error(e.to_string())),
        }
    }
}

pub struct FindSymbol(Arc<Servers>);

#[async_trait]
impl Tool for FindSymbol {
    fn name(&self) -> &str {
        "find_symbol"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_symbol".into(),
            description: "Find a function, type or constant anywhere in the workspace by name.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, _ctx: &ToolContext, args: &serde_json::Value) -> ToolResult<ToolOutcome> {
        let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
        let server = match self.0.any().await {
            Ok(server) => server,
            Err(e) => return Ok(ToolOutcome::error(e.to_string())),
        };
        match server.symbols(query).await {
            Ok(found) if found.is_empty() => Ok(ToolOutcome::ok(format!("no symbol matching {query:?}"))),
            Ok(found) => Ok(ToolOutcome::ok(
                found
                    .iter()
                    .take(100)
                    .map(|s| {
                        let container =
                            s.container.as_deref().map(|c| format!(" in {c}")).unwrap_or_default();
                        format!("{} {}{container} — {}:{}", s.kind, s.name, self.0.shorten(&s.path), s.line)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            Err(e) => Ok(ToolOutcome::error(e.to_string())),
        }
    }
}
