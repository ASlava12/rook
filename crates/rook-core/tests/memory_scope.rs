//! A fact's identity is its text, so the same sentence learned in two places is
//! one fact — and the scope it ends up with decides where it is ever seen.

use rook_core::memory::{Fact, Learned, MemoryBook, Scope};

fn fact(text: &str, scope: Scope) -> Fact {
    Fact::new(text, scope)
}

fn project(path: &str) -> Scope {
    Scope::Project(path.into())
}

#[test]
fn remembering_globally_widens_a_fact_learned_in_one_project() {
    let mut book = MemoryBook::default();
    book.learn(fact("prefer tabs", project("/work/a")));

    assert_eq!(book.learn(fact("prefer tabs", Scope::Global)), Learned::Merged);
    assert_eq!(book.facts[0].scope, Scope::Global);
    assert_eq!(book.in_scope("/work/b").count(), 1, "it must apply where it was widened to");
}

#[test]
fn remembering_it_again_in_a_project_does_not_narrow_it() {
    let mut book = MemoryBook::default();
    book.learn(fact("prefer tabs", Scope::Global));

    assert_eq!(book.learn(fact("prefer tabs", project("/work/a"))), Learned::Unchanged);
    assert_eq!(book.facts[0].scope, Scope::Global, "a narrower scope must not take the wider one away");
}

#[test]
fn a_nested_project_is_within_the_one_above_it() {
    let mut book = MemoryBook::default();
    book.learn(fact("use the vendored toolchain", project("/work/a/crates/inner")));

    assert_eq!(book.learn(fact("use the vendored toolchain", project("/work/a"))), Learned::Merged);
    assert_eq!(book.facts[0].scope, project("/work/a"));
}

#[test]
fn two_unrelated_projects_are_reported_rather_than_silently_merged() {
    let mut book = MemoryBook::default();
    book.learn(fact("deploy on Fridays", project("/work/a")));

    let learned = book.learn(fact("deploy on Fridays", project("/work/b")));

    assert_eq!(
        learned,
        Learned::ScopedElsewhere(project("/work/a")),
        "neither scope contains the other, so widening would leak it and keeping it loses it"
    );
    assert_eq!(book.in_scope("/work/b").count(), 0, "and it still does not apply here");
}

#[test]
fn a_fact_is_forgotten_by_id_or_by_its_text() {
    let mut book = MemoryBook::default();
    let id = book.facts.first().map(|f| f.id.clone());
    assert!(id.is_none());
    book.learn(fact("prefer tabs", Scope::Global));
    let id = book.facts[0].id.clone();

    assert!(book.forget(&id).is_some());
    book.learn(fact("prefer tabs", Scope::Global));
    assert!(book.forget("prefer tabs").is_some());
    assert!(book.facts.is_empty());
}
