//! A bounded companion to execution receipts, independent of prompt compaction.

use super::{AgentLoop, CHANGES_FILES};
use crate::{CoreError, Result};
use rook_llm::ToolCall;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::VecDeque;

type Fingerprint = [u8; 32];
const MAX_BYTES: usize = 128 * 1024;
const WARN: usize = 10;
const STOP: usize = 20;

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Guard {
    version: u8,
    scope: Fingerprint,
    history: VecDeque<Seen>,
    refusals: u8,
    warned: bool,
    stopped: Option<Reason>,
    workspace: Option<Fingerprint>,
    instruction: u64,
    instruction_session: u128,
    #[serde(skip)]
    key: String,
}

#[derive(Serialize, Deserialize)]
struct Seen {
    call: Fingerprint,
    arguments: Fingerprint,
    input_complete: bool,
    family: Fingerprint,
    result: Fingerprint,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum Reason {
    RepeatedCall,
    ArgumentChurn,
    UnknownTool,
}

impl Reason {
    fn name(self) -> &'static str {
        match self {
            Self::RepeatedCall => "repeated_call",
            Self::ArgumentChurn => "argument_churn",
            Self::UnknownTool => "unknown_tool_repeat",
        }
    }
}

// Serialize directly into a hasher: arguments and results can be very large,
// but the guard never makes another copy of either.
fn fingerprint(value: &impl Serialize) -> Result<Fingerprint> {
    struct Hash(sha2::Sha256);
    impl std::io::Write for Hash {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hash = Hash(sha2::Sha256::new());
    serde_json::to_writer(&mut hash, value)?;
    Ok(hash.0.finalize().into())
}

impl Guard {
    fn reset(&mut self) {
        self.history.clear();
        self.refusals = 0;
        self.warned = false;
        self.stopped = None;
    }

    fn same(&self, call: &Fingerprint) -> usize {
        let Some(last) = self.history.iter().rev().find(|seen| &seen.call == call) else {
            return 0;
        };
        self.history
            .iter()
            .rev()
            .filter(|seen| &seen.call == call)
            .take_while(|seen| seen.result == last.result)
            .count()
    }

    fn observe(&mut self, seen: Seen, limit: usize, unknown: bool) {
        if self.history.len() >= limit {
            self.history.pop_front();
        }
        let churn =
            self.history.iter().filter(|old| old.family == seen.family && old.result == seen.result).count()
                + 1;
        self.history.push_back(seen);
        if churn >= WARN {
            self.warned = true;
        }
        if churn >= STOP {
            self.stopped = Some(if unknown { Reason::UnknownTool } else { Reason::ArgumentChurn });
        }
    }

    fn encoded(&self) -> Result<Vec<u8>> {
        crate::persistence::encode_with_limit(self, MAX_BYTES)
    }
}

impl AgentLoop<'_> {
    fn cycle_identity(&self) -> Result<(String, Fingerprint)> {
        let root = std::fs::canonicalize(&self.rook.workspace)
            .map_err(|source| CoreError::Io { path: self.rook.workspace.clone(), source })?;
        let (key, boundary) = if self.depth == 0
            && !self.checking
            && let Some(run) = &self.managed_work
        {
            (
                format!("rook:tool-cycles:run:{}", rook_store::ObjectId::of(run.id.as_bytes()).to_hex()),
                fingerprint(&(&run.id, &run.generation))?,
            )
        } else {
            let execution = crate::execution::current(self.rook, self.session)?
                .ok_or_else(|| CoreError::Other("execution receipt is missing".into()))?;
            (
                format!("rook:tool-cycles:session:{}:{}", self.session, self.checking),
                fingerprint(&execution.completion_boundary())?,
            )
        };
        Ok((key, fingerprint(&(root, boundary, self.checking))?))
    }

    pub(super) fn init_tool_cycles(&self) -> Result<()> {
        if !self.rook.config.agent.tool_cycle_guard {
            return Ok(());
        }
        let (key, scope) = self.cycle_identity()?;
        let mut held = self.tool_cycles.lock().unwrap_or_else(|e| e.into_inner());
        if held.as_ref().is_some_and(|guard| guard.scope == scope) {
            return Ok(());
        }
        let mut guard = match self.rook.store.kv_get_limited(&key, MAX_BYTES)? {
            Some(bytes) => {
                let guard: Guard = serde_json::from_slice(&bytes).map_err(|why| {
                    CoreError::Other(format!(
                        "tool-cycle receipt is invalid: {why}; inspect session recovery"
                    ))
                })?;
                if guard.version != 1 || guard.history.len() > 128 || guard.refusals > 3 {
                    return Err(CoreError::Other(
                        "tool-cycle receipt exceeds its schema bounds; inspect session recovery".into(),
                    ));
                }
                if guard.scope == scope { guard } else { Guard::default() }
            }
            None => Guard::default(),
        };
        guard.version = 1;
        if self.fresh_cycle_instruction && self.managed_work.is_some() {
            guard.reset();
        }
        while guard.history.len() > self.rook.config.agent.max_tool_cycle_observations.clamp(20, 128) {
            guard.history.pop_front();
        }
        guard.key = key;
        guard.scope = scope;
        // Acceptance commits before delivery. Also inspect the durable labels
        // on recovery, so a crash between acceptance and this reset cannot
        // resurrect the old stall after a new user correction.
        let meta = self
            .rook
            .store
            .get_session(self.session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(self.session)))?;
        let mut at = if guard.history.is_empty() {
            meta.next_seq
        } else if guard.instruction_session == self.session {
            guard.instruction
        } else {
            0
        };
        while at < meta.next_seq {
            let events = self.rook.store.events(self.session, at, 128)?;
            if events.is_empty() {
                break;
            }
            for event in events {
                at = event.seq + 1;
                if event.record.kind == rook_store::EventKind::UserMessage
                    && event.record.label == "while running"
                {
                    guard.reset();
                }
            }
        }
        guard.instruction = meta.next_seq;
        guard.instruction_session = self.session;
        *held = Some(guard);
        Ok(())
    }

    // This is deliberately a small, complete content proof, never an mtime or
    // a successful tool name. An oversized/unreadable tree is unknown and
    // cannot be used to erase a stall. No new file blobs are retained.
    pub(super) fn cycle_workspace(&self) -> Option<Fingerprint> {
        crate::FileSet::content_of(&self.rook.workspace, &crate::CaptureLimits::for_skill())
            .ok()
            .and_then(|files| fingerprint(&files).ok())
    }

    pub(super) fn cycle_may_write(&self, call: &ToolCall) -> bool {
        CHANGES_FILES.contains(&call.name.as_str())
            || self.watching_a_command(call)
            || !self.hooks.is_empty()
            || self.tools.get(&call.name).is_some_and(|tool| !tool.touched_paths(&call.arguments).is_empty())
    }

    fn cycle_live_poll(&self, call: &ToolCall) -> bool {
        if call.name != "job" || call.arguments.get("stop").and_then(|v| v.as_bool()) == Some(true) {
            return false;
        }
        self.tool_ctx
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.is_running(call.arguments.get("id").and_then(|v| v.as_str())))
    }

    fn cycle_arguments(&self, call: &ToolCall) -> Result<Fingerprint> {
        // Only presentation fields of the built-in command are ignored. Do
        // not strip arbitrary MCP arguments that might change an operation.
        struct Arguments<'a>(&'a ToolCall);
        impl Serialize for Arguments<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap;
                let Some(args) = self.0.arguments.as_object() else {
                    return self.0.arguments.serialize(serializer);
                };
                let mut map = serializer.serialize_map(None)?;
                for (key, value) in args {
                    if (self.0.name == "run_command" && matches!(key.as_str(), "title" | "description"))
                        || (self.0.name == "job" && key.as_str() == "wait_secs")
                    {
                        continue;
                    }
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
        let registry =
            if call.name == "job" { self.tool_ctx.jobs.as_ref().map(|jobs| jobs.identity()) } else { None };
        fingerprint(&(&call.name, Arguments(call), registry))
    }

    fn cycle_call(&self, call: &ToolCall) -> Result<(Fingerprint, bool)> {
        let arguments = self.cycle_arguments(call)?;
        // File reads outside the small workspace proof (ignored files, or a
        // separately allowed path) still have their own bounded content input.
        // A changed input must not be refused based on the earlier answer.
        let mut input = sha2::Sha256::new();
        let mut total = 0;
        let mut complete = self.tool_ctx.files.is_none();
        if let Some(tool) = self.tools.get(&call.name) {
            let paths = tool.observed_paths(&call.arguments);
            if paths.len() > 32 {
                return Ok((arguments, false));
            }
            for path in paths {
                let Ok(path) = self.tool_ctx.resolve(&path) else {
                    complete = false;
                    continue;
                };
                input.update(path.as_os_str().to_string_lossy().as_bytes());
                let meta = match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(why) => {
                        complete &= why.kind() == std::io::ErrorKind::NotFound;
                        input.update(b"missing-or-unreadable");
                        continue;
                    }
                };
                if !meta.is_file() {
                    continue;
                }
                let allowance = (8 * 1024 * 1024u64).min((32 * 1024 * 1024u64).saturating_sub(total));
                if meta.len() > allowance {
                    complete = false;
                    input.update(b"unknown-large-input");
                    continue;
                }
                use std::io::Read;
                let Ok(file) = std::fs::File::open(&path) else {
                    complete = false;
                    input.update(b"unreadable");
                    continue;
                };
                let mut reader = file.take(allowance + 1);
                let hashed = rook_store::ObjectId::of_reader(&mut reader);
                let count = allowance + 1 - reader.limit();
                total += count;
                let Ok(hash) = hashed else {
                    complete = false;
                    input.update(b"unreadable");
                    continue;
                };
                if count > allowance {
                    complete = false;
                    input.update(b"unknown-growing-input");
                    continue;
                }
                input.update(hash.as_bytes());
            }
        }
        Ok((fingerprint(&(arguments, <[u8; 32]>::from(input.finalize())))?, complete))
    }

    pub(super) fn cycle_refusal(&self, call: &ToolCall, live_child: bool) -> Result<Option<(String, u64)>> {
        if !self.rook.config.agent.tool_cycle_guard {
            return Ok(None);
        }
        self.init_tool_cycles()?;
        if live_child || self.cycle_live_poll(call) {
            return Ok(None);
        }
        let (key, complete) = self.cycle_call(call)?;
        let arguments = self.cycle_arguments(call)?;
        let mut held = self.tool_cycles.lock().unwrap_or_else(|e| e.into_inner());
        let Some(guard) = held.as_mut() else {
            return Ok(None);
        };
        if guard
            .history
            .iter()
            .rev()
            .find(|seen| seen.arguments == arguments)
            .is_some_and(|seen| seen.input_complete && complete && seen.call != key)
        {
            // Same operation on newly observed bytes is content progress,
            // including a file excluded from the whole-workspace proof.
            guard.reset();
        }
        if guard.history.is_empty() && guard.workspace.is_none() && !self.cycle_may_write(call) {
            guard.workspace = self.cycle_workspace();
        }
        let times = guard.same(&key);
        if (times < 2 || !complete) && guard.stopped.is_none() {
            return Ok(None);
        }
        let workspace = self.cycle_workspace();
        if workspace.is_some() && guard.workspace.is_some() && workspace != guard.workspace {
            guard.reset();
            guard.workspace = workspace;
            return Ok(None);
        }
        guard.refusals = (guard.refusals + 1).min(3);
        if guard.refusals >= 3 {
            guard.stopped = Some(Reason::RepeatedCall);
        }
        let when = if self.fresh_cycle_instruction && self.managed_work.is_none() {
            "this turn"
        } else {
            "since the last instruction or verified file change"
        };
        let said = self.vault.redact(&format!(
            "`{}` with these same arguments was made {times} times {when} and answered the same each time; \
             tool-cycle guard: no verified workspace progress. Act on the answer above, or ask something different. \
             This receipt also survives continuation and compaction; it does not mark the task complete.", call.name));
        let value = guard.encoded()?;
        let [_, seq] = self.rook.store.append_events_with_values(
            self.session,
            [
                rook_store::NewEvent::new(
                    rook_store::EventKind::ToolCall,
                    rook_store::Kind::Message,
                    self.vault.redact(&call.arguments.to_string()).as_bytes(),
                )
                .label(&call.name),
                rook_store::NewEvent::new(
                    rook_store::EventKind::ToolResult,
                    rook_store::Kind::Message,
                    said.as_bytes(),
                )
                .label(&call.name),
            ],
            &[(guard.key.as_str(), value.as_slice())],
        )?;
        Ok(Some((said, seq)))
    }

    pub(super) fn cycle_observe(
        &self,
        call: &ToolCall,
        result: &str,
        failed: bool,
        before: Option<Fingerprint>,
        live_child: bool,
    ) -> Result<Option<(String, Vec<u8>)>> {
        if !self.rook.config.agent.tool_cycle_guard {
            return Ok(None);
        }
        let live = self.cycle_live_poll(call);
        let after = if self.cycle_may_write(call) { self.cycle_workspace() } else { before };
        let mut held = self.tool_cycles.lock().unwrap_or_else(|e| e.into_inner());
        let Some(guard) = held.as_mut() else {
            return Ok(None);
        };
        if before.is_some() && after.is_some() && before != after {
            guard.reset();
            guard.workspace = after;
        }
        if guard.workspace.is_none() {
            guard.workspace = after;
        }
        // Live polling does not occupy the history window or erase another
        // tool's cycle. Unknown/foreign "running" prose cannot grant this.
        if !live_child && !live {
            let unknown = failed && result.starts_with("tool error: unknown tool");
            let semantic = self.tool_cycle_outcome.lock().unwrap_or_else(|e| e.into_inner()).take();
            let (identity, complete) = self.cycle_call(call)?;
            let seen = Seen {
                call: identity,
                input_complete: complete,
                arguments: self.cycle_arguments(call)?,
                family: fingerprint(&if unknown { "unknown_tool" } else { &call.name })?,
                result: if unknown {
                    fingerprint(&"unknown_tool")?
                } else {
                    match semantic {
                        Some(hash) => hash,
                        None => fingerprint(&(result, failed))?,
                    }
                },
            };
            guard.observe(seen, self.rook.config.agent.max_tool_cycle_observations.clamp(20, 128), unknown);
        }
        Ok(Some((guard.key.clone(), guard.encoded()?)))
    }

    pub(super) fn cycle_semantic_result(&self, name: &str, outcome: &rook_tools::ToolOutcome) -> Result<()> {
        if self.rook.config.agent.tool_cycle_guard {
            let result = if name == "run_command" && outcome.meta.contains_key("job") {
                // Fresh job ids alone do not demonstrate new work. Actual
                // liveness exempts reading those jobs, not spawning duplicates.
                fingerprint(&("background command admitted", outcome.is_error))?
            } else {
                fingerprint(&(&outcome.content, outcome.is_error, &outcome.images))?
            };
            *self.tool_cycle_outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        }
        Ok(())
    }

    pub(super) fn reset_tool_cycles(&self) -> Result<()> {
        let mut held = self.tool_cycles.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(guard) = held.as_mut() {
            guard.reset();
            guard.instruction = self.rook.store.get_session(self.session)?.map_or(0, |meta| meta.next_seq);
            guard.instruction_session = self.session;
            let bytes = guard.encoded()?;
            self.rook.store.kv_set(&guard.key, &bytes)?;
            self.rook.store.flush()?;
        }
        Ok(())
    }

    pub(super) fn cycle_status(&self) -> Option<(&'static str, bool)> {
        let held = self.tool_cycles.lock().unwrap_or_else(|e| e.into_inner());
        let guard = held.as_ref()?;
        if let Some(reason) = guard.stopped {
            Some((reason.name(), true))
        } else if guard.warned {
            Some(("argument_churn", false))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(call: u8, family: u8, result: u8) -> Seen {
        Seen {
            call: [call; 32],
            arguments: [call; 32],
            input_complete: true,
            family: [family; 32],
            result: [result; 32],
        }
    }

    #[test]
    fn changed_results_are_new_evidence_and_history_is_bounded() {
        let mut guard = Guard::default();
        for n in 0..200 {
            guard.observe(seen(1, 1, n), 30, false);
        }
        assert_eq!(guard.history.len(), 30);
        assert_eq!(guard.same(&[1; 32]), 1);
        assert!(guard.stopped.is_none());
        assert!(guard.encoded().unwrap().len() < MAX_BYTES);
    }

    #[test]
    fn argument_churn_warns_then_stops_and_reset_restores_admission() {
        let mut guard = Guard::default();
        for n in 0..9 {
            guard.observe(seen(n, 1, 2), 30, false);
        }
        assert!(!guard.warned);
        guard.observe(seen(9, 1, 2), 30, false);
        assert!(guard.warned && guard.stopped.is_none());
        for n in 10..20 {
            guard.observe(seen(n, 1, 2), 30, false);
        }
        assert_eq!(guard.stopped.unwrap().name(), "argument_churn");
        guard.reset();
        assert!(guard.stopped.is_none() && !guard.warned && guard.history.is_empty());
    }

    #[test]
    fn the_largest_receipt_fits_and_invalid_configured_windows_are_refused() {
        let mut guard = Guard::default();
        for n in 0..200 {
            guard.observe(seen(n, n, n), 128, false);
        }
        assert_eq!(guard.history.len(), 128, "the maximum admitted count is actually reached");
        assert!(guard.encoded().unwrap().len() < MAX_BYTES);
        let mut config = crate::Config::default();
        for invalid in [0, 19, 129, usize::MAX] {
            config.agent.max_tool_cycle_observations = invalid;
            assert!(
                config.validation_errors().iter().any(|error| error.contains("max_tool_cycle_observations"))
            );
        }
    }

    #[test]
    fn large_arguments_and_results_are_hashed_without_retaining_a_second_text_copy() {
        let text = "large Unicode payload: абв 🦀\n".repeat(100_000);
        assert!(text.len() > MAX_BYTES);
        let hash = fingerprint(&text).unwrap();
        assert_eq!(hash, fingerprint(&text).unwrap());
        let mut guard = Guard::default();
        guard.observe(
            Seen { call: hash, arguments: hash, input_complete: true, family: hash, result: hash },
            30,
            false,
        );
        assert!(guard.encoded().unwrap().len() < 1024);
    }
}
