//! The workspace boundary. Lexical containment is not containment: a symlink
//! inside the workspace that points out of it reads and writes outside while
//! every path involved still looks contained.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use rook_tools::{Tool, ToolContext, files};

struct Dirs {
    workspace: PathBuf,
    outside: PathBuf,
}

fn dirs(name: &str) -> Dirs {
    let root = std::env::temp_dir().join(format!("rook-workspace-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let dirs = Dirs { workspace: root.join("ws"), outside: root.join("out") };
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
    let d = dirs("read");
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
    let d = dirs("write");
    link(&d.outside, &d.workspace.join("door"));

    let ctx = ToolContext::new(d.workspace.clone());
    assert!(write(&ctx, "door/planted.txt").await.is_err());
    assert!(!d.outside.join("planted.txt").exists(), "the write must not land outside");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_that_stays_inside_the_workspace_still_works() {
    let d = dirs("inside");
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
    let d = dirs("allowed");
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
    let d = dirs("spelling");
    std::fs::write(d.workspace.join("here.txt"), "inside").unwrap();

    let ctx = ToolContext::new(d.workspace.clone());
    assert!(read(&ctx, "here.txt").await.unwrap().contains("inside"));

    let spelled_differently = ToolContext::new(d.workspace.join("sub/..").to_path_buf());
    assert!(read(&spelled_differently, "here.txt").await.unwrap().contains("inside"));
}

#[tokio::test]
async fn plain_parent_traversal_is_still_refused() {
    let d = dirs("traversal");
    std::fs::write(d.outside.join("secret.txt"), "the private key").unwrap();

    let ctx = ToolContext::new(d.workspace.clone());
    assert!(read(&ctx, "../out/secret.txt").await.is_err());
    assert!(read(&ctx, &d.outside.join("secret.txt").display().to_string()).await.is_err());
}
