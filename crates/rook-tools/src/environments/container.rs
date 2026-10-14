use super::*;
use crate::Tool;
use rook_contain::files::RecoveryFiles;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
#[derive(Serialize, Deserialize)]
pub(super) struct State {
    pub(super) version: u32,
    pub(super) container: String,
    pub(super) identity: String,
    pub(super) status: String,
    #[serde(default)]
    pub(super) owner: String,
    #[serde(default)]
    pub(super) pid: u32,
}
pub(super) struct Resource {
    pub program: String,
    pub image: String,
    pub(super) root: PathBuf,
    pub(super) state_root: PathBuf,
    pub(super) identity: String,
    pub(super) owner: String,
    memory_mb: usize,
    pids: usize,
    network: bool,
    setup_timeout: u64,
    pub(super) gate: Arc<tokio::sync::Semaphore>,
    pub(super) initialized: AtomicBool,
    pub(super) container_id: Mutex<Option<String>>,
    pub(super) uncertain: AtomicBool,
}
impl Resource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        program: String,
        image: String,
        root: PathBuf,
        state_root: PathBuf,
        identity: String,
        memory_mb: usize,
        pids: usize,
        network: bool,
        setup_timeout: u64,
    ) -> Self {
        Self {
            program,
            image,
            root,
            state_root,
            identity,
            owner: ulid::Ulid::generate().to_string(),
            memory_mb,
            pids,
            network,
            setup_timeout,
            gate: Arc::new(tokio::sync::Semaphore::new(1)),
            initialized: AtomicBool::new(false),
            container_id: Mutex::new(None),
            uncertain: AtomicBool::new(false),
        }
    }
    pub(super) fn disposable(self: &Arc<Self>) -> bool {
        Arc::strong_count(self) == 1
            && !self.initialized.load(Ordering::Acquire)
            && !self.uncertain.load(Ordering::Acquire)
    }
    pub fn container(&self) -> String {
        self.container_id.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| self.name())
    }
    pub(super) fn name(&self) -> String {
        format!("rook-exec-{}", self.identity.chars().take(24).collect::<String>())
    }
    pub(super) fn state(&self, status: &str) -> Result<()> {
        let files = RecoveryFiles::open(&self.state_root, Path::new("execution-environments"), true)
            .map_err(|e| invalid(e.to_string()))?;
        let path = PathBuf::from(format!("{}.json", self.identity));
        let old = match files.read(&path, 16384) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(invalid(e.to_string())),
        };
        let data = serde_json::to_vec(&State {
            version: 1,
            container: self.container(),
            identity: self.identity.clone(),
            status: status.into(),
            owner: self.owner.clone(),
            pid: std::process::id(),
        })
        .map_err(|e| invalid(e.to_string()))?;
        if !files
            .replace_checked(&path, old.as_deref(), old.as_ref().map(|_| 0o600), Some(&data), 0o600)
            .map_err(|e| invalid(e.to_string()))?
        {
            return Err(invalid("execution resource state changed concurrently; inspect it"));
        }
        Ok(())
    }
    pub(super) async fn administrative(&self, args: &[String]) -> Result<crate::ToolOutcome> {
        let command = std::iter::once(self.program.as_str())
            .chain(args.iter().map(String::as_str))
            .map(quoted)
            .collect::<Vec<_>>()
            .join(" ");
        let mut ctx = ToolContext::new(self.root.clone());
        ctx.command_timeout = Duration::from_secs(self.setup_timeout);
        ctx.max_output_bytes = 4096;
        ctx.require_command_cleanup = true;
        crate::exec::RunCommand
            .call(&ctx, &serde_json::json!({"command":command,"timeout_secs":self.setup_timeout}))
            .await
    }
    pub async fn acquire(self: &Arc<Self>) -> Result<Arc<Lease>> {
        let permit = self.gate.clone().try_acquire_owned().map_err(|_| {
            invalid("this execution environment is busy; inspect its running command or background job")
        })?;
        if self.uncertain.load(Ordering::Acquire) {
            return Err(invalid(
                "execution resource was interrupted; inspect its saved state and owned container before reusing it",
            ));
        }
        if !self.initialized.load(Ordering::Acquire) {
            let files = RecoveryFiles::open(&self.state_root, Path::new("execution-environments"), true)
                .map_err(|e| invalid(e.to_string()))?;
            if let Ok(bytes) = files.read(Path::new(&format!("{}.json", self.identity)), 16384) {
                let old: State = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
                if old.version != 1 || old.identity != self.identity {
                    return Err(invalid("execution state identity mismatch"));
                }
                if old.status != "released" {
                    return Err(invalid(
                        "a previous process retained this environment; inspect its container and state before explicitly recovering it",
                    ));
                }
            }
            let check = self
                .administrative(&[
                    "image".into(),
                    "inspect".into(),
                    self.image.clone(),
                    "--format".into(),
                    "{{.Id}}".into(),
                ])
                .await?;
            if check.is_error {
                return Err(invalid(format!(
                    "container image unavailable; install the configured image first: {}",
                    check.content
                )));
            }
            let image_id = check
                .content
                .lines()
                .find(|line| {
                    line.starts_with("sha256:")
                        && line.len() == 71
                        && line[7..].bytes().all(|b| b.is_ascii_hexdigit())
                })
                .ok_or_else(|| invalid("Docker did not report an immutable image ID"))?
                .to_owned();
            self.state("starting")?;
            let mut setup = SetupGuard { resource: self.clone(), armed: true };
            let mut args = vec![
                "run".into(),
                "--pull".into(),
                "never".into(),
                "--detach".into(),
                "--name".into(),
                self.container(),
                "--label".into(),
                format!("rook.execution={}", self.identity),
                "--cap-drop".into(),
                "ALL".into(),
                "--security-opt".into(),
                "no-new-privileges".into(),
                "--pids-limit".into(),
                self.pids.to_string(),
                "--memory".into(),
                format!("{}m", self.memory_mb),
                "--cpus".into(),
                "1".into(),
                "--user".into(),
                current_user(),
                "--read-only".into(),
                "--tmpfs".into(),
                "/tmp:rw,nosuid,nodev,size=64m".into(),
                "--mount".into(),
                format!("type=bind,source={},target=/workspace", self.root.display()),
                "--workdir".into(),
                "/workspace".into(),
            ];
            if !self.network {
                args.extend(["--network".into(), "none".into()]);
            }
            args.extend([
                image_id,
                "sh".into(),
                "-c".into(),
                "trap 'exit 0' TERM INT; while :; do sleep 3600 & wait $!; done".into(),
            ]);
            let outcome = self.administrative(&args).await?;
            if outcome.is_error {
                self.uncertain.store(true, Ordering::Release);
                self.state("setup_failed")?;
                return Err(invalid(outcome.content));
            }
            let id = outcome
                .content
                .lines()
                .find(|line| line.len() == 64 && line.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| {
                    invalid("Docker did not report an owned immutable container ID; inspect setup state")
                })?;
            *self.container_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_owned());
            self.initialized.store(true, Ordering::Release);
            // Prove the Docker server sees this client workspace before executing user code.
            let token = ulid::Ulid::generate().to_string();
            let probe = Probe::new(&self.root, &token)?;
            let observed = self
                .administrative(&[
                    "exec".into(),
                    self.container(),
                    "cat".into(),
                    format!("/workspace/{}", probe.name),
                ])
                .await?;
            if observed.is_error || !observed.content.lines().any(|line| line == token) {
                return Err(invalid(
                    "container mount does not match the local workspace; inspect the Docker server's bind-path mapping",
                ));
            }
            drop(probe);
            self.state("ready")?;
            setup.armed = false;
        }
        self.state("running")?;
        Ok(Arc::new(Lease {
            resource: self.clone(),
            _permit: permit,
            finished: AtomicBool::new(false),
            stopped: Mutex::new(None),
            token: ulid::Ulid::generate().to_string(),
        }))
    }
    fn stop(&self) -> bool {
        self.control("kill")
    }
    fn control(&self, action: &str) -> bool {
        if self.container_id.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            return false;
        }
        let id = self.container();
        let args = if action == "rm" { vec!["rm", "--force", &id] } else { vec![action, &id] };
        self.control_args(&args)
    }
    pub(super) fn control_args(&self, args: &[&str]) -> bool {
        let mut command = std::process::Command::new(&self.program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let Ok(mut child) = rook_contain::quietly(&mut command).spawn() else { return false };
        let group = rook_contain::Group::holding(Some(child.id()));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    group.end();
                    let _ = child.kill();
                    // Never wait without a deadline for an unresponsive transport.
                    let reap_deadline = std::time::Instant::now() + Duration::from_millis(250);
                    while std::time::Instant::now() < reap_deadline {
                        if !matches!(child.try_wait(), Ok(None)) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    return false;
                }
            }
        }
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        if self.container_id.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            let released = self.control("rm");
            let _ = self.state(if released { "released" } else { "release_unknown" });
        }
    }
}

pub(crate) struct Lease {
    resource: Arc<Resource>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    finished: AtomicBool,
    stopped: Mutex<Option<bool>>,
    token: String,
}
impl Lease {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }
    pub(crate) fn complete(&self, exit: i32) -> Result<()> {
        // The client exit is insufficient: require the proxy's matching terminal record,
        // published only after its originally owned remote group became quiescent.
        let script = r#"test -f "/tmp/rook-exec/$1.done" && test "$(cat "/tmp/rook-exec/$1.done")" = "$2" && rm -f "/tmp/rook-exec/$1.done""#;
        let observed = exit >= 0
            && self.resource.control_args(&[
                "exec",
                &self.resource.container(),
                "sh",
                "-c",
                script,
                "rook-check",
                &self.token,
                &exit.to_string(),
            ]);
        if !observed {
            self.stop();
            return Err(invalid(
                "remote command completion could not be certified; the owned resource was stopped; inspect its state",
            ));
        }
        self.resource.state("ready")?;
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
    pub(crate) fn stop(&self) -> bool {
        let mut observed = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stopped) = *observed {
            return stopped;
        }
        let stopped = self.resource.stop();
        *observed = Some(stopped);
        self.resource.uncertain.store(true, Ordering::Release);
        let _ = self.resource.state(if stopped { "interrupted_stopped" } else { "interrupted_unknown" });
        stopped
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::Acquire) {
            self.stop();
        }
    }
}
pub(super) fn command(
    resource: &Resource,
    token: &str,
    cwd: &str,
    command: &str,
    secret_names: &[String],
) -> String {
    // A separately owned process group is stopped before the proxy reports its exit.
    let script = r#"mkdir -p /tmp/rook-exec || exit 125
setsid sh -c "$1" & owned=$!
wait "$owned"; code=$?
kill -TERM -- "-$owned" 2>/dev/null || :
sleep 0.05
kill -KILL -- "-$owned" 2>/dev/null || :
tries=0
while :; do
    members=$(ps -eo pgid,stat) || exit 125
    printf '%s\n' "$members" | awk -v group="$owned" '$1 == group && $2 !~ /^Z/ { found=1 } END { exit !found }'
    member_status=$?
    case "$member_status" in 1) break;; 0) :;; *) exit 125;; esac
    tries=$((tries + 1)); test "$tries" -lt 40 || exit 125
    sleep 0.05
done
printf '%s' "$code" > "/tmp/rook-exec/$2.done" || exit 125
exit "$code""#;
    let mut args = vec![resource.program.as_str(), "exec"];
    for name in secret_names {
        args.extend(["--env", name.as_str()]);
    }
    let id = resource.container();
    args.extend(["--workdir", cwd, &id, "sh", "-c", script, "rook-run", command, token]);
    args.iter().map(|v| quoted(v)).collect::<Vec<_>>().join(" ")
}

// Covers cancellation while Docker setup is in flight, before a command lease exists.
struct SetupGuard {
    resource: Arc<Resource>,
    armed: bool,
}
impl Drop for SetupGuard {
    fn drop(&mut self) {
        if self.armed {
            self.resource.uncertain.store(true, Ordering::Release);
            let stopped = self.resource.stop();
            let _ = self.resource.state(if stopped { "setup_stopped" } else { "setup_unknown" });
        }
    }
}
struct Probe {
    path: PathBuf,
    name: String,
}
impl Probe {
    fn new(root: &Path, token: &str) -> Result<Self> {
        use std::io::Write;
        let name = format!(".rook-exec-probe-{token}");
        let path = root.join(&name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|e| invalid(e.to_string()))?;
        let probe = Self { path, name };
        file.write_all(token.as_bytes()).map_err(|e| invalid(e.to_string()))?;
        Ok(probe)
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn current_user() -> String {
    #[cfg(unix)]
    {
        // Safety: these POSIX queries have no arguments or side effects.
        format!("{}:{}", unsafe { libc::geteuid() }, unsafe { libc::getegid() })
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

pub(super) fn foreign_owner_alive(state: &State, current_owner: &str) -> bool {
    if state.owner == current_owner || state.status == "released" {
        return false;
    }
    if state.pid == 0 {
        return true;
    }
    #[cfg(unix)]
    {
        // Safety: signal 0 only checks existence/access. Refuse ambiguous permission failures.
        (unsafe { libc::kill(state.pid as i32, 0) } == 0)
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        true
    }
}
