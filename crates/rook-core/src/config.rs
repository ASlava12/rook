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
pub struct Config {
    pub agent: AgentConfig,
    pub storage: StorageConfig,
    pub server: ServerConfig,
    pub sandbox: SandboxConfig,
    pub telemetry: TelemetryConfig,
    pub memory: MemoryConfig,
    pub web: WebConfig,
    /// Language servers, as `[[lsp]]` tables. When empty, known servers found
    /// on `PATH` are used.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lsp: Vec<rook_lsp::ServerConfig>,
    /// Where `skills search` and `skills install` look: a git repository or a
    /// directory. Nothing is fetched until one of those is run, or the agent
    /// asks and is approved.
    #[serde(default = "default_skill_sources")]
    pub skill_sources: Vec<String>,
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

impl Default for Config {
    fn default() -> Self {
        // Spelled out rather than derived: a `#[serde(default = "…")]` applies
        // when a field is missing from a file, and says nothing about the
        // config a machine with no file at all gets.
        Self {
            agent: AgentConfig::default(),
            storage: StorageConfig::default(),
            sandbox: SandboxConfig::default(),
            telemetry: TelemetryConfig::default(),
            memory: MemoryConfig::default(),
            web: WebConfig::default(),
            server: Default::default(),
            lsp: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            skill_sources: default_skill_sources(),
        }
    }
}

/// The Agent Skills repository, which is where the format's own examples live.
/// Replace it or add to it; it is a starting point rather than a blessing, and
/// installing from anywhere means reading what you installed.
fn default_skill_sources() -> Vec<String> {
    vec!["https://github.com/anthropics/skills".into()]
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
    /// How much thinking to allow: low, medium, high, xhigh or max. `xhigh`
    /// suits most coding work; sub-agents are run at `low` regardless.
    pub effort: String,
    /// Ask for a brief plan before multi-step work. One line in the system
    /// prompt, not a checklist tool — see ADR-0010.
    pub plan_first: bool,
    /// Overrides the provider's assumed context length, which is guesswork for
    /// anything self-hosted: a local model may serve 8k or a million.
    pub context_window: Option<usize>,
    /// How many delegated sub-tasks may run at once. Bounded because each one
    /// spends tokens against the same budget and the same provider rate limit.
    pub max_parallel_subagents: usize,
    /// How many sub-agents one turn may start in total, counting the ones its
    /// children start. `max_parallel_subagents` paces them; this is what stops
    /// a single tool call from being a thousand model calls, because the list
    /// of tasks is written by the model and nothing else bounds its length.
    pub max_subagents_per_turn: usize,
    /// How long the model may go silent mid-stream before the turn gives up.
    /// A dropped connection is indistinguishable from deep thought without it.
    pub stream_idle_timeout_secs: u64,
    /// How many skills the catalog names in the system prompt. The catalog is
    /// paid for on every request, and a machine that has collected skills for a
    /// year would otherwise pay for all of them; the ones left out are still
    /// reachable, because `load_skill` answers a miss with what it does have.
    pub max_skill_cards: usize,
    /// How long a question put to the person — an approval, or the `ask` tool —
    /// waits before it counts as unanswered. A closed tab or an abandoned
    /// terminal would otherwise hold the turn, and the store's write lock with
    /// it, for as long as the process lives.
    pub answer_timeout_secs: u64,
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
    /// Never collect an object written this recently, whatever the marking says.
    /// It is unreachable between being written and the event that names it, and
    /// maintenance runs on a timer while turns do.
    pub gc_grace_secs: i64,
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

/// Reaching the network.
///
/// Off by default and meant to stay off for anyone who does not want it: the
/// point of this agent is that it runs here, and a page it fetches is somebody
/// else's text arriving in the model's context.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Offer `web_fetch` at all. Nothing reaches the network while this is off.
    pub enabled: bool,
    /// How long one page has to arrive.
    pub timeout_secs: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { enabled: false, timeout_secs: 30 }
    }
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
    /// Lets file tools read and write outside the workspace, including through
    /// a symlink that leads out of it. Off by default: a workspace that cannot
    /// be left is the one boundary a prompt cannot argue its way past.
    pub allow_outside_workspace: bool,
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
            effort: "high".into(),
            plan_first: true,
            context_window: None,
            max_parallel_subagents: 4,
            max_subagents_per_turn: 16,
            max_skill_cards: 50,
            stream_idle_timeout_secs: 90,
            answer_timeout_secs: 600,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            compression_level: 9,
            gc_grace_secs: 600,
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

/// Where a command starts: the beginning, or after a shell separator, allowing
/// for the wrappers that precede one.
///
/// Not shell parsing — a dangerous command quoted inside a string after a `;`
/// still matches, which errs towards refusing. What it buys is that mentioning
/// a word is not the same as running it.
const COMMAND: &str = r"(^|[;&|]\s*|\n\s*)(sudo\s+|doas\s+|env\s+\S+=\S+\s+)*";

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
            // unrecoverable one. Anchored twice over, because a deny list that
            // cries wolf gets turned off and nothing can override a denial. The
            // argument is anchored — a substring rule for `rm -rf /` would also
            // block `rm -rf /tmp/scratch` — and so is the command, or `grep -r
            // mkfs docs/` is refused for saying the word.
            deny: [
                &format!(r"/{COMMAND}rm\s+(-[a-zA-Z]+\s+)*\/(\s|\*|$)/"),
                &format!(r"/{COMMAND}mkfs(\.|\s)/"),
                &format!(r"/{COMMAND}dd\s+[^|]*\bof=\/dev\//"),
                r"/>\s*\/dev\/(sd|nvme|disk)/",
                r"/:\(\)\s*\{.*\|.*&.*\}\s*;\s*:/",
                &format!(r"/{COMMAND}chmod\s+-R\s+777\s+\/\s*$/"),
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_output_bytes: 256 * 1024,
            command_timeout_secs: 120,
            allow_outside_workspace: false,
        }
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { upload: false, log_level: "warn".into(), max_log_bytes: 64 << 20 }
    }
}

impl AgentConfig {
    /// An unrecognised setting falls back to the default rather than failing a
    /// turn: the provider would reject it, and the value is a typo, not a
    /// reason to stop working.
    pub fn effort(&self) -> rook_llm::Effort {
        rook_llm::Effort::parse(&self.effort).unwrap_or_default()
    }

    pub fn answer_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.answer_timeout_secs)
    }

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
            crate::paths::private_dir(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, text)?;
        // An MCP server's headers and environment live in here, and that is
        // where its API key goes — the field says so. Model keys are read from
        // the environment and never written, but this file is not therefore
        // free of secrets.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}
