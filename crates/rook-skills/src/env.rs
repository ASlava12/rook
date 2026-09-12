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

    #[doc(hidden)]
    pub fn with_language(mut self, key: &str, version: &str) -> Self {
        self.languages.insert(key.to_string(), version.to_string());
        self
    }

    #[doc(hidden)]
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

/// Long enough that a `--version` on a loaded machine is never cut off, short
/// enough that one that will never answer does not hold the agent.
///
/// There was no bound at all: `output()` waits for the child, and sixteen of
/// these run before the first turn, before `/api/health` answers, and in
/// `doctor`. One tool that hangs — a shim waiting on a lock, an installer
/// asking a question nobody can see — hung all of it, with nothing to say so.
/// Read off hermes, where the same probe needed the same two things: a deadline
/// and a kill, because giving up on a child that is still running leaves it
/// running.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

fn probe_version(probe: &Probe) -> Option<String> {
    let out = ran_within(Command::new(program(probe.command)).args(probe.args), PATIENCE)?;
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

/// Run a command and give up on it, killing it rather than leaving it.
///
/// Polled rather than waited on a thread: this is called from a `OnceLock` on
/// whichever thread first asks what the machine has, and a probe is a hundred
/// milliseconds of work. Killing is the half that is easy to leave out — a
/// deadline that only stops waiting leaves the child holding whatever it was
/// holding, and sixteen of those is a startup that gets slower every time.
fn ran_within(command: &mut Command, patience: std::time::Duration) -> Option<std::process::Output> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + patience;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Err(_) => return None,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                // Reaped, so the answer is not a process nobody waited for.
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
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

// Every test here spawns `sh`, so the module is unix's: left on everywhere,
// Windows compiles a module whose only import nothing uses.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Sixteen of these run before the first turn, before `/api/health`
    /// answers, and in `doctor` — and `output()` waits for the child however
    /// long it takes. One tool that never answers hung all of it.
    #[test]
    fn a_probe_that_will_not_answer_is_given_up_on_and_killed() {
        let began = std::time::Instant::now();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let out = ran_within(&mut command, std::time::Duration::from_millis(200));

        assert!(out.is_none(), "a probe that did not answer answers nothing");
        // The precondition: it gave up rather than the command being quick.
        assert!(began.elapsed() < std::time::Duration::from_secs(5), "it gave up: {:?}", began.elapsed());
    }

    /// And the half that is easy to leave out: giving up on a child that is
    /// still running leaves it running.
    #[test]
    fn the_child_of_a_probe_that_was_given_up_on_is_gone() {
        let marker = format!("rook-probe-{}", std::process::id());
        let mut command = Command::new("sh");
        command.args(["-c", &format!("sleep 30 # {marker}")]);
        let _ = ran_within(&mut command, std::time::Duration::from_millis(200));

        // `pgrep -f` matches the whole command line, which is where the marker
        // is: a match here is a child nobody waited for.
        let found = Command::new("pgrep").args(["-f", &marker]).output();
        let still = found.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        assert!(still.is_empty(), "the child outlived the probe: {still}");
    }

    /// And one that answers is still answered, or this would pass by killing
    /// everything.
    #[test]
    fn a_probe_that_answers_is_read() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo 1.2.3"]);
        let out = ran_within(&mut command, std::time::Duration::from_secs(5)).expect("it answered");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1.2.3");
    }
}
