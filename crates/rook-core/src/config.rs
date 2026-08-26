//! Configuration, with defaults chosen so an unconfigured install is still safe.

use serde::{Deserialize, Serialize};

use rook_store::RetentionPolicy;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid: {message}")]
    Parse { path: std::path::PathBuf, message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub agent: AgentConfig,
    pub storage: StorageConfig,
    pub server: ServerConfig,
    pub sandbox: SandboxConfig,
    pub telemetry: TelemetryConfig,
    pub memory: MemoryConfig,
    /// Commands run at points in a turn, as `[[hooks]]` tables.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<crate::hooks::HookConfig>,
    /// External tool servers, as `[[mcp]]` tables in config.toml. Omitted from a
    /// written config when empty: TOML cannot hold both `mcp = []` and a later
    /// `[[mcp]]` table, so emitting the empty array would block the documented
    /// way of adding the first server.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<rook_mcp::ServerConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// `provider/model`, e.g. `ollama/qwen3-coder:30b` or `anthropic/claude-opus-5`.
    pub model: String,
    /// Fraction of the model's context the agent will fill before compacting.
    pub compact_at: f32,
    pub max_steps: u32,
    /// Send skill cards rather than skill bodies, loading a body only when the
    /// model asks for it. Off means every enabled skill is always in context.
    pub lazy_skills: bool,
    /// Same idea for tool schemas.
    pub lazy_tools: bool,
    /// How many delegated sub-tasks may run at once. Bounded because each one
    /// spends tokens against the same budget and the same provider rate limit.
    pub max_parallel_subagents: usize,
    /// How long the model may go silent mid-stream before the turn gives up.
    /// A dropped connection is indistinguishable from deep thought without it.
    pub stream_idle_timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    /// Ceiling on what recalled facts may cost in the system prompt. Memory that
    /// silently grows into the context window is the failure this prevents.
    pub context_budget_tokens: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { enabled: true, context_budget_tokens: 600 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// zstd level for new objects. 9 is the default knee; 19 is for archives.
    pub compression_level: i32,
    /// Retrain dictionaries once this many objects of a kind exist.
    pub train_dictionaries_after: usize,
    pub dictionary_bytes: usize,
    pub retention: RetentionPolicy,
    /// Run prune + gc on daemon start and then on this interval.
    pub maintenance_interval_hours: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub port: u16,
    /// Bound to loopback by default. An agent's transcript is the most sensitive
    /// thing on a developer's machine; it does not get exposed by accident.
    pub allow_remote: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub mode: rook_tools::policy::Mode,
    /// Patterns the agent may act on without asking. A plain string matches as a
    /// substring; `/…/` is a regular expression.
    pub allow: Vec<String>,
    /// Patterns that always prompt, even in `auto` mode.
    pub ask: Vec<String>,
    /// Patterns refused outright, which no approval can override.
    pub deny: Vec<String>,
    /// Cap on a single command's captured output, before it is stored and
    /// summarised rather than pasted into context.
    pub max_output_bytes: usize,
    pub command_timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Nothing leaves the machine. Present so the answer is discoverable rather
    /// than a question.
    pub upload: bool,
    pub log_level: String,
    /// Hard cap on the local log directory.
    pub max_log_bytes: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "ollama/qwen3-coder:30b".into(),
            compact_at: 0.75,
            max_steps: 200,
            lazy_skills: true,
            lazy_tools: true,
            max_parallel_subagents: 4,
            stream_idle_timeout_secs: 90,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            compression_level: 9,
            train_dictionaries_after: 512,
            dictionary_bytes: 16 * 1024,
            retention: RetentionPolicy::default(),
            maintenance_interval_hours: 24,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { bind: "127.0.0.1".into(), port: 7717, allow_remote: false }
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: rook_tools::policy::Mode::Ask,
            allow: vec![
                "git status".into(),
                "git diff".into(),
                "git log".into(),
                "/^(ls|cat|head|tail|wc|rg|grep|find)\\b/".into(),
            ],
            ask: Vec::new(),
            // Denied outright: the shapes that turn a bad turn into an
            // unrecoverable one. Anchored deliberately — a substring rule for
            // `rm -rf /` would also block `rm -rf /tmp/scratch`, and a deny list
            // that cries wolf gets turned off.
            deny: [
                r"/\brm\s+(-[a-zA-Z]+\s+)*\/(\s|\*|$)/",
                r"/\bmkfs(\.|\s)/",
                r"/\bdd\s+[^|]*\bof=\/dev\//",
                r"/>\s*\/dev\/(sd|nvme|disk)/",
                r"/:\(\)\s*\{.*\|.*&.*\}\s*;\s*:/",
                r"/\bchmod\s+-R\s+777\s+\/\s*$/",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_output_bytes: 256 * 1024,
            command_timeout_secs: 120,
        }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { upload: false, log_level: "warn".into(), max_log_bytes: 64 << 20 }
    }
}

impl AgentConfig {
    pub fn stream_idle(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.stream_idle_timeout_secs)
    }
}

impl Config {
    /// A malformed config is an error, not a fallback to defaults: silently
    /// ignoring it would also silently change which model the agent talks to.
    pub fn load() -> std::result::Result<Self, ConfigError> {
        let path = crate::paths::config_file();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        toml::from_str(&text).map_err(|e| ConfigError::Parse { path, message: e.to_string() })
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = crate::paths::config_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }
}
