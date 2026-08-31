//! Searching a workspace, which holds whatever the user put in it — including
//! files no search should read into memory to find that out.

use rook_tools::{Tool, ToolContext, search::Search};

async fn find(dir: &std::path::Path, args: serde_json::Value) -> String {
    let ctx = ToolContext::new(dir.to_path_buf());
    Search.call(&ctx, &args).await.unwrap().content
}

#[tokio::test]
async fn a_match_is_reported_with_its_path_and_line() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn one() {}\nfn two() {}\nfn three() {}\n").unwrap();

    let found = find(dir.path(), serde_json::json!({ "pattern": "fn two" })).await;
    assert!(found.contains("a.rs:2:fn two() {}"), "{found}");
}

#[tokio::test]
async fn a_binary_file_is_passed_over_rather_than_searched() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("blob.bin"), [b'x', 0, b'n', b'e', b'e', b'd', b'l', b'e']).unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();

    let found = find(dir.path(), serde_json::json!({ "pattern": "needle" })).await;
    assert!(found.contains("a.txt:1"), "{found}");
    assert!(!found.contains("blob.bin"), "{found}");
}

/// Every file was read whole, so a text log the size of memory was read into it
/// — and a search is not where anyone wants to discover that.
#[tokio::test]
async fn a_line_longer_than_any_source_line_ends_the_file_rather_than_the_machine() {
    let dir = tempfile::tempdir().unwrap();
    let mut minified = "x".repeat(4 << 20);
    minified.push_str("needle");
    std::fs::write(dir.path().join("bundle.js"), &minified).unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle\n").unwrap();

    let found = find(dir.path(), serde_json::json!({ "pattern": "needle" })).await;
    assert!(found.contains("a.txt:1"), "the real file is still searched: {found}");
    assert!(!found.contains("bundle.js"), "and the blob is given up on: {found}");
}

#[tokio::test]
async fn a_file_far_larger_than_memory_would_hold_is_still_searched_line_by_line() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("huge.log");
    // Ordinary lines: nothing here justifies holding the file at once.
    let mut text = String::with_capacity(8 << 20);
    for i in 0..200_000 {
        text.push_str(&format!("line {i} of a long and boring log\n"));
    }
    text.push_str("the needle is at the end\n");
    std::fs::write(&big, text).unwrap();

    let found = find(dir.path(), serde_json::json!({ "pattern": "the needle" })).await;
    assert!(found.contains("huge.log:200001"), "found at its real line number: {found}");
}

/// Searching one file printed `:12:text`: the path stripped of itself is empty,
/// and a hit that names nothing is a hit nobody can open.
#[tokio::test]
async fn searching_a_single_file_still_says_which_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.rs"), "fn needle() {}\n").unwrap();

    let found = find(dir.path(), serde_json::json!({ "pattern": "needle", "path": "one.rs" })).await;

    assert!(found.contains("one.rs:1:"), "{found}");
}

/// The hits were capped and the looking was not: the walk has no idea whether
/// it is in a workspace or a home directory until it is in one.
#[tokio::test]
async fn a_walk_that_will_not_end_is_given_up_on_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..12 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "nothing here\n").unwrap();
    }
    let mut ctx = ToolContext::new(dir.path().to_path_buf());
    ctx.max_files_searched = 5;

    let found = Search.call(&ctx, &serde_json::json!({ "pattern": "needle" })).await.unwrap();

    assert!(found.content.contains("stopped after 5 files"), "{}", found.content);
    assert!(found.content.contains("narrow"), "and says what to do about it: {}", found.content);
    assert!(found.truncated, "and the outcome says it was cut: {found:?}");
    assert_eq!(found.meta.get("files_scanned"), Some(&serde_json::json!(5)));
}
