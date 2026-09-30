//! Follow-ups are new turns, never steering for the turn they wait behind.
use crate::{CoreError, Result, Rook, work::managed};
use rook_proto::work::{FollowUp, RunIdentity, Steer, Steering};

fn bad(text: &str) -> CoreError {
    CoreError::Other(text.into())
}

pub(crate) fn target(rook: &Rook, session: u128) -> Result<Option<String>> {
    let goal = managed::for_session(rook, session)?;
    let execution = crate::execution::current(rook, session)?;
    if let Some(run) = goal.filter(|r| {
        !r.status.terminal()
            || (r.status == rook_proto::work::Status::Completed
                && execution.as_ref().is_none_or(|e| e.status == "evaluated"))
    }) {
        return Ok(Some(format!("goal.{}", run.identity().generation)));
    }
    Ok(execution.map(|state| format!("turn.{}", state.turn)))
}

pub(crate) fn submit(rook: &Rook, session: u128, after: &str, request: Steer) -> Result<Steering> {
    super::update(rook, session, |messages| {
        if let Some(existing) = messages.iter().find(|m| m.id == request.id) {
            if existing.follow_up.as_ref().is_none_or(|f| f.after != after) {
                return Err(bad("submission ID already belongs to another mode or completion boundary"));
            }
            return crate::work::receipts::submit(rook, messages, request, true);
        }
        if target(rook, session)?.as_deref() != Some(after) {
            return Err(bad("follow-up target changed; refresh the queue before submitting a new message"));
        }
        let goal = managed::for_session(rook, session)?.map(|run| run.identity());
        let mut receipt = crate::work::receipts::submit(rook, messages, request, true)?;
        receipt.follow_up =
            Some(FollowUp { after: after.into(), goal, ready: false, reserved: None, blocked: None });
        if let Some(stored) = messages.last_mut() {
            *stored = receipt.clone();
        }
        // Remember an already completed boundary before a later manual turn
        // replaces the latest execution receipt.
        if after.starts_with("turn.") && next(rook, session, std::slice::from_ref(&receipt))? == Some(0) {
            if let Some(follow) = &mut receipt.follow_up {
                follow.ready = true;
            }
            if let Some(stored) = messages.last_mut() {
                *stored = receipt.clone();
            }
        }
        Ok(receipt)
    })
}

/// Called under the queue writer lock. Readiness is a durable observation of a
/// completed predecessor; subsequent turns must finish too before draining more.
pub(crate) fn next(rook: &Rook, session: u128, messages: &[Steering]) -> Result<Option<usize>> {
    if !messages.iter().any(|m| m.queued() && m.follow_up.as_ref().is_some_and(|f| f.reserved.is_none())) {
        return Ok(None);
    }
    let goal = managed::for_session(rook, session)?;
    if goal.as_ref().is_some_and(|r| !r.status.terminal()) {
        return Ok(None);
    }
    let identity = goal.as_ref().map(|run| run.identity());
    let completed = rook.completed_turn(session)?.is_some_and(|o| crate::agent::finished(&o.stopped));
    let turn = crate::execution::current(rook, session)?.map(|state| format!("turn.{}", state.turn));
    let candidate = messages.iter().position(|message| {
        let Some(follow) = &message.follow_up else { return false };
        if !message.queued() || follow.reserved.is_some() || follow.goal != identity {
            return false;
        }
        if follow.after.starts_with("goal.")
            && goal.as_ref().is_none_or(|run| run.status != rook_proto::work::Status::Completed)
        {
            return false;
        }
        if follow.ready {
            return completed;
        }
        match &goal {
            Some(goal) if follow.after == format!("goal.{}", goal.identity().generation) => {
                goal.status == rook_proto::work::Status::Completed
            }
            _ => completed && turn.as_deref() == Some(follow.after.as_str()),
        }
    });
    // Recovery examines the session family. Do it only for eligible work,
    // not for every historical queue on every supervisor tick.
    if candidate.is_some() && rook.recovery_block(session)?.is_some() {
        return Ok(None);
    }
    Ok(candidate)
}

/// A lost process leaves no explicit stop reason on its reserved message.
/// A cancelled/failed live attempt does, and must never be restarted by a tick.
pub(crate) fn resumable(rook: &Rook, session: u128, messages: &[Steering]) -> Result<Option<usize>> {
    if !messages.iter().any(|m| {
        m.withdrawn_at.is_none()
            && m.follow_up.as_ref().is_some_and(|f| f.reserved.is_some() && f.blocked.is_none())
    }) {
        return Ok(None);
    }
    let Some(state) = crate::execution::current(rook, session)? else { return Ok(None) };
    if !matches!(state.status.as_str(), "interrupted" | "reviewed") {
        return Ok(None);
    }
    // Before prompt admission, a completed setup operation is not authority
    // to run configured hooks twice. Such a receipt needs explicit inspection.
    if state.prompt.is_none() && state.completed_operations > 0 {
        return Ok(None);
    }
    let goal = managed::for_session(rook, session)?;
    if goal.as_ref().is_some_and(|g| !g.status.terminal()) {
        return Ok(None);
    }
    let identity = goal.as_ref().map(|g| g.identity());
    let candidate = messages.iter().position(|message| {
        let Some(follow) = &message.follow_up else { return false };
        state.follow_up.as_deref() == Some(message.id.as_str())
            && follow.reserved.as_deref() == Some(state.turn.as_str())
            && follow.blocked.is_none()
            && message.withdrawn_at.is_none()
            && follow.goal == identity
            && (!follow.after.starts_with("goal.")
                || goal.as_ref().is_some_and(|g| g.status == rook_proto::work::Status::Completed))
    });
    if candidate.is_some() && rook.recovery_block(session)?.is_some() {
        return Ok(None);
    }
    Ok(candidate)
}

/// A scheduler can recover queued, unreserved follow-ups after the owner exits.
/// A reserved execution is inspected through the execution recovery journal.
pub fn ready(rook: &Rook, session: u128) -> Result<bool> {
    let _lock = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let messages = super::list(rook, session)?;
    Ok(resumable(rook, session, &messages)?.is_some() || next(rook, session, &messages)?.is_some())
}

pub(crate) fn arm(messages: &mut [Steering], after: &str, goal: &Option<RunIdentity>) {
    for message in messages {
        if let Some(follow) = &mut message.follow_up
            && &follow.goal == goal
            && follow.after == after
            && follow.reserved.is_none()
        {
            follow.ready = true;
        }
    }
}

pub(crate) fn stopped(store: &rook_store::Store, session: u128, turn: &str, reason: &str) -> Result<()> {
    let _lock = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut messages = super::read_from(store, session)?;
    let mut changed = false;
    for message in &mut messages {
        if let Some(follow) = &mut message.follow_up
            && follow.reserved.as_deref() == Some(turn)
        {
            follow.blocked = (!crate::agent::finished(reason)).then(|| reason.chars().take(2048).collect());
            changed = true;
        }
    }
    if changed {
        store.append_events_with_values(
            session,
            [],
            &[(&super::key(session), &crate::persistence::encode(&messages)?)],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{execution::Journal, message_queue::view};
    use rook_proto::{
        queue::Change,
        work::{Conversation, Start, Status},
    };

    fn engine(dir: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(dir.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.10.0"),
            rook_skills::SkillIndex::default(),
            dir.into(),
        )
    }
    fn goal(rook: &Rook, session: u128) -> rook_proto::work::Run {
        managed::start(
            rook,
            Start {
                goal: "first task".into(),
                workspace: None,
                conversation: Some(Conversation {
                    session: rook_store::format_session_id(session),
                    model: None,
                    effort: "high".into(),
                    stance: "assist".into(),
                    options: Default::default(),
                }),
                autonomous: false,
                max_iterations: None,
                max_tokens: None,
                max_seconds: None,
            },
        )
        .unwrap()
    }
    fn status(rook: &Rook, run: &rook_proto::work::Run, status: Status) {
        managed::update(rook, &run.id, |saved| {
            saved.run.status = status;
            Ok(())
        })
        .unwrap();
    }
    fn enqueue(rook: &Rook, session: u128, id: &str) -> Change {
        let request = Change::FollowUp {
            target: target(rook, session).unwrap().unwrap(),
            id: id.into(),
            text: "next task".into(),
        };
        view::change(rook, session, request.clone()).unwrap();
        request
    }

    #[test]
    fn only_whole_goal_completion_releases_followups_and_replacement_never_retargets_them() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("goal boundary").unwrap();
        let run = goal(&rook, session);
        let request = enqueue(&rook, session, "next");
        for state in [Status::Paused, Status::Cancelled] {
            status(&rook, &run, state);
            assert!(!ready(&rook, session).unwrap());
            assert!(Journal::reserve_follow_up(&rook, session).unwrap().is_none());
        }
        status(&rook, &run, Status::Completed);
        assert!(ready(&rook, session).unwrap());
        // A late submission to an already completed goal is eligible too.
        enqueue(&rook, session, "late");
        assert!(!view::read(&rook, session, "session.late").unwrap().receipt.follow_up.unwrap().ready);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        assert!(Journal::start(&rook, session, None).is_err());
        assert!(
            view::change(
                &rook,
                session,
                Change::Edit { reference: "session.next".into(), revision: 0, text: "changed".into() }
            )
            .is_err()
        );
        assert!(
            view::change(&rook, session, Change::Withdraw { reference: "session.next".into(), revision: 0 })
                .is_err()
        );
        let replacement = goal(&rook, session);
        assert_ne!(replacement.identity(), run.identity());
        assert!(journal.admit_prompt(&rook, "next task", "next task", "", None, None).is_err());
        drop(journal);
        status(&rook, &replacement, Status::Completed);
        assert!(!ready(&rook, session).unwrap());
        assert!(view::change(&rook, session, request).unwrap().receipt.queued());
        assert!(
            rook.store
                .events(session, 0, 100)
                .unwrap()
                .iter()
                .all(|e| e.record.kind != rook_store::EventKind::UserMessage)
        );
    }

    #[test]
    fn an_interrupted_reservation_survives_reopen_without_duplicate_admission() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("reserved").unwrap();
        let run = goal(&rook, session);
        let request = enqueue(&rook, session, "next");
        status(&rook, &run, Status::Completed);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        let turn = crate::execution::current(&rook, session).unwrap().unwrap().turn;
        drop(journal);
        drop(rook);
        let rook = engine(dir.path());
        let receipt = view::change(&rook, session, request).unwrap().receipt;
        assert_eq!(receipt.follow_up.as_ref().unwrap().reserved.as_deref(), Some(turn.as_str()));
        assert!(receipt.follow_up.unwrap().blocked.is_some());
        assert!(receipt.applied_at.is_none());
        assert!(Journal::reserve_follow_up(&rook, session).unwrap().is_none());
        view::change(&rook, session, Change::Withdraw { reference: "session.next".into(), revision: 0 })
            .unwrap();
    }

    #[test]
    fn followup_admission_commits_goal_prompt_receipt_and_execution_identity_together() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("admission").unwrap();
        let run = goal(&rook, session);
        enqueue(&rook, session, "next");
        status(&rook, &run, Status::Completed);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        let (seq, notice) = journal.admit_prompt(&rook, "next task", "next task", "", None, None).unwrap();
        assert_eq!(notice.unwrap().reference, "session.next");
        assert_eq!(journal.admit_prompt(&rook, "next task", "next task", "", None, None).unwrap().0, seq);
        assert_eq!(rook.goal(session).unwrap().as_deref(), Some("next task"));
        let state = crate::execution::current(&rook, session).unwrap().unwrap();
        assert_eq!(state.prompt.unwrap().seq, seq);
        let entry = view::read(&rook, session, "session.next").unwrap();
        assert!(entry.receipt.applied_at.is_some());
        assert!(
            view::change(&rook, session, Change::Withdraw { reference: entry.reference, revision: 0 })
                .is_err()
        );
        assert_eq!(
            rook.store
                .events(session, 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.record.kind == rook_store::EventKind::UserMessage)
                .count(),
            1
        );
    }

    #[test]
    fn steering_and_followups_share_byte_and_receipt_limits_without_changing_submission_modes() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.work.max_messages = 1;
        rook.config.work.max_message_bytes = 4;
        let session = rook.start_session("bounds").unwrap();
        goal(&rook, session);
        let target = target(&rook, session).unwrap().unwrap();
        assert!(submit(&rook, session, &target, Steer { id: "one".into(), text: "🙂x".into() }).is_err());
        submit(&rook, session, &target, Steer { id: "one".into(), text: "🙂".into() }).unwrap();
        assert_eq!(super::super::list(&rook, session).unwrap().len(), rook.config.work.max_messages);
        assert!(submit(&rook, session, &target, Steer { id: "two".into(), text: "a".into() }).is_err());
        assert!(
            view::change(
                &rook,
                session,
                Change::Submit { target: "session".into(), id: "one".into(), text: "🙂".into() }
            )
            .is_err()
        );
        assert!(super::super::pending(&rook, session).unwrap().is_empty());
        assert!(super::super::accept(&rook, session, "one", None).unwrap().is_none());
    }

    fn lost_owner(rook: &Rook, session: u128) {
        let mut state = crate::execution::current(rook, session).unwrap().unwrap();
        state.status = "running".into();
        state.owner = "lost-process".into();
        super::super::update(rook, session, |messages| {
            for message in messages {
                if let Some(follow) = &mut message.follow_up
                    && follow.reserved.as_deref() == Some(state.turn.as_str())
                {
                    follow.blocked = None;
                }
            }
            Ok(())
        })
        .unwrap();
        rook.store
            .kv_set(&format!("execution/{session:032x}"), &serde_json::to_vec(&state).unwrap())
            .unwrap();
        crate::execution::recover(&rook.store).unwrap();
    }

    #[test]
    fn recovery_reuses_reserved_execution_and_admitted_prompt_context() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("recover same execution").unwrap();
        let run = goal(&rook, session);
        enqueue(&rook, session, "one");
        status(&rook, &run, Status::Completed);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        let turn = crate::execution::current(&rook, session).unwrap().unwrap().turn;
        let (seq, _) = journal
            .admit_prompt(&rook, "next task", "next task", "", Some("original hook context"), None)
            .unwrap();
        journal.finish("end_turn", None).unwrap();
        drop(journal);
        lost_owner(&rook, session);
        assert!(ready(&rook, session).unwrap());
        let (journal, message) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        assert_eq!(crate::execution::current(&rook, session).unwrap().unwrap().turn, turn);
        let admitted = journal.recovered_prompt(&message.text).unwrap().unwrap();
        assert_eq!(admitted.seq, seq);
        assert_eq!(admitted.context.as_deref(), Some("original hook context"));
        assert!(journal.recovered_outcome().unwrap().is_none());
        assert!(journal.recovered_prompt("different").is_err());
        assert_eq!(
            rook.store
                .events(session, 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.record.kind == rook_store::EventKind::UserMessage)
                .count(),
            1
        );
    }

    #[test]
    fn a_saved_outcome_is_recovered_without_authorizing_another_execution() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("saved ending").unwrap();
        let run = goal(&rook, session);
        enqueue(&rook, session, "one");
        status(&rook, &run, Status::Completed);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        journal.admit_prompt(&rook, "next task", "next task", "", Some(""), None).unwrap();
        let outcome:crate::agent::TurnOutcome=serde_json::from_value(serde_json::json!({
            "steps":2,"stopped":"end_turn","reply":"recorded final reply", "input_tokens":10,"output_tokens":20,"cached_tokens":0,
            "tools_called":[],"skills_loaded":[],"skills_written":[],"facts_learned":[],"facts_forgotten":[],"delegated":[],"compactions":0
        })).unwrap();
        journal.record_outcome(&outcome).unwrap();
        journal.finish("end_turn", None).unwrap();
        drop(journal);
        lost_owner(&rook, session);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        assert_eq!(journal.recovered_outcome().unwrap().unwrap().reply, "recorded final reply");
        journal.finish("end_turn", None).unwrap();
        assert!(!ready(&rook, session).unwrap());
    }

    #[test]
    fn oversized_recovery_context_is_refused_before_prompt_admission() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("bounded hook context").unwrap();
        let journal = Journal::start(&rook, session, None).unwrap();
        let context = "x".repeat(1024 * 1024 + 1);
        assert!(context.len() > 1024 * 1024);
        assert!(journal.admit_prompt(&rook, "a", "a", "", Some(&context), None).is_err());
        assert!(rook.store.events(session, 0, 10).unwrap().is_empty());
    }

    #[test]
    fn interrupted_side_effects_block_followup_recovery_until_explicit_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("unknown operation").unwrap();
        let run = goal(&rook, session);
        enqueue(&rook, session, "one");
        status(&rook, &run, Status::Completed);
        let (journal, _) = Journal::reserve_follow_up(&rook, session).unwrap().unwrap();
        journal.admit_prompt(&rook, "next task", "next task", "", Some(""), None).unwrap();
        journal.begin("run_command", "write file", true, false, None).unwrap();
        journal.finish("end_turn", None).unwrap();
        drop(journal);
        lost_owner(&rook, session);
        let state = crate::execution::current(&rook, session).unwrap().unwrap();
        assert_eq!(state.unknown.len(), 1);
        assert!(!ready(&rook, session).unwrap());
        assert!(Journal::reserve_follow_up(&rook, session).unwrap().is_none());
        assert!(
            view::page(&rook, session, &Default::default())
                .unwrap()
                .follow_up_status
                .unwrap()
                .contains("/recovery")
        );
        rook.acknowledge_operation(
            session,
            &state.unknown[0].id,
            "Inspected the output and files; the operation had no effects",
        )
        .unwrap();
        assert!(ready(&rook, session).unwrap());
    }
}
