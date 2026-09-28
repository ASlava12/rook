use rook_core::{Rook, schedules as schedule, work::managed};
use rook_proto::schedule::{Action, Create, Spec};
use rook_proto::work::Status;
fn engine(workspace: &std::path::Path, store: &std::path::Path) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(store).unwrap(),
        rook_core::Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "0.8.0"),
        rook_skills::SkillIndex::discover(&[]).0,
        workspace.into(),
    )
}
fn timestamp(s: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp() as u64
}
fn request(rook: &Rook, timing: &str) -> Create {
    Create {
        id: rook_store::format_session_id(rook_store::new_session_id()),
        spec: Spec {
            goal: "Inspect project".into(),
            workspace: rook.workspace.canonicalize().unwrap().display().to_string(),
            timing: timing.into(),
            timezone: "Europe/Moscow".into(),
            stance: "assist".into(),
            max_seconds: 3600,
            max_tokens: 100000,
            max_iterations: 100,
        },
    }
}
#[test]
fn calendar_handles_zones_weekends_dst_and_invalid_input() {
    let fri = timestamp("2026-10-02T06:00:00Z");
    assert_eq!(
        schedule::next("weekdays 09:00", "Europe/Moscow", fri).unwrap(),
        Some(timestamp("2026-10-05T06:00:00Z"))
    );
    assert_eq!(
        schedule::next("weekly fri 09:00", "Europe/Moscow", fri).unwrap(),
        Some(timestamp("2026-10-09T06:00:00Z"))
    );
    assert_eq!(schedule::next("daily 09:00", "Europe/Moscow", fri - 1).unwrap(), Some(fri));
    // Spring's missing 02:30 is skipped; autumn's repeated 01:30 runs once.
    assert_eq!(
        schedule::next("daily 02:30", "America/New_York", timestamp("2026-03-08T05:00:00Z")).unwrap(),
        Some(timestamp("2026-03-09T06:30:00Z"))
    );
    assert_eq!(
        schedule::next("daily 01:30", "America/New_York", timestamp("2026-11-01T05:30:00Z")).unwrap(),
        Some(timestamp("2026-11-02T06:30:00Z"))
    );
    assert!(schedule::next("once 2026-11-01 01:30", "America/New_York", fri).is_err());
    for expression in [
        "every 0m",
        "every 99999999999999999999d",
        "every ж",
        "daily 25:00",
        "weekly xyz 01:00",
        "cron * * * * *",
    ] {
        assert!(schedule::next(expression, "UTC", fri).is_err(), "{expression}");
    }
    assert!(schedule::next("daily 09:00", "not-a-zone", fri).is_err());
    assert_eq!(schedule::next("every 30m", "UTC", fri).unwrap(), Some(fri + 1800));
}
#[test]
fn restart_reservations_nonoverlap_and_terminal_history() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let now = managed::now();
    let create = request(&rook, "every 1m");
    let task = schedule::create(&rook, create.clone(), now).unwrap();
    assert_eq!(schedule::create(&rook, create.clone(), now + 2).unwrap().id, task.id);
    let mut other = create;
    other.spec.goal = "different".into();
    assert!(schedule::create(&rook, other, now).is_err());
    let pending = schedule::due(&rook, now + 60).unwrap();
    let session = pending[0].pending.clone().unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert_eq!(schedule::due(&rook, now + 61).unwrap()[0].pending.as_deref(), Some(session.as_str()));
    schedule::launch(&rook, &task.id).unwrap();
    let run = managed::read(&rook, &session).unwrap().run;
    assert_eq!(run.conversation.unwrap().stance, "assist");
    assert_eq!(run.max_seconds, 3600);
    assert_eq!(run.max_tokens, 100000);
    let sid = rook_store::parse_session_id(&session).unwrap();
    assert!(rook.store.get_session(sid).unwrap().unwrap().tags.contains(&format!("schedule:{}", task.id)));
    // Simulate restart after start() committed but before reservation cleared.
    let mut saved = schedule::list(&rook).unwrap();
    saved[0].pending = Some(session.clone());
    rook.store.kv_set("schedule/tasks-v1", &serde_json::to_vec(&saved).unwrap()).unwrap();
    schedule::launch(&rook, &task.id).unwrap();
    assert_eq!(managed::list(&rook).unwrap().len(), 1);
    assert!(schedule::control(&rook, &task.id, Action::RunNow, now + 65).is_err());
    assert!(schedule::due(&rook, now + 120).unwrap().is_empty());
    assert!(schedule::list(&rook).unwrap()[0].note.contains("Skipped"));
    managed::update(&rook, &session, |s| {
        s.run.status = Status::Completed;
        s.run.reason = "verified".into();
        Ok(())
    })
    .unwrap();
    schedule::due(&rook, now + 121).unwrap();
    assert!(managed::list(&rook).unwrap().is_empty(), "completed runs cannot exhaust live registry");
    let history = schedule::list(&rook).unwrap();
    assert_eq!(history[0].history[0].status, "Completed");
    assert!(rook.store.get_session(sid).unwrap().is_some(), "history stays readable");
    let new = schedule::control(&rook, &task.id, Action::RunNow, now + 122).unwrap();
    assert_ne!(new.pending.unwrap(), session);
    assert!(schedule::delete(&rook, &task.id).is_err(), "cannot delete a reserved run");
}
#[test]
fn downtime_disable_once_and_limits_are_explicit() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let now = timestamp("2026-09-01T00:00:00Z");
    let task = schedule::create(&rook, request(&rook, "every 1m"), now).unwrap();
    assert!(schedule::due(&rook, now + 86400).unwrap().is_empty());
    assert_eq!(schedule::list(&rook).unwrap()[0].next_at, Some(now + 86460));
    schedule::control(&rook, &task.id, Action::Disable, now + 86401).unwrap();
    assert!(schedule::due(&rook, now + 100000).unwrap().is_empty());
    schedule::delete(&rook, &task.id).unwrap();
    let once = schedule::create(&rook, request(&rook, "once 2026-09-02 03:00"), now).unwrap();
    let due = schedule::due(&rook, now + 3 * 86400).unwrap();
    assert_eq!(due.len(), 1);
    assert!(!due[0].enabled);
    assert_eq!(due[0].next_at, None);
    schedule::fail_pending(&rook, &once.id, "workspace removed").unwrap();
    assert!(schedule::due(&rook, now + 4 * 86400).unwrap().is_empty());
    assert_eq!(schedule::list(&rook).unwrap()[0].history[0].status, "Blocked");
    let mut invalid = request(&rook, "daily 09:00");
    invalid.spec.max_tokens = 0;
    assert!(schedule::create(&rook, invalid, now).is_err());
}

#[test]
fn recurring_history_and_live_registry_remain_bounded() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.max_runs = 1;
    let now = managed::now();
    let task = schedule::create(&rook, request(&rook, "every 1d"), now).unwrap();
    let mut sessions = std::collections::BTreeSet::new();
    for i in 0..35 {
        let pending = schedule::control(&rook, &task.id, Action::RunNow, now + i).unwrap();
        let session = pending.pending.unwrap();
        assert!(sessions.insert(session.clone()));
        schedule::launch(&rook, &task.id).unwrap();
        managed::update(&rook, &session, |s| {
            s.run.status = Status::Completed;
            s.run.reason = "verified".into();
            Ok(())
        })
        .unwrap();
        schedule::due(&rook, now + i).unwrap();
        assert!(managed::list(&rook).unwrap().is_empty());
    }
    let history = schedule::list(&rook).unwrap();
    assert_eq!(history[0].history.len(), 16);
    assert!(history[0].history.iter().all(|r| r.status == "Completed"));
    assert_eq!(rook.store.list_sessions().unwrap().len(), 35);
    schedule::delete(&rook, &task.id).unwrap();
    assert!(schedule::list(&rook).unwrap().is_empty());
}

#[test]
fn a_reserved_launch_can_be_cancelled_before_any_worker_starts() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let now = managed::now();
    let task = schedule::create(&rook, request(&rook, "every 1d"), now).unwrap();
    schedule::control(&rook, &task.id, Action::RunNow, now).unwrap();
    let cancelled = schedule::control(&rook, &task.id, Action::CancelRun, now).unwrap();
    assert!(cancelled.pending.is_none());
    assert_eq!(cancelled.history[0].status, "Cancelled");
    schedule::launch(&rook, &task.id).unwrap();
    assert!(rook.store.list_sessions().unwrap().is_empty());
    schedule::delete(&rook, &task.id).unwrap();
}
