//! Two conversations in one project.
//!
//! Several of them can share an engine now, and nothing else stops two writing
//! the same file at the same moment. `edit_file` refuses on its own — it
//! replaces exact text, and text another turn has changed is not there to
//! replace — but `write_file` overwrites whole, so the loser of that race loses
//! its work without being told.

use std::path::PathBuf;

use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

fn rook(dir: &std::path::Path) -> Rook {
    Rook::from_parts(
        Store::open(dir.join("store")).unwrap(),
        Config::default(),
        Environment::bare("linux", "x86_64", "0.1.0"),
        SkillIndex::default(),
        dir.to_path_buf(),
    )
}

#[test]
fn a_path_one_turn_is_writing_is_refused_to_another() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let one = rook.start_session("first").unwrap();
    let two = rook.start_session("second").unwrap();
    let contested = vec![dir.path().join("main.rs")];
    let untouched = vec![dir.path().join("other.rs")];

    let held = rook.writing(one, &contested).expect("the first turn claims it");

    let refused = match rook.writing(two, &contested) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a path another turn is writing must be refused"),
    };
    assert!(
        refused.contains(&rook_store::format_session_id(one)),
        "the refusal has to name who is holding it, or there is nothing to do about it: {refused}"
    );
    assert!(refused.contains("main.rs"), "and which file: {refused}");

    rook.writing(two, &untouched).expect("a different file is not contested");
    rook.writing(one, &contested).expect("the holder may claim its own again — a turn writes twice");

    drop(held);
    rook.writing(two, &contested).expect("and once it lets go, the other may have it");
}

/// Released when the call returns, not when the turn ends: a turn that touched a
/// file at its second step must not hold it for the remaining hundred and
/// ninety-eight.
#[test]
fn a_claim_lasts_the_call_and_not_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let one = rook.start_session("first").unwrap();
    let two = rook.start_session("second").unwrap();
    let path: Vec<PathBuf> = vec![dir.path().join("notes.txt")];

    for _ in 0..3 {
        let held = rook.writing(one, &path).unwrap();
        assert!(rook.writing(two, &path).is_err(), "contested while the call is in flight");
        drop(held);
        rook.writing(two, &path).expect("and free the moment it is over");
    }
}
