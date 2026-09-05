//! Editing is where a wrong guess costs the most: a replacement in the wrong
//! place is a silent corruption, and a half-applied batch is worse than none.

use rook_tools::{
    Tool, ToolContext, ToolOutcome,
    files::{DeleteFile, EditFile, ReadFile, WriteFile},
};

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

/// Approving a write shown only a path is approving something you cannot see,
/// which is most of the value of being asked.
#[tokio::test]
async fn what_an_edit_would_change_can_be_seen_before_it_is_approved() {
    let f = File::with("let a = 1;\nlet b = 2;\n");
    let args = serde_json::json!({"path": "f.rs", "edits": [{"old": "let b = 2;", "new": "let b = 3;"}]});

    let preview = EditFile.preview(&f.ctx, &args).await.expect("an edit can say what it would change");

    assert!(preview.contains("-let b = 2;"), "the line that goes: {preview}");
    assert!(preview.contains("+let b = 3;"), "and the one that arrives: {preview}");
    assert_eq!(f.contents(), "let a = 1;\nlet b = 2;\n", "and nothing is written to say it");
}

/// The preview is the edits themselves, applied to a copy — anything else is an
/// approval of something other than what happens.
#[tokio::test]
async fn a_preview_of_edits_that_cannot_apply_promises_nothing() {
    let f = File::with("let a = 1;\n");
    let args = serde_json::json!({"path": "f.rs", "edits": [{"old": "not there", "new": "x"}]});

    assert!(EditFile.preview(&f.ctx, &args).await.is_none(), "there is no change to show");
}

/// A rename across five files was five calls, and a failure on the third left
/// two of them changed — the rule this already kept within one file, dropped at
/// the file boundary.
#[tokio::test]
async fn a_refactor_across_several_files_lands_whole_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.rs", "b.rs"] {
        std::fs::write(dir.path().join(name), "use old_name;\n").unwrap();
    }
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();

    let out = EditFile
        .call(
            &ctx,
            &serde_json::json!({"files": [
                {"path": "a.rs", "edits": [{"old": "old_name", "new": "new_name"}]},
                {"path": "b.rs", "edits": [{"old": "not there", "new": "x"}]}
            ]}),
        )
        .await
        .unwrap();

    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("any other file"), "and says so: {}", out.content);
    assert_eq!(read("a.rs"), "use old_name;\n", "the file before the failure must be untouched");

    let out = EditFile
        .call(
            &ctx,
            &serde_json::json!({"files": [
                {"path": "a.rs", "edits": [{"old": "old_name", "new": "new_name"}]},
                {"path": "b.rs", "edits": [{"old": "old_name", "new": "new_name"}]}
            ]}),
        )
        .await
        .unwrap();

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(read("a.rs"), "use new_name;\n");
    assert_eq!(read("b.rs"), "use new_name;\n");
    assert_eq!(out.meta.get("occurrences"), Some(&serde_json::json!(2)), "{:?}", out.meta);
}

/// Each entry reads the file on its own, so two for the same file would both
/// start from the original and the second write would undo the first.
#[tokio::test]
async fn one_file_named_twice_is_refused_rather_than_written_twice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one two\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());

    let refused = EditFile
        .call(
            &ctx,
            &serde_json::json!({"files": [
                {"path": "a.rs", "edits": [{"old": "one", "new": "1"}]},
                {"path": "a.rs", "edits": [{"old": "two", "new": "2"}]}
            ]}),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(refused.contains("appears twice"), "{refused}");
    assert!(refused.contains("one entry"), "and says what to do instead: {refused}");
    assert_eq!(std::fs::read_to_string(dir.path().join("a.rs")).unwrap(), "one two\n");
}

/// Both files are captured before either is written, and both are shown before
/// either is approved.
#[test]
fn a_refactor_names_every_file_it_would_touch() {
    let args = serde_json::json!({"files": [
        {"path": "a.rs", "edits": [{"old": "x", "new": "y"}]},
        {"path": "b.rs", "edits": [{"old": "x", "new": "y"}]}
    ]});
    assert_eq!(EditFile.touched_paths(&args), vec!["a.rs".to_string(), "b.rs".to_string()]);
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

/// The empty string matches between every pair of characters. With
/// `replace_all` that interleaved the replacement through the whole file and
/// reported it as a success — and without it, the count said "appears 12 times;
/// add surrounding context or set replace_all", which is an instruction to do
/// exactly that.
#[tokio::test]
async fn an_empty_old_is_refused_rather_than_matched_everywhere() {
    let f = File::with("let a = 1;\n");

    for args in [
        serde_json::json!({"edits": [{"old": "", "new": "// header\n"}]}),
        serde_json::json!({"edits": [{"old": "", "new": "X", "replace_all": true}]}),
    ] {
        let out = f.edit(args).await;
        assert!(out.is_error, "an edit that matches everywhere is not an edit: {}", out.content);
        assert!(out.content.contains("write_file"), "and it names what does work: {}", out.content);
    }
    assert_eq!(f.contents(), "let a = 1;\n", "nothing was written");
}

#[tokio::test]
async fn an_edit_that_would_change_nothing_says_so_instead_of_reporting_a_replacement() {
    let f = File::with("let a = 1;\n");

    let out = f.edit(serde_json::json!({"edits": [{"old": "let a = 1;", "new": "let a = 1;"}]})).await;

    assert!(out.is_error, "a step that did nothing must not read as progress: {}", out.content);
    assert_eq!(f.contents(), "let a = 1;\n");
}

/// A model handed `files` a list of path strings and was told `path` was
/// missing from an argument it had just written three times. The message
/// names what was actually wrong: the shape of the entry.
#[tokio::test]
async fn a_files_entry_that_is_not_an_object_is_named_by_what_it_is() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "port = 8080\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let err = EditFile.call(&ctx, &serde_json::json!({ "files": ["a.txt"] })).await.unwrap_err().to_string();
    assert!(err.contains("each entry of `files` is an object"), "{err}");
    assert!(err.contains("not a string"), "says what it got instead: {err}");
    assert!(!err.contains("\"path\" is missing"), "and not the misleading thing: {err}");
}

/// The two strings under the names another tool taught the model land the same
/// edit; a refusal for the spelling of a field is a refusal of nothing.
#[tokio::test]
async fn an_edit_spelled_from_and_to_lands() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    EditFile
        .call(&ctx, &serde_json::json!({ "path": "config.rs", "edits": [{ "from": "8443", "to": "9000" }] }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.rs")).unwrap(),
        "pub const PORT: u16 = 9000;\n"
    );

    EditFile
        .call(&ctx, &serde_json::json!({ "path": "config.rs", "edits": [{ "old_string": "9000", "new_string": "9001" }] }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.rs")).unwrap(),
        "pub const PORT: u16 = 9001;\n"
    );
}

/// A list of paths beside one `edits` means those edits in each file. It is
/// the shape a model wrote when told to change one thing in one file, and
/// refusing it taught it nothing.
#[tokio::test]
async fn a_list_of_paths_beside_one_edits_applies_them_to_each() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.toml", "b.toml"] {
        std::fs::write(dir.path().join(name), "port = 8080\n").unwrap();
    }
    let ctx = ToolContext::new(dir.path().to_path_buf());
    EditFile
        .call(&ctx, &serde_json::json!({ "files": ["a.toml", "b.toml"], "edits": [{ "old": "8080", "new": "9000" }] }))
        .await
        .unwrap();
    for name in ["a.toml", "b.toml"] {
        assert_eq!(std::fs::read_to_string(dir.path().join(name)).unwrap(), "port = 9000\n", "{name}");
    }

    // The same path twice is still the mistake it was.
    let err = EditFile
        .call(
            &ctx,
            &serde_json::json!({ "files": ["a.toml", "a.toml"], "edits": [{ "old": "9000", "new": "1" }] }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("appears twice"), "{err}");
}

/// Text that is not in the file is refused with the nearest line shown: a
/// model that guessed a value is told what is there, not only that it is not.
/// The refusal is a tool outcome the model reads, not an error in the arguments.
#[tokio::test]
async fn text_not_in_the_file_is_refused_with_the_nearest_line() {
    let dir = tempfile::tempdir().unwrap();
    let file = "pub const HOST: &str = \"::1\";\npub const PORT: u16 = 8443;\n";
    std::fs::write(dir.path().join("config.rs"), file).unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let guessed = serde_json::json!({
        "path": "config.rs",
        "edits": [{ "old": "pub const PORT: u16 = 9001;", "new": "pub const PORT: u16 = 9000;" }]
    });
    let refused = EditFile.call(&ctx, &guessed).await.unwrap();
    assert!(refused.is_error, "{}", refused.content);
    assert!(refused.content.contains("not in the file"), "{}", refused.content);
    assert!(
        refused.content.contains("The nearest line is: pub const PORT: u16 = 8443;"),
        "{}",
        refused.content
    );

    let unlike =
        serde_json::json!({ "path": "config.rs", "edits": [{ "old": "nothing like it", "new": "x" }] });
    let refused = EditFile.call(&ctx, &unlike).await.unwrap();
    assert!(refused.is_error);
    assert!(
        !refused.content.contains("nearest"),
        "no line shares a word, so none is offered: {}",
        refused.content
    );
}

/// A bare literal shares no word with any line, so the nearest-line rule finds
/// nothing and the refusal fell back to "read it again" — which is the advice
/// a model has already taken by the time it writes the same edit a second
/// time. Twice in two smoke runs one asked to replace `9001` in a file it had
/// read as `8443`, was told to read it again, read it, and wrote the identical
/// edit. What it needed was the line with a number on it.
#[tokio::test]
async fn a_value_that_is_not_there_is_answered_with_the_values_that_are() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.rs"),
        "pub const PORT: u16 = 8443;\npub const HOST: &str = \"::1\";\n",
    )
    .unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());

    let guessed = serde_json::json!({ "path": "config.rs", "edits": [{ "old": "9001", "new": "9000" }] });
    let refused = EditFile.call(&ctx, &guessed).await.unwrap();

    assert!(refused.is_error, "{}", refused.content);
    assert!(
        refused.content.contains("8443"),
        "the value that is there has to be in the refusal: {}",
        refused.content
    );
    assert!(
        !refused.content.contains("read it again"),
        "and it must not send the model back to a file it has read: {}",
        refused.content
    );
}

/// A model that means `src/main.rs` and writes `src` gets `Is a directory (os
/// error 21)` from the operating system — which names no argument, suggests
/// nothing, and reads as the tool being broken. Every tool that takes one file
/// says the same thing instead, and says where to look.
#[tokio::test]
async fn a_directory_where_a_file_goes_is_refused_in_words() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let at_a_directory = serde_json::json!({
        "path": "src",
        "content": "x",
        "edits": [{ "old": "a", "new": "b" }],
    });

    for (tool, name) in [
        (Box::new(EditFile) as Box<dyn Tool>, "edit_file"),
        (Box::new(WriteFile), "write_file"),
        (Box::new(ReadFile), "read_file"),
        (Box::new(DeleteFile), "delete_file"),
    ] {
        let refused = tool.call(&ctx, &at_a_directory).await.expect("a bad argument is not a crash");
        assert!(refused.is_error, "{name} accepted a directory: {}", refused.content);
        assert!(
            refused.content.contains("is a directory") && !refused.content.contains("os error"),
            "{name} must say what to do, not what errno it got: {}",
            refused.content
        );
    }
    assert!(dir.path().join("src").is_dir(), "and none of them touched it");
}

/// A model wrote `{path, replacement: <the whole class>}` inside `edits` and
/// was told "missing field `old`", so it wrote the same thing again — four
/// minutes of a turn on a shape that says plainly what it meant. An entry with
/// the new text and no `old` is a whole file, which is a different tool.
#[tokio::test]
async fn an_edit_that_is_really_a_whole_file_is_told_which_tool_that_is() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.py"), "class Mover(object):\n    pass\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    // The call as it was written: a path at the top, and inside `edits` the
    // path again with the new text beside it.
    let whole = serde_json::json!({
        "path": "f.py",
        "edits": [{ "path": "f.py", "replacement": "class Mover(object):\n    dump_dir = '/tmp'\n" }]
    });

    let refused = EditFile.call(&ctx, &whole).await.unwrap_err().to_string();

    assert!(refused.contains("write_file"), "it names the tool that does this: {refused}");
    assert!(refused.contains("replacement"), "and the field that gave it away: {refused}");
    assert!(!refused.contains("missing field"), "rather than serde's account of it: {refused}");
}

/// An entry that has both is an edit whose `new` was spelled unusually, and
/// that is a different thing from a file.
#[tokio::test]
async fn an_edit_with_an_old_is_still_an_edit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.py"), "port = 8443\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let named = serde_json::json!({ "path": "f.py", "edits": [{ "from": "8443", "to": "9000" }] });

    let done = EditFile.call(&ctx, &named).await.unwrap();

    assert!(!done.is_error, "{}", done.content);
    assert_eq!(std::fs::read_to_string(dir.path().join("f.py")).unwrap(), "port = 9000\n");
}
