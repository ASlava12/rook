//! "What changed today", which was a diff of two states and is a story.
//!
//! A fact learned and forgotten between the ends of a window cancels out of a
//! diff of those ends. That is exactly the case worth reporting: a live model,
//! told to remember something, recalled it in the next session, decided it did
//! not match the workspace and forgot it — and `memory since 1` answered that
//! nothing had happened all day.

use rook_core::memory::{Change, Fact, Scope};
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

fn rook(dir: &std::path::Path) -> Rook {
    let (skills, _) = SkillIndex::discover(&[]);
    let mut config = Config::default();
    config.storage.train_dictionaries_after = usize::MAX;
    Rook::from_parts(
        Store::open(dir).unwrap(),
        config,
        Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        dir.to_path_buf(),
    )
}

fn yesterday() -> i64 {
    rook_store::now_unix() - 86_400
}

#[test]
fn a_fact_learned_and_forgotten_in_the_window_is_both() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());

    rook.remember(Fact::new("deploys happen on fridays", Scope::Global), None).unwrap();
    let id = rook.memory().unwrap().facts[0].id.clone();
    rook.forget(&id, Some("forgotten by the agent".into())).unwrap();

    let changes = rook.memory_since(yesterday()).unwrap();
    let told: Vec<(Change, &str)> = changes.iter().map(|(c, f)| (*c, f.text.as_str())).collect();
    assert_eq!(
        told,
        [(Change::Learned, "deploys happen on fridays"), (Change::Forgotten, "deploys happen on fridays")],
        "in the order it happened, not netted out"
    );
}

#[test]
fn what_was_already_known_before_the_window_is_not_reported_again() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());

    rook.remember(Fact::new("an old fact", Scope::Global), None).unwrap();
    let boundary = rook_store::now_unix();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    rook.remember(Fact::new("a new fact", Scope::Global), None).unwrap();

    let changes = rook.memory_since(boundary).unwrap();
    let told: Vec<&str> = changes.iter().map(|(_, f)| f.text.as_str()).collect();
    assert_eq!(told, ["a new fact"], "the baseline is the state the window opened on");
}

#[test]
fn a_quiet_day_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());
    rook.remember(Fact::new("learned long ago", Scope::Global), None).unwrap();

    assert!(rook.memory_since(rook_store::now_unix() + 60).unwrap().is_empty());
}
