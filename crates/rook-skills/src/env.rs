//! The environment a skill is being resolved against.
//!
//! A skill that shells out to `sed -i` is correct on GNU userland and wrong on
//! BSD; one that uses `tokio::task::spawn_blocking` needs a Rust new enough to
//! have it. Rather than let the model discover this by failing, the environment
//! is detected once and skills declare what they need against it.

use std::collections::BTreeMap;
use std::process::Command;

const PROBE_PARALLELISM: usize = 4;

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
        let mut tools = BTreeMap::new();
        let probes: Vec<_> = LANGUAGE_PROBES.iter().chain(TOOL_PROBES).collect();
        let versions = probe_versions(&probes, PROBE_PARALLELISM, probe_version);
        for (index, (probe, version)) in probes.iter().zip(versions).enumerate() {
            if let Some(version) = version {
                let destination = if index < LANGUAGE_PROBES.len() { &mut languages } else { &mut tools };
                destination.insert(probe.key.to_string(), version);
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

/// Independent version commands may be slow shims. Keep a small pool rather
/// than multiplying their deadlines, and preserve the input slots even when
/// completion order differs. The caller participates so thread creation failure
/// cannot strand unclaimed probes or require another worker to make progress.
fn probe_versions(
    probes: &[&Probe],
    parallelism: usize,
    ask: impl Fn(&Probe) -> Option<String> + Sync,
) -> Vec<Option<String>> {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    if probes.is_empty() {
        return Vec::new();
    }
    let parallelism = parallelism.clamp(1, PROBE_PARALLELISM).min(probes.len());
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![None; probes.len()]);
    let work = || loop {
        let index = next.fetch_add(1, Ordering::Relaxed);
        let Some(probe) = probes.get(index) else { break };
        let version = ask(probe);
        results.lock().unwrap_or_else(|error| error.into_inner())[index] = version;
    };
    std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(parallelism - 1);
        for index in 1..parallelism {
            match std::thread::Builder::new().name(format!("rook-env-{index}")).spawn_scoped(scope, work) {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    tracing::debug!(%error, "version probe worker unavailable; remaining workers continue");
                    break;
                }
            }
        }
        work();
        for worker in workers {
            if worker.join().is_err() {
                tracing::warn!("version probe worker ended unexpectedly");
            }
        }
    });
    results.into_inner().unwrap_or_else(|error| error.into_inner())
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
    // Windows gives a process it starts a console unless told otherwise, and a
    // console is a window. Sixteen probes at startup is sixteen windows that
    // open and shut on the person's desktop before the first frame is drawn —
    // reported from a real one as a swarm of them.
    // A completed parent can leave descendants holding either pipe. Keep EOF
    // under the same patience, and reject banners beyond this metadata ceiling.
    rook_contain::captured_within(command, patience, 64 * 1024).ok()
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

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;

    #[test]
    fn independent_probes_reach_the_four_worker_bound_without_losing_input_order() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (entered, starts) = mpsc::sync_channel(16);
        let worker_gate = gate.clone();
        let worker_active = active.clone();
        let worker_peak = peak.clone();
        let running = std::thread::spawn(move || {
            let probes: Vec<_> = LANGUAGE_PROBES.iter().chain(TOOL_PROBES).collect();
            probe_versions(&probes, 8, |probe| {
                let current = worker_active.fetch_add(1, Ordering::SeqCst) + 1;
                worker_peak.fetch_max(current, Ordering::SeqCst);
                entered.send(probe.key).unwrap();
                let (lock, wake) = &*worker_gate;
                let released = lock.lock().unwrap();
                let (released, timeout) =
                    wake.wait_timeout_while(released, Duration::from_secs(30), |value| !*value).unwrap();
                let valid = *released && !timeout.timed_out();
                worker_active.fetch_sub(1, Ordering::SeqCst);
                valid.then(|| probe.key.to_owned())
            })
        });
        let first: Vec<_> = (0..4).map(|_| starts.recv_timeout(Duration::from_secs(30))).collect();
        // Always release the children before assertions, including a failed
        // admission precondition, so a failing test cannot strand its pool.
        {
            let (lock, wake) = &*gate;
            *lock.lock().unwrap() = true;
            wake.notify_all();
        }
        let versions = running.join().unwrap();
        assert!(first.iter().all(Result::is_ok), "four probes must actually enter before release: {first:?}");
        assert_eq!(
            peak.load(Ordering::SeqCst),
            4,
            "oversized requested parallelism must still cap active probes at four"
        );
        assert_eq!(active.load(Ordering::SeqCst), 0);
        let expected: Vec<_> =
            LANGUAGE_PROBES.iter().chain(TOOL_PROBES).map(|probe| Some(probe.key.to_owned())).collect();
        assert_eq!(versions, expected, "completion order cannot reorder or omit source slots");
        assert_eq!(
            starts.into_iter().count() + first.len(),
            16,
            "each source probe is admitted exactly once"
        );
    }

    #[test]
    fn empty_serial_and_partial_results_preserve_missing_tools_and_every_slot() {
        let probes: Vec<_> = LANGUAGE_PROBES.iter().chain(TOOL_PROBES).collect();
        let expected: Vec<_> = probes
            .iter()
            .map(|probe| if probe.key == "java" { None } else { Some(probe.key.to_owned()) })
            .collect();
        for parallelism in [0, 1, 4, 8] {
            let calls = AtomicUsize::new(0);
            let versions = probe_versions(&probes, parallelism, |probe| {
                calls.fetch_add(1, Ordering::SeqCst);
                if probe.key == "java" { None } else { Some(probe.key.to_owned()) }
            });
            assert_eq!(versions, expected);
            assert_eq!(calls.load(Ordering::SeqCst), 16);
        }
        assert!(probe_versions(&[], 4, |_| panic!("empty input cannot launch a probe")).is_empty());
    }

    #[test]
    fn a_later_probe_can_finish_first_without_reordering_the_returned_versions() {
        let probes = vec![&LANGUAGE_PROBES[0], &LANGUAGE_PROBES[1]];
        let gate = (Mutex::new(false), Condvar::new());
        let completion = Mutex::new(Vec::new());
        let versions = probe_versions(&probes, 4, |probe| {
            if probe.key == "rust" {
                let ready = gate.0.lock().unwrap();
                let (ready, timeout) =
                    gate.1.wait_timeout_while(ready, Duration::from_secs(30), |ready| !*ready).unwrap();
                if !*ready || timeout.timed_out() {
                    return None;
                }
                completion.lock().unwrap().push(probe.key);
            } else {
                completion.lock().unwrap().push(probe.key);
                *gate.0.lock().unwrap() = true;
                gate.1.notify_all();
            }
            Some(probe.key.to_owned())
        });
        assert_eq!(
            *completion.lock().unwrap(),
            vec!["python", "rust"],
            "the fixture must actually reverse completion order"
        );
        assert_eq!(versions, vec![Some("rust".into()), Some("python".into())]);
    }
}
