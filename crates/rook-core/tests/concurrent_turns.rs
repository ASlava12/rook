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

/// A guard releases when the call returns, when it panics on the way out, and
/// when the turn holding it is aborted. What it cannot release is a call that
/// never returns at all — `run_command` takes its timeout from the model, so
/// "for as long as the call takes" is not on its own a bound.
#[test]
fn a_claim_nobody_ever_released_stops_being_believed() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let vanished = rook.start_session("gone").unwrap();
    let live = rook.start_session("live").unwrap();
    let path: Vec<PathBuf> = vec![dir.path().join("held.txt")];

    // Leaked on purpose: this is the holder that never comes back.
    std::mem::forget(rook.writing(vanished, &path).unwrap());
    assert!(rook.writing(live, &path).is_err(), "held while the holder might still be there");
    assert_eq!(rook.being_written().len(), 1, "and visible while it is held");

    rook.age_claims_for_test(3_600);

    rook.writing(live, &path).expect("a claim older than any call can be is not believed");
    assert!(
        rook.being_written().iter().all(|(_, by)| by.session == live),
        "and the stale holder is gone from the registry"
    );
}

/// A panic inside the call still releases: the guard is dropped while unwinding.
#[test]
fn a_call_that_panics_does_not_keep_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    let one = rook.start_session("panicking").unwrap();
    let two = rook.start_session("after").unwrap();
    let path: Vec<PathBuf> = vec![dir.path().join("mid-write.txt")];

    let fell_over = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = rook.writing(one, &path).unwrap();
        panic!("the tool call blew up");
    }));
    assert!(fell_over.is_err(), "the panic has to have happened for this to test anything");

    rook.writing(two, &path).expect("the next turn may have it");
}
