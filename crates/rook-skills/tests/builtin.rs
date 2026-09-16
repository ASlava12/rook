//! The skills Rook ships with. They are packaged next to the binary by
//! `cargo xtask dist`, so nothing in a dev build loads them and a broken one
//! would first be noticed by whoever installed the release.

use std::path::PathBuf;

use rook_skills::{Environment, SkillIndex, SkillSource};

fn builtin() -> (SkillIndex, Vec<String>) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    assert!(dir.is_dir(), "{} is missing", dir.display());
    let (index, errors) = SkillIndex::discover(&[(dir, SkillSource::Builtin)]);
    (index, errors.into_iter().map(|e| e.to_string()).collect())
}

/// `bare` derives the userland from the OS, which is what a real machine does.
fn env(os: &str) -> Environment {
    Environment::bare(os, "x86_64", "0.1.0")
}

/// A machine with the toolchains a developer would have, which is who the
/// shipped skills are for.
fn equipped(os: &str) -> Environment {
    env(os).with_language("rust", "1.97.1").with_tool("cargo", "1.97.1").with_tool("git", "2.50.1")
}

#[test]
fn every_shipped_skill_parses() {
    let (index, errors) = builtin();

    assert!(errors.is_empty(), "{errors:?}");
    assert!(index.catalog(&env("linux")).len() >= 5, "the shipped skills went missing");
}

#[test]
fn every_shipped_skill_applies_where_its_requirements_are_met() {
    let (index, _) = builtin();

    for card in index.catalog(&equipped("linux")) {
        assert!(card.applicable, "{} applies nowhere even equipped: {:?}", card.name, card.mismatches);
        assert!(!card.description.is_empty(), "{} has no description to advertise", card.name);
    }
}

#[test]
fn a_skill_that_needs_a_toolchain_says_which_one_is_missing() {
    let (index, _) = builtin();

    let card = index.catalog(&env("linux")).into_iter().find(|c| c.name == "rust-release").unwrap();

    assert!(!card.applicable, "it needs cargo, and a bare box has none");
    assert!(
        card.mismatches.iter().any(|m| m.contains("cargo")),
        "a blocked skill must name what it wanted: {:?}",
        card.mismatches
    );
}

#[test]
fn the_platform_skill_swaps_its_body_rather_than_excluding_itself() {
    let (index, _) = builtin();

    let gnu = index.resolve("in-place-edit", &env("linux")).unwrap();
    let bsd = index.resolve("in-place-edit", &env("freebsd")).unwrap();
    let windows = index.resolve("in-place-edit", &env("windows")).unwrap();

    assert_ne!(gnu.body, bsd.body, "a BSD box needs the BSD spelling of sed -i");
    assert_ne!(gnu.body, windows.body);
    assert!(bsd.variant.is_some(), "the variant is what makes one skill serve every platform");
}

/// A card is paid for on every request and exists to answer one question: load
/// the body or not. So it names the situation rather than the subject, and it
/// stays small enough that fifty of them are affordable.
///
/// `Use when` / `Use before` is the shape, checked rather than asked for: it
/// was five descriptions of what each skill *is* before, which reads well and
/// leaves a model to load a body to find out whether it wanted it.
#[test]
fn a_shipped_card_says_when_to_use_it_and_stays_small() {
    let (index, _) = builtin();
    let cards = index.catalog(&equipped("linux"));
    assert!(cards.len() >= 5, "the shipped skills went missing");

    let mut whole = 0;
    for card in &cards {
        assert!(
            card.description.starts_with("Use when") || card.description.starts_with("Use before"),
            "{} describes itself rather than the moment to reach for it: {:?}",
            card.name,
            card.description
        );
        // The line the catalogue actually writes: "- name: description".
        let cost = (card.name.len() + card.description.len() + 4).div_ceil(4);
        assert!(
            cost <= 50,
            "{}'s card costs ~{cost} tokens on every request. Cut it to the situation; a \
             \"not for …\" clause is worth its tokens only where the confusion is real and \
             likely, as `in-place-edit`'s is.",
            card.name
        );
        whole += cost;
    }
    // `max_skill_cards` is 50, and these five are what a fresh install pays
    // before it has collected anything of its own. It was ~175 when each card
    // described its subject; the thirty tokens bought the situation, which is
    // the only thing a card is read for — and one body not loaded to find out
    // costs between 200 and 900.
    assert!(
        whole <= 220,
        "the shipped catalogue costs ~{whole} tokens on every request; cut a card before \
         raising this"
    );
}

/// What a card promises, the body has to deliver without filling the window it
/// was loaded into. Bounded here because these ship: a skill somebody writes is
/// their own business, and one Rook installs is Rook's.
#[test]
fn a_shipped_body_is_bounded_so_loading_one_is_affordable() {
    let (index, _) = builtin();

    for card in index.catalog(&equipped("linux")) {
        assert!(
            card.body_tokens <= 1_200,
            "{} costs ~{} tokens to load. Move the long part into a bundled file the body \
             names — `load_skill` reports those, and they are read only if they are needed.",
            card.name,
            card.body_tokens
        );
        // A body that says nothing is a card that should not have been shown.
        assert!(card.body_tokens >= 40, "{} has no instructions worth loading", card.name);
    }
}
