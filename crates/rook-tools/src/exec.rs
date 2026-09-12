//! Running commands, with the guards that keep one turn from taking down the box.

use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncReadExt;

use rook_llm::ToolSpec;

use crate::{Result, Tool, ToolContext, ToolError, ToolOutcome, arg_str};

pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_command".into(),
            description: "Run a shell command in the workspace. Output is captured up to a cap \
                          and the command is killed at the timeout."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "cwd": { "type": "string", "description": "Working directory, relative to the workspace." },
                    "timeout_secs": { "type": "integer" },
                    "background": {
                        "type": "boolean",
                        "description": "Leave it running and answer at once with a job id."
                    },
                    "secrets": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Names from `rook secrets`, put in the environment as $ROOK_SECRET_<NAME> for this command only. Values are never shown to you and are cut out of the output."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: &serde_json::Value) -> Result<ToolOutcome> {
        let command = arg_str(args, self.name(), "command")?;

        let cwd = match args.get("cwd").and_then(|v| v.as_str()) {
            Some(rel) => ctx.resolve(rel)?,
            None => ctx.workspace.clone(),
        };
        // A zero is the model asking for no time at all: it kills the command
        // before it starts and reports a timeout of zero seconds, which reads as
        // a broken tool rather than as the argument it was.
        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|secs| *secs > 0)
            .map(std::time::Duration::from_secs)
            .unwrap_or(ctx.command_timeout);

        if args.get("background").and_then(|b| b.as_bool()).unwrap_or(false) {
            let Some(jobs) = &ctx.jobs else {
                let why = "this front end does not keep background commands — give it a                            `timeout_secs` long enough instead";
                return Ok(ToolOutcome::error(why.to_string()));
            };
            let (isolation, _) = match crate::isolate::choose(ctx.isolate, &ctx.isolation) {
                Ok(chosen) => chosen,
                Err(refused) => return Ok(ToolOutcome::error(refused)),
            };
            let id = jobs.start(&command, &cwd, isolation)?;
            return Ok(ToolOutcome::ok(format!("started {id}; `job` reads what it prints")).with("job", id));
        }

        // Resolved before anything is spawned, so a name nobody set is a
        // refusal rather than a command that runs and fails halfway through
        // with an empty password.
        let asked_for = args
            .get("secrets")
            .and_then(|s| s.as_array())
            .map(|names| names.iter().filter_map(|n| n.as_str().map(str::to_string)).collect::<Vec<_>>())
            .unwrap_or_default();
        let env = match secret_env(ctx, &asked_for) {
            Ok(env) => env,
            Err(refused) => return Ok(ToolOutcome::error(refused)),
        };

        if let Some(terminals) = &ctx.terminals {
            if !env.is_empty() {
                return Ok(ToolOutcome::error(
                    "this front end runs commands in its own terminal, whose environment is not \
                     this one's — a secret cannot be put there. Run it without `secrets`, or \
                     without the editor.",
                ));
            }
            return elsewhere(terminals.as_ref(), &command, &cwd, ctx, timeout).await;
        }

        let (isolation, contained) = match crate::isolate::choose(ctx.isolate, &ctx.isolation) {
            Ok(chosen) => chosen,
            Err(refused) => return Ok(ToolOutcome::error(refused)),
        };
        // Written for the life of the command and removed after it: `ssh` reads
        // a password from a terminal and from nowhere else, so a value in the
        // environment reaches it only through an askpass helper. The helper
        // holds no value — it prints the variable it inherits.
        let helper = (env.len() == 1).then(|| askpass(&env[0].0)).flatten();
        let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        if let Some(helper) = &helper {
            env.extend(helper.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
        let mut child = spawn_shell(&command, &cwd, &env, isolation)?;
        let group = child.id();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

        // One cap at each end, so a runaway command costs bounded memory and
        // both the first error and the last line survive it. Outside the future
        // that fills them, so a timeout still has what was printed before it.
        let keep = ctx.max_output_bytes;
        // One file for both streams. Opened before the command runs, because
        // whether the middle matters is only known once it is gone.
        let spill = ctx
            .spill_dir
            .as_deref()
            .and_then(|dir| Spill::open(dir, ctx.max_spill_bytes))
            .map(|s| std::sync::Arc::new(std::sync::Mutex::new(s)));
        let mut out = Ends::new(keep, spill.clone());
        let mut err = Ends::new(keep, spill.clone());
        // Together, not one after the other: a pipe holds about 64 KiB, and a
        // command that fills stderr while stdout is being drained blocks on the
        // write — so it never finishes stdout and the drain never ends. Any
        // build with enough warnings did exactly that, and hung until the
        // timeout with nothing to show for it.
        macro_rules! capture {
            () => {
                async {
                    let reading_out = async {
                        if let Some(s) = stdout.as_mut() {
                            out.drain(s).await;
                        }
                    };
                    let reading_err = async {
                        if let Some(s) = stderr.as_mut() {
                            err.drain(s).await;
                        }
                    };
                    tokio::join!(reading_out, reading_err);
                }
            };
        }
        // The end of the output and the end of the command are two events, and
        // waiting only for the first is what hangs on `make &` or `nohup …`: a
        // backgrounded child inherits the pipe, so the write end stays open
        // after the shell has exited and the read never reaches EOF. The
        // command finished in milliseconds and was reported, one timeout later,
        // as one that never finished. So both are waited for, and whichever
        // arrives first decides.
        let mut exited = None;
        let ended = tokio::time::timeout(timeout, async {
            tokio::select! {
                _ = capture!() => Ended::Drained,
                status = child.wait() => {
                    exited = status.ok();
                    Ended::Exited
                }
            }
        })
        .await
        .unwrap_or(Ended::TimedOut);

        // Exited first: ordinarily the pipes close microseconds later and this
        // is the same drain finishing.
        //
        // The wait is generous because it is a wait and not a question. Two
        // seconds was not enough, and neither was five: under a full `cargo
        // xtask ci`, with a dozen other test binaries on the machine, an `echo`
        // paid the whole grace and was told something it had started was still
        // running. What it waits for is not a slow command — the command has
        // already exited — it is a scheduler, and no number is past what a
        // loaded machine can cost.
        //
        // So the claim is not the timing any more. Whether anything the command
        // started is still running is a question the operating system can be
        // asked outright, and `kill(-pgid, 0)` asks it: the shell was put in its
        // own group and has been reaped, so a group that still has a member has
        // one this command left behind. The wait stays, because the output has
        // to be drained either way and a drain that never ends must not hold
        // the turn.
        // Generous where the wait can end, short where something else ends it:
        // on a timeout the group is killed just below, and that is what closes
        // the write end.
        const PIPES_AFTER_EXIT: std::time::Duration = std::time::Duration::from_secs(30);
        const BEFORE_THE_KILL: std::time::Duration = std::time::Duration::from_secs(2);
        // `child.wait()` is what produced `Exited`, and waiting reaps — so the
        // shell is gone from the group by the time this asks, and a member left
        // is a member the command started.
        let orphaned = ended == Ended::Exited && group_alive(group);
        if !orphaned {
            let grace = match ended {
                Ended::TimedOut => BEFORE_THE_KILL,
                _ => PIPES_AFTER_EXIT,
            };
            let _ = tokio::time::timeout(grace, capture!()).await;
        }

        if ended == Ended::TimedOut {
            // The whole group, not the shell: `sh -c` may fork rather than
            // exec, and killing the shell alone leaves the real work running.
            let killed = kill_group(group);
            // A command that ran until the timeout is the one whose output is
            // most worth having, and the ends of it are the least of it.
            let printed = joined(&out, &err);
            let kept = settle(spill, out.seen + err.seen > printed.len());
            let outcome = ToolOutcome::error(format!(
                "{}{}",
                timed_out(timeout, killed, &printed),
                kept.as_ref().map(|(note, _)| note.as_str()).unwrap_or("")
            ))
            .with("timed_out", true);
            return Ok(match kept {
                Some((_, path)) => outcome.with("output_file", path),
                None => outcome,
            });
        }
        // Already reaped on the path that ended on the exit rather than on the
        // output; waiting again there is a second wait for a process nobody is
        // waiting for.
        let status = match exited {
            Some(status) => Ok(status),
            None => child.wait().await,
        };

        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let full = out.seen + err.seen;
        let mut combined = joined(&out, &err);
        let truncated = full > combined.len().min(ctx.max_output_bytes);
        if truncated {
            combined = crate::elide_middle(&combined, ctx.max_output_bytes);
        }

        let kept = settle(spill, truncated);

        // A refused write looks like any other permission error, and a model
        // that does not know the command was contained keeps trying: a failure
        // that reads as one says so, and what would widen it. Only that kind —
        // a note on every failure would have a missing Cargo.toml blamed on
        // the sandbox.
        let held = match code != 0 && isolation.is_some() && denied(&combined) {
            true => {
                "\n(ran contained: writes only to the workspace and scratch — `[sandbox] writable` adds a directory)"
            }
            false => "",
        };
        // Said, because the difference matters to whoever reads the answer: the
        // command is over, and something it started is not — so what is above
        // is all of the output there will be, and the rest goes nowhere.
        let left_running = match orphaned {
            true => {
                "\n(the command finished, but something it started is still running and still \
                 holds the output — nothing it prints from here is captured. `background: true` \
                 keeps a command whose output you want.)"
            }
            false => "",
        };
        let outcome = ToolOutcome {
            content: format!(
                "exit {code}\n{combined}{}{held}{left_running}",
                kept.as_ref().map(|(n, _)| n.as_str()).unwrap_or("")
            ),
            is_error: code != 0,
            truncated,
            full_bytes: full,
            meta: Default::default(),
        }
        .with("exit_code", code)
        .with("isolation", contained);
        Ok(match kept {
            Some((_, path)) => outcome.with("output_file", path),
            None => outcome,
        })
    }

    fn risk(&self, args: &serde_json::Value) -> crate::policy::Risk {
        crate::policy::Risk::Execute(Self::command_of(args))
    }
}

impl RunCommand {
    fn command_of(args: &serde_json::Value) -> String {
        args.get("command").and_then(|c| c.as_str()).unwrap_or_default().to_string()
    }
}

/// Everything a command printed, kept on disk because the ends alone discard the
/// middle as they stream.
///
/// The head holds the first error and the tail holds why it failed, which is why
/// they are what the model is shown; but a run whose interesting line is the
/// four hundredth of two thousand has it nowhere. This is where it is, and the
/// model reaches it with the shell it already has.
struct Spill {
    file: std::fs::File,
    path: std::path::PathBuf,
    written: u64,
    cap: u64,
    /// Bytes the cap kept out. A spill that silently stops is a file that reads
    /// as a complete record of a command that printed less than it did.
    dropped: u64,
}

impl Spill {
    /// `None` when there is nowhere to put it, which is not an error: the ends
    /// are still what the model is shown either way.
    fn open(dir: &std::path::Path, cap: u64) -> Option<Self> {
        if cap == 0 {
            return None;
        }
        std::fs::create_dir_all(dir).ok()?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // The clock alone is not unique: two delegated sub-agents starting
        // together get the same reading on a coarse one, and `File::create`
        // truncates — so the second would silently take the first's file.
        static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("{stamp:039}-{nth:06}.log"));
        let file = std::fs::File::create(&path).ok()?;
        Some(Self { file, path, written: 0, cap, dropped: 0 })
    }

    fn write(&mut self, bytes: &[u8]) {
        use std::io::Write;
        let room = self.cap.saturating_sub(self.written) as usize;
        if room == 0 {
            self.dropped += bytes.len() as u64;
            return;
        }
        let taking = room.min(bytes.len());
        if self.file.write_all(&bytes[..taking]).is_ok() {
            self.written += taking as u64;
        }
        self.dropped += (bytes.len() - taking) as u64;
    }

    /// What to tell the model, once the command has finished.
    fn note(&self) -> String {
        let past = match self.dropped {
            0 => String::new(),
            n => format!(", {n} bytes past `[sandbox] max_spill_bytes` not kept"),
        };
        format!("\n[whole output: {} ({} bytes{past})]", self.path.display(), self.written)
    }
}

/// Run it where the front end says, and report the same thing either way.
///
/// The runner does its own truncation, so the both-ends rule does not apply —
/// what is gained instead is the user watching it happen.
async fn elsewhere(
    terminals: &dyn crate::Terminals,
    command: &str,
    cwd: &std::path::Path,
    ctx: &ToolContext,
    timeout: std::time::Duration,
) -> Result<ToolOutcome> {
    let ran = terminals.run(command, cwd, ctx.max_output_bytes).await?;
    if ran.timed_out {
        return Ok(ToolOutcome::error(timed_out(timeout, true, &ran.output)).with("timed_out", true));
    }
    Ok(ToolOutcome {
        content: format!("exit {}\n{}", ran.exit_code, ran.output),
        is_error: ran.exit_code != 0,
        truncated: ran.truncated,
        full_bytes: ran.output.len(),
        meta: Default::default(),
    }
    .with("exit_code", i64::from(ran.exit_code)))
}

/// Whether output reads as a refusal by the operating system. Text, because
/// the kernel's answer reaches us as text: the shell prints it, and the exit
/// code says only that something failed.
fn denied(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    [
        "permission denied",
        "operation not permitted",
        "read-only file system",
        "access is denied",
        "eacces",
        "eperm",
        "erofs",
    ]
    .iter()
    .any(|word| lower.contains(word))
}

/// Start `command` the way the machine's shell would, with `env` added to
/// the child's, contained by `isolation` when there is one. Public because the
/// language-server installer runs `npm` and `go install` and needs the same
/// shell — on Windows `npm` is `npm.cmd`, and the shell is what knows that —
/// and `go install` needs `GOBIN` set.
pub fn spawn_shell(
    command: &str,
    cwd: &std::path::Path,
    env: &[(&str, &str)],
    isolation: Option<&crate::isolate::Isolation>,
) -> Result<tokio::process::Child> {
    #[cfg(windows)]
    // `cmd /C` rather than PowerShell: it is always present, and skills that
    // need PowerShell can invoke it explicitly. `raw_arg` rather than `arg`:
    // `arg` quotes for the C runtime's rules and escapes an embedded `"` as
    // `\"`, which `cmd.exe` does not read that way — it takes the backslash
    // literally. A command with a quotation mark in it, which is most of the
    // ones worth running, arrived at the shell mangled.
    let mut cmd = match isolation {
        // The launcher runs `cmd /C` itself, in the directory it is told.
        Some(isolation) => {
            let mut c = crate::isolate::contained(command, isolation)
                .map_err(|e| ToolError::Io { path: cwd.to_path_buf(), source: e })?;
            c.env(crate::isolate::CWD_ENV, cwd);
            c
        }
        None => {
            use std::os::windows::process::CommandExt;
            let mut c = tokio::process::Command::new("cmd");
            c.as_std_mut().raw_arg(format!("/C {command}"));
            c
        }
    };
    #[cfg(not(windows))]
    let mut cmd = match isolation {
        Some(isolation) => crate::isolate::contained(command, isolation)
            .map_err(|e| ToolError::Io { path: cwd.to_path_buf(), source: e })?,
        None => {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(command);
            c
        }
    };
    cmd.envs(env.iter().copied())
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        // Detach from the terminal's process group where the platform allows it,
        // so a runaway child does not inherit the TUI's signals.
        .kill_on_drop(true);
    #[cfg(unix)]
    // Its own process group, so a timeout can take the whole tree and a runaway
    // child does not inherit the TUI's terminal signals.
    cmd.process_group(0);
    cmd.spawn().map_err(|e| ToolError::Io { path: cwd.to_path_buf(), source: e })
}

/// The environment a command's named secrets become, or why it cannot have
/// them.
///
/// `ROOK_SECRET_<NAME>`, upper-cased, because that is what a shell can read and
/// what `sshpass -e`, `PGPASSWORD` and every other program of that shape
/// expect. Resolved here and not before: a value that is fetched when it is
/// used is a value that is not sitting in memory for the rest of the turn.
fn secret_env(ctx: &ToolContext, names: &[String]) -> std::result::Result<Vec<(String, String)>, String> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let Some(secrets) = &ctx.secrets else {
        return Err("this front end has no secrets to give a command — `rook secrets add <name>` \
                    keeps one, and the terminal or the browser can set it"
            .into());
    };
    let mut env = Vec::new();
    for name in names {
        let Some(value) = secrets.value(name) else {
            return Err(format!(
                "no secret {name:?}, or it did not answer — `rook secrets ls` says which are set \
                 and which resolve"
            ));
        };
        env.push((format!("ROOK_SECRET_{}", name.to_uppercase().replace('-', "_")), value));
    }
    Ok(env)
}

/// An askpass helper for one secret, and the variables that point programs at
/// it.
///
/// `ssh` takes a password from a terminal and from nowhere else — not from an
/// argument, not from the environment — so a secret reaches it only through the
/// helper OpenSSH already asks for. The same three variables serve `git` over
/// HTTPS and `sudo -A`. The script holds no value: it prints the variable it
/// inherits, so what is on disk is a line of shell and what is in memory is the
/// same environment the command already has.
struct Askpass {
    env: Vec<(String, String)>,
    /// Held so the directory outlives the command and is removed with it. Unix
    /// only, and the field is gated rather than the struct: `tempfile` is a
    /// dependency of this crate on unix alone, and a field naming a crate that
    /// is not there fails to compile on Windows however unreachable the code
    /// around it is — which is what CI said and a macOS gate could not.
    #[cfg(unix)]
    _dir: tempfile::TempDir,
}

#[cfg(unix)]
fn askpass(variable: &str) -> Option<Askpass> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().ok()?;
    let path = dir.path().join("askpass");
    let mut file = std::fs::File::create(&path).ok()?;
    file.write_all(format!("#!/bin/sh\nprintf '%s\\n' \"${{{variable}}}\"\n").as_bytes()).ok()?;
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).ok()?;
    let at = path.display().to_string();
    Some(Askpass {
        env: vec![
            ("SSH_ASKPASS".into(), at.clone()),
            // OpenSSH only consults the helper without a terminal unless it is
            // told to; 8.4 and later take `force`, and older ones want a
            // `DISPLAY` set, which is why both are here.
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            ("DISPLAY".into(), std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into())),
            ("GIT_ASKPASS".into(), at.clone()),
            ("SUDO_ASKPASS".into(), at),
        ],
        _dir: dir,
    })
}

/// No askpass on Windows: the programs that ask for one are not the ones people
/// run there, and inventing a path for it would be a mechanism nobody uses.
#[cfg(windows)]
fn askpass(_variable: &str) -> Option<Askpass> {
    None
}

/// Which of the two ends of a command arrived first.
///
/// The output ending and the command ending are not the same event, and a
/// command that backgrounds something ends without its output ever ending.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The pipes reached EOF: everything that was going to be printed was.
    Drained,
    /// The shell exited while the pipes were still open, which is either
    /// microseconds of ordinary lag or a child holding them open for good.
    Exited,
    TimedOut,
}

/// What to say about the kept copy, and where it is — or nothing, once the file
/// has been removed.
///
/// Only when something was actually left out. Naming a file that holds exactly
/// what is already on screen sends the model to read something it has, and a
/// copy of every `echo` ever run is the accumulator the cap exists to prevent.
fn settle(
    spill: Option<std::sync::Arc<std::sync::Mutex<Spill>>>,
    anything_lost: bool,
) -> Option<(String, String)> {
    let spill = spill?;
    let spill = spill.lock().unwrap_or_else(|e| e.into_inner());
    if !anything_lost {
        let _ = std::fs::remove_file(&spill.path);
        return None;
    }
    Some((spill.note(), spill.path.display().to_string()))
}

/// The same sentence wherever a command ran out of time: what it had printed is
/// the part worth reading, and a model told only that it timed out retries the
/// same command against the same limit.
fn timed_out(limit: std::time::Duration, killed: bool, printed: &str) -> String {
    format!(
        "command timed out after {}s{} — pass a larger `timeout_secs` if it needs longer. \
         What it printed first:\n{printed}",
        limit.as_secs(),
        if killed { " and was killed" } else { " and could not be killed" },
    )
}

/// Both streams as the model reads them, with stderr marked only when there is
/// some: a command that printed nothing to it should not appear to have.
fn joined(out: &Ends, err: &Ends) -> String {
    let mut combined = out.text();
    if err.seen > 0 {
        combined.push_str("\n--- stderr ---\n");
        combined.push_str(&err.text());
    }
    combined
}

/// Stop a child, and everything it started where the platform can say so.
///
/// The only place that difference is spelled out: a caller says "stop this" and
/// gets an answer, rather than each one branching on the platform.
pub(crate) async fn kill_tree(child: &mut tokio::process::Child) -> bool {
    #[cfg(unix)]
    if kill_group(child.id()) {
        return true;
    }
    child.kill().await.is_ok()
}

/// SIGKILL to the whole group. Windows has no equivalent that is not a job
/// object, so there `kill_on_drop` takes the shell and its children are left —
/// the timeout still reports what happened rather than claiming otherwise.
/// Whether anything is left in the command's process group.
///
/// Signal 0 is the question rather than an answer: it performs the permission
/// and existence checks and delivers nothing. The command's own shell is in
/// this group and has been reaped by the time this is asked, so a member left
/// is one the command started and did not wait for.
///
/// Elsewhere there is no group to ask about, and the caller's wait is all there
/// is — which is what it was everywhere before this.
fn group_alive(pid: Option<u32>) -> bool {
    match pid {
        #[cfg(unix)]
        Some(pid) => unsafe { libc::kill(-(pid as i32), 0) == 0 },
        #[cfg(not(unix))]
        Some(_) => true,
        None => false,
    }
}

pub(crate) fn kill_group(pid: Option<u32>) -> bool {
    match pid {
        #[cfg(unix)]
        Some(pid) => unsafe { libc::kill(-(pid as i32), libc::SIGKILL) == 0 },
        #[cfg(not(unix))]
        Some(_) => false,
        None => false,
    }
}

/// Both ends of a stream, bounded, and how much went past.
///
/// Reading to the end and cutting afterwards works until a command emits more
/// than memory. Keeping only the tail is what that first became, and it loses a
/// compiler's first error — the one that caused every later line. So both ends
/// are kept while reading, and the middle never lands anywhere.
struct Ends {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    seen: usize,
    cap: usize,
    /// Shared with the other stream, so the file holds both in the order they
    /// arrived — which is the order a terminal would have shown them.
    spill: Option<std::sync::Arc<std::sync::Mutex<Spill>>>,
}

impl Ends {
    fn new(cap: usize, spill: Option<std::sync::Arc<std::sync::Mutex<Spill>>>) -> Self {
        Self { head: Vec::new(), tail: Default::default(), seen: 0, cap, spill }
    }

    async fn drain(&mut self, reader: &mut (impl tokio::io::AsyncRead + Unpin)) {
        let mut chunk = vec![0u8; 64 * 1024];
        while let Ok(n) = reader.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            if let Some(spill) = &self.spill {
                spill.lock().unwrap_or_else(|e| e.into_inner()).write(&chunk[..n]);
            }
            self.seen += n;
            for &byte in &chunk[..n] {
                if self.head.len() < self.cap {
                    self.head.push(byte);
                    continue;
                }
                self.tail.push_back(byte);
                if self.tail.len() > self.cap {
                    self.tail.pop_front();
                }
            }
        }
    }

    fn text(&self) -> String {
        let head = String::from_utf8_lossy(&self.head);
        if self.tail.is_empty() {
            return head.into_owned();
        }
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let dropped = self.seen - self.head.len() - self.tail.len();
        let gap = match dropped {
            0 => String::new(),
            n => format!("\n[{n} bytes elided from the middle]\n"),
        };
        format!("{head}{gap}{}", String::from_utf8_lossy(&tail))
    }
}
