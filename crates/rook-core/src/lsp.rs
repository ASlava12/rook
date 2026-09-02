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
        .filter_map(|(language, command, args, extensions)| {
            // What `rook lsp install` fetched wins over PATH: it is the one the
            // user asked for by name, and it has a digest on record.
            let command = installed(command).or_else(|| on_path(command).then(|| (*command).to_string()))?;
            Some((language, command, args, extensions))
        })
        .map(|(language, command, args, extensions)| ServerConfig {
            language: (*language).into(),
            command,
            args: args.iter().map(|a| (*a).into()).collect(),
            extensions: extensions.iter().map(|e| (*e).into()).collect(),
            ..Default::default()
        })
        .collect()
}

/// The binary `rook lsp install` put under the state directory, if any.
fn installed(command: &str) -> Option<String> {
    let current = crate::install::current(command);
    current.is_file().then(|| current.to_string_lossy().into_owned())
}

fn on_path(command: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(command);
        candidate.is_file() || candidate.with_extension("exe").is_file()
    })
}

/// The servers this configuration actually asks for: what was configured, or
/// what was found when nothing was, minus the ones turned off.
///
/// Asked by the agent when it builds its tools and by `doctor` when it reports
/// what is here, and they were answering it differently — doctor called a server
/// the user had disabled broken.
pub fn configured(config: &crate::Config) -> Vec<ServerConfig> {
    let listed = if config.lsp.is_empty() { detected() } else { config.lsp.clone() };
    listed.into_iter().filter(|c| c.enabled).collect()
}

/// The configured servers that have something to work on here.
///
/// A `rust-analyzer` on `PATH` was offered to a Python project, and the model
/// spent a step asking it about a symbol. A server for a language the workspace
/// does not contain costs the prompt its four tool schemas and buys nothing —
/// and when none of them qualify, the tools are not advertised at all.
pub fn for_workspace(config: &crate::Config, root: &Path) -> Vec<ServerConfig> {
    let configured = configured(config);
    if configured.is_empty() {
        return configured;
    }
    // Bounded: the answer is "is there one of these anywhere near the top", and
    // a monorepo should not be walked to the bottom to find out.
    const LOOKED_AT: usize = 4_000;
    let mut wanted: Vec<bool> = vec![false; configured.len()];
    for entry in ignore::WalkBuilder::new(root)
        .max_depth(Some(6))
        .follow_links(false)
        .require_git(false)
        .build()
        .flatten()
        .take(LOOKED_AT)
    {
        for (i, server) in configured.iter().enumerate() {
            wanted[i] |= server.handles(entry.path());
        }
        if wanted.iter().all(|w| *w) {
            break;
        }
    }
    configured.into_iter().zip(wanted).filter(|(_, wanted)| *wanted).map(|(c, _)| c).collect()
}

/// The language servers available in one workspace.
pub struct Servers {
    configs: Vec<ServerConfig>,
    root: PathBuf,
    running: Mutex<HashMap<String, Arc<Server>>>,
    /// Why a server would not start, so the cost of finding out is paid once.
    /// rustup installs a `rust-analyzer` shim whether or not the component is,
    /// and a turn that asked it three questions waited for three failures.
    failed: Mutex<HashMap<String, String>>,
}

impl Servers {
    pub fn new(configs: Vec<ServerConfig>, root: &Path) -> Arc<Self> {
        Arc::new(Self {
            configs,
            // Canonical, because a server answers in whatever spelling it
            // resolved to: on macOS `/tmp` and `/private/tmp` are the same
            // directory and results arrive under both.
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            running: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashMap::new()),
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
    ///
    /// Each in turn rather than only the first: in a Python project with a
    /// `rust-analyzer` shim on PATH, the first is the one that cannot start.
    pub async fn any(&self) -> rook_lsp::Result<Arc<Server>> {
        let mut last = None;
        for config in &self.configs {
            match self.start(config).await {
                Ok(server) => return Ok(server),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| rook_lsp::LspError::NoServer("this workspace".into())))
    }

    async fn start(&self, config: &ServerConfig) -> rook_lsp::Result<Arc<Server>> {
        if let Some(complaint) = self.failed.lock().await.get(&config.language) {
            return Err(rook_lsp::LspError::Closed {
                server: config.language.clone(),
                complaint: Some(complaint.clone()),
            });
        }
        let mut running = self.running.lock().await;
        if let Some(server) = running.get(&config.language) {
            return Ok(server.clone());
        }
        match Server::start(config, &self.root).await {
            Ok(server) => {
                let server = Arc::new(server);
                running.insert(config.language.clone(), server.clone());
                Ok(server)
            }
            Err(e) => {
                self.failed.lock().await.insert(config.language.clone(), e.to_string());
                Err(e)
            }
        }
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
        // The very text the server was given: reading it again would be a second
        // answer, and a file that changed in between would leave every position
        // computed here pointing somewhere else in the server's document.
        let text = server.sync(&path, &language_id).await?;
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
