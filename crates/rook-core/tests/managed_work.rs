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
    completion_replies: Mutex<Option<Vec<rook_llm::Result<Response>>>>,
    completion_requests: Mutex<Vec<Request>>,
}
impl Script {
    fn new(replies: Vec<rook_llm::Result<Response>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies),
            seen: Mutex::new(Vec::new()),
            completion_replies: Mutex::new(None),
            completion_requests: Mutex::new(Vec::new()),
        })
    }

    fn with_completion(
        replies: Vec<rook_llm::Result<Response>>,
        checks: Vec<rook_llm::Result<Response>>,
    ) -> Arc<Self> {
        let script = Self::new(replies);
        *script.completion_replies.lock().unwrap() = Some(checks);
        script
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
            self.completion_requests.lock().unwrap().push(request);
            if let Some(replies) = self.completion_replies.lock().unwrap().as_mut() {
                assert!(!replies.is_empty(), "unexpected completion request");
                return replies.remove(0);
            }
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

fn defer(args: serde_json::Value) -> rook_llm::Result<Response> {
    let mut response = answer("").unwrap();
    response.stop_reason = StopReason::ToolUse;
    response.message.tool_calls.push(ToolCall {
        id: "wait".into(),
        name: "defer_work".into(),
        arguments: args,
    });
    Ok(response)
}

fn block(args: serde_json::Value) -> rook_llm::Result<Response> {
    let mut response = answer("").unwrap();
    response.stop_reason = StopReason::ToolUse;
    response.message.tool_calls.push(ToolCall {
        id: "human-prerequisite".into(),
        name: "block_work".into(),
        arguments: args,
    });
    Ok(response)
}

#[tokio::test]
async fn an_explicit_human_blocker_survives_restart_without_a_classifier_or_automatic_work() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let reason = "Please log into the game and test the physical mouse wheel, then explicitly resume.";
    let mut response = block(serde_json::json!({"reason":reason})).unwrap();
    response.message.tool_calls.push(ToolCall {
        id: "must-not-run".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path":"unwanted.txt","content":"not authorized after blocking"}),
    });
    let script = Script::with_completion(vec![Ok(response)], vec![]);
    let first =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, script.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(first.status, Status::Blocked);
    assert_eq!(first.iterations, 1);
    assert_eq!(first.reason, reason);
    assert_eq!(first.reply, reason);
    assert!(first.next_attempt_at.is_none());
    assert!(first.waiting.is_none());
    assert!(!workspace.path().join("unwanted.txt").exists());
    assert_eq!(script.seen.lock().unwrap().len(), 1);
    assert!(script.completion_requests.lock().unwrap().is_empty());
    let session = rook_store::parse_session_id(first.session.as_ref().unwrap()).unwrap();
    let outcome = rook.completed_turn(session).unwrap().unwrap();
    assert_eq!(outcome.stopped, "work_blocked");
    assert_eq!(outcome.reply, reason);
    assert_eq!(outcome.tools_called, ["block_work"]);
    assert!(rook.execution(session).unwrap().iter().all(|receipt| receipt.unknown.is_empty()));
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let parked = work::advance(&rook, &run.id, |_| panic!("human wait cannot automatically run"), |_| {})
        .await
        .unwrap();
    assert_eq!(parked.status, Status::Blocked);
    assert_eq!(parked.reply, reason);
    work::control(&rook, &run.id, Action::Resume).unwrap();
    let resumed = Script::with_completion(
        vec![block(serde_json::json!({"reason":"A second prerequisite needs review."}))],
        vec![],
    );
    let next =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, resumed.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(next.status, Status::Blocked);
    assert_eq!(next.iterations, 2);
    assert_eq!(next.session, first.session, "explicit resume retains the original transcript");
    assert!(resumed.seen.lock().unwrap()[0].contains(reason), "the committed blocker is in resumed context");
}

#[tokio::test]
async fn invalid_human_blockers_do_not_park_the_task_or_skip_later_valid_work() {
    for args in [
        serde_json::json!({}),
        serde_json::json!({"reason":" "}),
        serde_json::json!({"reason":"x".repeat(2049)}),
        serde_json::json!({"reason":"a\nb"}),
        serde_json::json!({"reason":"valid","seconds":1}),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        let script = Script::with_completion(
            vec![block(args), block(serde_json::json!({"reason":"Needs a physical input."}))],
            vec![],
        );
        let result = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, script.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.status, Status::Blocked);
        assert_eq!(result.iterations, 1);
        assert_eq!(result.reason, "Needs a physical input.");
        assert_eq!(script.seen.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn a_human_blocker_is_durable_even_if_the_stage_dies_before_recording_its_outcome() {
    use futures_util::FutureExt;
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let script =
        Script::with_completion(vec![block(serde_json::json!({"reason":"Needs a manual login."}))], vec![]);
    let stage = work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, script.clone(), session)),
        |progress| {
            if matches!(
                progress,
                rook_core::agent::Progress::ToolDone { name: "block_work", failed: false, .. }
            ) {
                panic!("fixture loses ownership immediately after the blocker transaction");
            }
        },
    );
    assert!(std::panic::AssertUnwindSafe(stage).catch_unwind().await.is_err());
    let saved = work::read(&rook, &run.id).unwrap();
    assert_eq!(saved.run.status, Status::Blocked);
    assert!(!saved.run.restart_guard.unfinished_stage);
    assert!(saved.active.unwrap().answer.is_none());
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let parked =
        work::advance(&rook, &run.id, |_| panic!("a saved human blocker cannot automatically run"), |_| {})
            .await
            .unwrap();
    assert_eq!(parked.reply, "Needs a manual login.");
    work::control(&rook, &run.id, Action::Resume).unwrap();
    let resumed = Script::with_completion(
        vec![block(serde_json::json!({"reason":"Manual check still pending."}))],
        vec![],
    );
    let next =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, resumed.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(next.session, parked.session);
    assert_eq!(next.iterations, 2);
    assert_eq!(next.reason, "Manual check still pending.");
}

#[tokio::test]
async fn old_json_runs_without_wait_or_control_revision_can_admit_a_deferred_stage() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let key = format!("work/managed/{}", run.id);
    let mut legacy = serde_json::to_value(work::read(&rook, &run.id).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("control_revision");
    legacy["run"].as_object_mut().unwrap().remove("waiting");
    rook.store.kv_set(&key, &serde_json::to_vec(&legacy).unwrap()).unwrap();
    rook.store.flush().unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert!(work::read(&rook, &run.id).unwrap().run.waiting.is_none());
    let script = Script::new(vec![defer(serde_json::json!({"seconds":60,"reason":"я".repeat(256)}))]);
    let after =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, script.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(
        after.waiting.unwrap().reason.len(),
        512,
        "the admitted Unicode reason must actually reach the byte cap"
    );
    assert_eq!(after.iterations, 1);
}

struct HeldWaitApproval {
    entered: tokio::sync::Notify,
}
#[async_trait]
impl rook_tools::policy::Approver for HeldWaitApproval {
    async fn ask(
        &self,
        _: &str,
        _: &rook_tools::policy::Risk,
        _: Option<&str>,
    ) -> rook_tools::policy::Approval {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_pending_wait_claims_external_effects_only_for_a_matching_pre_tool_hook() {
    use rook_core::hooks::{Event, HookConfig};
    use tracing::instrument::WithSubscriber;
    for (event, matcher, effects) in [
        (Event::SessionDelete, None, false),
        (Event::PreTool, Some("read_file"), false),
        (Event::PreTool, Some("defer_work"), true),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let mut rook = engine(workspace.path(), store.path());
        rook.config.hooks.push(HookConfig {
            event,
            matches: matcher.map(str::to_owned),
            command: "echo ready".into(),
            ..Default::default()
        });
        let run = start(&rook);
        work::update(&rook, &run.id, |saved| {
            saved.run.autonomous = false;
            Ok(())
        })
        .unwrap();
        let approval = Arc::new(HeldWaitApproval { entered: tokio::sync::Notify::new() });
        let script = Script::new(vec![defer(serde_json::json!({"seconds":60}))]);
        eprintln!("approval fixture case: event={event:?}, matcher={matcher:?}, expected_effects={effects}");
        let mut stage = Box::pin(
            work::advance(
                &rook,
                &run.id,
                |session| {
                    let mut agent = AgentLoop::new(&rook, script.clone(), session);
                    agent.policy.set_stance(rook_tools::policy::Stance::Assist);
                    agent.approver = approval.clone();
                    Ok(agent)
                },
                |_| {},
            )
            .with_subscriber(
                tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).with_test_writer().finish(),
            ),
        );
        tokio::select! {
            result = &mut stage => panic!("fixture must wait for an actual approval: event={event:?}, matcher={matcher:?}, result={result:?}, model_transcripts={:?}", script.seen.lock().unwrap()),
            entered = tokio::time::timeout(std::time::Duration::from_secs(60),approval.entered.notified()) => entered.unwrap(),
        }
        let saved = work::read(&rook, &run.id).unwrap();
        assert!(saved.run.waiting.is_none());
        let session = rook_store::parse_session_id(&saved.run.session.unwrap()).unwrap();
        let receipts = rook.execution(session).unwrap();
        let pending = receipts.iter().find_map(|receipt| receipt.pending.as_ref()).unwrap();
        assert_eq!(pending.tool, "defer_work");
        assert_eq!(pending.may_have_effects, effects);
        drop(stage);
        assert_eq!(
            rook.execution(session).unwrap().iter().any(|receipt| !receipt.unknown.is_empty()),
            effects,
            "only the possibly executed hook requires an external-effect inspection"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deferred_stage_releases_ownership_and_reopens_the_same_transcript_without_repeating_writes() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let mut first = defer(serde_json::json!({"seconds":86400,"reason":"external readiness"})).unwrap();
    first.message.tool_calls.insert(
        0,
        ToolCall {
            id: "before-wait".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"once.txt","content":"one physical write"}),
        },
    );
    first.message.tool_calls.push(ToolCall {
        id: "after-wait".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path":"must-not-run.txt","content":"unadmitted tail"}),
    });
    let script = Script::new(vec![Ok(first)]);
    let waiting =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, script.clone(), session)), |_| {})
            .await
            .unwrap();
    let until = waiting.waiting.as_ref().unwrap().until;
    assert!(until >= work::now() + 43200, "the fixture must actually be waiting far in the future");
    assert_eq!(waiting.waiting.as_ref().unwrap().seconds, 86400);
    assert_eq!(waiting.next_attempt_at, Some(until));
    assert_eq!(waiting.iterations, 1);
    assert_eq!(waiting.tokens, 15, "the wait accounts its physical request once");
    assert!(!waiting.restart_guard.unfinished_stage);
    assert_eq!(waiting.restart_guard.consecutive_interruptions, 0);
    assert_eq!(script.seen.lock().unwrap().len(), 1, "yield cannot ask for a final answer or verification");
    assert_eq!(std::fs::read_to_string(workspace.path().join("once.txt")).unwrap(), "one physical write");
    assert!(!workspace.path().join("must-not-run.txt").exists());
    let saved = work::read(&rook, &run.id).unwrap();
    assert!(saved.active.as_ref().unwrap().answer.is_none());
    let session = saved.active.unwrap().session;
    let session_id = rook_store::parse_session_id(&session).unwrap();
    let execution = rook.execution(session_id).unwrap();
    assert!(execution.iter().all(|receipt| receipt.pending.is_none() && receipt.unknown.is_empty()));
    drop(rook);

    let rook = engine(workspace.path(), store.path());
    let not_due =
        work::advance(&rook, &run.id, |_| panic!("a waiting stage cannot construct a provider"), |_| {})
            .await
            .unwrap();
    assert_eq!(not_due.waiting.as_ref().unwrap().until, until);
    assert_eq!(not_due.tokens, 15);
    // Advance the saved clock boundary; the native probe covers real elapsed
    // time and physical daemon restart rather than relying on this fixture.
    work::update(&rook, &run.id, |saved| {
        saved.run.waiting.as_mut().unwrap().until = work::now().saturating_sub(1);
        saved.run.next_attempt_at = Some(work::now().saturating_sub(1));
        Ok(())
    })
    .unwrap();
    let resumed = Script::new(vec![read(), defer(serde_json::json!({"seconds":60}))]);
    std::fs::write(workspace.path().join("evidence.txt"), "new observation").unwrap();
    let after = work::advance(
        &rook,
        &run.id,
        |session| {
            assert_eq!(session, session_id);
            Ok(AgentLoop::new(&rook, resumed.clone(), session))
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(after.iterations, 2);
    assert_eq!(after.tokens, 45);
    assert_eq!(work::read(&rook, &run.id).unwrap().active.unwrap().session, session);
    assert_eq!(resumed.seen.lock().unwrap().len(), 2);
    let events = rook.store.events(session_id, 0, 512).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.record.kind == rook_store::EventKind::ToolCall
                && event.record.label == "write_file")
            .count(),
        1
    );
    assert!(
        events.iter().any(|event| event.record.kind == rook_store::EventKind::ToolResult
            && event.record.label == "defer_work")
    );
    assert!(!workspace.path().join("must-not-run.txt").exists());
}

#[tokio::test]
async fn invalid_wait_arguments_do_not_schedule_or_request_approval_and_a_later_valid_wait_can_yield() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let oversized = "x".repeat(513);
    assert!(oversized.len() > 512);
    let cases = vec![
        serde_json::json!({"seconds":0}),
        serde_json::json!({"seconds":86401}),
        serde_json::json!({"seconds":1.5}),
        serde_json::json!({"seconds":"60"}),
        serde_json::json!({"seconds":60,"reason":oversized}),
        serde_json::json!({"seconds":60,"reason":"line\nbreak"}),
        serde_json::json!({"seconds":60,"unknown":true}),
        serde_json::json!({"seconds":60,"reason":"valid checkpoint"}),
    ];
    let mut response = answer("").unwrap();
    response.stop_reason = StopReason::ToolUse;
    for (index, args) in cases.into_iter().enumerate() {
        response.message.tool_calls.push(ToolCall {
            id: format!("wait-{index}"),
            name: "defer_work".into(),
            arguments: args,
        });
    }
    let script = Script::new(vec![Ok(response)]);
    let result =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, script.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(result.waiting.unwrap().reason, "valid checkpoint");
    assert_eq!(result.iterations, 1, "rejected requests cannot consume wait checkpoints");
    assert_eq!(script.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pause_cancel_and_identified_resume_do_not_let_an_old_control_recreate_or_erase_a_later_wait() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let first = Script::new(vec![defer(serde_json::json!({"seconds":60}))]);
    work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, first.clone(), session)), |_| {})
        .await
        .unwrap();
    work::control(&rook, &run.id, Action::Pause).unwrap();
    work::advance(&rook, &run.id, |_| panic!("pause cannot wake"), |_| {}).await.unwrap();
    let resume = || control("resume-before-deadline", &run.generation, Action::Resume);
    let resumed = work::control_identified(&rook, &run.id, resume()).unwrap();
    assert!(resumed.run.waiting.is_none());
    assert!(resumed.run.next_attempt_at.is_none());
    let second = Script::new(vec![defer(serde_json::json!({"seconds":120,"reason":"second wait"}))]);
    let second_wait =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, second.clone(), session)), |_| {})
            .await
            .unwrap();
    let original_until = second_wait.waiting.unwrap().until;
    let retry = work::control_identified(&rook, &run.id, resume()).unwrap();
    assert!(retry.already_applied);
    assert_eq!(retry.run.waiting.unwrap().until, original_until);
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    let cancelled = work::advance(&rook, &run.id, |_| panic!("cancel cannot wake"), |_| {}).await.unwrap();
    assert_eq!(cancelled.status, Status::Cancelled);
    assert!(cancelled.waiting.is_none());
    assert!(cancelled.next_attempt_at.is_none());
}

#[tokio::test]
async fn a_wait_cannot_exceed_the_goal_lifetime_or_spend_the_last_iteration_on_a_promised_continuation() {
    for last_iteration in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        work::update(&rook, &run.id, |saved| {
            if last_iteration {
                saved.run.max_iterations = 1;
            } else {
                saved.run.max_seconds = 86400;
            }
            Ok(())
        })
        .unwrap();
        let script = Script::new(vec![defer(serde_json::json!({"seconds":86400}))]);
        let result = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, script.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert!(result.waiting.is_none());
        let session = rook_store::parse_session_id(result.session.as_ref().unwrap()).unwrap();
        let entries = rook.transcript(session, 0, 128, 4096).unwrap();
        assert!(entries.iter().any(|entry| entry.body.contains(if last_iteration {
            "no iteration allowance"
        } else {
            "remaining goal lifetime"
        })));
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_turn_end_hook_keeps_the_wait_but_requires_review_of_its_unknown_effects() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.hooks.push(rook_core::hooks::HookConfig {
        event: rook_core::hooks::Event::TurnEnd,
        command: "sleep 60".into(),
        timeout_secs: 60,
        ..Default::default()
    });
    let run = start(&rook);
    let signal = Arc::new(tokio::sync::Notify::new());
    let notified = signal.notified();
    let script = Script::new(vec![defer(serde_json::json!({"seconds":60}))]);
    let mut stage = Box::pin(work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, script.clone(), session)),
        |progress| {
            if matches!(
                progress,
                rook_core::agent::Progress::ToolDone { name: "defer_work", failed: false, .. }
            ) {
                signal.notify_one();
            }
        },
    ));
    tokio::select! {
        _ = notified => {},
        result = &mut stage => panic!("turn-end fixture must hold the stage after its checkpoint: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => panic!("checkpoint was never committed"),
    }
    let saved = work::read(&rook, &run.id).unwrap();
    assert!(saved.run.waiting.is_some());
    assert!(!saved.run.restart_guard.unfinished_stage);
    assert!(
        saved.active.as_ref().unwrap().answer.is_none(),
        "the future must stop before stage close records its outcome"
    );
    drop(stage);
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    work::advance(&rook, &run.id, |_| panic!("a not-due recovered wait cannot start"), |_| {}).await.unwrap();
    work::update(&rook, &run.id, |saved| {
        let at = work::now().saturating_sub(1);
        saved.run.next_attempt_at = Some(at);
        saved.run.waiting.as_mut().unwrap().until = at;
        Ok(())
    })
    .unwrap();
    let script = Script::new(vec![defer(serde_json::json!({"seconds":60,"reason":"after recovery"}))]);
    let after =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, script.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(after.status, Status::Blocked);
    assert_eq!(after.iterations, 1);
    assert_eq!(after.restart_guard.consecutive_interruptions, 0);
    assert!(!after.restart_guard.unfinished_stage);
    assert!(script.seen.lock().unwrap().is_empty(), "unknown hooks cannot be replayed or bypassed");
}

#[tokio::test]
async fn loss_immediately_after_the_atomic_tool_checkpoint_does_not_lose_the_wait_or_count_as_a_crash() {
    use futures_util::FutureExt;
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let script = Script::new(vec![defer(serde_json::json!({"seconds":60}))]);
    let stage = work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, script.clone(), session)),
        |progress| {
            if matches!(
                progress,
                rook_core::agent::Progress::ToolDone { name: "defer_work", failed: false, .. }
            ) {
                panic!("fixture loses its owner immediately after the atomic checkpoint");
            }
        },
    );
    assert!(std::panic::AssertUnwindSafe(stage).catch_unwind().await.is_err());
    let saved = work::read(&rook, &run.id).unwrap();
    assert!(saved.run.waiting.is_some());
    assert!(saved.active.unwrap().answer.is_none());
    assert_eq!(saved.run.iterations, 1);
    assert!(!saved.run.restart_guard.unfinished_stage);
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    work::advance(&rook, &run.id, |_| panic!("wait is not due"), |_| {}).await.unwrap();
    work::update(&rook, &run.id, |saved| {
        let at = work::now().saturating_sub(1);
        saved.run.next_attempt_at = Some(at);
        saved.run.waiting.as_mut().unwrap().until = at;
        Ok(())
    })
    .unwrap();
    let script = Script::new(vec![defer(serde_json::json!({"seconds":60,"reason":"continued after loss"}))]);
    let after =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, script.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(after.waiting.unwrap().reason, "continued after loss");
    assert_eq!(after.iterations, 2);
    assert_eq!(after.restart_guard.consecutive_interruptions, 0);
    assert_eq!(script.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_late_model_response_cannot_recreate_a_wait_after_pause_and_resume_changed_its_stage() {
    for tool in ["defer_work", "block_work"] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        let provider = held(vec![if tool == "block_work" {
            block(serde_json::json!({"reason":"Stale prerequisite"}))
        } else {
            defer(serde_json::json!({"seconds":60}))
        }]);
        let mut stage = Box::pin(work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        ));
        tokio::select! {
            result = &mut stage => panic!("request must be held before control changes: {result:?}"),
            entered = tokio::time::timeout(std::time::Duration::from_secs(60), provider.entered.notified()) => entered.unwrap(),
        }
        work::control(&rook, &run.id, Action::Pause).unwrap();
        work::control(&rook, &run.id, Action::Resume).unwrap();
        provider.release.notify_one();
        let after = stage.await.unwrap();
        assert!(after.waiting.is_none(), "an earlier physical request cannot replace explicit resume");
        let session = rook_store::parse_session_id(after.session.as_ref().unwrap()).unwrap();
        assert!(
            rook.transcript(session, 0, 128, 4096)
                .unwrap()
                .iter()
                .any(|entry| entry.body.contains("checkpoint was not committed"))
        );
        work::control(&rook, &run.id, Action::Resume).unwrap();
        let current = Script::new(vec![defer(serde_json::json!({"seconds":60,"reason":"fresh stage"}))]);
        let resumed = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, current.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(resumed.waiting.unwrap().reason, "fresh stage");
    }
}

#[tokio::test]
async fn a_wait_is_not_a_final_answer_and_does_not_validate_repair_or_write_its_output_contract() {
    for tool in ["defer_work", "block_work"] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        let script = Script::with_completion(
            vec![if tool == "block_work" {
                block(serde_json::json!({"reason":"Needs a manual verification"}))
            } else {
                defer(serde_json::json!({"seconds":60}))
            }],
            vec![],
        );
        let after = work::advance(&rook, &run.id, |session| {
        let mut agent = AgentLoop::new(&rook, script.clone(), session);
        agent.options.output_schema = Some(serde_json::json!({"type":"object","required":["ok"],"properties":{"ok":{"type":"boolean"}},"additionalProperties":false}));
        agent.options.output = Some("final.json".into());
        Ok(agent)
    }, |_| {}).await.unwrap();
        assert_eq!(
            after.status,
            if tool == "block_work" { Status::Blocked } else { Status::Queued },
            "a deferred boundary cannot become an output/provider failure: {after:?}"
        );
        assert_eq!(after.consecutive_failures, 0);
        if tool == "block_work" {
            assert!(after.next_attempt_at.is_none());
        } else {
            assert_eq!(after.next_attempt_at, Some(after.waiting.unwrap().until));
        }
        let session = rook_store::parse_session_id(after.session.as_ref().unwrap()).unwrap();
        assert_eq!(
            rook.completed_turn(session).unwrap().unwrap().stopped,
            if tool == "block_work" { "work_blocked" } else { "work_deferred" }
        );
        assert!(
            !rook
                .store
                .events(session, 0, 128)
                .unwrap()
                .iter()
                .any(|event| event.record.kind == rook_store::EventKind::Error
                    && event.record.label == "output")
        );
        assert!(
            !workspace.path().join("final.json").exists(),
            "only an actual final answer may produce the requested artifact"
        );
        assert_eq!(script.seen.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn tool_cycle_receipts_survive_worker_iterations_and_reset_for_accepted_steering_and_new_generations() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "unchanged").unwrap();
    let rook = engine(workspace.path(), store.path());
    let session = rook.start_session("goal cycle scope").unwrap();
    let run = conversation_goal(&rook, session, "Read evidence without spinning");
    for n in 0..3 {
        let provider = Script::new(vec![read(), read(), answer("remaining work")]);
        let result = work::advance(
            &rook,
            &run.id,
            |session| {
                let mut agent = AgentLoop::new(&rook, provider.clone(), session);
                agent.max_steps = 2;
                Ok(agent)
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_ne!(result.status, Status::Completed, "a cycle never completes a goal");
        let outcome = rook.completed_turn(session).unwrap().unwrap();
        assert_eq!(outcome.tools_called.len(), if n == 0 { 2 } else { 0 }, "iteration {n}: {outcome:?}");
        if n == 2 {
            assert_eq!(outcome.stopped, "looping");
        }
    }
    // Resume the same generation if its existing no-progress supervisor has
    // blocked it, then accept a correction at the usual durable boundary.
    if work::read(&rook, &run.id).unwrap().run.status == Status::Blocked {
        work::control(&rook, &run.id, Action::Resume).unwrap();
    }
    work::steer(&rook, &run.id, correction("new-check", "Recheck evidence for this correction")).unwrap();
    let provider = Script::new(vec![read(), read(), answer("still more work")]);
    work::advance(
        &rook,
        &run.id,
        |session| {
            let mut agent = AgentLoop::new(&rook, provider.clone(), session);
            agent.max_steps = 2;
            Ok(agent)
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(rook.completed_turn(session).unwrap().unwrap().tools_called.len(), 2);
    assert!(work::read(&rook, &run.id).unwrap().run.instructions[0].applied_at.is_some());
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    let replacement = conversation_goal(&rook, session, "New goal in the same conversation");
    assert_ne!(run.generation, replacement.generation);
    let provider = Script::new(vec![read(), read(), answer("new generation work remains")]);
    work::advance(
        &rook,
        &replacement.id,
        |session| {
            let mut agent = AgentLoop::new(&rook, provider.clone(), session);
            agent.max_steps = 2;
            Ok(agent)
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(rook.completed_turn(session).unwrap().unwrap().tools_called.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_outage_resumes_the_saved_session_and_a_correction_reaches_verification() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.retry_initial_secs = 1800;
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
    assert!(first.next_attempt_at.unwrap() > work::now() + 1700);
    let session = first.session.clone();
    work::steer(&rook, &run.id, correction("answer-language", "Include the file name")).unwrap();
    let ready = work::read(&rook, &run.id).unwrap().run;
    assert_eq!(ready.status, Status::Queued, "fresh guidance must wake an ordinary retry wait");
    assert!(ready.next_attempt_at.is_none());
    assert_eq!(ready.consecutive_failures, first.consecutive_failures);
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

#[test]
fn only_fresh_guidance_or_a_new_edit_wakes_an_ordinary_retry_wait() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::steer(&rook, &run.id, correction("old", "already submitted")).unwrap();
    let deadline = work::now() + 1800;
    let park = || {
        work::update(&rook, &run.id, |saved| {
            saved.run.status = Status::RetryWait;
            saved.run.next_attempt_at = Some(deadline);
            saved.run.consecutive_failures = 9;
            saved.idle = 4;
            Ok(())
        })
        .unwrap();
    };
    park();
    work::steer(&rook, &run.id, correction("old", "already submitted")).unwrap();
    assert_eq!(work::read(&rook, &run.id).unwrap().run.next_attempt_at, Some(deadline));
    assert_eq!(work::read(&rook, &run.id).unwrap().idle, 4);
    work::steer(&rook, &run.id, correction("new", "new explicit guidance")).unwrap();
    let ready = work::read(&rook, &run.id).unwrap().run;
    assert_eq!(ready.status, Status::Queued);
    assert!(ready.next_attempt_at.is_none());
    assert_eq!(ready.consecutive_failures, 9, "a message is not a successful provider request");
    assert_eq!(work::read(&rook, &run.id).unwrap().idle, 0);
    park();
    work::steer(&rook, &run.id, correction("new", "new explicit guidance")).unwrap();
    assert_eq!(work::read(&rook, &run.id).unwrap().run.next_attempt_at, Some(deadline));
    let edit = || EditInstruction { revision: 0, text: "corrected guidance".into() };
    work::edit_instruction(&rook, &run.id, "new", edit()).unwrap();
    assert!(work::read(&rook, &run.id).unwrap().run.next_attempt_at.is_none());
    park();
    work::edit_instruction(&rook, &run.id, "new", edit()).unwrap();
    let repeated = work::read(&rook, &run.id).unwrap().run;
    assert_eq!(repeated.status, Status::RetryWait);
    assert_eq!(repeated.next_attempt_at, Some(deadline));
    assert_eq!(work::read(&rook, &run.id).unwrap().idle, 4);
}

#[test]
fn fresh_guidance_does_not_resume_a_paused_blocked_or_cancelled_goal() {
    for status in [Status::Paused, Status::Blocked, Status::Cancelled, Status::Limited] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        work::update(&rook, &run.id, |saved| {
            saved.run.status = status;
            saved.run.reason = "requires explicit control".into();
            Ok(())
        })
        .unwrap();
        let result = work::steer(&rook, &run.id, correction("new", "a queued correction"));
        assert_eq!(result.is_err(), status.terminal());
        let after = work::read(&rook, &run.id).unwrap().run;
        assert_eq!(after.status, status);
        assert_eq!(after.reason, "requires explicit control");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_guidance_and_edits_preserve_an_explicit_deferred_work_deadline() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider = Script::new(vec![defer(serde_json::json!({
        "seconds":1800,"reason":"external readiness"
    }))]);
    let waiting =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert!(waiting.next_attempt_at.unwrap() > work::now() + 1700);
    work::steer(&rook, &run.id, correction("later", "guidance for the next admitted stage")).unwrap();
    work::edit_instruction(
        &rook,
        &run.id,
        "later",
        EditInstruction { revision: 0, text: "corrected guidance for the next stage".into() },
    )
    .unwrap();
    let after = work::read(&rook, &run.id).unwrap().run;
    assert_eq!(after.status, waiting.status);
    assert_eq!(after.next_attempt_at, waiting.next_attempt_at);
    assert_eq!(after.waiting.unwrap().until, waiting.waiting.unwrap().until);
    assert_eq!(provider.seen.lock().unwrap().len(), 1);
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

async fn interrupt_managed_stage(rook: &Rook, id: &str, expected_interruptions: u32) {
    let provider = held(vec![]);
    let mut stage = Box::pin(work::advance(
        rook,
        id,
        |session| Ok(AgentLoop::new(rook, provider.clone(), session)),
        |_| {},
    ));
    tokio::select! {
        result = &mut stage => panic!("the stage must reach its held physical request: {result:?}"),
        entered = tokio::time::timeout(std::time::Duration::from_secs(30), provider.entered.notified()) => entered.unwrap(),
    }
    let saved = work::read(rook, id).unwrap();
    assert!(saved.run.restart_guard.unfinished_stage, "ownership must be durable before a physical request");
    assert_eq!(saved.run.restart_guard.consecutive_interruptions, expected_interruptions);
    drop(stage);
    assert!(
        work::read(rook, id).unwrap().run.restart_guard.unfinished_stage,
        "dropping the future cannot pretend its stage returned normally"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_stages_survive_store_reopen_and_park_before_another_request_at_the_limit() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.restart_limit = 2;
    let run = start(&rook);
    for interruptions in 0..2 {
        interrupt_managed_stage(&rook, &run.id, interruptions).await;
        drop(rook);
        rook = engine(workspace.path(), store.path());
        assert_eq!(
            work::read(&rook, &run.id).unwrap().run.restart_guard.limit,
            2,
            "the goal retains its admitted restart limit after reopening with another default"
        );
    }
    let parked = work::advance(&rook, &run.id, |_| panic!("a parked stage must not build a model"), |_| {})
        .await
        .unwrap();
    assert_eq!(parked.status, Status::Blocked);
    assert_eq!(parked.restart_guard.consecutive_interruptions, 2);
    assert!(parked.reason.contains("restart limit 2"));
    assert!(parked.next_attempt_at.is_none());
    let again =
        work::advance(&rook, &run.id, |_| panic!("a parked task must stay parked"), |_| {}).await.unwrap();
    assert_eq!(again.restart_guard.consecutive_interruptions, 2);
    let resumed =
        work::control_identified(&rook, &run.id, control("retry-once", &run.generation, Action::Resume))
            .unwrap();
    assert_eq!(resumed.run.restart_guard.consecutive_interruptions, 0);
    assert!(!resumed.run.restart_guard.unfinished_stage);
    interrupt_managed_stage(&rook, &run.id, 0).await;
    let retry =
        work::control_identified(&rook, &run.id, control("retry-once", &run.generation, Action::Resume))
            .unwrap();
    assert!(retry.already_applied);
    assert!(
        retry.run.restart_guard.unfinished_stage,
        "retrying an old resume receipt cannot erase a later interrupted stage"
    );
    interrupt_managed_stage(&rook, &run.id, 1).await;
    let parked =
        work::advance(&rook, &run.id, |_| panic!("still bounded after exact control retries"), |_| {})
            .await
            .unwrap();
    assert_eq!(parked.status, Status::Blocked);
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    let cancelled =
        work::advance(&rook, &run.id, |_| panic!("cancel never starts a request"), |_| {}).await.unwrap();
    assert_eq!(cancelled.status, Status::Cancelled);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_resume_and_a_returned_stage_retire_the_guard_without_confusing_provider_retries() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.restart_limit = 3;
    let run = start(&rook);
    interrupt_managed_stage(&rook, &run.id, 0).await;
    work::control(&rook, &run.id, Action::Pause).unwrap();
    let paused = work::advance(&rook, &run.id, |_| panic!("pause is not a restart"), |_| {}).await.unwrap();
    assert_eq!(paused.restart_guard.consecutive_interruptions, 0);
    work::control(&rook, &run.id, Action::Resume).unwrap();
    let provider = Script::new(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
    let retried =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(retried.status, Status::RetryWait);
    assert_eq!(retried.consecutive_failures, 1);
    assert_eq!(retried.restart_guard.consecutive_interruptions, 0);
    assert!(!retried.restart_guard.unfinished_stage);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_saved_budget_precedes_interruption_recovery_and_cannot_be_resumed() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.restart_limit = 1;
    let run = start(&rook);
    interrupt_managed_stage(&rook, &run.id, 0).await;
    work::update(&rook, &run.id, |saved| {
        saved.run.max_seconds = 1;
        saved.run.created_at = work::now().saturating_sub(60);
        Ok(())
    })
    .unwrap();
    let result =
        work::advance(&rook, &run.id, |_| panic!("neither limit may open a request"), |_| {}).await.unwrap();
    assert_eq!(
        result.status,
        Status::Limited,
        "the exhausted budget must not be mistaken for a resumable crash-loop block"
    );
    assert!(result.reason.contains("time"), "{}", result.reason);
    assert_eq!(result.restart_guard.consecutive_interruptions, 0);
    assert!(work::control(&rook, &run.id, Action::Resume).is_err());
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
        assert_eq!(
            work::read(&rook, &first.id).unwrap().run.restart_guard.consecutive_interruptions,
            0,
            "a competing observer cannot count the currently owned stage as a crash"
        );
        assert!(!work::read(&rook, &second.id).unwrap().run.restart_guard.unfinished_stage);
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
async fn completion_outages_recheck_the_retained_reply_and_park_after_three_failures() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let mut rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let reply = "I need you to log into the game and press the key with a physical keyboard to continue.";
    let mut empty = answer("").unwrap();
    empty.stop_reason = StopReason::MaxTokens;
    empty.usage.output_tokens = 512;
    let provider =
        Script::with_completion(vec![read(), answer(reply)], vec![Ok(empty), answer("not JSON"), answer("")]);
    for attempt in 1..=3 {
        let result = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.consecutive_failures, attempt);
        assert_eq!(result.iterations, 0);
        assert_eq!(result.reply, reply, "attempt {attempt}: {result:?}");
        assert_eq!(provider.seen.lock().unwrap().len(), 2, "work/tools must never be replayed");
        let saved = work::read(&rook, &run.id).unwrap();
        let retained = saved.active.unwrap().answer.unwrap();
        assert_eq!(retained.reply, reply);
        assert_eq!(retained.tools_called, ["read_file"]);
        if attempt < 3 {
            assert_eq!(result.status, Status::RetryWait);
            assert!(result.next_attempt_at.is_some());
            work::update(&rook, &run.id, |s| {
                s.run.next_attempt_at = Some(0);
                if attempt == 1 {
                    // Crash after the turn receipt committed but before Active.answer did.
                    s.active.as_mut().unwrap().answer = None;
                }
                Ok(())
            })
            .unwrap();
            drop(rook);
            rook = engine(workspace.path(), store.path());
        } else {
            assert_eq!(result.status, Status::Blocked);
            assert!(result.next_attempt_at.is_none());
            assert!(result.reason.contains("saved reply retained"));
            let again =
                work::advance(&rook, &run.id, |_| panic!("blocked work must not create an agent"), |_| {})
                    .await
                    .unwrap();
            assert_eq!(again.status, Status::Blocked);
        }
    }
    let requests = provider.completion_requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.tools.is_empty() && r.max_output_tokens == 4096));
    assert!(requests.iter().all(|r| {
        let data: serde_json::Value = serde_json::from_str(&r.messages[1].content).unwrap();
        data["reply"] == reply
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovered_completion_verdict_blocks_or_verifies_without_replaying_work() {
    for verdict in ["blocked", "finish", "continue"] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        let provider = Script::with_completion(
            vec![answer("evidence.txt contains done."), read(), answer("VERDICT: holds")],
            vec![
                answer(""),
                answer(&format!(r#"{{"action":"{verdict}"}}"#)),
                answer(r#"{"action":"finish"}"#),
            ],
        );
        let first = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(first.status, Status::RetryWait);
        work::update(&rook, &run.id, |s| {
            s.run.next_attempt_at = Some(0);
            Ok(())
        })
        .unwrap();
        let result = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.reply, first.reply);
        assert_eq!(result.iterations, 1);
        assert_eq!(result.consecutive_failures, 0);
        let expected = match verdict {
            "blocked" => Status::Blocked,
            "finish" => Status::Completed,
            _ => Status::Queued,
        };
        assert_eq!(result.status, expected, "{result:?}");
        let seen = provider.seen.lock().unwrap();
        if verdict == "finish" {
            assert_eq!(seen.len(), 3, "only independent verification may run after classification");
            assert!(seen[1].contains("the agent has just finished a turn"));
        } else {
            assert_eq!(seen.len(), 1, "no new work before an explicit continuation decision");
        }
        assert!(result.tokens > first.tokens, "retry classification usage must be charged");
    }
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
        assert!(!result.restart_guard.unfinished_stage);
        assert_eq!(
            result.restart_guard.consecutive_interruptions, 0,
            "handled provider failures are not interrupted stages"
        );
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
