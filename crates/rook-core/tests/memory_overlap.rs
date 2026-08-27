//! Where memory stops repeating itself, and where it must not try.
//!
//! The measurements in these tests are why there are two thresholds: term
//! overlap cannot tell a restatement from a narrowing or a contradiction,
//! because the word that distinguishes them is the one that differs.

use rook_core::memory::{Fact, SAME_FACT, Scope, WORTH_MENTIONING, overlap, select};

fn fact(text: &str) -> Fact {
    Fact::new(text, Scope::Global)
}

#[test]
fn a_sentence_reordered_is_the_same_fact() {
    let score = overlap("Vart prefers Russian in replies", "Vart prefers replies in Russian");
    assert!(score >= SAME_FACT, "{score}");
}

#[test]
fn a_narrowed_or_contradicted_fact_is_not_the_same_fact() {
    for (a, b) in [
        ("prefer tabs", "prefer tabs in Makefiles"),
        ("the API listens on port 7717", "the API listens on port 8080"),
        ("the build runs through cargo xtask ci", "builds go through cargo xtask ci"),
    ] {
        let score = overlap(a, b);
        assert!(score < SAME_FACT, "{a:?} vs {b:?} scored {score} and would be suppressed");
        assert!(score >= WORTH_MENTIONING, "{a:?} vs {b:?} scored {score}, too low to mention");
    }
}

#[test]
fn unrelated_facts_are_not_even_worth_mentioning() {
    let score = overlap("deploy with make release", "tests run with make test");
    assert!(score < WORTH_MENTIONING, "{score}");
}

#[test]
fn recall_spends_the_budget_once_on_a_fact_said_twice() {
    let facts = [
        fact("deployments go through the staging cluster first"),
        fact("deployments first go through the staging cluster"),
        fact("the staging cluster runs on two nodes"),
    ];

    let chosen = select(facts.iter(), "staging cluster deployments", 1000);

    assert_eq!(chosen.len(), 2, "{:?}", chosen.iter().map(|f| &f.text).collect::<Vec<_>>());
    assert!(chosen.iter().any(|f| f.text.contains("two nodes")), "the other fact must survive");
}

#[test]
fn recall_keeps_a_fact_that_narrows_another() {
    let facts = [fact("prefer tabs"), fact("prefer tabs in Makefiles")];

    let chosen = select(facts.iter(), "tabs", 1000);

    assert_eq!(chosen.len(), 2, "a narrowing must not be suppressed as a repeat: {chosen:?}");
}

#[test]
fn the_budget_still_bounds_what_recall_returns() {
    let facts: Vec<Fact> =
        (0..200).map(|i| fact(&format!("service number {i} listens for traffic on its own port"))).collect();

    let chosen = select(facts.iter(), "service listens traffic port", 100);
    let cost: usize = chosen.iter().map(|f| f.tokens()).sum();

    assert!(cost <= 100, "recall spent {cost} of a 100-token budget");
    assert!(!chosen.is_empty());
}
