//! Where a session was compacted, and where a fork left its parent.
//!
//! Both are recoverable by reading the log, which is what made them easy to get
//! wrong: the reader was O(events) and ran at the start of every turn, and the
//! fork point was legible only inside a title string.

use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::{EventKind, Store};

fn rook(dir: &std::path::Path) -> Rook {
    let store = Store::open(dir).unwrap();
    let mut config = Config::default();
    config.storage.train_dictionaries_after = usize::MAX;
    let (skills, _) = SkillIndex::discover(&[]);
    Rook::from_parts(store, config, Environment::bare("linux", "x86_64", "0.1.0"), skills, dir.to_path_buf())
}

fn compaction_note(through: u64, summary: &str) -> String {
    serde_json::json!({ "through_seq": through, "summary": summary }).to_string()
}

#[test]
fn replay_starts_after_the_last_compaction_not_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let session = rook.start_session("compacted twice").unwrap();

    rook.log(session, EventKind::UserMessage, "user", "one").unwrap();
    rook.log(session, EventKind::Compaction, "auto", &compaction_note(0, "first")).unwrap();
    rook.log(session, EventKind::UserMessage, "user", "two").unwrap();
    rook.log(session, EventKind::Compaction, "auto", &compaction_note(2, "second")).unwrap();

    let (from, summary) = rook.last_compaction(session).unwrap();
    assert_eq!(from, 3, "replay resumes after the event the newest compaction covered");
    assert_eq!(summary.as_deref(), Some("second"), "and carries that compaction's summary");
}

#[test]
fn a_session_written_before_the_mark_existed_still_reads_and_heals() {
    let dir = tempfile::tempdir().unwrap();
    let session = {
        let rook = rook(dir.path());
        let session = rook.start_session("older build").unwrap();
        rook.log(session, EventKind::UserMessage, "user", "one").unwrap();
        // Straight to the store, which is what a build that recorded no position
        // did: the compaction event lands, the key beside it never exists.
        rook.store
            .append_event(
                session,
                rook_store::NewEvent::new(
                    EventKind::Compaction,
                    rook_store::Kind::Message,
                    compaction_note(1, "summarised").as_bytes(),
                ),
            )
            .unwrap();
        session
    };

    let rook = rook(dir.path());
    assert_eq!(
        rook.last_compaction(session).unwrap(),
        (2, Some("summarised".into())),
        "the log still answers when the mark is missing"
    );

    assert!(
        rook.store.kv_get(&format!("compacted/{session:032x}")).unwrap().is_some(),
        "and the answer is written down, so the next turn does not read the log again"
    );
}

#[test]
fn a_fork_that_cuts_past_the_compaction_does_not_inherit_its_position() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let session = rook.start_session("rewound").unwrap();

    rook.log(session, EventKind::UserMessage, "user", "one").unwrap();
    rook.log(session, EventKind::Compaction, "auto", &compaction_note(1, "summarised")).unwrap();
    rook.log(session, EventKind::UserMessage, "user", "two").unwrap();

    let rewind = rook.rewind(session, 1, false).unwrap();
    let forked = rook_store::parse_session_id(&rewind.session).unwrap();

    assert_eq!(
        rook.last_compaction(forked).unwrap(),
        (0, None),
        "the fork kept only the first event, so nothing in it was ever compacted"
    );
    assert_eq!(rook.last_compaction(session).unwrap().0, 2, "and the parent it forked from is untouched");
}

#[test]
fn a_fork_records_where_it_diverged() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let session = rook.start_session("original").unwrap();
    for turn in 0..4 {
        rook.log(session, EventKind::UserMessage, "user", &format!("turn {turn}")).unwrap();
    }

    let rewind = rook.rewind(session, 2, false).unwrap();
    let forked = rook_store::parse_session_id(&rewind.session).unwrap();

    assert_eq!(rook.forked_at(forked).unwrap(), Some(2), "the fork knows its own divergence point");
    assert_eq!(rook.forked_at(session).unwrap(), None, "a session nobody forked has none");

    let listed = rook.session_summaries().unwrap();
    let entry = listed.iter().find(|s| s.meta.id == forked).expect("the fork is listed");
    assert_eq!(entry.forked_at, Some(2), "and the listing every front end reads carries it");
}

#[test]
fn deleting_a_session_takes_what_was_kept_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let session = rook.start_session("short-lived").unwrap();
    rook.set_goal(session, "prove the keys do not outlive the session").unwrap();
    rook.log(session, EventKind::Compaction, "auto", &compaction_note(0, "summarised")).unwrap();

    let beside = |rook: &Rook| {
        [format!("goal/{session:032x}"), format!("compacted/{session:032x}")]
            .iter()
            .filter(|key| rook.store.kv_get(key).unwrap().is_some())
            .count()
    };
    assert_eq!(beside(&rook), 2, "the goal and the compaction position are both recorded");

    rook.store.delete_session(session).unwrap();
    assert_eq!(
        beside(&rook),
        0,
        "retention deletes sessions on a timer, so anything it leaves behind grows without bound"
    );
}

#[test]
fn last_means_the_most_recent_session_in_this_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());

    let elsewhere = rook.start_session("another project").unwrap();
    let mut meta = rook.store.get_session(elsewhere).unwrap().unwrap();
    meta.workspace = "/somewhere/else".into();
    meta.updated_at = rook_store::now_unix() + 3_600;
    rook.store.create_session(&meta).unwrap();

    let older = rook.start_session("here, first").unwrap();
    let newer = rook.start_session("here, second").unwrap();

    assert_eq!(
        rook.session_named("last").unwrap(),
        newer,
        "the newest of this workspace, not the newest overall"
    );
    assert_eq!(rook.session_named(&rook_store::format_session_id(older)).unwrap(), older);
    assert!(rook.session_named("LAST").is_ok(), "the word is what matters, not its case");
}

#[test]
fn asking_for_last_where_nothing_has_run_says_so_rather_than_naming_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());

    let err = rook.session_named("last").unwrap_err().to_string();
    assert!(err.contains("no session has been started"), "{err}");
    assert!(err.contains(&dir.path().display().to_string()), "and where it looked: {err}");

    let err = rook.session_named("not-an-id").unwrap_err().to_string();
    assert!(err.contains("neither a session id nor `last`"), "{err}");
}
