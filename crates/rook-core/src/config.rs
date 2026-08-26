//! Configuration, with defaults chosen so an unconfigured install is still safe.

use serde::{Deserialize, Serialize};

use rook_store::RetentionPolicy;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub agent: AgentConfig,
    pub storage: StorageConfig,
    pub server: ServerConfig,
    pub sandbox: SandboxConfig,
    pub telemetry: TelemetryConfig,
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
    /// Commands the agent may run without asking.
    pub allow: Vec<String>,
    /// Commands that are refused even when the user approves interactively.
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
            allow: vec!["git status".into(), "git diff".into(), "cargo check".into()],
            // Denied outright: these are the shapes that turn a bad turn into an
            // unrecoverable one.
            deny: vec!["rm -rf /".into(), "mkfs".into(), "dd if=".into(), ":(){ :|:& };:".into()],
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

impl Config {
    pub fn load() -> Self {
        let path = crate::paths::config_file();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("{}: {e}; falling back to defaults", path.display());
                Self::default()
            }
        }
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
