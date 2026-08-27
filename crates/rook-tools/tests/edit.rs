//! Editing is where a wrong guess costs the most: a replacement in the wrong
//! place is a silent corruption, and a half-applied batch is worse than none.

use rook_tools::{Tool, ToolContext, ToolOutcome, files::EditFile};

struct File {
    dir: tempfile::TempDir,
    ctx: ToolContext,
}

impl File {
    fn with(contents: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.rs"), contents).unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        Self { dir, ctx }
    }

    fn contents(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("f.rs")).unwrap()
    }

    async fn edit(&self, args: serde_json::Value) -> ToolOutcome {
        let mut args = args;
        args["path"] = "f.rs".into();
        EditFile.call(&self.ctx, &args).await.unwrap()
    }
}

#[tokio::test]
async fn edits_apply_in_order_each_seeing_the_last() {
    let f = File::with("let a = 1;\nlet b = 2;\n");

    let out = f
        .edit(serde_json::json!({"edits": [
            {"old": "let a = 1;", "new": "let a = 10;"},
            {"old": "let a = 10;", "new": "const A: i32 = 10;"}
        ]}))
        .await;

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(f.contents(), "const A: i32 = 10;\nlet b = 2;\n");
}

#[tokio::test]
async fn a_batch_that_cannot_finish_writes_nothing() {
    let f = File::with("let a = 1;\nlet b = 2;\n");

    let out = f
        .edit(serde_json::json!({"edits": [
            {"old": "let a = 1;", "new": "let a = 10;"},
            {"old": "let c = 3;", "new": "let c = 30;"}
        ]}))
        .await;

    assert!(out.is_error);
    assert!(out.content.contains("edit 2 of 2"), "it must say which one: {}", out.content);
    assert!(out.content.contains("Nothing was written"), "{}", out.content);
    assert_eq!(f.contents(), "let a = 1;\nlet b = 2;\n", "the first edit must not survive");
}

#[tokio::test]
async fn an_ambiguous_edit_is_refused_rather_than_guessed() {
    let f = File::with("x = 1;\nx = 1;\n");

    let out = f.edit(serde_json::json!({"edits": [{"old": "x = 1;", "new": "x = 2;"}]})).await;

    assert!(out.is_error);
    assert!(out.content.contains("appears 2 times"), "{}", out.content);
    assert_eq!(f.contents(), "x = 1;\nx = 1;\n");
}

#[tokio::test]
async fn replace_all_takes_every_occurrence() {
    let f = File::with("x = 1;\nx = 1;\n");

    let out =
        f.edit(serde_json::json!({"edits": [{"old": "x = 1;", "new": "x = 2;", "replace_all": true}]})).await;

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(f.contents(), "x = 2;\nx = 2;\n");
    assert_eq!(out.meta["occurrences"], 2);
}

#[tokio::test]
async fn an_edit_that_becomes_ambiguous_only_after_an_earlier_one_is_still_caught() {
    let f = File::with("a = 1;\nb = 2;\n");

    let out = f
        .edit(serde_json::json!({"edits": [
            {"old": "b = 2;", "new": "a = 1;"},
            {"old": "a = 1;", "new": "c = 3;"}
        ]}))
        .await;

    assert!(out.is_error, "uniqueness is checked against the text as it stands: {}", out.content);
    assert_eq!(f.contents(), "a = 1;\nb = 2;\n");
}

#[tokio::test]
async fn the_single_edit_shape_still_works() {
    let f = File::with("let a = 1;\n");

    let out = f.edit(serde_json::json!({"old": "let a = 1;", "new": "let a = 2;"})).await;

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(f.contents(), "let a = 2;\n");
}

#[tokio::test]
async fn asking_for_no_edits_says_so() {
    let f = File::with("x\n");
    let err = EditFile
        .call(&f.ctx, &serde_json::json!({"path": "f.rs", "edits": []}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing to change"), "{err}");

    let err = EditFile.call(&f.ctx, &serde_json::json!({"path": "f.rs"})).await.unwrap_err().to_string();
    assert!(err.contains("edits: [{old, new}]"), "{err}");
}

#[tokio::test]
async fn a_file_outside_the_workspace_is_refused_before_it_is_read() {
    let f = File::with("x\n");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "x").unwrap();

    let err = EditFile
        .call(
            &f.ctx,
            &serde_json::json!({
                "path": outside.path().join("secret").display().to_string(),
                "edits": [{"old": "x", "new": "y"}]
            }),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("outside the workspace"), "{err}");
}
