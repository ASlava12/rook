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
//! There is none on Windows or FreeBSD yet. `Mode::Auto` runs a command as it
//! is there and says so; `Mode::Required` refuses instead.

use std::path::{Path, PathBuf};

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
}

impl Isolation {
    pub fn for_workspace(workspace: impl Into<PathBuf>) -> Self {
        Self { workspace: workspace.into(), scratch: vec![std::env::temp_dir()], network: true }
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
}

impl Backend {
    pub fn describe(self, isolation: &Isolation) -> String {
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
    if !Path::new(SANDBOX_EXEC).exists() {
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// The command `/bin/sh -c <command>` becomes under `isolation`.
#[cfg(target_os = "macos")]
pub(crate) fn contained(command: &str, isolation: &Isolation) -> std::io::Result<tokio::process::Command> {
    let mut cmd = tokio::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile(isolation)).arg("--").arg("/bin/sh").arg("-c").arg(command);
    Ok(cmd)
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
    for root in std::iter::once(&isolation.workspace).chain(&isolation.scratch) {
        let real = root.canonicalize().unwrap_or_else(|_| root.clone());
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            real.display().to_string().replace('"', "\\\"")
        ));
    }
    if isolation.network {
        p.push_str("(allow network*)\n");
    }
    p
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
    let writable: Vec<&Path> = std::iter::once(isolation.workspace.as_path())
        .chain(isolation.scratch.iter().map(PathBuf::as_path))
        .collect();
    let created = ruleset
        .and_then(|r| r.create())
        .and_then(|r| r.add_rules(path_beneath_rules(["/"], AccessFs::from_read(abi))))
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn contained(_: &str, _: &Isolation) -> std::io::Result<tokio::process::Command> {
    Err(std::io::Error::other(available().unwrap_err()))
}
