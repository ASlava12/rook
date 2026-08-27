//! Finding something that reached the store through a file rather than through
//! the conversation.
//!
//! A checkpoint keeps whatever was on disk — a `.env` included — so "where did
//! that end up" is a question the search has to be able to answer. It scanned
//! those objects and could never report them, which made the option that
//! includes them a promise nothing kept.

use std::path::PathBuf;

use rook_core::search::Search;
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

struct Fixture {
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    rook: Rook,
}

fn fixture() -> Fixture {
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
    Fixture { _home: home, workspace, rook }
}

#[test]
fn a_secret_captured_into_a_checkpoint_can_be_found_again() {
    let f = fixture();
    std::fs::write(f.workspace.path().join(".env"), "SECRET_TOKEN=abc123xyz\n").unwrap();
    f.rook.checkpoint("before", None).unwrap();

    let found = f.rook.search("abc123xyz", &Search::default()).unwrap();

    assert_eq!(found.hits.len(), 1, "{found:?}");
    let hit = &found.hits[0];
    assert_eq!(hit.file.as_deref(), Some(".env"), "it must name the file: {hit:?}");
    assert!(hit.title.contains("before"), "and the capture it was in: {hit:?}");
    assert!(hit.snippet.contains("abc123xyz"));
}

#[test]
fn asking_for_the_conversation_only_leaves_captured_files_out() {
    let f = fixture();
    std::fs::write(f.workspace.path().join(".env"), "SECRET_TOKEN=abc123xyz\n").unwrap();
    f.rook.checkpoint("before", None).unwrap();

    let found =
        f.rook.search("abc123xyz", &Search { conversation_only: true, ..Default::default() }).unwrap();

    assert!(found.hits.is_empty(), "{found:?}");
    assert_eq!(found.objects_scanned, 0, "and the file is not even read");
}

#[test]
fn a_hit_in_something_said_still_belongs_to_its_session() {
    let f = fixture();
    let session = f.rook.start_session("a session").unwrap();
    f.rook.log(session, rook_store::EventKind::UserMessage, "prompt", "the parser is wrong").unwrap();

    let found = f.rook.search("parser", &Search::default()).unwrap();

    assert_eq!(found.hits.len(), 1, "{found:?}");
    let hit = &found.hits[0];
    assert!(hit.file.is_none(), "not a file: {hit:?}");
    assert_eq!(hit.session, rook_store::format_session_id(session));
    assert_eq!(hit.title, "a session");
}

#[test]
fn a_file_the_same_as_one_in_the_conversation_is_reported_once() {
    let f = fixture();
    let session = f.rook.start_session("a session").unwrap();
    std::fs::write(f.workspace.path().join("notes.txt"), "shared text\n").unwrap();
    f.rook.checkpoint("before", None).unwrap();
    f.rook.log(session, rook_store::EventKind::ToolResult, "read_file", "shared text\n").unwrap();

    let found = f.rook.search("shared", &Search::default()).unwrap();

    assert_eq!(
        found.hits.len(),
        1,
        "content addressing makes these one object; reporting it twice would be noise: {found:?}"
    );
    assert!(found.hits[0].file.is_none(), "the conversation is the more useful answer: {found:?}");
}
