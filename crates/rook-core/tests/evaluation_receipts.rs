//! Durable evaluator receipts prevent repeating operator commands on resume.
use rook_core::evaluation::{Check, Scorecard, witness};

fn engine(workspace: &std::path::Path, store: &std::path::Path) -> rook_core::Rook {
    rook_core::Rook::from_parts(
        rook_store::Store::open(store).unwrap(),
        rook_core::Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "0.4.0"),
        rook_skills::SkillIndex::discover(&[]).0,
        workspace.to_owned(),
    )
}

#[test]
fn completed_checks_are_reused_after_reopen_and_changed_evidence_is_named() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("guard"), "before").unwrap();
    let card = Scorecard {
        checks: vec![Check {
            name: "append once".into(),
            run: "echo x >> effect".into(),
            guards: vec!["guard".into()],
            ..Default::default()
        }],
    };
    let baseline = witness(workspace.path(), &card);
    let rook = engine(workspace.path(), home.path());
    let session = rook.start_session("evaluate").unwrap();
    assert!(rook.evaluate_recorded(session, &card, &baseline, None).unwrap().clean());
    let effect = std::fs::read(workspace.path().join("effect")).unwrap();
    drop(rook);
    let rook = engine(workspace.path(), home.path());
    assert!(rook.evaluate_recorded(session, &card, &baseline, None).unwrap().clean());
    assert_eq!(std::fs::read(workspace.path().join("effect")).unwrap(), effect);
    std::fs::write(workspace.path().join("guard"), "changed").unwrap();
    let report = rook.evaluate_recorded(session, &card, &baseline, None).unwrap();
    assert!(!report.clean());
    assert!(report.checks[0].touched.iter().any(|path| path.contains("changed since")));
    assert_eq!(std::fs::read(workspace.path().join("effect")).unwrap(), effect);
    let mut different = card.clone();
    different.checks[0].run = "echo y >> effect".into();
    assert!(rook.evaluate_recorded(session, &different, &baseline, None).is_err());
    assert_eq!(std::fs::read(workspace.path().join("effect")).unwrap(), effect);
}

#[test]
fn a_saved_work_plan_protects_its_inflight_session_and_counts_delegated_tokens() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), home.path());
    let session = rook.start_session("work").unwrap();
    let child = rook.fork_for_subtask(session, "subtask").unwrap();
    for (id, input) in [(session, 10), (child, 20)] {
        rook.store
            .append_event(
                id,
                rook_store::NewEvent::new(
                    rook_store::EventKind::AssistantMessage,
                    rook_store::Kind::Message,
                    b"answer",
                )
                .usage(input, 1),
            )
            .unwrap();
    }
    let mut state = rook_core::work::RunState {
        plan: rook_core::work::Plan { goal: "work".into(), most: 3, tokens: 50, until_clean: true },
        card: Scorecard::default(),
        active: Some(rook_core::work::ActiveIteration {
            at: 1,
            session: rook_store::format_session_id(session),
            before: Default::default(),
            answer: None,
            report: None,
        }),
    };
    rook_core::work::save_state(&rook, "run", &state).unwrap();
    assert!(rook.store.get_session(session).unwrap().unwrap().tags.contains(&"rook:work".into()));
    assert_eq!(rook_core::work::spent(&rook, session).unwrap(), 32);
    assert_eq!(rook_core::work::read_state(&rook, "run").unwrap().unwrap().plan.tokens, 50);
    let forked = rook.fork_session(session, 1).unwrap();
    assert!(
        !forked.tags.contains(&"rook:work".into()),
        "a conversation fork is not another active work iteration"
    );
    let later_child = rook.fork_for_subtask(session, "another subtask").unwrap();
    rook.config.storage.retention.max_sessions = Some(0);
    let prune = rook.prune(true).unwrap();
    assert_eq!(prune.sessions_deleted, 1, "only the independent fork is eligible");
    assert_eq!(prune.protected, 3, "active work includes earlier and later children");
    state.active = None;
    rook_core::work::save_state(&rook, "run", &state).unwrap();
    assert!(!rook.store.get_session(session).unwrap().unwrap().tags.contains(&"rook:work".into()));
    for id in [child, later_child] {
        assert!(!rook.store.get_session(id).unwrap().unwrap().tags.contains(&"rook:work".into()));
    }
    assert_eq!(rook.prune(true).unwrap().sessions_deleted, 4);
}
