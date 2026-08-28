use rook_core::{Config, Rook};
use rook_skills::Environment;
use rook_skills::SkillIndex;
use rook_store::{EventKind, Kind, NewEvent, SessionMeta, Store};

struct Fixture {
    _dir: tempfile::TempDir,
    rook: Rook,
}

/// Sessions of a known age, newest last, each holding a body no other session
/// shares — so deleting one is the only way to free its bytes.
fn fixture(count: usize, cap: Option<u64>) -> (Fixture, Vec<u128>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let now = rook_store::now_unix();

    let ids = (0..count)
        .map(|i| {
            let id = rook_store::new_session_id();
            let mut meta = SessionMeta::new(id, format!("session {i}"), "/tmp", now);
            store.create_session(&meta).unwrap();
            let body = distinct_body(i);
            store
                .append_event(id, NewEvent::new(EventKind::UserMessage, Kind::Message, body.as_bytes()))
                .unwrap();
            meta.updated_at = now - (count - i) as i64 * 3_600;
            store.create_session(&meta).unwrap();
            id
        })
        .collect();

    let mut config = Config::default();
    config.storage.retention.max_total_bytes = cap;
    // The grace period is for objects written while a turn is in flight; here
    // every object is seconds old and the point is the byte budget.
    config.storage.gc_grace_secs = 0;
    config.storage.retention.protect_tags = vec!["keep".into()];
    config.storage.train_dictionaries_after = usize::MAX;

    let env = Environment::bare("linux", "x86_64", "0.1.0");
    let (skills, _) = SkillIndex::discover(&[]);
    let rook = Rook::from_parts(store, config, env, skills, dir.path().to_path_buf());
    (Fixture { _dir: dir, rook }, ids)
}

/// Poorly compressible and unique per session, so each one costs real bytes.
fn distinct_body(seed: usize) -> String {
    let mut state = seed as u64 * 2_654_435_761 + 1;
    (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'a' + (state % 26) as u8)
        })
        .collect()
}

#[test]
fn a_size_budget_deletes_the_oldest_sessions_until_it_fits() {
    // The same eight sessions cost the same bytes twice over, because
    // `distinct_body` is seeded by index rather than by chance.
    let full = fixture(8, None).0.rook.content_bytes().unwrap();
    let (f, ids) = fixture(8, Some(full / 2));

    let report = f.rook.maintenance(false).unwrap();

    assert_eq!(report.over_budget_by, 0, "the cap was reachable and must be met");
    assert!(f.rook.content_bytes().unwrap() <= full / 2);
    assert!(
        f.rook.store.get_session(*ids.last().unwrap()).unwrap().is_some(),
        "the newest session is the last thing a size cap takes"
    );
    assert!(f.rook.store.get_session(ids[0]).unwrap().is_none(), "and the oldest is the first");
}

#[test]
fn a_cap_nothing_can_meet_reports_the_overage_instead_of_looping() {
    let (f, ids) = fixture(4, Some(1));
    let mut protected = f.rook.store.get_session(ids[0]).unwrap().unwrap();
    protected.tags.push("keep".into());
    f.rook.store.create_session(&protected).unwrap();

    let report = f.rook.maintenance(false).unwrap();

    assert!(report.over_budget_by > 0, "a protected session holds it over the cap");
    assert!(f.rook.store.get_session(ids[0]).unwrap().is_some(), "and protection outranks the budget");
    assert_eq!(report.prune.sessions_deleted, 3, "everything else went");
}

#[test]
fn a_dry_run_deletes_nothing_and_still_reports_the_overage() {
    let (f, ids) = fixture(4, Some(1));

    let report = f.rook.maintenance(true).unwrap();

    assert!(report.over_budget_by > 0);
    for id in &ids {
        assert!(f.rook.store.get_session(*id).unwrap().is_some());
    }
}

/// A ref keeps its object reachable and nothing here removes refs, so bytes held
/// by one are bytes no amount of deleting sessions can free. Rounds that freed
/// nothing used to keep going anyway, and the history was what paid for a cap
/// that could not be reached.
#[test]
fn a_cap_held_by_a_ref_is_not_paid_for_with_the_sessions() {
    let (f, ids) = fixture(4, Some(1));
    // Every session's body pinned by a ref, so deleting the session releases
    // nothing at all — which is the case the loop could not tell from progress.
    for (i, id) in ids.iter().enumerate() {
        for event in f.rook.store.events(*id, 0, usize::MAX).unwrap() {
            f.rook.store.set_ref(&format!("checkpoint/pinned-{i}"), &event.record.body).unwrap();
        }
    }

    let report = f.rook.maintenance(false).unwrap();

    assert!(report.over_budget_by > 0, "the cap has to stay unreachable for this to test anything");
    assert!(
        report.prune.sessions_deleted <= 1,
        "one round freed nothing, so the rest of the history must not have been spent: {} deleted",
        report.prune.sessions_deleted
    );
    assert!(
        f.rook.store.get_session(*ids.last().unwrap()).unwrap().is_some(),
        "and the newest session is still there"
    );
}
