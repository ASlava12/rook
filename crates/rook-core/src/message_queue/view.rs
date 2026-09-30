//! Frontends use opaque receipt references, never a row number as authority.
use crate::{
    CoreError, Result, Rook,
    work::{managed, receipts},
};
use rook_proto::{
    queue::{Change, Entry, Page, Query},
    work::{EditInstruction, Run, Steering, WithdrawInstruction},
};

fn stale() -> CoreError {
    CoreError::Other("queue reference is no longer current; refresh the queue before changing it".into())
}

fn generation(run: &Run) -> String {
    run.identity().generation
}

fn reference(run: Option<&Run>, message: &Steering) -> String {
    match run {
        Some(run) => format!("goal.{}.{}", generation(run), message.id),
        None => format!("session.{}", message.id),
    }
}

/// Build while holding the receipt mutation lock, so a replacement goal cannot
/// lend its generation to a receipt accepted by the previous goal.
pub(crate) fn notice(session: u128, run: Option<&Run>, message: &Steering) -> rook_proto::queue::Notice {
    rook_proto::queue::Notice::new(rook_store::format_session_id(session), reference(run, message), message)
}

fn entry(reference: String, message: &Steering, limit: usize) -> Entry {
    let end = crate::context::at_boundary(&message.text, limit.min(message.text.len()));
    Entry {
        reference,
        truncated: end < message.text.len(),
        receipt: Steering {
            id: message.id.clone(),
            text: message.text[..end].into(),
            submitted_at: message.submitted_at,
            applied_at: message.applied_at,
            session: message.session.clone(),
            revision: message.revision,
            withdrawn_at: message.withdrawn_at,
            submitted_hash: message.submitted_hash.clone(),
            follow_up: message.follow_up.clone(),
        },
    }
}

pub fn page(rook: &Rook, session: u128, query: &Query) -> Result<Page> {
    let _lock = receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let ordinary = super::list(rook, session)?;
    let work = managed::for_session(rook, session)?;
    let settings = rook.config.transcript.bounded();
    let submission_target = work
        .as_ref()
        .filter(|run| !run.status.terminal())
        .map_or_else(|| "session".into(), |run| format!("goal.{}", generation(run)));
    let mut page = Page {
        items: Vec::new(),
        submission_target,
        follow_up_target: super::followups::target(rook, session)?,
        follow_up_status: if ordinary.iter().any(|m| m.follow_up.is_some())
            && rook.recovery_block(session)?.is_some()
        {
            Some("Follow-up awaits /recovery; inspect unknown operations.".into())
        } else {
            None
        },
        next: None,
        total: 0,
        max_message_bytes: rook.config.work.max_message_bytes.min(8 * 1024 * 1024),
    };
    let mut reached = query.after.is_none();
    // Reserve metadata space for the daemon's bounded recovery status too.
    let mut used = 1024;
    let mut full = false;
    for (run, message) in ordinary
        .iter()
        .map(|m| (None, m))
        .chain(work.as_ref().into_iter().flat_map(|run| run.instructions.iter().map(move |m| (Some(run), m))))
    {
        let wanted = query.include_finished || message.queued();
        page.total += usize::from(wanted);
        let address = reference(run, message);
        if !reached {
            reached = query.after.as_ref() == Some(&address);
            continue;
        }
        if !wanted || full {
            continue;
        }
        // Even escaping every byte must leave space for metadata in the first
        // row. Admission precedes copying the text into a view.
        let body = settings.body_bytes.min(settings.page_bytes.saturating_sub(1536) / 6);
        let item = entry(address, message, body);
        let encoded = crate::persistence::encode_with_limit(&item, settings.page_bytes)?;
        if page.items.len() >= settings.page_entries
            || encoded.len() + 1 > settings.page_bytes.saturating_sub(used)
        {
            page.next = page.items.last().map(|last| last.reference.clone());
            full = true;
            continue;
        }
        used += encoded.len() + 1;
        page.items.push(item);
    }
    if !reached {
        return Err(stale());
    }
    Ok(page)
}

pub fn read(rook: &Rook, session: u128, address: &str) -> Result<Entry> {
    let _lock = receipts::WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let message = if let Some(id) = address.strip_prefix("session.") {
        super::list(rook, session)?.into_iter().find(|m| m.id == id).ok_or_else(stale)?
    } else {
        let run = managed::for_session(rook, session)?.ok_or_else(stale)?;
        run.instructions.iter().find(|m| reference(Some(&run), m) == address).cloned().ok_or_else(stale)?
    };
    Ok(Entry { reference: address.into(), receipt: message, truncated: false })
}

pub fn change(rook: &Rook, session: u128, change: Change) -> Result<Entry> {
    let change = match change {
        Change::FollowUp { target, id, text } => {
            let receipt =
                super::followups::submit(rook, session, &target, rook_proto::work::Steer { id, text })?;
            return Ok(Entry { reference: reference(None, &receipt), receipt, truncated: false });
        }
        Change::Submit { target, id, text } => {
            return submit(rook, session, &target, rook_proto::work::Steer { id, text });
        }
        change => change,
    };
    let address = match &change {
        Change::Edit { reference, .. } | Change::Withdraw { reference, .. } => reference.clone(),
        Change::Submit { .. } | Change::FollowUp { .. } => return Err(stale()),
    };
    let apply = |messages: &mut [Steering], id: &str, open: bool| match change {
        Change::Submit { .. } | Change::FollowUp { .. } => Err(stale()),
        Change::Edit { revision, text, .. } => {
            receipts::edit(rook, messages, id, EditInstruction { revision, text }, open)
        }
        Change::Withdraw { revision, .. } => {
            super::check_withdraw(rook, session, messages, id)?;
            receipts::withdraw(messages, id, WithdrawInstruction { revision }, open)
        }
    };
    let message = if let Some(id) = address.strip_prefix("session.") {
        super::update(rook, session, |messages| apply(messages, id, true))?
    } else {
        let id = rook_store::format_session_id(session);
        managed::update(rook, &id, |saved| {
            let prefix = format!("goal.{}.", generation(&saved.run));
            let message = address.strip_prefix(&prefix).ok_or_else(stale)?;
            if saved.run.conversation.is_none() {
                return Err(stale());
            }
            apply(&mut saved.run.instructions, message, !saved.run.status.terminal())
        })?
    };
    Ok(Entry { reference: address, receipt: message, truncated: false })
}

fn submit(rook: &Rook, session: u128, target: &str, request: rook_proto::work::Steer) -> Result<Entry> {
    if target == "session" {
        let (receipt, notice) = super::submit_noticed(rook, session, request)?;
        return Ok(Entry { reference: notice.reference, receipt, truncated: false });
    }
    let named = rook_store::format_session_id(session);
    managed::update(rook, &named, |saved| {
        if target != format!("goal.{}", generation(&saved.run)) || saved.run.conversation.is_none() {
            return Err(stale());
        }
        let receipt =
            receipts::submit(rook, &mut saved.run.instructions, request, !saved.run.status.terminal())?;
        saved.idle = 0;
        Ok(Entry { reference: reference(Some(&saved.run), &receipt), receipt, truncated: false })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rook_proto::work::{Action, Conversation, Start, Steer};

    fn engine(dir: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(dir.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.7.2"),
            rook_skills::SkillIndex::discover(&[]).0,
            dir.into(),
        )
    }
    fn goal(rook: &Rook, session: u128) -> Run {
        managed::start(
            rook,
            Start {
                goal: "inspect the project".into(),
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
    fn steer(id: &str, text: &str) -> Steer {
        Steer { id: id.into(), text: text.into() }
    }

    #[test]
    fn legacy_goal_mutations_report_the_generation_that_was_changed() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("goal notices").unwrap();
        let run = goal(&rook, session);
        managed::steer(&rook, &run.id, steer("one", "first")).unwrap();
        let (edited, notice) = managed::edit_instruction_noticed(
            &rook,
            &run.id,
            "one",
            EditInstruction { revision: 0, text: "second".into() },
        )
        .unwrap();
        let reference = format!("goal.{}.one", run.identity().generation);
        assert_eq!(notice.reference, reference);
        assert_eq!(notice.session, rook_store::format_session_id(session));
        assert_eq!(notice.revision, edited.revision);
        assert_eq!(notice.status, rook_proto::queue::Status::Queued);
        let (withdrawn, notice) =
            managed::withdraw_instruction_noticed(&rook, &run.id, "one", WithdrawInstruction { revision: 1 })
                .unwrap();
        assert_eq!(notice.reference, reference);
        assert_eq!(notice.revision, withdrawn.revision);
        assert_eq!(notice.status, rook_proto::queue::Status::Withdrawn);
        assert_eq!(withdrawn.text, "second");
    }

    #[test]
    fn scoped_submissions_survive_lost_replies_restart_and_goal_replacement_without_retargeting() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("stable submission").unwrap();
        let ordinary_target = page(&rook, session, &Query::default()).unwrap().submission_target;
        let ordinary = Change::Submit {
            target: ordinary_target.clone(),
            id: "ordinary".into(),
            text: "original".into(),
        };
        let saved = change(&rook, session, ordinary.clone()).unwrap();
        change(
            &rook,
            session,
            Change::Edit { reference: saved.reference, revision: 0, text: "edited".into() },
        )
        .unwrap();
        super::super::accept(&rook, session, "ordinary", None).unwrap();
        let run = goal(&rook, session);
        let target = page(&rook, session, &Query::default()).unwrap().submission_target;
        let goal_request = Change::Submit {
            target: target.clone(),
            id: "goal-message".into(),
            text: "goal correction".into(),
        };
        let goal_receipt = change(&rook, session, goal_request.clone()).unwrap();
        change(&rook, session, Change::Withdraw { reference: goal_receipt.reference, revision: 0 }).unwrap();
        assert!(
            change(
                &rook,
                session,
                Change::Submit { target: ordinary_target, id: "late".into(), text: "do not retarget".into() }
            )
            .is_err()
        );
        drop(rook);
        let rook = engine(dir.path());
        let retried = change(&rook, session, ordinary).unwrap();
        assert_eq!(retried.receipt.text, "edited");
        assert!(retried.receipt.applied_at.is_some());
        let retried = change(&rook, session, goal_request.clone()).unwrap();
        assert!(retried.receipt.withdrawn_at.is_some());
        managed::control(&rook, &run.id, Action::Cancel).unwrap();
        assert!(change(&rook, session, goal_request.clone()).unwrap().receipt.withdrawn_at.is_some());
        let replacement = goal(&rook, session);
        assert_ne!(replacement.generation, run.generation);
        assert!(
            change(&rook, session, goal_request).is_err(),
            "an old target cannot resolve a new generation"
        );
        assert!(replacement.instructions.is_empty());
        assert_eq!(super::super::list(&rook, session).unwrap().len(), 1);
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
    fn concurrent_retries_have_one_receipt_and_still_work_at_the_receipt_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.work.max_messages = 1;
        let session = rook.start_session("concurrent submission").unwrap();
        let request = Change::Submit { target: "session".into(), id: "same".into(), text: "🙂".into() };
        let barrier = std::sync::Barrier::new(2);
        let (left, right) = std::thread::scope(|scope| {
            let send = || {
                barrier.wait();
                change(&rook, session, request.clone()).unwrap()
            };
            let one = scope.spawn(send);
            let two = scope.spawn(send);
            (one.join().unwrap(), two.join().unwrap())
        });
        assert_eq!(left.reference, right.reference);
        assert_eq!(super::super::list(&rook, session).unwrap().len(), rook.config.work.max_messages);
        assert!(change(&rook, session, request.clone()).is_ok());
        assert!(
            change(
                &rook,
                session,
                Change::Submit { target: "session".into(), id: "other".into(), text: "new".into() }
            )
            .is_err()
        );
        assert!(
            change(
                &rook,
                session,
                Change::Submit { target: "session".into(), id: "same".into(), text: "different".into() }
            )
            .is_err()
        );
        assert!(
            rook.store.events(session, 0, 100).unwrap().is_empty(),
            "submitting alone never starts or logs a turn"
        );
    }

    #[test]
    fn queue_pages_reach_their_byte_bound_and_keep_unicode_and_receipt_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let mut rook = engine(dir.path());
        rook.config.transcript.page_bytes = 4096;
        rook.config.transcript.body_bytes = 4096;
        rook.config.transcript.page_entries = 64;
        let session = rook.start_session("bounded view").unwrap();
        let text = "🙂\n\"".repeat(600);
        for index in 0..16 {
            super::super::submit(&rook, session, steer(&index.to_string(), &text)).unwrap();
        }
        assert!(text.len() * 16 > rook.config.transcript.page_bytes);
        let mut query = Query::default();
        let mut seen = Vec::new();
        loop {
            let page = page(&rook, session, &query).unwrap();
            if query.after.is_none() {
                assert!(page.next.is_some(), "the setup must fill a page");
            }
            assert!(serde_json::to_vec(&page).unwrap().len() <= rook.config.transcript.page_bytes);
            assert!(!page.items.is_empty());
            for item in &page.items {
                assert!(item.truncated);
                assert!(text.starts_with(&item.receipt.text));
                assert_eq!(read(&rook, session, &item.reference).unwrap().receipt.text, text);
                seen.push(item.receipt.id.clone());
            }
            let Some(after) = page.next else {
                break;
            };
            assert!(page.items.len() < rook.config.transcript.page_entries, "this page hit bytes, not rows");
            // The cursor still names an accepted receipt even though the next
            // pending-only page no longer displays that receipt.
            let last = page.items.last().unwrap();
            super::super::accept(&rook, session, &last.receipt.id, None).unwrap();
            query.after = Some(after);
        }
        assert_eq!(seen, (0..16).map(|index| index.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn opaque_references_distinguish_equal_ids_and_reject_a_new_goal_generation() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("scopes").unwrap();
        super::super::submit(&rook, session, steer("same", "ordinary")).unwrap();
        let run = goal(&rook, session);
        managed::steer(&rook, &run.id, steer("same", "goal correction")).unwrap();
        let original = page(&rook, session, &Query::default()).unwrap();
        assert_eq!(original.items.len(), 2);
        let ordinary = &original.items[0];
        let work = &original.items[1];
        assert_ne!(ordinary.reference, work.reference);
        change(
            &rook,
            session,
            Change::Edit {
                reference: ordinary.reference.clone(),
                revision: 0,
                text: "edited ordinary".into(),
            },
        )
        .unwrap();
        assert_eq!(read(&rook, session, &work.reference).unwrap().receipt.text, "goal correction");
        managed::control(&rook, &run.id, Action::Cancel).unwrap();
        let next = goal(&rook, session);
        assert_ne!(next.generation, run.generation);
        managed::steer(&rook, &next.id, steer("same", "new goal correction")).unwrap();
        assert!(
            change(&rook, session, Change::Withdraw { reference: work.reference.clone(), revision: 0 })
                .is_err()
        );
        assert!(read(&rook, session, &work.reference).is_err());
        assert!(
            page(&rook, session, &Query { after: Some(work.reference.clone()), include_finished: true })
                .is_err()
        );
        let current = page(&rook, session, &Query::default()).unwrap();
        let new_work = &current.items[1];
        assert_eq!(new_work.receipt.text, "new goal correction");
        managed::accept(&rook, &next.identity(), session, "same").unwrap();
        assert!(
            change(&rook, session, Change::Withdraw { reference: new_work.reference.clone(), revision: 0 })
                .is_err()
        );
    }

    #[test]
    fn legacy_goal_receipts_have_stable_view_references_until_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("legacy").unwrap();
        let run = goal(&rook, session);
        managed::update(&rook, &run.id, |saved| {
            saved.run.generation.clear();
            Ok(())
        })
        .unwrap();
        managed::steer(&rook, &run.id, steer("one", "legacy message")).unwrap();
        let item = page(&rook, session, &Query::default()).unwrap().items.remove(0);
        assert!(item.reference.starts_with("goal.legacy-"));
        change(
            &rook,
            session,
            Change::Edit { reference: item.reference.clone(), revision: 0, text: "updated".into() },
        )
        .unwrap();
        assert_eq!(page(&rook, session, &Query::default()).unwrap().items[0].reference, item.reference);
        let legacy = managed::read(&rook, &run.id).unwrap().run.identity();
        assert_eq!(managed::pending(&rook, &legacy).unwrap(), ["one"]);
        let accepted = managed::accept(&rook, &legacy, session, "one").unwrap().unwrap();
        assert_eq!(accepted.receipt.reference, item.reference);
        managed::control(&rook, &run.id, Action::Cancel).unwrap();
        goal(&rook, session);
        assert!(managed::should_stop(&rook, &legacy).unwrap());
    }
}
