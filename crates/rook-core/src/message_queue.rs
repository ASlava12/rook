//! Durable session messages. Steering updates an existing turn; follow-ups
//! authorize a separate turn at their captured completion boundary. Both use
//! bounded receipts, revision checks and atomic acceptance.
pub mod followups;
pub mod view;

use rook_proto::work::{EditInstruction, Steer, Steering, WithdrawInstruction};
use rook_store::{EventKind, Kind, NewEvent};

use crate::{
    CoreError, Result, Rook,
    work::{managed, receipts},
};

pub(crate) fn key(session: u128) -> String {
    // Store::delete_session also removes companions with this suffix.
    format!("message-queue/{session:032x}")
}

pub fn list(rook: &Rook, session: u128) -> Result<Vec<Steering>> {
    if rook.store.get_session(session)?.is_none() {
        return Err(CoreError::Other("no such session".into()));
    }
    read_from(&rook.store, session)
}

pub(crate) fn read_from(store: &rook_store::Store, session: u128) -> Result<Vec<Steering>> {
    Ok(crate::persistence::read_json(store, &key(session))?.unwrap_or_default())
}

fn update<T>(rook: &Rook, session: u128, change: impl FnOnce(&mut Vec<Steering>) -> Result<T>) -> Result<T> {
    let _lock = receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let mut messages = list(rook, session)?;
    let value = change(&mut messages)?;
    let encoded = crate::persistence::encode(&messages)?;
    rook.store.append_events_with_values(session, [], &[(&key(session), &encoded)])?;
    Ok(value)
}

pub fn submit(rook: &Rook, session: u128, request: Steer) -> Result<Steering> {
    submit_noticed(rook, session, request).map(|(receipt, _)| receipt)
}

pub fn submit_noticed(
    rook: &Rook,
    session: u128,
    request: Steer,
) -> Result<(Steering, rook_proto::queue::Notice)> {
    update(rook, session, |messages| {
        if messages.iter().any(|m| m.id == request.id && m.follow_up.is_some()) {
            return Err(CoreError::Other(
                "this ID belongs to a follow-up; retry with its original mode and target".into(),
            ));
        }
        // Check under the same lock as goal admission. Existing receipt retries
        // still work after promotion, but new corrections belong to the goal.
        if !messages.iter().any(|m| m.id == request.id)
            && managed::for_session(rook, session)?.is_some_and(|run| !run.status.terminal())
        {
            return Err(CoreError::Other(
                "this session has an active goal; submit the correction to its work queue".into(),
            ));
        }
        let receipt = receipts::submit(rook, messages, request, true)?;
        let notice = view::notice(session, None, &receipt);
        Ok((receipt, notice))
    })
}

/// Receipt-only compatibility API for embedded callers.
#[doc(hidden)]
pub fn edit(rook: &Rook, session: u128, id: &str, request: EditInstruction) -> Result<Steering> {
    edit_noticed(rook, session, id, request).map(|(receipt, _)| receipt)
}

pub fn edit_noticed(
    rook: &Rook,
    session: u128,
    id: &str,
    request: EditInstruction,
) -> Result<(Steering, rook_proto::queue::Notice)> {
    update(rook, session, |messages| {
        let receipt = receipts::edit(rook, messages, id, request, true)?;
        let notice = view::notice(session, None, &receipt);
        Ok((receipt, notice))
    })
}

/// Receipt-only compatibility API for embedded callers.
#[doc(hidden)]
pub fn withdraw(rook: &Rook, session: u128, id: &str, request: WithdrawInstruction) -> Result<Steering> {
    withdraw_noticed(rook, session, id, request).map(|(receipt, _)| receipt)
}

pub fn withdraw_noticed(
    rook: &Rook,
    session: u128,
    id: &str,
    request: WithdrawInstruction,
) -> Result<(Steering, rook_proto::queue::Notice)> {
    update(rook, session, |messages| {
        check_withdraw(rook, session, messages, id)?;
        let receipt = receipts::withdraw(messages, id, request, true)?;
        let notice = view::notice(session, None, &receipt);
        Ok((receipt, notice))
    })
}

pub(crate) fn check_withdraw(rook: &Rook, session: u128, messages: &[Steering], id: &str) -> Result<()> {
    if messages.iter().any(|m| m.id == id && m.follow_up.as_ref().is_some_and(|f| f.reserved.is_some()))
        && crate::execution::is_active(rook, session)
    {
        return Err(CoreError::Other(
            "follow-up is starting; stop the active execution before withdrawing an unaccepted message"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn pending(rook: &Rook, session: u128) -> Result<Vec<String>> {
    Ok(list(rook, session)?
        .into_iter()
        .filter(|m| m.queued() && m.follow_up.is_none())
        .map(|m| m.id)
        .collect())
}

/// Text and identity from one successful acceptance, never a subsequent read.
pub struct Accepted {
    pub text: String,
    pub receipt: rook_proto::queue::Notice,
}

pub(crate) fn accept(
    rook: &Rook,
    session: u128,
    id: &str,
    consumer: Option<&rook_proto::work::RunIdentity>,
) -> Result<Option<Accepted>> {
    let _lock = receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    match consumer {
        Some(identity) if managed::should_stop(rook, identity)? => return Ok(None),
        None if managed::for_session(rook, session)?.is_some_and(|run| !run.status.terminal()) => {
            // Promotion may happen after the loop read its input list. Attach
            // to that goal before consuming pre-promotion session messages.
            return Ok(None);
        }
        _ => {}
    }
    let mut messages = list(rook, session)?;
    let message = messages
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| CoreError::Other("unknown instruction receipt".into()))?;
    if !message.queued() || message.follow_up.is_some() {
        return Ok(None);
    }
    message.applied_at = Some(managed::now());
    message.session = Some(rook_store::format_session_id(session));
    let text = message.text.clone();
    let receipt = view::notice(session, None, message);
    let encoded = crate::persistence::encode(&messages)?;
    rook.store.append_events_with_values(
        session,
        [NewEvent::new(EventKind::UserMessage, Kind::Message, text.as_bytes()).label("while running")],
        &[(&key(session), &encoded)],
    )?;
    Ok(Some(Accepted { text, receipt }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(dir: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(dir.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.7.2"),
            rook_skills::SkillIndex::discover(&[]).0,
            dir.into(),
        )
    }

    fn request(id: &str, text: &str) -> Steer {
        Steer { id: id.into(), text: text.into() }
    }

    #[test]
    fn legacy_session_mutations_report_the_committed_receipt_identity() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("legacy notices").unwrap();
        submit(&rook, session, request("one", "first")).unwrap();
        let (edited, notice) =
            edit_noticed(&rook, session, "one", EditInstruction { revision: 0, text: "second".into() })
                .unwrap();
        assert_eq!(notice.reference, "session.one");
        assert_eq!(notice.session, rook_store::format_session_id(session));
        assert_eq!(notice.revision, edited.revision);
        assert_eq!(notice.status, rook_proto::queue::Status::Queued);
        let (withdrawn, notice) =
            withdraw_noticed(&rook, session, "one", WithdrawInstruction { revision: 1 }).unwrap();
        assert_eq!(notice.reference, "session.one");
        assert_eq!(notice.revision, withdrawn.revision);
        assert_eq!(notice.status, rook_proto::queue::Status::Withdrawn);
        assert_eq!(withdrawn.text, "second");
    }

    #[test]
    fn ordinary_receipts_survive_restart_without_creating_a_goal_and_accept_the_latest_text_once() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("ordinary").unwrap();
        submit(&rook, session, request("one", "original")).unwrap();
        let observed = pending(&rook, session).unwrap();
        edit(&rook, session, "one", EditInstruction { revision: 0, text: "new text".into() }).unwrap();
        assert!(edit(&rook, session, "one", EditInstruction { revision: 0, text: "stale".into() }).is_err());
        submit(&rook, session, request("two", "withdraw me")).unwrap();
        withdraw(&rook, session, "two", WithdrawInstruction { revision: 0 }).unwrap();
        drop(rook);
        let rook = engine(dir.path());
        assert!(managed::for_session(&rook, session).unwrap().is_none());
        assert!(rook.goal(session).unwrap().is_none());
        assert_eq!(submit(&rook, session, request("one", "original")).unwrap().text, "new text");
        assert!(submit(&rook, session, request("two", "withdraw me")).unwrap().withdrawn_at.is_some());
        assert_eq!(
            accept(&rook, session, &observed[0], None)
                .unwrap()
                .as_ref()
                .map(|accepted| accepted.text.as_str()),
            Some("new text")
        );
        assert!(accept(&rook, session, "two", None).unwrap().is_none());
        assert!(
            !edit(&rook, session, "one", EditInstruction { revision: 0, text: "new text".into() })
                .unwrap()
                .queued()
        );
        assert!(withdraw(&rook, session, "one", WithdrawInstruction { revision: 1 }).is_err());
        drop(rook);
        let rook = engine(dir.path());
        assert!(accept(&rook, session, "one", None).unwrap().is_none());
        let events = rook.store.events(session, 0, 100).unwrap();
        assert_eq!(events.iter().filter(|e| e.record.kind == EventKind::UserMessage).count(), 1);
        rook.delete_session(session).unwrap();
        assert!(rook.store.kv_get(&key(session)).unwrap().is_none(), "retention removes the queue too");
        assert!(submit(&rook, session, request("late", "after deletion")).is_err());
    }

    #[test]
    fn an_oversized_saved_queue_refuses_reads_and_mutations_without_losing_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("reader bound").unwrap();
        submit(&rook, session, request("one", "pending 🙂")).unwrap();
        let mut bytes = rook.store.kv_get(&key(session)).unwrap().unwrap();
        bytes.resize(crate::persistence::MAX_JSON_BYTES, b' ');
        rook.store.kv_set(&key(session), &bytes).unwrap();
        assert_eq!(list(&rook, session).unwrap()[0].text, "pending 🙂");
        bytes.push(b' ');
        assert!(bytes.len() > crate::persistence::MAX_JSON_BYTES);
        assert!(serde_json::from_slice::<Vec<Steering>>(&bytes).is_ok(), "valid JSON exceeds the cap");
        rook.store.kv_set(&key(session), &bytes).unwrap();
        let bound = |error: CoreError| {
            assert!(error.to_string().contains("exceeds 8388608 bytes"), "{error}");
        };
        bound(list(&rook, session).unwrap_err());
        bound(submit(&rook, session, request("two", "refused")).unwrap_err());
        bound(
            edit(&rook, session, "one", EditInstruction { revision: 0, text: "refused".into() }).unwrap_err(),
        );
        bound(withdraw(&rook, session, "one", WithdrawInstruction { revision: 0 }).unwrap_err());
        bound(accept(&rook, session, "one", None).err().unwrap());
        bound(followups::ready(&rook, session).unwrap_err());
        assert_eq!(rook.store.kv_get(&key(session)).unwrap().unwrap(), bytes);
        assert!(rook.store.events(session, 0, 100).unwrap().is_empty());
    }

    #[test]
    fn ordinary_receipt_limits_include_tombstones_so_a_retry_cannot_reanimate_a_withdrawal() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.work.max_messages = 1;
        rook.config.work.max_message_bytes = 4;
        let session = rook.start_session("bounded").unwrap();
        assert!(submit(&rook, session, request("big", "abcde")).is_err());
        assert!(list(&rook, session).unwrap().is_empty());
        submit(&rook, session, request("one", "🙂")).unwrap();
        assert_eq!(list(&rook, session).unwrap().len(), rook.config.work.max_messages);
        withdraw(&rook, session, "one", WithdrawInstruction { revision: 0 }).unwrap();
        assert!(submit(&rook, session, request("two", "more")).is_err());
        assert!(submit(&rook, session, request("one", "🙂")).unwrap().withdrawn_at.is_some());
        assert!(edit(&rook, session, "one", EditInstruction { revision: 1, text: "🙂a".into() }).is_err());
    }

    #[test]
    fn pre_promotion_messages_require_the_current_goal_consumer_and_survive_its_replacement() {
        use rook_proto::work::{Action, Conversation, Start};
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("promotion").unwrap();
        submit(&rook, session, request("one", "before promotion")).unwrap();
        submit(&rook, session, request("two", "still pending")).unwrap();
        let start = || {
            managed::start(
                &rook,
                Start {
                    goal: "current goal".into(),
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
        };
        let old = start();
        assert!(accept(&rook, session, "one", None).unwrap().is_none());
        assert!(accept(&rook, session, "one", Some(&old.identity())).unwrap().is_some());
        managed::control(&rook, &old.id, Action::Pause).unwrap();
        assert!(accept(&rook, session, "two", Some(&old.identity())).unwrap().is_none());
        managed::control(&rook, &old.id, Action::Cancel).unwrap();
        let current = start();
        assert_ne!(old.identity(), current.identity());
        assert!(accept(&rook, session, "two", Some(&old.identity())).unwrap().is_none());
        assert_eq!(pending(&rook, session).unwrap(), ["two"]);
        let accepted = accept(&rook, session, "two", Some(&current.identity())).unwrap().unwrap();
        assert_eq!(accepted.text, "still pending");
        assert_eq!(accepted.receipt.reference, "session.two");
        assert!(pending(&rook, session).unwrap().is_empty());
        assert_eq!(
            rook.store
                .events(session, 0, 100)
                .unwrap()
                .iter()
                .filter(|event| event.record.kind == EventKind::UserMessage)
                .count(),
            2
        );
    }

    #[test]
    fn withdrawing_and_accepting_an_ordinary_message_have_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("race").unwrap();
        for index in 0..16 {
            let id = index.to_string();
            submit(&rook, session, request(&id, "racing")).unwrap();
            let barrier = std::sync::Barrier::new(2);
            let (accepted, withdrawn) = std::thread::scope(|scope| {
                let accepting = scope.spawn(|| {
                    barrier.wait();
                    accept(&rook, session, &id, None).unwrap()
                });
                let withdrawing = scope.spawn(|| {
                    barrier.wait();
                    withdraw(&rook, session, &id, WithdrawInstruction { revision: 0 })
                });
                (accepting.join().unwrap(), withdrawing.join().unwrap())
            });
            assert_ne!(accepted.is_some(), withdrawn.is_ok());
            let receipt = list(&rook, session).unwrap().into_iter().find(|m| m.id == id).unwrap();
            assert_ne!(receipt.applied_at.is_some(), receipt.withdrawn_at.is_some());
            assert!(!receipt.queued());
        }
        let accepted = list(&rook, session).unwrap().iter().filter(|m| m.applied_at.is_some()).count();
        assert_eq!(
            rook.store
                .events(session, 0, 100)
                .unwrap()
                .iter()
                .filter(|e| e.record.kind == EventKind::UserMessage)
                .count(),
            accepted
        );
    }
}
