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
    let goal = managed::for_session(rook, session)?;
    if goal.as_ref().is_some_and(|r| !r.status.terminal()) || rook.recovery_block(session)?.is_some() {
        return Ok(None);
    }
    let identity = goal.as_ref().map(|run| run.identity());
    let completed = rook.completed_turn(session)?.is_some_and(|o| crate::agent::finished(&o.stopped));
    let turn = crate::execution::current(rook, session)?.map(|state| format!("turn.{}", state.turn));
    Ok(messages.iter().position(|message| {
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
    }))
}

/// A scheduler can recover queued, unreserved follow-ups after the owner exits.
/// A reserved execution is inspected through the execution recovery journal.
pub fn ready(rook: &Rook, session: u128) -> Result<bool> {
    let _lock = crate::work::receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    Ok(next(rook, session, &super::list(rook, session)?)?.is_some())
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
        assert!(journal.admit_prompt(&rook, "next task", "next task", "").is_err());
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
        let (seq, notice) = journal.admit_prompt(&rook, "next task", "next task", "").unwrap();
        assert_eq!(notice.unwrap().reference, "session.next");
        assert_eq!(journal.admit_prompt(&rook, "next task", "next task", "").unwrap().0, seq);
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
}
