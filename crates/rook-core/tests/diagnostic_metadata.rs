//! Support readers admit metadata before decoding private variable-length fields.
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::{EventKind, Kind, NewEvent, Store};

fn fixture(root: &std::path::Path) -> Rook {
    Rook::from_parts(
        Store::open(root.join("store")).unwrap(),
        Config::default(),
        Environment::bare("fixture", "fixture", "fixture"),
        SkillIndex::default(),
        root.into(),
    )
}

#[test]
fn oversized_event_metadata_is_omitted_without_losing_neighbors_or_activity() {
    let root = tempfile::tempdir().unwrap();
    let rook = fixture(root.path());
    let id = rook.start_session("private support fixture").unwrap();
    let before = rook.log(id, EventKind::UserMessage, "", "private before").unwrap();
    let label = "private-oversized-label".repeat(1024);
    let oversized = rook
        .store
        .append_event(id, NewEvent::new(EventKind::Note, Kind::Message, b"private body").label(&label))
        .unwrap();
    let after = rook.log(id, EventKind::AssistantMessage, "", "private after").unwrap();
    let state = rook_core::activity::Tracker::new(id, 30).snapshot(true);
    rook_core::activity::begin(&rook.store, id, &state).unwrap();
    let report = rook.diagnostics(id, false).unwrap();
    let seqs: Vec<_> = report.events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(seqs.contains(&before) && seqs.contains(&after));
    assert!(!seqs.contains(&oversized), "oversized raw metadata must not enter the report reader");
    assert!(report.notices.iter().any(|s| s.contains("event metadata")));
    assert_eq!(report.activity.as_ref().unwrap().generation, state.generation);
    assert_eq!(report.activity.as_ref().unwrap().last_progress_at, state.last_progress_at);
    assert!(!report.json().unwrap().contains("private"));
    let stored = rook.store.events(id, oversized, 1).unwrap();
    assert_eq!(stored[0].record.label, label, "support omission must preserve saved metadata");
}

#[test]
fn oversized_primary_session_metadata_is_refused_before_building_the_report() {
    let root = tempfile::tempdir().unwrap();
    let rook = fixture(root.path());
    let id = rook.start_session("support metadata").unwrap();
    let title = "private-title".repeat(8192);
    rook.store.update_session(id, |meta| meta.title = title.clone()).unwrap();
    let error =
        rook.diagnostics(id, false).expect_err("oversized session metadata must be admitted before decoding");
    assert!(error.to_string().contains("metadata"), "{error}");
    assert!(!error.to_string().contains("private-title"));
    assert_eq!(rook.store.get_session(id).unwrap().unwrap().title, title);
}

#[test]
fn oversized_ancestor_metadata_omits_only_the_trace() {
    let root = tempfile::tempdir().unwrap();
    let rook = fixture(root.path());
    let parent = rook.start_session("parent").unwrap();
    let child = rook.start_session("child").unwrap();
    rook.store.update_session(parent, |meta| meta.title = "private-ancestor".repeat(8192)).unwrap();
    rook.store.update_session(child, |meta| meta.parent = Some(parent)).unwrap();
    let seq = rook.log(child, EventKind::UserMessage, "", "private child").unwrap();
    let report = rook.diagnostics(child, false).unwrap();
    assert!(report.session["trace"].is_null(), "support trace traversal must also admit ancestor metadata");
    assert!(report.events.iter().any(|e| e["seq"].as_u64() == Some(seq)));
    assert!(report.notices.iter().any(|s| s.contains("correlation is unavailable")));
    assert!(!report.json().unwrap().contains("private"));
}
