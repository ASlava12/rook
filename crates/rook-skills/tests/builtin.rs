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
    assert!(index.catalog(&env("linux")).len() >= 3, "the shipped skills went missing");
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
