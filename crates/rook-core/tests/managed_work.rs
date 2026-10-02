//! Durable scheduling and steering tested without a model or wall-clock days.
use async_trait::async_trait;
use rook_core::{Rook, agent::AgentLoop, work::managed as work};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
use rook_proto::work::{
    Action, EditInstruction, IdentifiedControl, Start, Status, Steer, WithdrawInstruction,
};
use std::sync::{Arc, Mutex};

fn engine(workspace: &std::path::Path, store: &std::path::Path) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(store).unwrap(),
        rook_core::Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "0.7.2"),
        rook_skills::SkillIndex::discover(&[]).0,
        workspace.into(),
    )
}

fn start(rook: &Rook) -> rook_proto::work::Run {
    work::start(
        rook,
        Start {
            conversation: None,
            goal: "Inspect evidence.txt and report its contents".into(),
            workspace: None,
            autonomous: true,
            max_iterations: None,
            max_tokens: None,
            max_seconds: None,
        },
    )
    .unwrap()
}

fn correction(id: &str, text: &str) -> Steer {
    Steer { id: id.into(), text: text.into() }
}

fn control(id: &str, generation: &str, action: Action) -> IdentifiedControl {
    IdentifiedControl { id: id.into(), generation: generation.into(), action }
}

#[test]
fn saved_work_and_its_index_are_admitted_before_reads_or_controls() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("work reader bound").unwrap();
    let run = conversation_goal(&rook, session, "paused goal");
    work::control(&rook, &run.id, Action::Pause).unwrap();
    let key = format!("work/managed/{}", run.id);
    let original = rook.store.kv_get(&key).unwrap().unwrap();
    let cap = 8 * 1024 * 1024;
    let mut bytes = original.clone();
    bytes.resize(cap, b' ');
    rook.store.kv_set(&key, &bytes).unwrap();
    assert_eq!(work::read(&rook, &run.id).unwrap().run.status, Status::Paused);
    assert_eq!(work::for_session(&rook, session).unwrap().unwrap().generation, run.generation);
    assert!(work::pending(&rook, &run.identity()).unwrap().is_empty());
    bytes.push(b' ');
    assert!(bytes.len() > cap);
    assert!(serde_json::from_slice::<work::Saved>(&bytes).is_ok());
    rook.store.kv_set(&key, &bytes).unwrap();
    let bound = |error: rook_core::CoreError| {
        assert!(error.to_string().contains("exceeds 8388608 bytes"), "{error}");
    };
    bound(work::read(&rook, &run.id).unwrap_err());
    bound(work::list(&rook).unwrap_err());
    bound(work::for_session(&rook, session).unwrap_err());
    bound(work::pending(&rook, &run.identity()).unwrap_err());
    bound(
        work::control_identified(&rook, &run.id, control("refused", &run.generation, Action::Resume))
            .unwrap_err(),
    );
    assert_eq!(rook.store.kv_get(&key).unwrap().unwrap(), bytes);
    rook.store.kv_set(&key, &original).unwrap();
    let index_key = "work/managed-index";
    let original_index = rook.store.kv_get(index_key).unwrap().unwrap();
    let mut index = original_index.clone();
    index.resize(cap, b' ');
    rook.store.kv_set(index_key, &index).unwrap();
    assert_eq!(work::list(&rook).unwrap().len(), 1);
    index.push(b' ');
    assert!(index.len() > cap);
    assert!(serde_json::from_slice::<Vec<String>>(&index).is_ok());
    rook.store.kv_set(index_key, &index).unwrap();
    bound(work::list(&rook).unwrap_err());
    assert_eq!(rook.store.kv_get(index_key).unwrap().unwrap(), index);
    assert_eq!(rook.store.kv_get(&key).unwrap().unwrap(), original);
}

#[test]
fn identified_controls_survive_reopen_without_reapplying_or_crossing_a_goal_generation() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    assert!(work::control_identified(&rook, &run.id, control("", &run.generation, Action::Pause)).is_err());
    assert!(
        work::control_identified(&rook, &run.id, control("bad id", &run.generation, Action::Pause)).is_err()
    );
    let paused =
        work::control_identified(&rook, &run.id, control("pause-one", &run.generation, Action::Pause))
            .unwrap();
    assert!(!paused.already_applied);
    assert_eq!(paused.run.status, Status::Paused);
    drop(rook);

    let rook = engine(workspace.path(), store.path());
    let resumed =
        work::control_identified(&rook, &run.id, control("resume-one", &run.generation, Action::Resume))
            .unwrap();
    assert_eq!(resumed.run.status, Status::Queued);
    let retry =
        work::control_identified(&rook, &run.id, control("pause-one", &run.generation, Action::Pause))
            .unwrap();
    assert!(retry.already_applied);
    assert_eq!(retry.run.status, Status::Queued, "the old pause must not undo a later resume");
    assert!(
        work::control_identified(&rook, &run.id, control("pause-one", &run.generation, Action::Cancel))
            .is_err()
    );
    let cancelled =
        work::control_identified(&rook, &run.id, control("cancel-one", &run.generation, Action::Cancel))
            .unwrap();
    assert_eq!(cancelled.run.status, Status::Cancelled);
    assert!(
        work::control_identified(&rook, &run.id, control("cancel-one", &run.generation, Action::Cancel))
            .unwrap()
            .already_applied
    );
    assert!(
        work::control_identified(&rook, &run.id, control("cancel-two", &run.generation, Action::Cancel))
            .is_err()
    );

    let session = rook.start_session("new goal").unwrap();
    let old = conversation_goal(&rook, session, "old");
    work::control_identified(&rook, &old.id, control("old-cancel", &old.generation, Action::Cancel)).unwrap();
    let replacement = conversation_goal(&rook, session, "replacement");
    assert_ne!(old.generation, replacement.generation);
    assert!(
        work::control_identified(
            &rook,
            &replacement.id,
            control("old-cancel", &old.generation, Action::Cancel)
        )
        .is_err()
    );
    assert_eq!(work::read(&rook, &replacement.id).unwrap().run.status, Status::Queued);
}

#[test]
fn identified_control_receipts_have_a_bound_without_losing_retry_identity() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::control_identified(&rook, &run.id, control("first", &run.generation, Action::Pause)).unwrap();
    work::update(&rook, &run.id, |saved| {
        saved.controls.extend(
            (1..1024)
                .map(|number| work::ControlReceipt { id: format!("old-{number}"), action: Action::Pause }),
        );
        Ok(())
    })
    .unwrap();
    assert!(
        work::control_identified(&rook, &run.id, control("first", &run.generation, Action::Pause))
            .unwrap()
            .already_applied
    );
    assert!(
        work::control_identified(&rook, &run.id, control("next", &run.generation, Action::Resume)).is_err()
    );
    assert_eq!(work::read(&rook, &run.id).unwrap().run.status, Status::Paused);
}

#[test]
fn continuation_claim_and_control_survive_reopen_without_resuming_a_replacement() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.max_messages = 1;
    let session = rook.start_session("continuation ownership").unwrap();
    let run = conversation_goal(&rook, session, "original goal");
    work::control(&rook, &run.id, Action::Pause).unwrap();
    let options = Default::default();
    let claim = rook_core::chat_submission::claim(
        &rook,
        Some(session),
        "resume-once",
        rook_core::agent::CARRY_ON,
        &options,
    )
    .unwrap();
    let request = || control("resume-once", &run.generation, Action::Resume);
    let resumed = work::resume_with_claim(&rook, &run.id, request(), &claim.key).unwrap();
    assert!(!resumed.already_applied);
    assert_eq!(resumed.run.status, Status::Queued);
    let admitted =
        rook_core::chat_submission::read(&rook, session, "resume-once", rook_core::agent::CARRY_ON, &options)
            .unwrap()
            .unwrap();
    assert_eq!(admitted.status, rook_core::chat_submission::Status::Admitted);
    assert_eq!(admitted.turn.as_deref(), Some(run.generation.as_str()));
    assert!(
        rook_core::chat_submission::claim(
            &rook,
            Some(session),
            "above-cap",
            rook_core::agent::CARRY_ON,
            &options,
        )
        .is_err(),
        "a second caller exceeds the one-claim cap"
    );
    work::control(&rook, &run.id, Action::Pause).unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let retry = work::resume_with_claim(&rook, &run.id, request(), &claim.key).unwrap();
    assert!(retry.already_applied);
    assert_eq!(retry.run.status, Status::Paused);
    assert_eq!(work::read(&rook, &run.id).unwrap().controls.len(), 1);
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    let replacement = conversation_goal(&rook, session, "replacement goal");
    work::control(&rook, &replacement.id, Action::Pause).unwrap();
    assert!(work::resume_with_claim(&rook, &replacement.id, request(), &claim.key).is_err());
    // Even substituting the current generation cannot reuse the admitted old
    // prompt: the claim's immutable owner and control receipt must agree.
    assert!(
        work::resume_with_claim(
            &rook,
            &replacement.id,
            control("resume-once", &replacement.generation, Action::Resume),
            &claim.key,
        )
        .is_err()
    );
    let untouched = work::read(&rook, &replacement.id).unwrap();
    assert_eq!(untouched.run.status, Status::Paused);
    assert!(untouched.controls.is_empty());
}

#[test]
fn refused_continuation_claim_leaves_the_goal_and_control_receipts_unchanged() {
    for invalid in ["caller", "oversize", "budget", "cap"] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let session = rook.start_session("refused continuation").unwrap();
        let run = conversation_goal(&rook, session, "goal");
        work::control(&rook, &run.id, Action::Pause).unwrap();
        let claim = rook_core::chat_submission::claim(
            &rook,
            Some(session),
            "resume-once",
            rook_core::agent::CARRY_ON,
            &Default::default(),
        )
        .unwrap();
        if invalid == "oversize" {
            // The record bound is 75 bytes; this fixture exceeds it before a
            // value is copied or the control can be committed.
            assert_eq!(rook.store.kv_get(&claim.key).unwrap().unwrap().len(), 75);
            rook.store.kv_set(&claim.key, &[0; 76]).unwrap();
        } else if invalid == "budget" {
            work::update(&rook, &run.id, |saved| {
                saved.run.status = Status::Limited;
                Ok(())
            })
            .unwrap();
        } else if invalid == "cap" {
            work::update(&rook, &run.id, |saved| {
                saved.controls = (0..1024)
                    .map(|i| work::ControlReceipt { id: format!("used-{i}"), action: Action::Pause })
                    .collect();
                Ok(())
            })
            .unwrap();
        }
        let before = rook.store.kv_get(&format!("work/managed/{}", run.id)).unwrap().unwrap();
        let claim_before = rook.store.kv_get(&claim.key).unwrap().unwrap();
        let caller = if invalid == "caller" { "another-caller" } else { "resume-once" };
        let error = work::resume_with_claim(
            &rook,
            &run.id,
            control(caller, &run.generation, Action::Resume),
            &claim.key,
        )
        .unwrap_err();
        if invalid == "oversize" {
            assert!(error.to_string().contains("exceeds 75 bytes"), "{error}");
        }
        assert_eq!(rook.store.kv_get(&format!("work/managed/{}", run.id)).unwrap().unwrap(), before);
        assert_eq!(rook.store.kv_get(&claim.key).unwrap().unwrap(), claim_before);
    }
}

#[test]
fn steering_is_durable_idempotent_bounded_and_acknowledged_only_on_delivery() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.max_messages = 1;
    rook.config.work.max_message_bytes = 12;
    rook.config.work.max_runs = 1;
    let run = start(&rook);
    assert!(
        work::start(
            &rook,
            Start {
                conversation: None,
                goal: "duplicate".into(),
                workspace: None,
                autonomous: false,
                max_iterations: None,
                max_tokens: None,
                max_seconds: None
            }
        )
        .is_err()
    );
    let receipt = work::steer(&rook, &run.id, correction("one", "use Russian")).unwrap();
    assert!(receipt.applied_at.is_none());
    assert_eq!(work::pending(&rook, &run.identity()).unwrap().len(), 1);
    assert_eq!(work::steer(&rook, &run.id, correction("one", "use Russian")).unwrap().id, "one");
    assert!(work::steer(&rook, &run.id, correction("one", "different")).is_err());
    assert!(work::steer(&rook, &run.id, correction("two", "another")).is_err());
    assert!(work::steer(&rook, &run.id, correction("long", "x".repeat(13).as_str())).is_err());
    work::control(&rook, &run.id, Action::Pause).unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert_eq!(work::read(&rook, &run.id).unwrap().run.status, Status::Paused);
    let session = rook.start_session("receipt").unwrap();
    let incoming = work::pending(&rook, &run.identity()).unwrap();
    assert!(
        work::accept(&rook, &run.identity(), session, &incoming[0]).unwrap().is_none(),
        "pause keeps messages queued"
    );
    work::control(&rook, &run.id, Action::Resume).unwrap();
    assert!(work::accept(&rook, &run.identity(), session, &incoming[0]).unwrap().is_some());
    assert!(work::pending(&rook, &run.identity()).unwrap().is_empty());
    assert!(rook.goal(session).unwrap().unwrap().contains("use Russian"));
    assert_eq!(
        work::read(&rook, &run.id).unwrap().run.instructions[0].session.as_deref(),
        Some(rook_store::format_session_id(session).as_str())
    );
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    work::forget(&rook, &run.id).unwrap();
    assert!(work::list(&rook).unwrap().is_empty());
    assert!(rook.store.kv_get(&format!("work/managed/{}", run.id)).unwrap().is_none());
}

#[test]
fn editing_or_withdrawing_a_queued_goal_command_does_not_change_the_goal_until_acceptance() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("goal edits").unwrap();
    let id = rook_store::format_session_id(session);
    rook_core::message_queue::submit(&rook, session, correction("before", "prior ordinary guidance"))
        .unwrap();
    let run = work::start(
        &rook,
        Start {
            conversation: Some(rook_proto::work::Conversation {
                session: id,
                model: None,
                effort: "high".into(),
                stance: "assist".into(),
                options: Default::default(),
            }),
            goal: "original goal".into(),
            workspace: None,
            autonomous: false,
            max_iterations: None,
            max_tokens: None,
            max_seconds: None,
        },
    )
    .unwrap();
    assert!(rook_core::message_queue::submit(&rook, session, correction("late", "belongs to goal")).is_err());
    work::steer(&rook, &run.id, correction("withdrawn", "/goal unwanted goal")).unwrap();
    work::withdraw_instruction(&rook, &run.id, "withdrawn", WithdrawInstruction { revision: 0 }).unwrap();
    assert_eq!(work::read(&rook, &run.id).unwrap().run.goal, "original goal");
    work::steer(&rook, &run.id, correction("edited", "/goal first draft")).unwrap();
    work::edit_instruction(
        &rook,
        &run.id,
        "edited",
        EditInstruction { revision: 0, text: "/goal final goal".into() },
    )
    .unwrap();
    assert_eq!(work::read(&rook, &run.id).unwrap().run.goal, "original goal");
    assert_eq!(rook.goal(session).unwrap().as_deref(), Some("original goal"));
    work::accept(&rook, &run.identity(), session, "edited").unwrap();
    assert_eq!(work::read(&rook, &run.id).unwrap().run.goal, "final goal");
    assert!(rook.goal(session).unwrap().unwrap().starts_with("final goal"));
}

#[tokio::test]
async fn an_ordinary_turn_admits_durable_steering_without_becoming_a_goal() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("ordinary queue").unwrap();
    rook_core::message_queue::submit(&rook, session, correction("one", "ORIGINAL_GUIDANCE")).unwrap();
    rook_core::message_queue::edit(
        &rook,
        session,
        "one",
        EditInstruction { revision: 0, text: "EDITED_GUIDANCE".into() },
    )
    .unwrap();
    rook_core::message_queue::submit(&rook, session, correction("two", "WITHDRAWN_GUIDANCE")).unwrap();
    rook_core::message_queue::withdraw(&rook, session, "two", WithdrawInstruction { revision: 0 }).unwrap();
    let provider = Script::new(vec![answer("Done")]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    agent.run("Proceed").await.unwrap();
    let seen = provider.seen.lock().unwrap().join("\n");
    assert!(seen.contains("EDITED_GUIDANCE"));
    assert!(!seen.contains("ORIGINAL_GUIDANCE"));
    assert!(!seen.contains("WITHDRAWN_GUIDANCE"));
    assert!(work::for_session(&rook, session).unwrap().is_none());
    assert!(!rook_core::message_queue::list(&rook, session).unwrap()[0].queued());
}

#[tokio::test]
async fn promotion_attaches_the_existing_turn_to_goal_steering_and_pause() {
    for paused in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let session = rook.start_session("promoted").unwrap();
        rook_core::message_queue::submit(&rook, session, correction("before", "PRE_PROMOTION_GUIDANCE"))
            .unwrap();
        let provider = Script::new(vec![answer("More work remains")]);
        let mut agent = AgentLoop::new(&rook, provider.clone(), session);
        assert!(agent.managed_work.is_none());
        let run = work::start(
            &rook,
            Start {
                conversation: Some(rook_proto::work::Conversation {
                    session: rook_store::format_session_id(session),
                    model: None,
                    effort: "high".into(),
                    stance: "assist".into(),
                    options: Default::default(),
                }),
                goal: "inspect the project".into(),
                workspace: None,
                autonomous: false,
                max_iterations: None,
                max_tokens: None,
                max_seconds: None,
            },
        )
        .unwrap();
        work::steer(&rook, &run.id, correction("after", "POST_PROMOTION_GUIDANCE")).unwrap();
        if paused {
            work::control(&rook, &run.id, Action::Pause).unwrap();
        }
        let outcome = agent.run("Proceed").await.unwrap();
        if paused {
            assert_eq!(outcome.stopped, "work_paused");
            assert!(provider.seen.lock().unwrap().is_empty(), "a promoted paused turn makes no request");
        } else {
            let seen = provider.seen.lock().unwrap().join("\n");
            assert!(seen.contains("PRE_PROMOTION_GUIDANCE"));
            assert!(seen.contains("POST_PROMOTION_GUIDANCE"));
        }
        assert_eq!(rook_core::message_queue::list(&rook, session).unwrap()[0].queued(), paused);
        assert_eq!(work::read(&rook, &run.id).unwrap().run.instructions[0].queued(), paused);
    }
}

fn conversation_goal(rook: &Rook, session: u128, goal: &str) -> rook_proto::work::Run {
    work::start(
        rook,
        Start {
            conversation: Some(rook_proto::work::Conversation {
                session: rook_store::format_session_id(session),
                model: None,
                effort: "high".into(),
                stance: "assist".into(),
                options: Default::default(),
            }),
            goal: goal.into(),
            workspace: None,
            autonomous: false,
            max_iterations: None,
            max_tokens: None,
            max_seconds: None,
        },
    )
    .unwrap()
}

#[test]
fn a_replaced_goal_cannot_lend_its_receipts_to_an_old_consumer_even_when_ids_match() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("generation").unwrap();
    let old = conversation_goal(&rook, session, "old goal");
    work::steer(&rook, &old.id, correction("same", "old text")).unwrap();
    let observed = work::pending(&rook, &old.identity()).unwrap();
    work::control(&rook, &old.id, Action::Cancel).unwrap();
    let replacement = conversation_goal(&rook, session, "replacement goal");
    assert_eq!(old.id, replacement.id);
    assert_ne!(old.identity(), replacement.identity());
    work::steer(&rook, &replacement.id, correction("same", "/goal revised replacement")).unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert!(work::should_stop(&rook, &old.identity()).unwrap());
    assert!(work::pending(&rook, &old.identity()).unwrap().is_empty());
    assert!(work::accept(&rook, &old.identity(), session, &observed[0]).unwrap().is_none());
    assert_eq!(rook.goal(session).unwrap().as_deref(), Some("replacement goal"));
    assert!(work::read(&rook, &replacement.id).unwrap().run.instructions[0].queued());
    let other = rook.start_session("unrelated").unwrap();
    assert!(work::accept(&rook, &replacement.identity(), other, "same").is_err());
    assert!(rook.store.events(other, 0, 10).unwrap().is_empty());
    let accepted = work::accept(&rook, &replacement.identity(), session, "same").unwrap().unwrap();
    assert_eq!(accepted.receipt.reference, format!("goal.{}.same", replacement.generation));
    assert_eq!(work::read(&rook, &replacement.id).unwrap().run.goal, "revised replacement");
    work::control(&rook, &replacement.id, Action::Cancel).unwrap();
    work::forget(&rook, &replacement.id).unwrap();
    assert!(work::should_stop(&rook, &replacement.identity()).unwrap());
    assert!(work::pending(&rook, &replacement.identity()).unwrap().is_empty());
    assert!(work::accept(&rook, &replacement.identity(), session, "same").unwrap().is_none());
}

#[tokio::test]
async fn replacing_a_promoted_goal_during_acceptance_stops_its_old_loop_before_another_request() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("promoted replacement").unwrap();
    let provider = Script::new(vec![answer("must not ask")]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    let old = conversation_goal(&rook, session, "old goal");
    work::steer(&rook, &old.id, correction("first", "first old message")).unwrap();
    work::steer(&rook, &old.id, correction("same", "second old message")).unwrap();
    let mut replacement = None;
    let mut heard = Vec::new();
    let outcome = agent
        .run_with("Proceed", |progress| {
            if let rook_core::agent::Progress::Heard { text, receipt } = progress {
                heard.push(text.to_owned());
                let receipt = receipt.unwrap();
                assert_eq!(receipt.reference, format!("goal.{}.first", old.generation));
                work::control(&rook, &old.id, Action::Cancel).unwrap();
                let next = conversation_goal(&rook, session, "replacement goal");
                work::steer(&rook, &next.id, correction("same", "NEW_GENERATION_TEXT")).unwrap();
                replacement = Some(next);
            }
        })
        .await
        .unwrap();
    let next = replacement.expect("replacement must happen between observed receipts");
    assert_eq!(agent.managed_work, Some(old.identity()), "an attached loop never adopts a new generation");
    assert_eq!(outcome.stopped, "work_paused");
    assert_eq!(heard.len(), 1);
    assert!(provider.seen.lock().unwrap().is_empty());
    assert!(work::read(&rook, &next.id).unwrap().run.instructions[0].queued());
    assert!(
        !rook
            .transcript(session, 0, 100, 8192)
            .unwrap()
            .iter()
            .any(|e| e.body.contains("NEW_GENERATION_TEXT"))
    );

    let mut replacement_agent = AgentLoop::new(&rook, Script::new(vec![answer("done")]), session);
    replacement_agent.run("Continue the replacement").await.unwrap();
    assert_eq!(replacement_agent.managed_work, Some(next.identity()));
    assert!(!work::read(&rook, &next.id).unwrap().run.instructions[0].queued());
}

#[tokio::test]
async fn replacement_after_context_preparation_cannot_start_an_old_generations_model_request() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("replacement at request boundary").unwrap();
    let old = conversation_goal(&rook, session, "original goal");
    let provider = Script::new(vec![answer("must not ask")]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    let mut replaced = false;
    let outcome = agent
        .run_with("Proceed", |progress| {
            if matches!(progress, rook_core::agent::Progress::Context { .. }) && !replaced {
                work::control(&rook, &old.id, Action::Cancel).unwrap();
                conversation_goal(&rook, session, "replacement goal");
                replaced = true;
            }
        })
        .await
        .unwrap();
    assert!(replaced, "the test must reach the last preparation boundary");
    assert_eq!(outcome.stopped, "work_paused");
    assert!(provider.seen.lock().unwrap().is_empty());
}

#[test]
fn a_failed_transcript_append_leaves_the_instruction_queued_for_retry() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::steer(&rook, &run.id, correction("retry", "keep this correction")).unwrap();
    let missing = rook_store::new_session_id();
    assert!(work::accept(&rook, &run.identity(), missing, "retry").is_err());
    assert_eq!(work::pending(&rook, &run.identity()).unwrap(), ["retry"]);
    assert!(rook.goal(missing).unwrap().is_none());
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("recovered").unwrap();
    assert!(work::accept(&rook, &run.identity(), session, "retry").unwrap().is_some());
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert!(work::pending(&rook, &run.identity()).unwrap().is_empty());
    assert!(work::accept(&rook, &run.identity(), session, "retry").unwrap().is_none());
    let messages: Vec<_> = rook
        .store
        .events(session, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.record.kind == rook_store::EventKind::UserMessage)
        .collect();
    assert_eq!(messages.len(), 1, "retry after restart never appends the correction twice");
    assert!(rook.goal(session).unwrap().unwrap().contains("keep this correction"));
}

#[test]
fn concurrent_acceptance_publishes_one_message_and_excludes_unaccepted_text_from_the_goal() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::steer(&rook, &run.id, correction("one", "ACCEPTED_TEXT")).unwrap();
    work::steer(&rook, &run.id, correction("two", "STILL_QUEUED_TEXT")).unwrap();
    let session = rook.start_session("race").unwrap();
    let barrier = std::sync::Barrier::new(8);
    let accepted = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    work::accept(&rook, &run.identity(), session, "one").unwrap().is_some()
                })
            })
            .collect();
        handles.into_iter().map(|handle| usize::from(handle.join().unwrap())).sum::<usize>()
    });
    assert_eq!(accepted, 1);
    assert_eq!(work::pending(&rook, &run.identity()).unwrap(), ["two"]);
    let goal = rook.goal(session).unwrap().unwrap();
    assert!(goal.contains("ACCEPTED_TEXT"));
    assert!(!goal.contains("STILL_QUEUED_TEXT"));
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

#[tokio::test]
async fn a_live_message_cannot_acknowledge_a_receipt_by_spelling_its_text_prefix() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::steer(&rook, &run.id, correction("one", "AUTHENTIC_CORRECTION")).unwrap();
    let session = rook.start_session("typed receipts").unwrap();
    let provider = Script::new(vec![answer("Done")]);
    let mut agent = AgentLoop::new(&rook, provider.clone(), session);
    agent.managed_work = Some(run.identity());
    let spelled = "[work instruction one]\nthis is only user text";
    agent.interjections.say(spelled);
    let mut saw_live = false;
    let mut accepted_notice = None;
    agent
        .run_with("Continue", |progress| {
            if let rook_core::agent::Progress::Heard { text, receipt } = progress {
                if text == spelled {
                    saw_live = true;
                    assert!(receipt.is_none(), "user text cannot mint a receipt");
                    assert!(work::read(&rook, &run.id).unwrap().run.instructions[0].applied_at.is_none());
                } else {
                    accepted_notice = receipt.cloned();
                }
            }
        })
        .await
        .unwrap();
    assert!(saw_live);
    let notice = accepted_notice.expect("durable acceptance has typed identity");
    assert_eq!(notice.reference, format!("goal.{}.one", run.generation));
    assert_eq!(notice.session, rook_store::format_session_id(session));
    assert_eq!(notice.status, rook_proto::queue::Status::Accepted);
    assert!(work::read(&rook, &run.id).unwrap().run.instructions[0].applied_at.is_some());
    assert!(provider.seen.lock().unwrap()[0].contains("AUTHENTIC_CORRECTION"));
}

#[test]
fn editing_keeps_submission_identity_and_acceptance_reads_the_latest_revision() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.max_message_bytes = 12;
    let run = start(&rook);
    work::steer(&rook, &run.id, correction("edit", "original")).unwrap();
    let pending = work::pending(&rook, &run.identity()).unwrap();
    assert!(
        work::edit_instruction(&rook, &run.id, "edit", EditInstruction { revision: 0, text: "x".repeat(13) })
            .is_err()
    );
    let edit = EditInstruction { revision: 0, text: "corrected".into() };
    let edited = work::edit_instruction(&rook, &run.id, "edit", edit.clone()).unwrap();
    assert_eq!(edited.revision, 1);
    assert_eq!(work::edit_instruction(&rook, &run.id, "edit", edit.clone()).unwrap().revision, 1);
    assert_eq!(work::steer(&rook, &run.id, correction("edit", "original")).unwrap().text, "corrected");
    assert!(
        work::steer(&rook, &run.id, correction("edit", "corrected")).is_err(),
        "submission identity still names the original request"
    );
    assert!(
        work::edit_instruction(&rook, &run.id, "edit", EditInstruction { revision: 0, text: "stale".into() })
            .is_err()
    );
    let session = rook.start_session("edited").unwrap();
    let text = work::accept(&rook, &run.identity(), session, &pending[0]).unwrap().unwrap();
    assert_eq!(text.receipt.reference, format!("goal.{}.edit", run.generation));
    assert_eq!(text.receipt.revision, 1);
    assert_eq!(text.receipt.status, rook_proto::queue::Status::Accepted);
    assert!(text.text.ends_with("corrected"));
    assert!(!text.text.contains("original"));
    assert!(
        work::edit_instruction(&rook, &run.id, "edit", edit).unwrap().applied_at.is_some(),
        "retrying the acknowledged edit returns its receipt without editing accepted context"
    );
    assert!(
        work::edit_instruction(
            &rook,
            &run.id,
            "edit",
            EditInstruction { revision: 1, text: "too late".into() }
        )
        .is_err()
    );
}

#[test]
fn withdrawing_is_durable_and_a_submission_retry_cannot_requeue_it() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::steer(&rook, &run.id, correction("withdraw", "do this later")).unwrap();
    let observed = work::pending(&rook, &run.identity()).unwrap();
    let receipt =
        work::withdraw_instruction(&rook, &run.id, "withdraw", WithdrawInstruction { revision: 0 }).unwrap();
    assert!(receipt.withdrawn_at.is_some());
    assert!(receipt.applied_at.is_none());
    assert_eq!(receipt.revision, 1);
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert!(work::pending(&rook, &run.identity()).unwrap().is_empty());
    assert!(
        work::steer(&rook, &run.id, correction("withdraw", "do this later")).unwrap().withdrawn_at.is_some()
    );
    let again =
        work::withdraw_instruction(&rook, &run.id, "withdraw", WithdrawInstruction { revision: 0 }).unwrap();
    assert_eq!(again.revision, 1);
    let session = rook.start_session("stale pending snapshot").unwrap();
    assert!(work::accept(&rook, &run.identity(), session, &observed[0]).unwrap().is_none());
    assert!(rook.store.events(session, 0, 10).unwrap().is_empty());
}

#[test]
fn editing_or_withdrawing_races_acceptance_without_mutating_accepted_context() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let session = rook.start_session("queue races").unwrap();
    for n in 0..16 {
        let id = format!("edit-{n}");
        work::steer(&rook, &run.id, correction(&id, "before")).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (edit, accepted) = std::thread::scope(|scope| {
            let edit = scope.spawn(|| {
                barrier.wait();
                work::edit_instruction(
                    &rook,
                    &run.id,
                    &id,
                    EditInstruction { revision: 0, text: "after".into() },
                )
            });
            let accept = scope.spawn(|| {
                barrier.wait();
                work::accept(&rook, &run.identity(), session, &id)
            });
            (edit.join().unwrap(), accept.join().unwrap().unwrap().unwrap())
        });
        assert!(accepted.text.ends_with(if edit.is_ok() { "after" } else { "before" }));
        let receipt =
            work::read(&rook, &run.id).unwrap().run.instructions.into_iter().find(|m| m.id == id).unwrap();
        assert!(accepted.text.ends_with(&receipt.text));
        assert!(receipt.applied_at.is_some());

        let id = format!("withdraw-{n}");
        work::steer(&rook, &run.id, correction(&id, "queued")).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let (withdrawn, accepted) = std::thread::scope(|scope| {
            let withdraw = scope.spawn(|| {
                barrier.wait();
                work::withdraw_instruction(&rook, &run.id, &id, WithdrawInstruction { revision: 0 })
            });
            let accept = scope.spawn(|| {
                barrier.wait();
                work::accept(&rook, &run.identity(), session, &id)
            });
            (withdraw.join().unwrap(), accept.join().unwrap().unwrap())
        });
        assert_ne!(withdrawn.is_ok(), accepted.is_some(), "exactly one side wins for {id}");
        let receipt =
            work::read(&rook, &run.id).unwrap().run.instructions.into_iter().find(|m| m.id == id).unwrap();
        assert_ne!(receipt.withdrawn_at.is_some(), receipt.applied_at.is_some());
    }
}

#[test]
fn older_receipts_acquire_edit_metadata_without_changing_submission_identity() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let legacy = serde_json::from_value(serde_json::json!({
        "id": "legacy", "text": "original", "submitted_at": 1,
        "applied_at": null, "session": null,
    }))
    .unwrap();
    work::update(&rook, &run.id, |saved| {
        saved.run.instructions.push(legacy);
        Ok(())
    })
    .unwrap();
    work::edit_instruction(&rook, &run.id, "legacy", EditInstruction { revision: 0, text: "edited".into() })
        .unwrap();
    assert_eq!(work::steer(&rook, &run.id, correction("legacy", "original")).unwrap().text, "edited");
}

struct Script {
    replies: Mutex<Vec<rook_llm::Result<Response>>>,
    seen: Mutex<Vec<String>>,
}
impl Script {
    fn new(replies: Vec<rook_llm::Result<Response>>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies), seen: Mutex::new(Vec::new()) })
    }
}
#[async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "managed/script"
    }
    fn context_window(&self) -> usize {
        32_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        if request.messages.iter().any(|m| m.content.starts_with("Classify whether")) {
            return answer(r#"{"action":"finish"}"#);
        }
        self.seen
            .lock()
            .unwrap()
            .push(request.messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n"));
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            return Err(rook_llm::LlmError::Other("script exhausted".into()));
        }
        replies.remove(0)
    }
}
fn answer(text: &str) -> rook_llm::Result<Response> {
    Ok(Response {
        message: Message::assistant(text),
        stop_reason: StopReason::EndTurn,
        usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
        model: "managed/script".into(),
    })
}
fn read() -> rook_llm::Result<Response> {
    let mut response = answer("").unwrap();
    response.message.tool_calls.push(ToolCall {
        id: "read".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"evidence.txt"}),
    });
    response.stop_reason = StopReason::ToolUse;
    Ok(response)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_outage_resumes_the_saved_session_and_a_correction_reaches_verification() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider =
        Script::new(vec![read(), Err(rook_llm::LlmError::Other("provider temporarily unavailable".into()))]);
    let first =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(first.status, Status::RetryWait);
    assert_eq!(first.iterations, 0, "outages are not idle turns");
    assert!(first.tokens > 0, "work before the failure is billed");
    let session = first.session.clone();
    work::steer(&rook, &run.id, correction("answer-language", "Include the file name")).unwrap();
    work::update(&rook, &run.id, |s| {
        s.run.next_attempt_at = Some(0);
        Ok(())
    })
    .unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let provider = Script::new(vec![
        answer("evidence.txt contains done."),
        read(),
        answer("Verified contents and answer.\nVERDICT: holds"),
    ]);
    let result =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(result.session, session);
    assert!(result.tokens > first.tokens);
    assert!(result.instructions[0].applied_at.is_some());
    let seen = provider.seen.lock().unwrap();
    assert!(seen[0].contains("Include the file name"));
    assert!(seen.iter().any(|prompt| prompt.contains("the agent has just finished a turn")
        && prompt.contains("Include the file name")));
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_turns_continue_and_total_budgets_survive_days_and_resume() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::update(&rook, &run.id, |s| {
        s.run.created_at = work::now() - 3 * 86400;
        s.run.max_iterations = 3;
        Ok(())
    })
    .unwrap();
    for count in 1..=3 {
        let provider = Script::new(vec![read(), answer("More work remains")]);
        let result = work::advance(
            &rook,
            &run.id,
            |session| {
                let mut agent = AgentLoop::new(&rook, provider.clone(), session);
                agent.max_steps = 1;
                Ok(agent)
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.iterations, count);
        assert_eq!(result.status, if count == 3 { Status::Limited } else { Status::Queued }, "{result:?}");
    }
    assert!(work::control(&rook, &run.id, Action::Resume).is_err());
    let bill = work::read(&rook, &run.id).unwrap().run.tokens;
    assert!(bill > 0);
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert_eq!(work::read(&rook, &run.id).unwrap().run.tokens, bill);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_correction_received_during_verification_prevents_false_completion() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider = Script::new(vec![answer("done"), read(), answer("VERDICT: holds")]);
    let mut corrected = false;
    let result = work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
        |event| {
            if !corrected && matches!(event, rook_core::agent::Progress::Delegating { .. }) {
                work::steer(&rook, &run.id, correction("late", "Also explain the evidence")).unwrap();
                corrected = true;
            }
        },
    )
    .await
    .unwrap();
    assert!(corrected, "the correction must really arrive during verification");
    assert_eq!(result.status, Status::Queued, "{result:?}");
    assert!(result.instructions[0].applied_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn exceeding_time_or_tokens_stops_before_another_provider_call() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::update(&rook, &run.id, |s| {
        s.run.created_at = work::now() - 8 * 86400;
        Ok(())
    })
    .unwrap();
    let result =
        work::advance(&rook, &run.id, |_| panic!("no model call after seven days"), |_| {}).await.unwrap();
    assert_eq!(result.status, Status::Limited);
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    let run = start(&rook);
    work::update(&rook, &run.id, |s| {
        s.run.max_tokens = 10;
        s.run.tokens = 10;
        Ok(())
    })
    .unwrap();
    let result =
        work::advance(&rook, &run.id, |_| panic!("no model call over token budget"), |_| {}).await.unwrap();
    assert_eq!(result.status, Status::Limited);
}

struct Held {
    script: Arc<Script>,
    first: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl Provider for Held {
    fn id(&self) -> &str {
        "managed/held"
    }
    fn context_window(&self) -> usize {
        32_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.script.complete(request).await
    }
}
fn held(script: Vec<rook_llm::Result<Response>>) -> Arc<Held> {
    Arc::new(Held {
        script: Script::new(script),
        first: true.into(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_sent_during_a_model_request_is_acknowledged_at_the_next_boundary() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider =
        held(vec![read(), answer("evidence.txt contains done."), read(), answer("VERDICT: holds")]);
    let running =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {});
    let steering = async {
        provider.entered.notified().await;
        let receipt = work::steer(&rook, &run.id, correction("during", "Include the filename")).unwrap();
        assert!(receipt.applied_at.is_none(), "receiving is not delivering");
        provider.release.notify_one();
    };
    let (result, ()) = tokio::join!(running, steering);
    let result = result.unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert!(result.instructions[0].applied_at.is_some());
    let seen = provider.script.seen.lock().unwrap();
    assert!(!seen[0].contains("Include the filename"));
    assert!(seen[1].contains("Include the filename"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_cancel_wait_for_the_current_request_without_starting_another_turn() {
    for action in [Action::Pause, Action::Cancel] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        let provider = held(vec![Err(rook_llm::LlmError::Other("provider unavailable".into()))]);
        let running = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        );
        let controlling = async {
            provider.entered.notified().await;
            work::control(&rook, &run.id, action).unwrap();
            provider.release.notify_one();
        };
        let (result, ()) = tokio::join!(running, controlling);
        let result = result.unwrap();
        assert_eq!(
            result.status,
            match action {
                Action::Pause => Status::Paused,
                _ => Status::Cancelled,
            }
        );
        if matches!(action, Action::Cancel) {
            assert!(work::read(&rook, &run.id).unwrap().active.is_none());
            work::forget(&rook, &run.id).unwrap();
        } else {
            let again =
                work::advance(&rook, &run.id, |_| panic!("paused runs cannot start requests"), |_| {})
                    .await
                    .unwrap();
            assert_eq!(again.status, Status::Paused);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_finished_paused_stage_releases_its_context_for_a_replacement_goal() {
    for identified in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let session = rook.start_session("paused goal cancellation").unwrap();
        let run = conversation_goal(&rook, session, "old goal");
        let provider = held(vec![answer("partial reply")]);
        let running = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        );
        let pause = async {
            provider.entered.notified().await;
            work::control(&rook, &run.id, Action::Pause).unwrap();
            assert!(work::read(&rook, &run.id).unwrap().active.is_some(), "an owned request is retained");
            provider.release.notify_one();
        };
        let (result, ()) = tokio::join!(running, pause);
        assert_eq!(result.unwrap().status, Status::Paused);
        let paused = work::read(&rook, &run.id).unwrap();
        assert!(paused.active.is_some(), "paused context remains available for resume");
        if identified {
            work::control_identified(
                &rook,
                &run.id,
                control("cancel-paused", &run.generation, Action::Cancel),
            )
            .unwrap();
        } else {
            work::control(&rook, &run.id, Action::Cancel).unwrap();
        }
        assert!(
            work::read(&rook, &run.id).unwrap().active.is_none(),
            "cancel retires an unowned paused context"
        );
        assert!(!rook.store.get_session(session).unwrap().unwrap().tags.iter().any(|tag| tag == "rook:work"));
        // A record produced by an earlier runner can still contain that context.
        work::update(&rook, &run.id, |saved| {
            saved.active = paused.active;
            Ok(())
        })
        .unwrap();
        assert!(work::read(&rook, &run.id).unwrap().active.is_some());
        drop(rook);
        let rook = engine(workspace.path(), store.path());
        let replacement = conversation_goal(&rook, session, "replacement goal");
        assert_ne!(replacement.generation, run.generation);
        assert!(work::read(&rook, &replacement.id).unwrap().active.is_none());
        assert_eq!(rook.goal(session).unwrap().as_deref(), Some("replacement goal"));
        assert!(
            work::control_identified(
                &rook,
                &replacement.id,
                control("old-stop", &run.generation, Action::Pause)
            )
            .is_err()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_before_execution_keeps_the_owned_stage_until_its_future_returns() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("before execution").unwrap();
    let run = conversation_goal(&rook, session, "old goal");
    let provider = Script::new(vec![answer("must not be requested")]);
    let result = work::advance(
        &rook,
        &run.id,
        |session| {
            assert!(rook.execution(session).unwrap().iter().all(|execution| execution.status != "running"));
            assert!(work::read(&rook, &run.id).unwrap().active.is_some());
            work::control(&rook, &run.id, Action::Cancel).unwrap();
            assert!(
                work::read(&rook, &run.id).unwrap().active.is_some(),
                "no execution receipt does not mean no supervisor owner"
            );
            assert!(
                work::start(
                    &rook,
                    Start {
                        goal: "replacement".into(),
                        workspace: None,
                        autonomous: false,
                        max_iterations: None,
                        max_tokens: None,
                        max_seconds: None,
                        conversation: run.conversation.clone(),
                    }
                )
                .is_err(),
                "replacement cannot overtake the suspended old stage"
            );
            Ok(AgentLoop::new(&rook, provider.clone(), session))
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Cancelled);
    assert!(provider.seen.lock().unwrap().is_empty());
    assert!(work::read(&rook, &run.id).unwrap().active.is_none());
    let replacement = conversation_goal(&rook, session, "replacement");
    assert_ne!(replacement.generation, run.generation);
}

#[tokio::test(flavor = "multi_thread")]
async fn stage_ownership_is_bounded_and_released_when_the_future_finishes() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.max_parallel_runs = 1;
    let first = conversation_goal(&rook, rook.start_session("first").unwrap(), "first");
    let second = conversation_goal(&rook, rook.start_session("second").unwrap(), "second");
    let provider = held(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
    let running = work::advance(
        &rook,
        &first.id,
        |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
        |_| {},
    );
    let competing = async {
        provider.entered.notified().await;
        assert_eq!(rook.config.work.max_parallel_runs, 1);
        assert_eq!(
            work::read(&rook, &first.id).unwrap().run.status,
            Status::Running,
            "the only stage slot is held"
        );
        let denied = work::advance(
            &rook,
            &second.id,
            |_| panic!("a stage above the cap cannot construct an agent"),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(denied.to_string().contains("stage limit"), "{denied}");
        let duplicate = work::advance(
            &rook,
            &first.id,
            |_| panic!("an already owned stage cannot construct another agent"),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(duplicate.to_string().contains("already running"), "{duplicate}");
        provider.release.notify_one();
    };
    let (result, ()) = tokio::join!(running, competing);
    assert_eq!(result.unwrap().status, Status::RetryWait);
    let provider = Script::new(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
    let next = work::advance(
        &rook,
        &second.id,
        |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(next.status, Status::RetryWait);
    assert_eq!(provider.seen.lock().unwrap().len(), 1, "the released slot is reusable");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_effect_blocks_the_scheduler_before_it_can_retry() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let session = rook.start_session("unknown effect").unwrap();
    let named = rook_store::format_session_id(session);
    work::update(&rook, &run.id, |s| {
        s.active = Some(work::Active {
            start_seq: 0,
            session: named.clone(),
            before: Default::default(),
            answer: None,
            accounted_tokens: 0,
        });
        s.run.session = Some(named.clone());
        Ok(())
    })
    .unwrap();
    let receipt = serde_json::json!({
        "version":1,"session":named,"turn":"old-turn","owner":"dead-process","pid":0,
        "started_at":0,"updated_at":0,"status":"interrupted","task":"write one file","start_seq":0,
        "completed_operations":0,"last_result_seq":null,"background":[],"pending":null,
        "unknown":[{"session":named,"id":"operation-one","tool":"write_file","arguments":"{}",
            "started_at":0,"call_seq":0,"may_have_effects":true,"job":null,"registry":null}]
    });
    rook.store.kv_set(&format!("execution/{session:032x}"), &serde_json::to_vec(&receipt).unwrap()).unwrap();
    let result = work::advance(
        &rook,
        &run.id,
        |_| panic!("must inspect unknown effects before using the model"),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Blocked);
    assert!(result.reason.contains("unknown"), "{}", result.reason);
    let cancelled = work::control(&rook, &run.id, Action::Cancel).unwrap();
    assert_eq!(cancelled.status, Status::Cancelled);
    assert!(work::read(&rook, &run.id).unwrap().active.is_some());
    let replacement = work::start(
        &rook,
        Start {
            conversation: None,
            goal: "replacement must not hide an unknown write".into(),
            workspace: None,
            autonomous: true,
            max_iterations: None,
            max_tokens: None,
            max_seconds: None,
        },
    );
    assert!(replacement.is_err());
    assert!(work::read(&rook, &run.id).unwrap().active.is_some());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &rook.store.kv_get(&format!("execution/{session:032x}")).unwrap().unwrap()
        )
        .unwrap()["unknown"][0]["id"],
        "operation-one"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verification_outage_retries_the_check_without_losing_the_completed_turn() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider = Script::new(vec![
        answer("evidence.txt contains done."),
        Err(rook_llm::LlmError::Other("provider temporarily unavailable".into())),
    ]);
    let first =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(first.status, Status::RetryWait);
    assert_eq!(first.iterations, 0);
    assert!(work::read(&rook, &run.id).unwrap().active.unwrap().answer.is_some());
    work::update(&rook, &run.id, |s| {
        s.run.next_attempt_at = Some(0);
        Ok(())
    })
    .unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let provider = Script::new(vec![read(), answer("VERDICT: holds")]);
    let result =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(result.reply, "evidence.txt contains done.");
    assert_eq!(result.session, first.session);
    assert!(provider.seen.lock().unwrap()[0].contains("the agent has just finished a turn"));
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_back_off_until_the_outage_window_requires_attention() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.retry_initial_secs = 9;
    rook.config.work.retry_max_secs = 12;
    let run = start(&rook);
    for attempt in 1..=3 {
        let provider = Script::new(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
        let result = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.consecutive_failures, attempt);
        if attempt == 3 {
            assert_eq!(result.status, Status::Blocked);
        } else {
            assert_eq!(result.status, Status::RetryWait);
            let delay = result.next_attempt_at.unwrap().saturating_sub(result.updated_at);
            assert_eq!(delay, if attempt == 1 { 9 } else { 12 });
            work::update(&rook, &run.id, |s| {
                s.run.next_attempt_at = Some(0);
                if attempt == 2 {
                    s.failed_since = Some(work::now() - 86401);
                }
                Ok(())
            })
            .unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pausing_between_tools_keeps_the_remaining_batch_from_running() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let mut response = answer("").unwrap();
    response.stop_reason = StopReason::ToolUse;
    for path in ["first.txt", "second.txt"] {
        response.message.tool_calls.push(ToolCall {
            id: path.into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":path,"content":"effect"}),
        });
    }
    let provider = Script::new(vec![Ok(response)]);
    let result = work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
        |event| {
            if matches!(event, rook_core::agent::Progress::ToolDone { .. }) {
                work::control(&rook, &run.id, Action::Pause).unwrap();
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Paused);
    assert_eq!(std::fs::read_to_string(workspace.path().join("first.txt")).unwrap(), "effect");
    assert!(
        !workspace.path().join("second.txt").exists(),
        "the remaining operation was never authorized to continue after pause"
    );
    work::control(&rook, &run.id, Action::Resume).unwrap();
    let resumed = Script::new(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
    let continued =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, resumed.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(continued.status, Status::RetryWait);
    assert_eq!(continued.session, result.session, "pause preserves the partial turn's context");
    assert!(resumed.seen.lock().unwrap()[0].contains("first.txt"));
    assert!(!workspace.path().join("second.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_continues_the_existing_conversation_across_limits_and_restart() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let mut rook = engine(workspace.path(), store.path());
    let session = rook.start_session("existing conversation").unwrap();
    let named = rook_store::format_session_id(session);
    let provider = Script::new(vec![answer("Earlier discussion saved.")]);
    AgentLoop::new(&rook, provider, session).run("ORIGINAL_REQUIREMENT: preserve the API").await.unwrap();
    rook.config.work.max_iterations = 1;
    rook.config.work.max_tokens = 1;
    rook.config.work.max_seconds = 1;
    let run = work::start(
        &rook,
        Start {
            conversation: Some(rook_proto::work::Conversation {
                session: named.clone(),
                model: None,
                effort: "high".into(),
                stance: "assist".into(),
                options: Default::default(),
            }),
            goal: "Inspect evidence.txt".into(),
            workspace: None,
            autonomous: false,
            max_iterations: Some(0),
            max_tokens: Some(0),
            max_seconds: Some(0),
        },
    )
    .unwrap();
    assert_eq!(run.id, named, "the goal is this session, not a new user-visible task");
    work::update(&rook, &run.id, |s| {
        s.run.created_at = work::now() - 9 * 86400;
        Ok(())
    })
    .unwrap();
    let provider = Script::new(vec![read(), answer("More work remains")]);
    let first = work::advance(
        &rook,
        &run.id,
        |id| {
            assert_eq!(id, session);
            let mut agent = AgentLoop::new(&rook, provider.clone(), id);
            agent.max_steps = 1;
            Ok(agent)
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(first.recent[0].stopped, "max_steps", "the stage really reaches its bound");
    assert_eq!(first.tokens, 30, "earlier chat tokens are not billed again");
    assert_eq!(first.status, Status::Queued);
    assert!(
        rook.store.get_session(session).unwrap().unwrap().tags.iter().any(|tag| tag == "rook:work"),
        "retention must keep the conversation between stages"
    );
    assert!(provider.seen.lock().unwrap()[0].contains("ORIGINAL_REQUIREMENT"));
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    work::steer(&rook, &named, correction("new-scope", "/goal Report the file name and contents")).unwrap();
    let provider = Script::new(vec![
        answer("evidence.txt contains done."),
        read(),
        answer("Checked evidence.\nVERDICT: holds"),
    ]);
    let result = work::advance(
        &rook,
        &named,
        |id| {
            assert_eq!(id, session);
            Ok(AgentLoop::new(&rook, provider.clone(), id))
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(result.session.as_deref(), Some(named.as_str()));
    assert_eq!(result.iterations, 2, "a previous completed stage must not be replayed");
    assert_eq!(result.goal, "Report the file name and contents");
    assert!(
        !rook.store.get_session(session).unwrap().unwrap().tags.iter().any(|tag| tag == "rook:work"),
        "completed goals no longer pin their transcript"
    );
    assert!(result.instructions[0].applied_at.is_some());
    assert!(provider.seen.lock().unwrap()[0].contains("ORIGINAL_REQUIREMENT"));
    assert!(provider.seen.lock().unwrap().iter().any(|p| p.contains("More work remains")));
}
