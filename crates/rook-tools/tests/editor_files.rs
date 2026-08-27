//! Reading and writing through whatever owns the files.
//!
//! An editor holds buffers the disk has never seen. An agent that reads around
//! them sees the file as it was before the user's last change and edits it
//! back, so every file tool has to go through the same door.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use rook_tools::{Files, Tool, ToolContext, files};

#[derive(Default)]
struct Buffers {
    contents: Mutex<HashMap<String, String>>,
    reads: Mutex<Vec<String>>,
}

#[async_trait]
impl Files for Buffers {
    async fn read(&self, path: &Path) -> rook_tools::Result<String> {
        let key = path.display().to_string();
        self.reads.lock().unwrap().push(key.clone());
        self.contents.lock().unwrap().get(&key).cloned().ok_or_else(|| rook_tools::ToolError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such buffer"),
        })
    }

    async fn write(&self, path: &Path, contents: &str) -> rook_tools::Result<()> {
        self.contents.lock().unwrap().insert(path.display().to_string(), contents.to_string());
        Ok(())
    }
}

struct Workspace {
    dir: tempfile::TempDir,
    ctx: ToolContext,
    buffers: std::sync::Arc<Buffers>,
}

/// Paths reach the provider as the boundary check resolved them — through
/// symlinks, which on macOS turns `/var` into `/private/var`. The truthful path
/// is the one to key a buffer by.
fn resolved(dir: &Path) -> String {
    dir.canonicalize().unwrap().join("f.rs").display().to_string()
}

impl Workspace {
    /// The same file on disk and in a buffer, saying different things — which is
    /// what an unsaved edit looks like.
    fn new(on_disk: &str, in_buffer: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.rs"), on_disk).unwrap();
        let buffers = std::sync::Arc::new(Buffers::default());
        if let Some(text) = in_buffer {
            buffers.contents.lock().unwrap().insert(resolved(dir.path()), text.to_string());
        }
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.files = Some(buffers.clone());
        Self { dir, ctx, buffers }
    }

    fn on_disk(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("f.rs")).unwrap()
    }

    fn in_buffer(&self) -> Option<String> {
        self.buffers.contents.lock().unwrap().get(&resolved(self.dir.path())).cloned()
    }
}

#[tokio::test]
async fn reading_sees_the_buffer_rather_than_the_file() {
    let w = Workspace::new("let a = 1;\n", Some("let a = 2; // not saved yet\n"));

    let out = files::ReadFile.call(&w.ctx, &serde_json::json!({"path": "f.rs"})).await.unwrap();

    assert!(out.content.contains("not saved yet"), "{}", out.content);
    assert!(!out.content.contains("let a = 1;"), "the stale disk copy must not win: {}", out.content);
    assert_eq!(w.buffers.reads.lock().unwrap().len(), 1, "and it must have asked");
}

#[tokio::test]
async fn writing_goes_to_the_buffer_and_leaves_the_file_alone() {
    let w = Workspace::new("let a = 1;\n", Some("let a = 1;\n"));

    files::WriteFile
        .call(&w.ctx, &serde_json::json!({"path": "f.rs", "content": "let a = 3;\n"}))
        .await
        .unwrap();

    assert_eq!(w.in_buffer().unwrap(), "let a = 3;\n");
    assert_eq!(w.on_disk(), "let a = 1;\n", "the editor owns saving, not the agent");
}

#[tokio::test]
async fn editing_reads_and_writes_the_same_buffer() {
    let w = Workspace::new("let a = 1;\n", Some("let a = 2; // unsaved\n"));

    let out = files::EditFile
        .call(
            &w.ctx,
            &serde_json::json!({"path": "f.rs", "edits": [{"old": "let a = 2;", "new": "let a = 4;"}]}),
        )
        .await
        .unwrap();

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(w.in_buffer().unwrap(), "let a = 4; // unsaved\n", "an edit against the disk would miss");
    assert_eq!(w.on_disk(), "let a = 1;\n");
}

#[tokio::test]
async fn a_path_outside_the_workspace_is_still_refused() {
    let w = Workspace::new("x\n", Some("x\n"));
    let outside = tempfile::tempdir().unwrap();

    let err = files::ReadFile
        .call(&w.ctx, &serde_json::json!({"path": outside.path().join("secret").display().to_string()}))
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("outside the workspace"), "the boundary is not the editor's to widen: {err}");
    assert!(w.buffers.reads.lock().unwrap().is_empty(), "and it never got asked");
}

#[tokio::test]
async fn without_a_provider_the_disk_is_still_what_is_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.rs"), "from the disk\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());

    let out = files::ReadFile.call(&ctx, &serde_json::json!({"path": "f.rs"})).await.unwrap();

    assert!(out.content.contains("from the disk"), "{}", out.content);
}
