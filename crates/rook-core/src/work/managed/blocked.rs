//! Human prerequisites park a checkpoint without asking another model to classify prose.
use super::*;
use serde_json::Value;

pub(crate) const TOOL: &str = "block_work";
pub(crate) const MAX_REASON: usize = 2048;

pub(crate) fn parse(args: &Value) -> Result<String> {
    let fields = args.as_object().ok_or_else(|| bad("block_work expects an object"))?;
    if fields.keys().any(|key| key != "reason") {
        return Err(bad("block_work accepts only reason"));
    }
    fields
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| {
            !reason.trim().is_empty() && reason.len() <= MAX_REASON && !reason.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            bad("reason must explain the prerequisite in 1..2048 bytes without control characters")
        })
}

pub(crate) fn commit(
    rook: &Rook,
    identity: &RunIdentity,
    call: deferred::Call<'_>,
    reason: String,
    outcome: &crate::agent::TurnOutcome,
    write: impl FnOnce(&str, &[u8], &str) -> Result<u64>,
) -> Result<(u64, String)> {
    let reason = parse(&serde_json::json!({"reason":reason}))?;
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut saved = deferred::checkpoint(rook, identity, &call)?;
    let active = saved.active.as_mut().ok_or_else(|| bad("the managed stage has no checkpoint"))?;
    let spent = crate::work::spent(rook, call.session)?;
    saved.run.tokens = saved.run.tokens.saturating_add(spent.saturating_sub(active.accounted_tokens));
    active.accounted_tokens = spent;
    active.answer = None;
    saved.run.iterations = saved.run.iterations.saturating_add(1);
    saved.run.recent.push(rook_proto::work::Iteration {
        number: saved.run.iterations,
        session: active.session.clone(),
        stopped: "work_blocked".into(),
        changed: outcome.files_changed.iter().take(64).cloned().collect(),
        summary: reason.clone(),
    });
    let excess = saved.run.recent.len().saturating_sub(rook.config.work.retained_iterations);
    saved.run.recent.drain(..excess);
    saved.run.status = Status::Blocked;
    saved.run.reason = reason.clone();
    saved.run.reply = reason.clone();
    saved.run.waiting = None;
    saved.run.next_attempt_at = None;
    saved.run.updated_at = now();
    saved.run.restart_guard.unfinished_stage = false;
    saved.run.restart_guard.consecutive_interruptions = 0;
    saved.idle = 0;
    let encoded = crate::persistence::encode(&saved)?;
    let seq = write(&key(&identity.id)?, &encoded, &reason)?;
    Ok((seq, reason))
}
