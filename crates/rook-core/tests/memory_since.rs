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

#[test]
fn a_turn_says_what_it_changed_about_what_it_believes() {
    use rook_core::agent::TurnOutcome;

    let quiet = TurnOutcome {
        steps: 1,
        stopped: "end_turn".into(),
        reply: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        tools_called: Vec::new(),
        skills_loaded: Vec::new(),
        skills_written: Vec::new(),
        facts_learned: Vec::new(),
        facts_forgotten: Vec::new(),
        delegated: Vec::new(),
        compactions: 0,
        decisions: Vec::new(),
        open_questions: Vec::new(),
    };
    assert!(quiet.memory_note().is_none(), "a turn that changed nothing says nothing");

    let busy = TurnOutcome {
        facts_learned: vec!["deploys happen on fridays".into()],
        facts_forgotten: vec!["deploys happen on tuesdays".into()],
        ..quiet
    };
    let note = busy.memory_note().expect("something to say");
    assert!(note.contains("remembered: deploys happen on fridays"), "{note}");
    assert!(note.contains("forgot: deploys happen on tuesdays"), "{note}");
}

/// The history is read back in the order its keys sort, so writes faster than
/// the clock have to keep their order all the same.
#[test]
fn a_history_written_faster_than_the_clock_still_comes_back_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let rook = rook(dir.path());

    let notes: Vec<String> = (0..12).map(|i| format!("change {i:02}")).collect();
    for (i, note) in notes.iter().enumerate() {
        rook.remember(Fact::new(format!("fact {i}"), Scope::Global), Some(note.clone())).unwrap();
    }

    let history = rook.memory_history().unwrap();
    assert_eq!(history.len(), notes.len(), "every change is a version of its own");

    let read_back: Vec<&str> = history.iter().rev().filter_map(|v| v.note.as_deref()).collect();
    let expected: Vec<&str> = notes.iter().map(String::as_str).collect();
    assert_eq!(read_back, expected, "oldest to newest, in the order they were written");
}
