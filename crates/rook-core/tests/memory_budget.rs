//! What recall costs a request.
//!
//! Memory is the one accumulator the user adds to on purpose, and `remember`
//! lets the model add to it too — so the budget is the only thing standing
//! between a month of pinning and a window with no room for the prompt.

use rook_core::memory::{Fact, Scope, select};

fn pinned(text: &str) -> Fact {
    let mut fact = Fact::new(text, Scope::Global);
    fact.pinned = true;
    fact
}

fn cost(facts: &[Fact]) -> usize {
    facts.iter().map(|f| f.tokens()).sum()
}

#[test]
fn pinning_wins_over_relevance() {
    let facts = [
        Fact::new("the deploy script lives in ops/deploy.sh", Scope::Global),
        pinned("never touch the production database"),
    ];

    let chosen = select(facts.iter(), "where is the deploy script", 1000);
    assert_eq!(chosen[0].text, "never touch the production database", "{chosen:?}");
    assert_eq!(chosen.len(), 2, "and the match is still recalled: {chosen:?}");
}

/// A pinned fact went in whatever it cost, so the bound was the number of facts
/// somebody had pinned — which is not a bound.
#[test]
fn pinning_does_not_win_over_the_budget() {
    let facts: Vec<Fact> =
        (0..200).map(|i| pinned(&format!("a pinned fact number {i} about something or other"))).collect();

    let budget = 100;
    let chosen = select(facts.iter(), "anything", budget);

    assert!(!chosen.is_empty(), "some of them still reach the model");
    assert!(cost(&chosen) <= budget, "{} tokens against a budget of {budget}", cost(&chosen));
}

#[test]
fn what_is_pinned_is_taken_before_what_merely_matches_when_room_runs_out() {
    let mut facts: Vec<Fact> = (0..40)
        .map(|i| Fact::new(format!("anything at all about widgets, number {i}"), Scope::Global))
        .collect();
    facts.push(pinned("the one that matters"));

    let chosen = select(facts.iter(), "widgets", 30);
    assert!(
        chosen.iter().any(|f| f.text == "the one that matters"),
        "the pinned one is not what gets squeezed out: {chosen:?}"
    );
}
