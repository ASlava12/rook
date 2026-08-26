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
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

        let capture = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut out).await;
            }
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut err).await;
            }
            let status = child.wait().await;
            (out, err, status)
        };

        let (out, err, status) = match tokio::time::timeout(timeout, capture).await {
            Ok(v) => v,
            Err(_) => {
                return Ok(ToolOutcome::error(format!(
                    "command timed out after {}s and was killed: {command}",
                    timeout.as_secs()
                ))
                .with("timed_out", true));
            }
        };

        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let mut combined = String::from_utf8_lossy(&out).into_owned();
        if !err.is_empty() {
            combined.push_str("\n--- stderr ---\n");
            combined.push_str(&String::from_utf8_lossy(&err));
        }
        let full = combined.len();
        let truncated = full > ctx.max_output_bytes;
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
    cmd.spawn().map_err(|e| ToolError::Io { path: cwd.to_path_buf(), source: e })
}
