//! Durable runs. Store mutations are serialized independently of a running turn,
//! so a user's correction can be saved while the model or a tool is busy.
use super::receipts::WRITING;

use rook_proto::work::{
    Action, ControlOutcome, EditInstruction, IdentifiedControl, Run, RunIdentity, Start, Status, Steer,
    Steering, WithdrawInstruction,
};
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result, Rook};

const INDEX: &str = "work/managed-index";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Active {
    pub session: String,
    /// Older records used a new session per iteration and can start at zero.
    #[serde(default)]
    pub start_seq: u64,
    pub before: crate::evaluation::Witness,
    pub answer: Option<crate::agent::TurnOutcome>,
    pub accounted_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub run: Run,
    pub card: Option<crate::evaluation::Scorecard>,
    pub active: Option<Active>,
    pub failed_since: Option<u64>,
    pub idle: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlReceipt>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlReceipt {
    pub id: String,
    pub action: Action,
}

const MAX_CONTROL_RECEIPTS: usize = 1024;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn bad(message: impl Into<String>) -> CoreError {
    CoreError::Other(message.into())
}

fn key(id: &str) -> Result<String> {
    if rook_store::parse_session_id(id).is_none() {
        return Err(bad("invalid work id"));
    }
    Ok(format!("work/managed/{id}"))
}

fn ids(rook: &Rook) -> Result<Vec<String>> {
    rook.store
        .kv_get(INDEX)?
        .map(|b| serde_json::from_slice(&b).map_err(Into::into))
        .unwrap_or(Ok(Vec::new()))
}

pub fn read(rook: &Rook, id: &str) -> Result<Saved> {
    let bytes = rook.store.kv_get(&key(id)?)?.ok_or_else(|| bad("no such work run"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn list(rook: &Rook) -> Result<Vec<Run>> {
    ids(rook)?.iter().map(|id| read(rook, id).map(|s| s.run)).collect()
}

pub fn for_session(rook: &Rook, session: u128) -> Result<Option<Run>> {
    let id = rook_store::format_session_id(session);
    let Some(bytes) = rook.store.kv_get(&key(&id)?)? else { return Ok(None) };
    let saved: Saved = serde_json::from_slice(&bytes)?;
    Ok(saved.run.conversation.is_some().then_some(saved.run))
}

/// The closure always sees concurrent steering and control changes. Never save a
/// snapshot held across an await over a newer version of the run.
pub fn update<T>(rook: &Rook, id: &str, change: impl FnOnce(&mut Saved) -> Result<T>) -> Result<T> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut saved = read(rook, id)?;
    let old_session = saved.active.as_ref().map(|a| a.session.clone());
    let result = change(&mut saved)?;
    saved.run.updated_at = now();
    save(rook, &saved)?;
    if old_session != saved.active.as_ref().map(|a| a.session.clone())
        && let Some(session) = old_session.and_then(|s| rook_store::parse_session_id(&s))
    {
        for meta in super::family(rook, session)? {
            if !saved.run.status.terminal()
                && saved
                    .run
                    .conversation
                    .as_ref()
                    .is_some_and(|c| rook_store::parse_session_id(&c.session) == Some(meta.id))
            {
                continue;
            }
            rook.store.update_session(meta.id, |m| m.tags.retain(|t| t != "rook:work"))?;
        }
        rook.store.flush()?;
    }
    Ok(result)
}

fn save(rook: &Rook, saved: &Saved) -> Result<()> {
    if let Some(session) = saved.active.as_ref().and_then(|a| rook_store::parse_session_id(&a.session)) {
        for meta in super::family(rook, session)? {
            rook.store.update_session(meta.id, |m| {
                if !m.tags.iter().any(|t| t == "rook:work") {
                    m.tags.push("rook:work".into());
                }
            })?;
        }
    }
    // A lasting conversation is still needed between stages and on pause.
    // Retention must not evict its transcript just because no turn owns it now.
    if saved.active.is_none()
        && let Some(session) =
            saved.run.conversation.as_ref().and_then(|c| rook_store::parse_session_id(&c.session))
    {
        rook.store.update_session(session, |m| {
            if saved.run.status.terminal() {
                m.tags.retain(|t| t != "rook:work");
            } else if !m.tags.iter().any(|t| t == "rook:work") {
                m.tags.push("rook:work".into());
            }
        })?;
    }
    crate::persistence::save_json(&rook.store, &key(&saved.run.id)?, saved)
}

pub fn start(rook: &Rook, request: Start) -> Result<Run> {
    start_with_claim(rook, request, None)
}

/// Start a conversation goal and atomically acknowledge its caller-owned
/// socket claim. Legacy callers omit the claim and retain the same API.
pub fn start_with_claim(rook: &Rook, request: Start, claim_key: Option<&str>) -> Result<Run> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let config = &rook.config.work;
    if request.goal.trim().is_empty() || request.goal.len() > config.max_goal_bytes {
        return Err(bad(format!("goal must contain text and fit in {} bytes", config.max_goal_bytes)));
    }
    let mut index = ids(rook)?;
    let conversation = request
        .conversation
        .as_ref()
        .map(|c| {
            if rook_tools::policy::Stance::parse(&c.stance).is_none()
                || rook_llm::Effort::parse(&c.effort).is_none()
                || c.model.as_ref().is_some_and(|model| model.len() > 1024)
            {
                return Err(bad("invalid conversation settings"));
            }
            crate::attachments::prepare(&request.goal, &c.options.attachments)?;
            crate::output::Contract::compile(&c.options, &rook.workspace)?;
            if serde_json::to_vec(&c.options)?.len() > crate::attachments::MAX_FRAME_BYTES {
                return Err(bad("conversation options exceed the chat frame limit"));
            }
            let session = rook_store::parse_session_id(&c.session).ok_or_else(|| bad("invalid session"))?;
            let meta = rook.store.get_session(session)?.ok_or_else(|| bad("no such session"))?;
            if std::path::Path::new(&meta.workspace).canonicalize().ok() != rook.workspace.canonicalize().ok()
            {
                return Err(bad("goal belongs to another workspace"));
            }
            Ok(rook_store::format_session_id(session))
        })
        .transpose()?;
    if index.len() >= config.max_runs && !conversation.as_ref().is_some_and(|id| index.contains(id)) {
        return Err(bad("work run limit reached; forget a completed or cancelled run first"));
    }
    let workspace = rook.workspace.canonicalize().map_err(|e| bad(e.to_string()))?.display().to_string();
    for id in &index {
        let existing = read(rook, id)?;
        if existing.run.workspace == workspace
            && (!existing.run.status.terminal() || existing.active.is_some())
            && (conversation.is_none()
                || existing.run.conversation.is_none()
                || conversation.as_ref() == Some(id))
        {
            return Err(bad(format!(
                "workspace already has work {}; resume or cancel it first",
                existing.run.id
            )));
        }
    }
    let card = crate::evaluation::read(&rook.workspace).map_err(bad)?;
    if card.as_ref().is_some_and(|c| c.checks.is_empty()) {
        return Err(bad(
            "evaluation.toml contains no checks; add acceptance checks or remove the empty file",
        ));
    }
    let at = now();
    let run = Run {
        generation: rook_store::format_session_id(rook_store::new_session_id()),
        id: conversation.unwrap_or_else(|| rook_store::format_session_id(rook_store::new_session_id())),
        conversation: request.conversation,
        workspace,
        goal: request.goal.trim().into(),
        status: Status::Queued,
        reason: "scheduled".into(),
        created_at: at,
        updated_at: at,
        next_attempt_at: None,
        autonomous: request.autonomous,
        max_iterations: request.max_iterations.unwrap_or(config.max_iterations),
        max_tokens: request.max_tokens.unwrap_or(config.max_tokens),
        max_seconds: request.max_seconds.unwrap_or(config.max_seconds),
        iterations: 0,
        tokens: 0,
        consecutive_failures: 0,
        session: None,
        instructions: Vec::new(),
        recent: Vec::new(),
        reply: String::new(),
        verification: String::new(),
    };
    let record = crate::persistence::encode(&Saved {
        run: run.clone(),
        card,
        active: None,
        failed_since: None,
        idle: 0,
        controls: Vec::new(),
    })?;
    if !index.contains(&run.id) {
        index.push(run.id.clone());
    }
    let index = crate::persistence::encode(&index)?;
    let run_key = key(&run.id)?;
    if let Some(conversation) = &run.conversation {
        let session =
            rook_store::parse_session_id(&conversation.session).ok_or_else(|| bad("invalid session"))?;
        let mut values =
            vec![(INDEX.to_string(), index), (format!("goal/{session:032x}"), run.goal.as_bytes().to_vec())];
        if let Some(claim_key) = claim_key {
            values.push((
                claim_key.to_owned(),
                crate::chat_submission::goal_admitted_value(
                    &rook.store,
                    claim_key,
                    session,
                    &run.generation,
                )?,
            ));
        }
        let references: Vec<_> = values.iter().map(|(key, bytes)| (key.as_str(), bytes.as_slice())).collect();
        rook.store.append_events_with_receipt(
            session,
            [rook_store::NewEvent::new(
                rook_store::EventKind::Note,
                rook_store::Kind::Message,
                run.goal.as_bytes(),
            )
            .label("goal")],
            &run_key,
            &references,
            |_| Ok(record),
        )?;
        rook.store.flush()?;
        rook.store.update_session(session, |m| {
            if !m.tags.iter().any(|t| t == "rook:work") {
                m.tags.push("rook:work".into());
            }
        })?;
        rook.store.flush()?;
    } else {
        if claim_key.is_some() {
            return Err(bad("a chat goal claim requires a conversation"));
        }
        rook.store.kv_update(&[(INDEX, &index), (&run_key, &record)], &[])?;
        rook.store.flush()?;
    }
    Ok(run)
}

pub fn steer(rook: &Rook, id: &str, request: Steer) -> Result<Steering> {
    steer_noticed(rook, id, request).map(|(receipt, _)| receipt)
}

pub fn steer_noticed(rook: &Rook, id: &str, request: Steer) -> Result<(Steering, rook_proto::queue::Notice)> {
    update(rook, id, |saved| {
        let receipt = super::receipts::submit(
            rook,
            &mut saved.run.instructions,
            request,
            !saved.run.status.terminal(),
        )?;
        saved.idle = 0;
        let session = rook_store::parse_session_id(&saved.run.id).ok_or_else(|| bad("invalid run id"))?;
        let notice = crate::message_queue::view::notice(session, Some(&saved.run), &receipt);
        Ok((receipt, notice))
    })
}

/// Receipt-only compatibility API for embedded callers.
#[doc(hidden)]
pub fn edit_instruction(rook: &Rook, run: &str, id: &str, request: EditInstruction) -> Result<Steering> {
    edit_instruction_noticed(rook, run, id, request).map(|(receipt, _)| receipt)
}

pub fn edit_instruction_noticed(
    rook: &Rook,
    run: &str,
    id: &str,
    request: EditInstruction,
) -> Result<(Steering, rook_proto::queue::Notice)> {
    update(rook, run, |saved| {
        let receipt = super::receipts::edit(
            rook,
            &mut saved.run.instructions,
            id,
            request,
            !saved.run.status.terminal(),
        )?;
        let session = rook_store::parse_session_id(&saved.run.id).ok_or_else(|| bad("invalid run id"))?;
        let notice = crate::message_queue::view::notice(session, Some(&saved.run), &receipt);
        Ok((receipt, notice))
    })
}

/// Receipt-only compatibility API for embedded callers.
#[doc(hidden)]
pub fn withdraw_instruction(
    rook: &Rook,
    run: &str,
    id: &str,
    request: WithdrawInstruction,
) -> Result<Steering> {
    withdraw_instruction_noticed(rook, run, id, request).map(|(receipt, _)| receipt)
}

pub fn withdraw_instruction_noticed(
    rook: &Rook,
    run: &str,
    id: &str,
    request: WithdrawInstruction,
) -> Result<(Steering, rook_proto::queue::Notice)> {
    update(rook, run, |saved| {
        let receipt = super::receipts::withdraw(
            &mut saved.run.instructions,
            id,
            request,
            !saved.run.status.terminal(),
        )?;
        let session = rook_store::parse_session_id(&saved.run.id).ok_or_else(|| bad("invalid run id"))?;
        let notice = crate::message_queue::view::notice(session, Some(&saved.run), &receipt);
        Ok((receipt, notice))
    })
}

pub fn control(rook: &Rook, id: &str, action: Action) -> Result<Run> {
    update(rook, id, |saved| {
        apply_control(saved, action)?;
        Ok(saved.run.clone())
    })
}

/// Apply once per caller ID and generation, including after a daemon restart.
/// The receipt and changed status share one stored JSON value.
pub fn control_identified(rook: &Rook, id: &str, request: IdentifiedControl) -> Result<ControlOutcome> {
    if request.id.is_empty()
        || request.id.len() > 64
        || !request.id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(bad("control id must be 1–64 letters, digits, hyphens or underscores"));
    }
    if rook_store::parse_session_id(&request.generation).is_none() {
        return Err(bad("invalid control generation"));
    }
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut saved = read(rook, id)?;
    if saved.run.generation != request.generation {
        return Err(bad("this control belongs to an earlier run generation; inspect the current goal"));
    }
    if let Some(known) = saved.controls.iter().find(|known| known.id == request.id) {
        if known.action != request.action {
            return Err(bad("control id was already used for another action"));
        }
        return Ok(ControlOutcome {
            id: request.id,
            generation: request.generation,
            already_applied: true,
            run: saved.run,
        });
    }
    if saved.controls.len() >= MAX_CONTROL_RECEIPTS {
        return Err(bad("control receipt limit reached for this run; inspect it before another control"));
    }
    apply_control(&mut saved, request.action)?;
    saved.controls.push(ControlReceipt { id: request.id.clone(), action: request.action });
    saved.run.updated_at = now();
    save(rook, &saved)?;
    Ok(ControlOutcome {
        id: request.id,
        generation: request.generation,
        already_applied: false,
        run: saved.run,
    })
}

fn apply_control(saved: &mut Saved, action: Action) -> Result<()> {
    if saved.run.status.terminal() {
        return Err(bad("this run has ended"));
    }
    match action {
        Action::Pause => {
            saved.run.status = Status::Paused;
            saved.run.reason = "paused by user; an active operation finishes before stopping".into();
        }
        Action::Cancel => {
            saved.run.status = Status::Cancelled;
            saved.run.reason = "cancelled by user; an active operation finishes before stopping".into();
        }
        Action::Resume => {
            if saved.run.status == Status::Limited {
                return Err(bad(
                    "the saved run reached its budget; start a new run with an explicit new budget",
                ));
            }
            saved.run.status = Status::Queued;
            saved.run.reason = "resumed by user".into();
            saved.run.next_attempt_at = None;
            saved.run.consecutive_failures = 0;
            saved.failed_since = None;
            saved.idle = 0;
        }
    }
    Ok(())
}

pub fn forget(rook: &Rook, id: &str) -> Result<()> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let saved = read(rook, id)?;
    if !saved.run.status.terminal() || saved.active.is_some() {
        return Err(bad("only a completed or fully stopped cancelled run may be forgotten"));
    }
    let mut index = ids(rook)?;
    index.retain(|item| item != id);
    let index = crate::persistence::encode(&index)?;
    rook.store.kv_update(&[(INDEX, &index)], &[&key(id)?])?;
    rook.store.flush()?;
    Ok(())
}

/// Pending receipt IDs. Text is read again at acceptance, under the same lock
/// as queue edits; a previously observed string is never authority to deliver.
pub fn pending(rook: &Rook, identity: &RunIdentity) -> Result<Vec<String>> {
    let Some(saved) = read_identity(rook, identity)? else { return Ok(Vec::new()) };
    Ok(saved.run.instructions.iter().filter(|m| m.queued()).map(|m| m.id.clone()).collect())
}

fn read_identity(rook: &Rook, identity: &RunIdentity) -> Result<Option<Saved>> {
    let Some(bytes) = rook.store.kv_get(&key(&identity.id)?)? else { return Ok(None) };
    let saved: Saved = serde_json::from_slice(&bytes)?;
    Ok((saved.run.identity() == *identity).then_some(saved))
}

/// Accept exactly once and publish the transcript, goal and receipt in one
/// durable transaction. Callers only put the returned text into model context.
pub fn accept(
    rook: &Rook,
    run: &RunIdentity,
    session: u128,
    id: &str,
) -> Result<Option<crate::message_queue::Accepted>> {
    let _lock = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut saved) = read_identity(rook, run)? else { return Ok(None) };
    if !saved.run.status.runnable() {
        return Ok(None);
    }
    if saved
        .run
        .conversation
        .as_ref()
        .is_some_and(|conversation| rook_store::parse_session_id(&conversation.session) != Some(session))
    {
        return Err(bad("instruction belongs to another conversation"));
    }
    let index = saved
        .run
        .instructions
        .iter()
        .position(|m| m.id == id)
        .ok_or_else(|| bad("unknown instruction receipt"))?;
    let message = &mut saved.run.instructions[index];
    if !message.queued() {
        return Ok(None);
    }
    let text = format!("[work instruction {}]\n{}", message.id, message.text);
    if saved.run.conversation.is_some()
        && let Some(goal) = message.text.strip_prefix("/goal ").map(str::trim).filter(|s| !s.is_empty())
    {
        saved.run.goal = goal.into();
    }
    message.applied_at = Some(now());
    message.session = Some(rook_store::format_session_id(session));
    let receipt =
        crate::message_queue::view::notice(session, Some(&saved.run), &saved.run.instructions[index]);
    saved.run.updated_at = now();
    let current = goal(&saved.run);
    let encoded = crate::persistence::encode(&saved)?;
    let record = key(&run.id)?;
    let goal_key = format!("goal/{session:032x}");
    use rook_store::{EventKind, Kind, NewEvent};
    rook.store.append_events_with_values(
        session,
        [
            NewEvent::new(EventKind::Note, Kind::Message, current.as_bytes()).label("goal"),
            NewEvent::new(EventKind::UserMessage, Kind::Message, text.as_bytes()).label("while running"),
        ],
        &[(&record, &encoded), (&goal_key, current.as_bytes())],
    )?;
    Ok(Some(crate::message_queue::Accepted { text, receipt }))
}

pub fn should_stop(rook: &Rook, identity: &RunIdentity) -> Result<bool> {
    Ok(read_identity(rook, identity)?.is_none_or(|saved| !saved.run.status.runnable()))
}

fn clipped(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn goal(run: &Run) -> String {
    let mut text = run.goal.clone();
    for message in run.instructions.iter().filter(|m| m.applied_at.is_some()) {
        text.push_str(&format!("\n\nUser correction {}:\n{}", message.id, message.text));
    }
    text
}

fn prompt(saved: &Saved, rook: &Rook) -> String {
    let mut text = format!(
        "{}\n\nThis is iteration {} of a durable task. Complete the user's goal in verifiable \
         increments. Keep .rook/plan.md current: requirements, completed steps and evidence, \
         remaining steps, failed approaches, and the next action. Do not substitute passing \
         existing tests for the requested outcome. A bounded turn may end while the task continues. \
         The supervisor checks completion independently. Preserve the user's scope and permissions.",
        saved.run.goal,
        saved.run.iterations.saturating_add(1)
    );
    for message in saved.run.instructions.iter().filter(|m| m.applied_at.is_some()) {
        text.push_str(&format!("\n\nUser correction {}:\n{}", message.id, message.text));
    }
    if let Some(notes) = super::notes(&rook.workspace) {
        text.push_str(&format!("\n\nRecorded plan (inspect evidence before trusting claims):\n{notes}"));
    }
    for previous in saved.run.recent.iter().rev().take(5).rev() {
        text.push_str(&format!(
            "\nIteration {} (session {}): {}",
            previous.number, previous.session, previous.summary
        ));
    }
    if !saved.run.verification.is_empty() {
        text.push_str(&format!("\n\nPrevious independent verification:\n{}", saved.run.verification));
    }
    text.push_str("\n\nOn recovery, inspect recorded operations and current files before repeating work. A provider failure does not roll back completed edits.");
    text
}

fn limit(run: &Run, at: u64) -> Option<&'static str> {
    if run.max_seconds > 0 && at.saturating_sub(run.created_at) >= run.max_seconds {
        Some("run time budget reached")
    } else if run.max_tokens > 0 && run.tokens >= run.max_tokens {
        Some("run token budget reached")
    } else if run.max_iterations > 0 && run.iterations >= run.max_iterations {
        Some("run iteration budget reached")
    } else {
        None
    }
}

/// A provider outage is not an idle iteration. Keep its session and receipt,
/// and store a retry deadline which survives a daemon restart.
fn failed(rook: &Rook, id: &str, why: &str) -> Result<()> {
    update(rook, id, |saved| {
        if !saved.run.status.runnable() {
            if saved.run.status == Status::Cancelled {
                saved.active = None;
            }
            return Ok(());
        }
        let at = now();
        let since = *saved.failed_since.get_or_insert(at);
        saved.run.consecutive_failures = saved.run.consecutive_failures.saturating_add(1);
        let lower = why.to_ascii_lowercase();
        let permanent = [
            "returned 401",
            "returned 403",
            "unauthorized",
            "invalid api key",
            "no model",
            "no provider called",
            "no endpoint",
            "unknown model",
            "out of credit",
            "insufficient_quota",
        ]
        .iter()
        .any(|s| lower.contains(s));
        if permanent || at.saturating_sub(since) >= rook.config.work.retry_window_secs {
            saved.run.status = Status::Blocked;
            saved.run.reason = format!("needs attention: {}", clipped(why, 2048));
            saved.run.next_attempt_at = None;
        } else {
            let exponent = saved.run.consecutive_failures.saturating_sub(1).min(16);
            let delay = rook
                .config
                .work
                .retry_initial_secs
                .max(1)
                .saturating_mul(1 << exponent)
                .min(rook.config.work.retry_max_secs.max(1));
            saved.run.status = Status::RetryWait;
            saved.run.reason = clipped(why, 2048);
            saved.run.next_attempt_at = Some(at.saturating_add(delay));
        }
        Ok(())
    })
}

fn account(rook: &Rook, id: &str, session: u128) -> Result<()> {
    let spent = super::spent(rook, session)?;
    update(rook, id, |saved| {
        if let Some(active) = &mut saved.active {
            saved.run.tokens = saved.run.tokens.saturating_add(spent.saturating_sub(active.accounted_tokens));
            active.accounted_tokens = spent;
        }
        Ok(())
    })
}

/// Perform at most one bounded turn and its verification. A daemon schedules the
/// next call; this function owns all state transitions and can be driven in tests.
pub async fn advance<'a>(
    rook: &'a Rook,
    id: &str,
    make_agent: impl Fn(u128) -> Result<crate::agent::AgentLoop<'a>>,
    mut progress: impl FnMut(crate::agent::Progress<'_>),
) -> Result<Run> {
    let mut saved = read(rook, id)?;
    if rook.workspace.canonicalize().map_err(|e| bad(e.to_string()))?.display().to_string()
        != saved.run.workspace
    {
        return Err(bad("work belongs to another workspace"));
    }
    if !saved.run.status.runnable() || saved.run.next_attempt_at.is_some_and(|at| at > now()) {
        return Ok(saved.run);
    }
    if let Some(active) = &saved.active {
        account(
            rook,
            id,
            rook_store::parse_session_id(&active.session).ok_or_else(|| bad("invalid active session"))?,
        )?;
        saved = read(rook, id)?;
    }
    if let Some(reason) = limit(&saved.run, now()) {
        return update(rook, id, |s| {
            s.run.status = Status::Limited;
            s.run.reason = reason.into();
            Ok(s.run.clone())
        });
    }
    if saved.active.is_none() {
        let session = match &saved.run.conversation {
            Some(c) => rook_store::parse_session_id(&c.session).ok_or_else(|| bad("invalid conversation"))?,
            None => rook.start_session(&format!(
                "work {} iteration {}",
                id,
                saved.run.iterations.saturating_add(1)
            ))?,
        };
        let start_seq = rook.store.get_session(session)?.ok_or_else(|| bad("no such session"))?.next_seq;
        let accounted_tokens = super::spent(rook, session)?;
        let before = crate::evaluation::witness(&rook.workspace, &saved.card.clone().unwrap_or_default());
        update(rook, id, |s| {
            if !s.run.status.runnable() {
                return Ok(());
            }
            s.active = Some(Active {
                session: rook_store::format_session_id(session),
                start_seq,
                before,
                answer: None,
                accounted_tokens,
            });
            s.run.session = Some(rook_store::format_session_id(session));
            Ok(())
        })?;
        saved = read(rook, id)?;
    }
    let Some(active) = saved.active.clone() else {
        return Ok(saved.run);
    };
    let session =
        rook_store::parse_session_id(&active.session).ok_or_else(|| bad("invalid active session"))?;
    if let Some(reason) = rook.recovery_block(session)? {
        return update(rook, id, |s| {
            s.run.status = Status::Blocked;
            s.run.reason = reason;
            Ok(s.run.clone())
        });
    }
    if active.answer.is_none()
        && rook
            .execution(session)?
            .iter()
            .any(|e| e.session == active.session && e.start_seq >= active.start_seq)
        && let Some(answer) = rook.completed_turn(session)?
        && !matches!(answer.stopped.as_str(), "completion_unchecked" | "work_paused")
    {
        update(rook, id, |s| {
            s.active.as_mut().ok_or_else(|| bad("active work session disappeared"))?.answer = Some(answer);
            Ok(())
        })?;
        saved = read(rook, id)?;
    }
    let mut agent = match make_agent(session) {
        Ok(agent) => agent,
        Err(error) => {
            failed(rook, id, &error.to_string())?;
            return Ok(read(rook, id)?.run);
        }
    };
    agent.managed_work = Some(saved.run.identity());
    if saved.run.autonomous && saved.run.conversation.is_none() {
        agent.allow_everything_not_denied();
    }
    if saved.run.max_tokens > 0 {
        let remaining = saved.run.max_tokens.saturating_sub(saved.run.tokens).max(1);
        agent.max_turn_tokens =
            if agent.max_turn_tokens == 0 { remaining } else { agent.max_turn_tokens.min(remaining) };
    }
    if saved.run.max_seconds > 0 {
        let remaining =
            saved.run.max_seconds.saturating_sub(now().saturating_sub(saved.run.created_at)).max(1);
        agent.max_turn_secs =
            if agent.max_turn_secs == 0 { remaining } else { agent.max_turn_secs.min(remaining) };
    }
    update(rook, id, |s| {
        if s.run.status.runnable() {
            s.run.status = Status::Running;
            s.run.reason = if s.active.as_ref().is_some_and(|a| a.answer.is_some()) {
                "verifying saved outcome"
            } else {
                "working"
            }
            .into();
            s.run.next_attempt_at = None;
        }
        Ok(())
    })?;
    rook.set_goal(session, &goal(&saved.run))?;
    let mut outcome = match saved.active.as_ref().and_then(|a| a.answer.clone()) {
        Some(answer) => answer,
        None => match agent.run_with(&prompt(&saved, rook), &mut progress).await {
            Ok(answer) => {
                update(rook, id, |s| {
                    s.active.as_mut().ok_or_else(|| bad("active work session disappeared"))?.answer =
                        Some(answer.clone());
                    Ok(())
                })?;
                answer
            }
            Err(error) => {
                account(rook, id, session)?;
                failed(rook, id, &error.to_string())?;
                return Ok(read(rook, id)?.run);
            }
        },
    };
    account(rook, id, session)?;
    saved = read(rook, id)?;
    // Pause/cancel is graceful: completed operations retain receipts; the active
    // session stays available until its answer has been safely stored.
    if !saved.run.status.runnable() {
        return update(rook, id, |s| {
            s.run.reply = clipped(&outcome.reply, 16384);
            if s.run.status == Status::Paused {
                // Resume the same transcript after a mid-turn pause; a fresh
                // iteration would lose the context of partially completed work.
                if let Some(active) = &mut s.active {
                    active.answer = None;
                }
            } else {
                s.active = None;
            }
            Ok(s.run.clone())
        });
    }
    // A classifier outage leaves a finished model reply, but not a decision
    // that the task is blocked. Retry the turn with its existing transcript.
    if outcome.stopped == "completion_unchecked" {
        update(rook, id, |s| {
            if let Some(active) = &mut s.active {
                active.answer = None;
            }
            Ok(())
        })?;
        failed(rook, id, &outcome.open_questions.join("; "))?;
        return Ok(read(rook, id)?.run);
    }
    if let Some(reason) = rook.recovery_block(session)? {
        return update(rook, id, |s| {
            s.run.status = Status::Blocked;
            s.run.reason = reason;
            Ok(s.run.clone())
        });
    }
    let report = if let Some(card) = &saved.card {
        // The receipt cache avoids replay while rechecking the witness: a saved
        // passing report alone cannot certify files changed during downtime.
        Some(tokio::task::block_in_place(|| rook.evaluate_recorded(session, card, &active.before, None))?)
    } else {
        None
    };
    let revision = read(rook, id)?.run.instructions.len();
    let pending = !pending(rook, &saved.run.identity())?.is_empty();
    let checks_pass = report.as_ref().is_none_or(|r| r.clean());
    let candidate = crate::agent::finished(&outcome.stopped)
        && checks_pass
        && !pending
        && outcome.open_questions.is_empty();
    let verification = if candidate && limit(&read(rook, id)?.run, now()).is_none() {
        let current = read(rook, id)?;
        agent.verify_work(&goal(&current.run), &mut outcome, &mut progress).await
    } else {
        (String::new(), None)
    };
    account(rook, id, session)?;
    if verification.0.starts_with("could not check:") {
        failed(rook, id, &verification.0)?;
        return Ok(read(rook, id)?.run);
    }
    update(rook, id, |s| {
        s.run.iterations = s.run.iterations.saturating_add(1);
        s.run.consecutive_failures = 0;
        s.failed_since = None;
        s.run.reply = clipped(&outcome.reply, 16384);
        s.run.verification = clipped(&verification.0, 8192);
        let summary = report
            .as_ref()
            .map(|r| r.summary())
            .unwrap_or_else(|| "no project scorecard; independent goal verification required".into());
        s.run.recent.push(rook_proto::work::Iteration {
            number: s.run.iterations,
            session: active.session.clone(),
            stopped: outcome.stopped.clone(),
            changed: outcome.files_changed.iter().take(64).cloned().collect(),
            summary: clipped(&format!("{summary}; {}", outcome.reply), 2048),
        });
        let excess = s.run.recent.len().saturating_sub(rook.config.work.retained_iterations);
        s.run.recent.drain(..excess);
        let session_pending = s
            .run
            .conversation
            .as_ref()
            .and_then(|c| rook_store::parse_session_id(&c.session))
            .map(|session| crate::message_queue::pending(rook, session))
            .transpose()?
            .is_some_and(|messages| !messages.is_empty());
        let corrected = s.run.instructions.len() != revision
            || s.run.instructions.iter().any(|m| m.queued())
            || session_pending;
        if outcome.tools_called.is_empty() && !corrected {
            s.idle = s.idle.saturating_add(1);
        } else {
            s.idle = 0;
        }
        if s.run.status.runnable() {
            if candidate && !corrected && verification.1.as_deref() == Some("holds") {
                s.run.status = Status::Completed;
                s.run.reason = "goal verified against the current instructions and project checks".into();
            } else if !corrected
                && (outcome.stopped == "blocked"
                    || (!outcome.open_questions.is_empty()
                        && !matches!(outcome.stopped.as_str(), "time" | "budget" | "max_steps")))
            {
                s.run.status = Status::Blocked;
                s.run.reason =
                    clipped(&format!("{} {}", outcome.reply, outcome.open_questions.join("; ")), 4096);
            } else if s.idle >= rook.config.work.idle_iterations.max(1) {
                s.run.status = Status::Blocked;
                s.run.reason = "repeated turns took no action; send guidance and resume".into();
            } else if let Some(reason) = limit(&s.run, now()) {
                s.run.status = Status::Limited;
                s.run.reason = reason.into();
            } else {
                s.run.status = Status::Queued;
                s.run.reason = if corrected {
                    "continuing with user corrections"
                } else {
                    "continuing the unfinished goal"
                }
                .into();
            }
        }
        s.active = None;
        s.run.next_attempt_at = None;
        Ok(s.run.clone())
    })
}
