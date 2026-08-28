//! What a crate offers, read from the copy on this machine.

use rook_tools::{Tool, ToolContext, crates::CrateApi};

fn here() -> ToolContext {
    ToolContext::new(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

async fn ask(args: serde_json::Value) -> rook_tools::ToolOutcome {
    CrateApi.call(&here(), &args).await.unwrap()
}

#[tokio::test]
async fn a_dependency_of_this_project_lists_its_public_items() {
    let out = ask(serde_json::json!({ "crate": "semver" })).await;

    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("semver "),
        "the version it resolved to is part of the answer: {}",
        out.content
    );
    assert!(out.content.contains("pub struct Version"), "{}", out.content);
    assert!(
        out.content.contains("VersionReq::pub fn matches") || out.content.contains("pub fn matches"),
        "a method is attributed to the type it is on: {}",
        out.content
    );
}

/// A crate the size of `syn` has thousands of items, and a list nobody can read
/// is not an answer.
#[tokio::test]
async fn narrowing_to_an_entity_brings_back_its_methods_and_not_the_rest() {
    let all = ask(serde_json::json!({ "crate": "semver" })).await;
    let narrowed = ask(serde_json::json!({ "crate": "semver", "entity": "VersionReq" })).await;

    let count = |o: &rook_tools::ToolOutcome| o.meta["items"].as_u64().unwrap();
    assert!(count(&narrowed) > 0, "something must match, or this proves nothing: {}", narrowed.content);
    assert!(
        count(&narrowed) < count(&all),
        "narrowing has to narrow: {} of {}",
        count(&narrowed),
        count(&all)
    );
    assert!(narrowed.content.contains("VersionReq"), "{}", narrowed.content);
}

#[tokio::test]
async fn a_crate_this_project_does_not_use_says_so_rather_than_guessing() {
    let out = ask(serde_json::json!({ "crate": "not-a-real-crate-here" })).await;

    assert!(out.is_error);
    assert!(out.content.contains("Cargo.lock"), "the message says where it looked: {}", out.content);
}
