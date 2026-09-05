//! Containment for a command, where the platform has some.
//!
//! The boundary before this was text: a path check in the file tools and
//! pattern rules over the command line, which a command's own children never
//! meet. What runs here is the operating system's: Seatbelt on macOS, Landlock
//! on Linux. Each is best-effort in the same shape — the workspace and a
//! scratch directory may be written, everything else only read, and the
//! network is a switch — and what was applied is said, not assumed, because a
//! sandbox that quietly did nothing is worse than none.
//!
//! On Windows the command runs at low integrity, through a launcher that is
//! this same binary lowered: reading is everywhere, writing only to what is
//! labelled low — the workspace and scratch, labelled by the parent — and the
//! network is not restrained at all. There is none on FreeBSD yet. `Mode::Auto`
//! runs a command as it is there and says so; `Mode::Required` refuses instead.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where a command may write, and whether it may reach the network. Reading
/// is allowed everywhere: a build needs its toolchain, and a sandbox that
/// hides `/usr` is one that gets turned off.
#[derive(Clone, Debug)]
pub struct Isolation {
    pub workspace: PathBuf,
    /// Written to as well as the workspace: the temporary directory, and
    /// whatever the caller adds.
    pub scratch: Vec<PathBuf>,
    pub network: bool,
    /// Kept out of reach entirely, read included.
    ///
    /// Reading is otherwise allowed everywhere, because a build needs its
    /// toolchain — but the agent's own state directory is every project's
    /// transcripts, every checkpoint's contents and everything it was told to
    /// remember. A command run for one project has no business reading
    /// another's, and with the network on, reading is the whole of what an
    /// exfiltration needs.
    pub unreadable: Vec<PathBuf>,
}

impl Isolation {
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            scratch: vec![std::env::temp_dir()],
            network: true,
            unreadable: Vec::new(),
        }
    }
}

/// Whether commands are contained: never, where the platform can, or only
/// where it can.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    #[default]
    Auto,
    Required,
}

/// What this platform contains a command with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `sandbox-exec`, deprecated by Apple for a decade and present in every
    /// release since.
    Seatbelt,
    /// The kernel's own, unprivileged. `tcp` is whether the kernel is new
    /// enough (6.7) to restrict connections too; UDP, and so DNS, it never
    /// does.
    Landlock { tcp: bool },
    /// A low integrity level, which forbids writing anything not labelled
    /// low and says nothing about the network.
    LowIntegrity,
}

impl Backend {
    /// Whether this backend can keep a directory unreadable. An integrity
    /// level is about writing; it says nothing about reads, so on Windows the
    /// store is as readable to a contained command as to any other.
    pub fn hides_paths(self) -> bool {
        !matches!(self, Self::LowIntegrity)
    }

    pub fn describe(self, isolation: &Isolation) -> String {
        let kept = match (isolation.unreadable.is_empty(), self.hides_paths()) {
            (true, _) => "",
            (false, true) => ", and cannot read the agent's own store",
            (false, false) => ", and an integrity level cannot stop it reading the agent's own store",
        };
        format!("{}{kept}", self.without_paths(isolation))
    }

    fn without_paths(self, isolation: &Isolation) -> String {
        match self {
            Self::Seatbelt if isolation.network => {
                "seatbelt: writes to the workspace and scratch, network open".into()
            }
            Self::Seatbelt => "seatbelt: writes to the workspace and scratch, no network".into(),
            Self::Landlock { .. } if isolation.network => {
                "landlock: writes to the workspace and scratch, network open".into()
            }
            Self::Landlock { tcp: true } => {
                "landlock: writes to the workspace and scratch, no tcp (udp is not restrained)".into()
            }
            Self::Landlock { tcp: false } => {
                "landlock: writes to the workspace and scratch; this kernel cannot restrain the network"
                    .into()
            }
            Self::LowIntegrity => {
                "low integrity: writes to the workspace and scratch, labelled low for it; the network is not restrained"
                    .into()
            }
        }
    }
}

/// What contains a command here, or why nothing does. Probed once.
pub fn available() -> Result<Backend, String> {
    static PROBED: std::sync::OnceLock<Result<Backend, String>> = std::sync::OnceLock::new();
    PROBED.get_or_init(probe).clone()
}

/// Asked by running one: a process already inside a sandbox — a CI step, an
/// app's helper — cannot apply another, and `sandbox-exec` says so only when
/// tried, which would otherwise be on the first command of every turn.
#[cfg(target_os = "macos")]
fn probe() -> Result<Backend, String> {
    if !std::path::Path::new(SANDBOX_EXEC).exists() {
        return Err(format!("no sandbox: {SANDBOX_EXEC} is missing, so commands run as they are"));
    }
    let tried = std::process::Command::new(SANDBOX_EXEC)
        .args(["-p", "(version 1)(allow default)", "--", "/usr/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match tried {
        Ok(out) if out.status.success() => Ok(Backend::Seatbelt),
        Ok(out) => Err(format!(
            "no sandbox: {SANDBOX_EXEC} cannot apply a profile here ({}), so commands run as they are",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => {
            Err(format!("no sandbox: {SANDBOX_EXEC} would not start ({e}), so commands run as they are"))
        }
    }
}

#[cfg(target_os = "linux")]
fn probe() -> Result<Backend, String> {
    use landlock::{ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, Ruleset, RulesetAttr};
    let fs = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V1))
        .and_then(|r| r.create());
    if let Err(e) = fs {
        return Err(format!("no sandbox: this kernel has no Landlock ({e}), so commands run as they are"));
    }
    let tcp = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessNet::from_all(ABI::V4))
        .and_then(|r| r.create())
        .is_ok();
    Ok(Backend::Landlock { tcp })
}

/// The launcher is a rook binary that said so at start. A process that never
/// did — a test binary — has no way to lower a command, and must not try to
/// start itself as one.
#[cfg(windows)]
fn probe() -> Result<Backend, String> {
    match std::env::var_os(rook_contain::LAUNCHER) {
        Some(_) => Ok(Backend::LowIntegrity),
        None => {
            Err("no sandbox: this process is not a rook binary, so nothing here can run as the launcher"
                .into())
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn probe() -> Result<Backend, String> {
    Err(format!("no sandbox on {}: commands run as they are", std::env::consts::OS))
}

/// The isolation a command gets under `mode`, and the words for it — or the
/// refusal, when the mode requires what the platform has not got.
pub fn choose(mode: Mode, isolation: &Isolation) -> Result<(Option<&Isolation>, String), String> {
    match (mode, available()) {
        (Mode::Off, _) => Ok((None, "off".into())),
        (_, Ok(backend)) => Ok((Some(isolation), backend.describe(isolation))),
        (Mode::Auto, Err(why)) => Ok((None, why)),
        (Mode::Required, Err(why)) => Err(format!(
            "{why} — and `[sandbox] isolate = \"required\"` says not to; set it to `auto` to run them anyway"
        )),
    }
}

#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Where the launcher is told to run the command, since the launcher and
/// not `spawn_shell` is what starts it.
#[cfg(windows)]
pub(crate) const CWD_ENV: &str = rook_contain::ENV_CWD;

/// The command `/bin/sh -c <command>` becomes under `isolation`.
#[cfg(target_os = "macos")]
pub(crate) fn contained(command: &str, isolation: &Isolation) -> std::io::Result<tokio::process::Command> {
    let mut cmd = tokio::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile(isolation)).arg("--").arg("/bin/sh").arg("-c").arg(command);
    Ok(cmd)
}

/// A path as the profile spells one. The only character the language cares
/// about inside a string is the quote that ends it.
#[cfg(target_os = "macos")]
fn quoted(path: &std::path::Path) -> String {
    path.display().to_string().replace('"', "\\\"")
}

/// Deny by default and allow what a build needs: every read, its own
/// processes, the kernel's answers about the machine, and writes where the
/// policy says. Paths are the real ones — Seatbelt matches after resolving
/// symlinks, and `/tmp` is `/private/tmp`.
#[cfg(target_os = "macos")]
fn profile(isolation: &Isolation) -> String {
    let mut p = String::from(
        "(version 1)\n(deny default)\n(allow process-exec)\n(allow process-fork)\n(allow signal)\n\
         (allow sysctl-read)\n(allow mach-lookup)\n(allow ipc-posix*)\n(allow file-read*)\n\
         (allow file-ioctl)\n(allow system-socket)\n(allow file-write-data (literal \"/dev/null\"))\n",
    );
    // A contained command inherits the terminal rook was started from, and
    // `TIOCSTI` queues bytes into it as if they had been typed — read by the
    // shell that resumes when rook exits. The whole point of the containment
    // is that a command cannot reach past the workspace, and this reaches
    // past every boundary there is, so it is denied after the blanket ioctl
    // allowance where the last matching rule wins.
    p.push_str("(deny file-ioctl (ioctl-command TIOCSTI))\n");
    // After the blanket read and before the writes: Seatbelt takes the last
    // rule that matches, so this is where a denial has to sit to hold.
    for kept in &isolation.unreadable {
        let real = kept.canonicalize().unwrap_or_else(|_| kept.clone());
        p.push_str(&format!("(deny file-read* (subpath \"{}\"))\n", quoted(&real)));
    }
    // Both, and after the denials: a command must be able to read the directory
    // it works in, and a workspace inside a hidden root — `ROOK_HOME` pointed
    // at a scratch tree, which is what a test and a curious person both do —
    // was hidden with it. What that looks like is a shell that cannot resolve
    // its own cwd, and ten steps of a real turn spent guessing why. The state
    // directory is hidden to keep other projects' transcripts out of reach;
    // hiding the workspace serves nothing.
    for root in std::iter::once(&isolation.workspace).chain(&isolation.scratch) {
        let real = root.canonicalize().unwrap_or_else(|_| root.clone());
        p.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", quoted(&real)));
        p.push_str(&format!("(allow file-write* (subpath \"{}\"))\n", quoted(&real)));
    }
    if isolation.network {
        p.push_str("(allow network*)\n");
    }
    p
}

/// Everything readable, expressed as roots, with `kept` left out.
///
/// Landlock grants and never denies, so "everything except this" has to be
/// spelled as a list of everything else: walk down the excluded path and, at
/// each level, name every sibling but the one the path goes through. A
/// directory that cannot be read is left out, which errs towards refusing a
/// read rather than allowing one — the safe direction for a mistake here.
///
/// With nothing excluded this is `/`, which is what it was before.
#[cfg(target_os = "linux")]
fn readable(kept: &[PathBuf]) -> Vec<PathBuf> {
    if kept.is_empty() {
        return vec![PathBuf::from("/")];
    }
    let mut roots = vec![PathBuf::from("/")];
    for hidden in kept {
        let hidden = hidden.canonicalize().unwrap_or_else(|_| hidden.clone());
        let mut allowed = Vec::new();
        for root in roots {
            // A root the excluded path does not pass through survives whole.
            let Ok(rest) = hidden.strip_prefix(&root) else {
                allowed.push(root);
                continue;
            };
            let mut walked = root;
            for step in rest.iter() {
                let Ok(entries) = std::fs::read_dir(&walked) else { break };
                for entry in entries.flatten() {
                    if entry.file_name() != step {
                        allowed.push(entry.path());
                    }
                }
                walked = walked.join(step);
            }
        }
        roots = allowed;
    }
    roots
}

/// `/bin/sh -c <command>`, with the ruleset applied in the child before it
/// executes. The ruleset is built here, in the parent, where allocating is
/// fine; what runs after the fork is the one syscall that applies it.
#[cfg(target_os = "linux")]
pub(crate) fn contained(command: &str, isolation: &Isolation) -> std::io::Result<tokio::process::Command> {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
        path_beneath_rules,
    };
    let abi = ABI::V4;
    let mut ruleset =
        Ruleset::default().set_compatibility(CompatLevel::BestEffort).handle_access(AccessFs::from_all(abi));
    if !isolation.network {
        ruleset = ruleset.and_then(|r| r.handle_access(AccessNet::from_all(abi)));
    }
    let writable: Vec<&std::path::Path> = std::iter::once(isolation.workspace.as_path())
        .chain(isolation.scratch.iter().map(PathBuf::as_path))
        .collect();
    let created = ruleset
        .and_then(|r| r.create())
        .and_then(|r| {
            r.add_rules(path_beneath_rules(readable(&isolation.unreadable), AccessFs::from_read(abi)))
        })
        .and_then(|r| r.add_rules(path_beneath_rules(writable, AccessFs::from_all(abi))))
        .and_then(|r| r.add_rules(path_beneath_rules(["/dev/null"], AccessFs::WriteFile)))
        .map_err(|e| std::io::Error::other(format!("building the landlock ruleset: {e}")))?;

    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    let ruleset = std::sync::Mutex::new(Some(created));
    // Safety: after the fork and before the exec, the child may only make
    // syscalls. `restrict_self` is two — no_new_privs and the restriction —
    // and the lock is a fresh, unlocked copy that nothing else can hold.
    unsafe {
        cmd.pre_exec(move || {
            let Some(ruleset) = ruleset.lock().map_err(|_| std::io::Error::other("ruleset lock"))?.take()
            else {
                return Ok(());
            };
            ruleset.restrict_self().map(|_| ()).map_err(std::io::Error::other)
        });
    }
    Ok(cmd)
}

/// This binary again, as the launcher: it lowers itself and runs the command
/// through `cmd /C`, which inherits the level and the pipes. The directories
/// the command may write are labelled low here first — once per directory
/// per process, because the label persists and the call walks the tree.
#[cfg(windows)]
pub(crate) fn contained(command: &str, isolation: &Isolation) -> std::io::Result<tokio::process::Command> {
    static LABELLED: std::sync::Mutex<std::collections::BTreeSet<PathBuf>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    // Not the user's whole temporary directory, which the label would walk
    // and mark for every program that uses it: a directory of rook's own
    // under it, which the command is told is its TEMP.
    let temp = std::env::temp_dir();
    let scratch = temp.join("rook-scratch");
    std::fs::create_dir_all(&scratch)?;
    let roots = std::iter::once(&isolation.workspace)
        .chain(std::iter::once(&scratch))
        .chain(isolation.scratch.iter().filter(|dir| **dir != temp));
    for root in roots {
        let mut done = LABELLED.lock().map_err(|_| std::io::Error::other("label lock"))?;
        if done.contains(root) || !root.exists() {
            continue;
        }
        rook_contain::label_low(root).map_err(std::io::Error::other)?;
        done.insert(root.clone());
    }
    let launcher = std::env::var_os(rook_contain::LAUNCHER)
        .ok_or_else(|| std::io::Error::other("no launcher: this process is not a rook binary"))?;
    let mut cmd = tokio::process::Command::new(launcher);
    cmd.env(rook_contain::ENV, command).env(rook_contain::ENV_SCRATCH, &scratch);
    Ok(cmd)
}

// Unix without a sandbox — FreeBSD — reaches `contained` only through
// `choose`, which never asks for it there; the arm exists so the shell path
// compiles.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(crate) fn contained(_: &str, _: &Isolation) -> std::io::Result<tokio::process::Command> {
    Err(std::io::Error::other(available().unwrap_err()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Seatbelt takes the last rule that matches, so a denial written before
    /// the blanket `file-ioctl` allowance would be overridden by it and read
    /// exactly like one that holds.
    ///
    /// This asserts the profile and not the kernel, deliberately: on macOS 26
    /// the ioctl is refused under this profile with or without the rule, so a
    /// test that ran a command would pass with the rule deleted. The rule is
    /// here for the versions where the blanket allowance is honoured — it is
    /// how [codex hardened the same
    /// profile](../../../references/PORTED.md) — and what can be checked
    /// here is that it is not written where it cannot work.
    /// Found by a real turn: the state directory is hidden from commands, the
    /// workspace was inside it, and every command answered `getcwd: cannot
    /// access parent directories: Operation not permitted`. The model spent
    /// ten steps working out what it could not read, and never could have.
    #[test]
    fn a_workspace_inside_a_hidden_root_is_still_readable() {
        let state = tempfile::tempdir().unwrap();
        let workspace = state.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let real = workspace.canonicalize().unwrap();

        let text = profile(&Isolation {
            workspace,
            scratch: vec![],
            network: false,
            unreadable: vec![state.path().to_path_buf()],
        });

        let denied = text.find("(deny file-read*").expect("the state directory is hidden");
        let allowed = text
            .find(&format!("(allow file-read* (subpath \"{}\"))", quoted(&real)))
            .expect("and the workspace inside it is read back");
        assert!(allowed > denied, "the last matching rule wins, so this one has to be last:\n{text}");
    }

    #[test]
    fn terminal_injection_is_denied_after_the_ioctl_allowance() {
        let text = profile(&Isolation::for_workspace(std::env::temp_dir()));
        let allowed = text.find("(allow file-ioctl)").expect("the allowance is there to be overridden");
        let denied = text.find("(deny file-ioctl (ioctl-command TIOCSTI))").expect("and the denial");
        assert!(denied > allowed, "the denial has to come last to hold:\n{text}");
    }
}
