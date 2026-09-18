//! Approval, checkpoints, write tracking and post-edit diagnostics.

use super::{AgentLoop, Reported, STANCE, WROTE};
use crate::hooks::{self};
use rook_store::EventKind;
use rook_tools::policy::{Decision, Stance};

/// What to show whoever is asked to approve a call.
///
/// A variant rather than a string because building it costs a file read and a
/// diff, and most calls are never asked about.
pub(super) enum Shown<'a> {
    Nothing,
    Text(&'a str),
    Tool(&'a std::sync::Arc<dyn rook_tools::Tool>),
}

/// What [`AgentLoop::checkpoint_before`] hands back: the claim to hold for the
/// duration of the call, and whatever the model has to be told.
type ClaimedResult<'a> = std::result::Result<(Option<crate::service::Writing<'a>>, Option<String>), String>;

/// The same for the toolbox. `run_command` is deliberately absent — verifying a
/// claim means running things — so this stops a checker editing the work it is
/// judging, and is not a sandbox.
pub(super) const CHANGES_FILES: &[&str] = &["write_file", "edit_file", "delete_file", "move_file"];

fn canonical(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// How long a check made on the model's behalf waits for a server to answer.
/// A question somebody asked is worth the configured three seconds; this is
/// paid on every write, and what is not ready by then is simply not reported.
const CHECK_WAIT: std::time::Duration = std::time::Duration::from_millis(1_200);

/// How many of a call's files are checked, and how many problems are named.
/// Both bounded because this is appended to a tool result: a refactor across
/// forty files must not answer with forty analyses.
const FILES_CHECKED: usize = 3;

const PROBLEMS_REPORTED: usize = 5;

impl Shown<'_> {
    async fn build(&self, ctx: &rook_tools::ToolContext, args: &serde_json::Value) -> Option<String> {
        let preview = match self {
            Shown::Nothing => None,
            Shown::Text(text) => Some((*text).to_string()),
            Shown::Tool(tool) => tool.preview(ctx, args).await,
        };
        // What the call would spend, said where the question is asked. A person
        // approving a command should see that it comes with a password
        // attached, whatever else the preview shows — and here rather than in
        // the tool, because the answer is the same for every tool that grows
        // the argument.
        let named: Vec<&str> = args
            .get("secrets")
            .and_then(|s| s.as_array())
            .map(|names| names.iter().filter_map(|n| n.as_str()).collect())
            .unwrap_or_default();
        if named.is_empty() {
            return preview;
        }
        let spent = format!("uses the secret {}", named.join(", "));
        Some(match preview {
            Some(preview) => format!("{spent}\n\n{preview}"),
            None => spent,
        })
    }
}

impl<'a> AgentLoop<'a> {
    /// Consult the policy and any `pre_tool` hooks, and the user when the answer
    /// is to ask. Returns the refusal to hand back to the model, or `None` when
    /// the call may proceed.
    pub(super) async fn gate(&self, call: &rook_llm::ToolCall) -> Option<String> {
        let tool = self.tools.get(&call.name)?;
        let risk = tool.risk(&call.arguments);
        self.gate_risk(&call.name, &call.arguments, risk, Shown::Tool(tool)).await
    }

    pub(super) async fn gate_risk(
        &self,
        name: &str,
        arguments: &serde_json::Value,
        risk: rook_tools::policy::Risk,
        shown: Shown<'_>,
    ) -> Option<String> {
        // The policy runs first so a hook cannot unlock what the deny list
        // forbids; everything else, a hook may override.
        let mut decision = self.policy.decide(&risk);
        if !matches!(decision, Decision::Deny(_)) && !self.hooks.is_empty() {
            let payload = self.payload(serde_json::json!({
                "tool": name,
                "input": arguments,
                "action": risk.describe(),
            }));
            let outcome = self.hooks.run(hooks::Event::PreTool, name, &payload).await;
            if let Some(hooked) = outcome.decision {
                decision = hooked;
            }
        }

        match decision {
            Decision::Allow => None,
            Decision::Deny(why) => Some(format!("refused: {why}")),
            // Built here rather than earlier because a hook may turn an allowed
            // call into one somebody is asked about, and a diff of a call nobody
            // is asked about is work nobody reads.
            Decision::Ask => match self
                .approver
                .ask(name, &risk, shown.build(&self.tool_ctx, arguments).await.as_deref())
                .await
            {
                rook_tools::policy::Approval::Once => None,
                rook_tools::policy::Approval::ForRun => {
                    self.policy.grant_for_run(&risk.subject());
                    None
                }
                rook_tools::policy::Approval::KindForRun => {
                    self.policy.grant_kind_for_run(&risk);
                    None
                }
                rook_tools::policy::Approval::Deny(why) => {
                    self.report(Reported::Decision(format!("{name}: {} — declined", risk.describe())));
                    Some(format!("refused: {why}"))
                }
                rook_tools::policy::Approval::Unanswered(why) => {
                    self.report(Reported::Open(format!(
                        "{name} wanted to {}, and nobody was here to say",
                        risk.describe()
                    )));
                    Some(rook_tools::policy::no_one_answered(&why))
                }
            },
        }
    }

    /// The agent asking for more latitude. A person grants it or nobody can;
    /// the agent never raises its own stance.
    pub(super) async fn request_stance(&self, args: &serde_json::Value) -> String {
        let Some(to) = args.get("to").and_then(|t| t.as_str()).and_then(Stance::parse) else {
            let names: Vec<&str> = Stance::ALL.iter().map(|s| s.as_str()).collect();
            return format!("stance needs `to`: one of {}", names.join(", "));
        };
        let now = self.policy.stance();
        if to <= now {
            return format!("already at `{}`; a stance is only ever asked up", now.as_str());
        }
        let why = args.get("why").and_then(|w| w.as_str()).unwrap_or("").trim().to_string();
        let shown = match why.is_empty() {
            true => Shown::Nothing,
            false => Shown::Text(&why),
        };
        match self.gate_risk(STANCE, args, rook_tools::policy::Risk::Stance(to), shown).await {
            Some(refusal) => refusal,
            None => {
                self.policy.set_stance(to);
                let note = format!("stance raised to `{}` for the rest of the run", to.as_str());
                self.rook.log(self.session, EventKind::Note, "stance", &note).ok();
                self.report(Reported::Decision(note.clone()));
                note
            }
        }
    }

    /// Snapshot whatever a mutating tool is about to touch, so `rook session
    /// rewind` can put the files back. Read-only tools report no paths and cost
    /// nothing here.
    /// Returns what the model has to be told, which is nothing when the files
    /// were captured.
    ///
    /// A capture that fails takes the session's undo with it: `session rewind`
    /// restores from these, so a file edited without one is edited for good.
    /// That was a line in the log file, where neither the model nor the user was
    /// looking, and both believed the edit was recoverable.
    /// Whether this call is a command: something that changes the machine and
    /// will not say what it touches. A tool that declares its paths is
    /// checkpointed and diffed properly, and a read needs no watching.
    pub(super) fn watching_a_command(&self, call: &rook_llm::ToolCall) -> bool {
        let Some(tool) = self.tools.get(&call.name) else { return false };
        tool.touched_paths(&call.arguments).is_empty()
            && matches!(tool.risk(&call.arguments), rook_tools::policy::Risk::Execute(_))
    }

    /// Record what the command wrote, or that the workspace was too large to
    /// tell. Both belong in the log: the second is why the first is empty.
    pub(super) fn note_what_was_written(&self, since: std::time::SystemTime) -> Vec<std::path::PathBuf> {
        let (written, whole) = self.rook.written_since(since, &crate::CaptureLimits::for_skill());
        if written.is_empty() && whole {
            return Vec::new();
        }
        let note = serde_json::json!({ "paths": written, "complete": whole });
        self.rook.log(self.session, EventKind::Note, WROTE, &note.to_string()).ok();
        written.iter().filter_map(|p| self.tool_ctx.resolve(p).ok()).collect()
    }

    /// What a language server makes of the files a call just wrote, minus what
    /// it already made of them.
    ///
    /// A model has to think to ask `diagnostics`; it never has to think to
    /// read the answer to the call it just made. The turn that asked for this
    /// broke an indent with `sed -i` and then spent three steps and three
    /// identical `py_compile` runs finding out, with a server running beside
    /// it that knew at once.
    ///
    /// Errors the file already had are not reported: they are somebody else's
    /// news, and on every write they are noise.
    pub(super) async fn what_this_broke(&self, paths: &[std::path::PathBuf]) -> Option<String> {
        if self.servers.is_empty() {
            return None;
        }
        let mut new_lines: Vec<String> = Vec::new();
        for path in paths.iter().take(FILES_CHECKED) {
            let Some(now) = self.servers.errors_in(path, CHECK_WAIT).await else { continue };
            let before = self.remember_problems(path, &now);
            new_lines.extend(now.iter().filter(|line| !before.contains(line)).cloned());
        }
        if new_lines.is_empty() {
            return None;
        }
        let more = new_lines.len().saturating_sub(PROBLEMS_REPORTED);
        new_lines.truncate(PROBLEMS_REPORTED);
        Some(format!(
            "the language server reports {} not there before this call:\n{}{}",
            match new_lines.len() + more {
                1 => "a problem".to_string(),
                n => format!("{n} problems"),
            },
            new_lines.join("\n"),
            match more {
                0 => String::new(),
                n => format!("\n… and {n} more"),
            }
        ))
    }

    /// The problems this file had last time, replaced by the ones it has now.
    /// An unseen file counts as having had all of them, so the first write to
    /// somebody's file reports nothing and the next one reports what changed.
    fn remember_problems(&self, path: &std::path::Path, now: &[String]) -> Vec<String> {
        let mut seen = self.problems_before.lock().unwrap_or_else(|e| e.into_inner());
        // Canonical, because the two routes here spell a path differently — a
        // tool's declared argument and a walk of what a command wrote — and two
        // keys for one file would make its own history look like new problems.
        match seen.insert(canonical(path), now.to_vec()) {
            Some(before) => before,
            None => now.to_vec(),
        }
    }

    /// Kept as the model asked for them — relative to the workspace, which is
    /// how they will be read back — and bounded, because everything that
    /// accumulates here is.
    pub(super) fn note_paths(&self, paths: &[std::path::PathBuf]) {
        const MOST: usize = 200;
        let mut wrote = self.wrote_paths.lock().unwrap_or_else(|e| e.into_inner());
        // Both spellings of the workspace, because one route here canonicalises
        // and the other does not: on macOS `/var` and `/private/var` are the
        // same directory, and a path stripped against the wrong one is reported
        // to the model in full, from the root.
        let root = canonical(&self.rook.workspace);
        for path in paths {
            if wrote.len() >= MOST {
                return;
            }
            let shown =
                path.strip_prefix(&root).or_else(|_| path.strip_prefix(&self.rook.workspace)).unwrap_or(path);
            wrote.insert(shown.display().to_string());
        }
    }

    /// Whether this turn has yet to look at a file, and so needs the reading
    /// before the write: where it has one, that is the baseline, and reading
    /// again before every write is a second wait for what is already known.
    pub(super) fn unseen(&self, path: &std::path::Path) -> bool {
        !self.problems_before.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&canonical(path))
    }

    /// The files a call declares it will write, resolved. A command declares
    /// none — what it wrote is discovered afterwards instead.
    pub(super) fn will_write(&self, call: &rook_llm::ToolCall) -> Vec<std::path::PathBuf> {
        let Some(tool) = self.tools.get(&call.name) else { return Vec::new() };
        tool.touched_paths(&call.arguments).iter().filter_map(|p| self.tool_ctx.resolve(p).ok()).collect()
    }

    pub(super) fn checkpoint_before(&self, call: &rook_llm::ToolCall) -> ClaimedResult<'_> {
        // Not a tool of the toolbox — the loop's own, which write through their
        // own paths and take their own checkpoints.
        let Some(tool) = self.tools.get(&call.name) else { return Ok((None, None)) };
        let paths: Vec<std::path::PathBuf> = tool
            .touched_paths(&call.arguments)
            .iter()
            .filter_map(|p| self.tool_ctx.resolve(p).ok())
            .collect();
        if paths.is_empty() {
            return Ok((None, None));
        }
        if tool.overwrites()
            && let Some(unseen) = self.rook.overwriting_unseen(self.session, &paths)
        {
            return Err(unseen);
        }
        // The paths a checkpoint is about to capture are exactly the ones
        // another turn in this project must not be writing, so the claim is
        // asked for here, where they are already known.
        let held = self.rook.writing(self.session, &paths).map_err(|e| e.to_string())?;
        // Writing it makes this turn the one that has seen it.
        self.rook.touched(self.session, &paths);

        let Some(failure) = self
            .rook
            .checkpoint_paths(self.session, &call.name, &paths, &crate::CaptureLimits::for_skill())
            .err()
        else {
            return Ok((Some(held), None));
        };
        let note = format!(
            "no checkpoint was taken first ({failure}), so `rook session rewind` cannot undo this one."
        );
        tracing::warn!("checkpoint before {}: {failure}", call.name);
        self.rook.log(self.session, EventKind::Error, "checkpoint", &note).ok();
        Ok((Some(held), Some(note)))
    }
}
