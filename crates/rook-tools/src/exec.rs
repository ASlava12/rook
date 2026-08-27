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
        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .map(std::time::Duration::from_secs)
            .unwrap_or(ctx.command_timeout);

        let mut child = spawn_shell(&command, &cwd)?;
        let group = child.id();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

        // Twice the cap: enough to know the output overflowed and to keep a
        // meaningful tail, without holding a runaway command's gigabytes.
        let keep = ctx.max_output_bytes.saturating_mul(2);
        let capture = async {
            let mut out = Tail::new(keep);
            let mut err = Tail::new(keep);
            if let Some(s) = stdout.as_mut() {
                out.drain(s).await;
            }
            if let Some(s) = stderr.as_mut() {
                err.drain(s).await;
            }
            let status = child.wait().await;
            (out, err, status)
        };

        let (out, err, status) = match tokio::time::timeout(timeout, capture).await {
            Ok(v) => v,
            Err(_) => {
                // The whole group, not the shell: `sh -c` may fork rather than
                // exec, and killing the shell alone leaves the real work running.
                let killed = kill_group(group);
                return Ok(ToolOutcome::error(format!(
                    "command timed out after {}s{}: {command}",
                    timeout.as_secs(),
                    if killed { " and was killed" } else { " and could not be killed" }
                ))
                .with("timed_out", true));
            }
        };

        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let full = out.seen + err.seen;
        let mut combined = String::from_utf8_lossy(&out.kept).into_owned();
        if !err.kept.is_empty() {
            combined.push_str("\n--- stderr ---\n");
            combined.push_str(&String::from_utf8_lossy(&err.kept));
        }
        let truncated = full > combined.len().min(ctx.max_output_bytes);
        if truncated {
            // Keep the tail: exit messages and stack traces live at the end.
            let start = combined.len() - ctx.max_output_bytes;
            let start =
                (start..combined.len()).find(|i| combined.is_char_boundary(*i)).unwrap_or(combined.len());
            combined = format!(
                "[{} bytes elided; showing the last {}]\n{}",
                start,
                combined.len() - start,
                &combined[start..]
            );
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

fn spawn_shell(command: &str, cwd: &std::path::Path) -> Result<tokio::process::Child> {
    let mut cmd = if cfg!(windows) {
        // `cmd /C` rather than PowerShell: it is always present, and skills that
        // need PowerShell can invoke it explicitly.
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
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
fn kill_group(pid: Option<u32>) -> bool {
    match pid {
        #[cfg(unix)]
        Some(pid) => unsafe { libc::kill(-(pid as i32), libc::SIGKILL) == 0 },
        #[cfg(not(unix))]
        Some(_) => false,
        None => false,
    }
}

/// The last `keep` bytes of a stream, and how many went past.
///
/// Reading to the end first and capping afterwards works until a command emits
/// more than memory: the cap has to bound the read, not the reply.
struct Tail {
    kept: Vec<u8>,
    seen: usize,
    keep: usize,
}

impl Tail {
    fn new(keep: usize) -> Self {
        Self { kept: Vec::new(), seen: 0, keep }
    }

    async fn drain(&mut self, reader: &mut (impl tokio::io::AsyncRead + Unpin)) {
        let mut chunk = vec![0u8; 64 * 1024];
        while let Ok(n) = reader.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            self.seen += n;
            self.kept.extend_from_slice(&chunk[..n]);
            if self.kept.len() > self.keep {
                let cut = self.kept.len() - self.keep;
                self.kept.drain(..cut);
            }
        }
    }
}
