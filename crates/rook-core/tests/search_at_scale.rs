//! Search over a store shaped like a repository checkpointed many times.
//!
//! Attributing a match to the file it came from used to walk every session for
//! every match, which is quadratic in the thing that grows. On this store —
//! twenty checkpoints of thirty files — that was 1.1 seconds; indexing the
//! captures once made it 24 milliseconds. This one is smaller, because building
//! the store costs more than searching it. There is no timing assertion here,
//! because that would measure the machine; what it holds is that every match is
//! still attributed, which is what the slow version was doing.
use std::path::PathBuf;

use rook_core::search::Search;
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

#[test]
fn every_match_in_a_captured_file_is_attributed_however_many_there_are() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };
    let store = Store::open(home.path().join("store")).unwrap();
    let (skills, _) = SkillIndex::discover(&[]);
    let rook = Rook::from_parts(
        store,
        Config::default(),
        Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        PathBuf::from(workspace.path()),
    );

    // Eight checkpoints over a growing tree of files that all mention the same
    // word — a repository is exactly this shape, and each capture holds every
    // file written so far.
    for s in 0..8 {
        let session = rook.start_session(&format!("session {s}")).unwrap();
        for f in 0..10 {
            std::fs::write(
                workspace.path().join(format!("f{s}_{f}.txt")),
                format!("the parser handles case {s}_{f}\n"),
            )
            .unwrap();
        }
        let (_, id) = rook.checkpoint("work", None).unwrap();
        rook.log(session, rook_store::EventKind::Checkpoint, "work", &id.to_hex()).unwrap();
    }

    let found = rook.search("parser", &Search::default()).unwrap();

    assert_eq!(found.hits.len(), 40, "the default limit, from many more matching objects");
    for hit in &found.hits {
        assert!(hit.file.is_some(), "every hit names the file it came from: {hit:?}");
        assert!(!hit.title.is_empty(), "and the capture: {hit:?}");
    }
}
