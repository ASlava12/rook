//! User-supplied commands that run at points in a turn.
//!
//! The demand this answers is "run the formatter after every edit", "block this
//! shape of command", "tell me when it finishes" — things people should not have
//! to fork an agent to get. codex defines twelve events with a schema each; five
//! cover the same ground, and a smaller surface is one a person can hold in mind.
//!
//! A hook reads JSON on stdin and may write JSON on stdout to influence what
//! happens next. Plain text on stdout is treated as context, so `echo` works.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use rook_tools::policy::{Decision, Rule};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// Once per loop, before anything is sent. Its context joins the system prompt.
    SessionStart,
    /// Before a turn runs. May refuse the turn.
    Prompt,
    /// Before a tool call. May allow, deny, or force an approval prompt.
    PreTool,
    /// After a tool call. Its context is appended to what the model sees, which
    /// is where "format the file that was just written" belongs.
    PostTool,
    /// After the turn finishes. Nothing it says changes the outcome.
    TurnEnd,
}

impl Event {
    /// The spelling the user writes in `config.toml`, which is what they should
    /// read back — the debug name is a Rust identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Event::SessionStart => "session_start",
            Event::Prompt => "prompt",
            Event::PreTool => "pre_tool",
            Event::PostTool => "post_tool",
            Event::TurnEnd => "turn_end",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HookConfig {
    pub event: Event,
    /// Restricts the hook to matching subjects: a tool name for the tool events,
    /// the prompt otherwise. A plain string matches as a substring, `/…/` is a
    /// regular expression.
    #[serde(rename = "match")]
    pub matches: Option<String>,
    pub command: String,
    pub timeout_secs: u64,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self { event: Event::PostTool, matches: None, command: String::new(), timeout_secs: 30 }
    }
}

/// What a hook asked for.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HookReply {
    /// `allow` skips the approval prompt, `ask` forces one, `deny` refuses.
    /// It cannot unlock something the deny list forbids — see ADR-0009.
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Extra text for the model.
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub decision: Option<Decision>,
    pub context: Vec<String>,
}

impl Outcome {
    pub fn context(&self) -> Option<String> {
        (!self.context.is_empty()).then(|| self.context.join("\n"))
    }
}

pub struct Hooks {
    hooks: Vec<(HookConfig, Option<Rule>)>,
}

impl Hooks {
    /// Unusable match patterns are reported rather than dropped: a hook that
    /// silently never fires is worse than one that fails loudly.
    pub fn compile(configs: &[HookConfig]) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let hooks = configs
            .iter()
            .filter(|c| !c.command.trim().is_empty())
            .map(|config| {
                let rule = config.matches.as_deref().and_then(|pattern| {
                    Rule::parse(pattern).map_err(|e| errors.push(format!("{}: {e}", config.command))).ok()
                });
                (config.clone(), rule)
            })
            .collect();
        (Self { hooks }, errors)
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Run every hook registered for `event` whose matcher accepts `subject`.
    ///
    /// The first denial stops the rest: once the answer is no, running further
    /// commands only delays it.
    pub async fn run(&self, event: Event, subject: &str, payload: &serde_json::Value) -> Outcome {
        let mut outcome = Outcome::default();
        for (config, rule) in &self.hooks {
            if config.event != event {
                continue;
            }
            if let Some(rule) = rule
                && !rule.matches(subject)
            {
                continue;
            }

            let reply = match invoke(config, payload).await {
                Ok(reply) => reply,
                Err(e) => {
                    tracing::warn!("hook {:?} failed: {e}", config.command);
                    // A hook that cannot run must not silently approve what it
                    // was there to check.
                    if event == Event::PreTool {
                        outcome.decision = Some(Decision::Deny(format!("hook failed: {e}")));
                        return outcome;
                    }
                    continue;
                }
            };

            if let Some(context) = reply.context.filter(|c| !c.trim().is_empty()) {
                outcome.context.push(context);
            }
            match reply.decision.as_deref() {
                Some("deny") | Some("block") => {
                    let reason = reply.reason.unwrap_or_else(|| config.command.clone());
                    outcome.decision = Some(Decision::Deny(format!("hook: {reason}")));
                    return outcome;
                }
                Some("ask") => outcome.decision = Some(Decision::Ask),
                Some("allow") if outcome.decision.is_none() => outcome.decision = Some(Decision::Allow),
                _ => {}
            }
        }
        outcome
    }
}

/// A hook's command is a line for a shell, and the two shells do not agree on
/// how to get one there.
///
/// On Windows `arg` is wrong: Rust quotes an argument for the C runtime's rules,
/// escaping an embedded `"` as `\"`, and `cmd.exe` parses neither — it takes the
/// backslash literally. A hook whose command contains a quotation mark, which is
/// most of the useful ones, arrived mangled. `raw_arg` hands the line over
/// untouched, which is what `cmd /C` wants.
#[cfg(windows)]
fn shell(command: &str) -> tokio::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = tokio::process::Command::new("cmd");
    c.as_std_mut().raw_arg(format!("/C {command}"));
    c
}

#[cfg(not(windows))]
fn shell(command: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("/bin/sh");
    c.arg("-c").arg(command);
    c
}

async fn invoke(config: &HookConfig, payload: &serde_json::Value) -> std::io::Result<HookReply> {
    let mut command = shell(&config.command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.to_string().as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let output = tokio::time::timeout(Duration::from_secs(config.timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no answer within {}s", config.timeout_secs),
            )
        })??;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(std::io::Error::other(format!(
            "exit {}: {}",
            output.status.code().unwrap_or(-1),
            if stderr.is_empty() { stdout } else { stderr }
        )));
    }

    // JSON when it is JSON, otherwise whatever was printed is the context.
    Ok(serde_json::from_str(&stdout)
        .unwrap_or(HookReply { context: (!stdout.is_empty()).then_some(stdout), ..Default::default() }))
}
