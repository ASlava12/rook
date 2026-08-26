//! The environment a skill is being resolved against.
//!
//! A skill that shells out to `sed -i` is correct on GNU userland and wrong on
//! BSD; one that uses `tokio::task::spawn_blocking` needs a Rust new enough to
//! have it. Rather than let the model discover this by failing, the environment
//! is detected once and skills declare what they need against it.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Environment {
    /// `linux`, `macos`, `windows`, `freebsd`, ...
    pub os: String,
    /// `x86_64`, `aarch64`, ...
    pub arch: String,
    /// `gnu`, `bsd`, `msvc`, `musl` — decides which flavour of the classic
    /// command-line tools a skill can assume.
    pub userland: String,
    /// Detected language toolchains, e.g. `rust -> 1.97.1`, `python -> 3.12.4`.
    pub languages: BTreeMap<String, String>,
    /// Detected standalone tools, e.g. `git -> 2.45.0`, `docker -> 27.1.1`.
    pub tools: BTreeMap<String, String>,
    /// The agent's own version, so a skill can require a feature this build has.
    pub agent_version: String,
}

/// How a language or tool version is discovered.
pub struct Probe {
    pub key: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
}

pub const LANGUAGE_PROBES: &[Probe] = &[
    Probe { key: "rust", command: "rustc", args: &["--version"] },
    Probe { key: "python", command: "python3", args: &["--version"] },
    Probe { key: "node", command: "node", args: &["--version"] },
    Probe { key: "go", command: "go", args: &["version"] },
    Probe { key: "java", command: "java", args: &["-version"] },
    Probe { key: "ruby", command: "ruby", args: &["--version"] },
    Probe { key: "php", command: "php", args: &["--version"] },
    Probe { key: "dotnet", command: "dotnet", args: &["--version"] },
];

pub const TOOL_PROBES: &[Probe] = &[
    Probe { key: "git", command: "git", args: &["--version"] },
    Probe { key: "cargo", command: "cargo", args: &["--version"] },
    Probe { key: "npm", command: "npm", args: &["--version"] },
    Probe { key: "uv", command: "uv", args: &["--version"] },
    Probe { key: "docker", command: "docker", args: &["--version"] },
    Probe { key: "kubectl", command: "kubectl", args: &["version", "--client"] },
    Probe { key: "rg", command: "rg", args: &["--version"] },
    Probe { key: "gh", command: "gh", args: &["--version"] },
];

impl Environment {
    /// Detect the current environment. Probes run once; a missing tool is simply
    /// absent from the map rather than an error.
    pub fn detect(agent_version: &str) -> Self {
        let mut languages = BTreeMap::new();
        for probe in LANGUAGE_PROBES {
            if let Some(v) = probe_version(probe) {
                languages.insert(probe.key.to_string(), v);
            }
        }
        let mut tools = BTreeMap::new();
        for probe in TOOL_PROBES {
            if let Some(v) = probe_version(probe) {
                tools.insert(probe.key.to_string(), v);
            }
        }
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            userland: detect_userland(),
            languages,
            tools,
            agent_version: agent_version.to_string(),
        }
    }

    /// An environment with nothing detected — used in tests and when resolving
    /// for a target other than the machine we are running on.
    pub fn bare(os: &str, arch: &str, agent_version: &str) -> Self {
        Self {
            os: os.to_string(),
            arch: arch.to_string(),
            userland: userland_for(os),
            languages: BTreeMap::new(),
            tools: BTreeMap::new(),
            agent_version: agent_version.to_string(),
        }
    }

    pub fn with_language(mut self, key: &str, version: &str) -> Self {
        self.languages.insert(key.to_string(), version.to_string());
        self
    }

    pub fn with_tool(mut self, key: &str, version: &str) -> Self {
        self.tools.insert(key.to_string(), version.to_string());
        self
    }
}

fn detect_userland() -> String {
    userland_for(std::env::consts::OS)
}

fn userland_for(os: &str) -> String {
    match os {
        "linux" => "gnu",
        "macos" | "freebsd" | "openbsd" | "netbsd" | "dragonfly" => "bsd",
        "windows" => "msvc",
        _ => "unknown",
    }
    .to_string()
}

fn probe_version(probe: &Probe) -> Option<String> {
    let out = Command::new(probe.command).args(probe.args).output().ok()?;
    if !out.status.success() && out.stderr.is_empty() {
        return None;
    }
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    extract_version(&text)
}

/// Pull the first `x.y[.z]` looking token out of a `--version` banner.
pub fn extract_version(text: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for token in text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '"' || c == ',') {
        // Strip any leading prefix: `v1.2.3`, `go1.22.5`, `V2.0`.
        let t = token.trim_start_matches(|c: char| !c.is_ascii_digit());
        let core: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let parts: Vec<&str> = core.split('.').filter(|p| !p.is_empty()).collect();
        if parts.len() < 2 || parts.iter().any(|p| p.parse::<u64>().is_err()) {
            continue;
        }
        let normalized = match parts.len() {
            2 => format!("{}.{}.0", parts[0], parts[1]),
            _ => format!("{}.{}.{}", parts[0], parts[1], parts[2]),
        };
        best = Some(normalized);
        break;
    }
    best
}
