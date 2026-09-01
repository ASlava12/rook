//! The workspace boundary. Lexical containment is not containment: a symlink
//! inside the workspace that points out of it reads and writes outside while
//! every path involved still looks contained.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use rook_tools::{Tool, ToolContext, files};

struct Dirs {
    /// Kept so the fixture outlives the test that made it.
    _root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
}

/// A fresh pair of directories. These sat at a fixed path under the system
/// temp directory and were cleared on the way in, so two runs of this binary at
/// once deleted each other's fixtures — which reads as three symlink guards
/// failing and is nothing of the kind.
fn dirs() -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let dirs = Dirs { workspace: root.path().join("ws"), outside: root.path().join("out"), _root: root };
    std::fs::create_dir_all(&dirs.workspace).unwrap();
    std::fs::create_dir_all(&dirs.outside).unwrap();
    dirs
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

async fn read(ctx: &ToolContext, path: &str) -> rook_tools::Result<String> {
    files::ReadFile.call(ctx, &serde_json::json!({ "path": path })).await.map(|o| o.content)
}

#[cfg(unix)]
async fn write(ctx: &ToolContext, path: &str) -> rook_tools::Result<String> {
    files::WriteFile.call(ctx, &serde_json::json!({ "path": path, "content": "x" })).await.map(|o| o.content)
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_out_of_the_workspace_does_not_read_through_it() {
    let d = dirs();
    std::fs::write(d.outside.join("secret.txt"), "the private key").unwrap();
    link(&d.outside.join("secret.txt"), &d.workspace.join("innocent.txt"));

    let ctx = ToolContext::new(d.workspace.clone());
    let err = read(&ctx, "innocent.txt").await.unwrap_err().to_string();

    assert!(err.contains("outside the workspace"), "{err}");
    assert!(err.contains("through a symlink"), "the message must say what actually happened: {err}");
    assert!(err.contains("secret.txt"), "and where it really led: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_directory_does_not_plant_files_outside_the_workspace() {
    let d = dirs();
    link(&d.outside, &d.workspace.join("door"));

    let ctx = ToolContext::new(d.workspace.clone());
    assert!(write(&ctx, "door/planted.txt").await.is_err());
    assert!(!d.outside.join("planted.txt").exists(), "the write must not land outside");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_that_stays_inside_the_workspace_still_works() {
    let d = dirs();
    std::fs::create_dir_all(d.workspace.join("real")).unwrap();
    std::fs::write(d.workspace.join("real/notes.txt"), "kept").unwrap();
    link(&d.workspace.join("real"), &d.workspace.join("shortcut"));

    let ctx = ToolContext::new(d.workspace.clone());
    let out = read(&ctx, "shortcut/notes.txt").await.unwrap();

    assert!(out.contains("kept"), "containment is about where it leads, not how: {out}");
}

#[cfg(unix)]
#[tokio::test]
async fn widening_the_sandbox_deliberately_opens_the_door_again() {
    let d = dirs();
    std::fs::write(d.outside.join("secret.txt"), "the private key").unwrap();
    link(&d.outside.join("secret.txt"), &d.workspace.join("innocent.txt"));

    let mut ctx = ToolContext::new(d.workspace.clone());
    ctx.allow_outside_workspace = true;

    assert!(read(&ctx, "innocent.txt").await.unwrap().contains("the private key"));
}

#[tokio::test]
async fn a_workspace_reached_through_a_symlink_contains_its_own_files() {
    // macOS gives `/tmp` as a symlink to `/private/tmp`, so a workspace spelled
    // one way and a file resolved the other must still match.
    let d = dirs();
    std::fs::write(d.workspace.join("here.txt"), "inside").unwrap();

    let ctx = ToolContext::new(d.workspace.clone());
    assert!(read(&ctx, "here.txt").await.unwrap().contains("inside"));

    let spelled_differently = ToolContext::new(d.workspace.join("sub/..").to_path_buf());
    assert!(read(&spelled_differently, "here.txt").await.unwrap().contains("inside"));
}

#[tokio::test]
async fn plain_parent_traversal_is_still_refused() {
    let d = dirs();
    std::fs::write(d.outside.join("secret.txt"), "the private key").unwrap();

    let ctx = ToolContext::new(d.workspace.clone());
    assert!(read(&ctx, "../out/secret.txt").await.is_err());
    assert!(read(&ctx, &d.outside.join("secret.txt").display().to_string()).await.is_err());
}
