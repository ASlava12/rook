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
                    "timeout_secs": { "type": "integer" }
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

        if let Some(terminals) = &ctx.terminals {
            return elsewhere(terminals.as_ref(), &command, &cwd, ctx, timeout).await;
        }

        let mut child = spawn_shell(&command, &cwd)?;
        let group = child.id();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

        // One cap at each end, so a runaway command costs bounded memory and
        // both the first error and the last line survive it. Outside the future
        // that fills them, so a timeout still has what was printed before it.
        let keep = ctx.max_output_bytes;
        let mut out = Ends::new(keep);
        let mut err = Ends::new(keep);
        let overran = {
            // Together, not one after the other: a pipe holds about 64 KiB, and
            // a command that fills stderr while stdout is being drained blocks
            // on the write — so it never finishes stdout and the drain never
            // ends. Any build with enough warnings did exactly that, and hung
            // until the timeout with nothing to show for it.
            let capture = async {
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
            };
            tokio::time::timeout(timeout, capture).await.is_err()
        };

        if overran {
            // The whole group, not the shell: `sh -c` may fork rather than
            // exec, and killing the shell alone leaves the real work running.
            let killed = kill_group(group);
            return Ok(
                ToolOutcome::error(timed_out(timeout, killed, &joined(&out, &err))).with("timed_out", true)
            );
        }
        let status = child.wait().await;

        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let full = out.seen + err.seen;
        let mut combined = joined(&out, &err);
        let truncated = full > combined.len().min(ctx.max_output_bytes);
        if truncated {
            combined = crate::elide_middle(&combined, ctx.max_output_bytes);
        }

        Ok(ToolOutcome {
            content: format!("exit {code}\n{combined}"),
            is_error: code != 0,
            truncated,
            full_bytes: full,
            meta: Default::default(),
        }
        .with("exit_code", code))
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

fn spawn_shell(command: &str, cwd: &std::path::Path) -> Result<tokio::process::Child> {
    #[cfg(windows)]
    // `cmd /C` rather than PowerShell: it is always present, and skills that
    // need PowerShell can invoke it explicitly. `raw_arg` rather than `arg`:
    // `arg` quotes for the C runtime's rules and escapes an embedded `"` as
    // `\"`, which `cmd.exe` does not read that way — it takes the backslash
    // literally. A command with a quotation mark in it, which is most of the
    // ones worth running, arrived at the shell mangled.
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = tokio::process::Command::new("cmd");
        c.as_std_mut().raw_arg(format!("/C {command}"));
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("/bin/sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(cwd)
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

/// SIGKILL to the whole group. Windows has no equivalent that is not a job
/// object, so there `kill_on_drop` takes the shell and its children are left —
/// the timeout still reports what happened rather than claiming otherwise.
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

fn kill_group(pid: Option<u32>) -> bool {
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
}

impl Ends {
    fn new(cap: usize) -> Self {
        Self { head: Vec::new(), tail: Default::default(), seen: 0, cap }
    }

    async fn drain(&mut self, reader: &mut (impl tokio::io::AsyncRead + Unpin)) {
        let mut chunk = vec![0u8; 64 * 1024];
        while let Ok(n) = reader.read(&mut chunk).await {
            if n == 0 {
                break;
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
