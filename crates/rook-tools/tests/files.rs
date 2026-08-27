//! What the file tools promise. Every claim in a description is a claim the
//! model relies on: paging that silently returns nothing, or a cap that is not
//! enforced, is worse than a refusal.

use rook_tools::{Tool, ToolContext, ToolOutcome, files};

struct Workspace {
    dir: tempfile::TempDir,
    ctx: ToolContext,
}

impl Workspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        Self { dir, ctx }
    }

    fn file(&self, name: &str, contents: &str) -> &Self {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        self
    }

    async fn read(&self, args: serde_json::Value) -> ToolOutcome {
        files::ReadFile.call(&self.ctx, &args).await.unwrap()
    }

    async fn list(&self, args: serde_json::Value) -> ToolOutcome {
        files::ListDir.call(&self.ctx, &args).await.unwrap()
    }
}

#[tokio::test]
async fn a_file_comes_back_with_line_numbers() {
    let w = Workspace::new();
    w.file("a.txt", "first\nsecond\n");

    let out = w.read(serde_json::json!({"path": "a.txt"})).await;

    assert_eq!(out.content, "     1\tfirst\n     2\tsecond\n");
    assert!(!out.truncated);
    assert_eq!(out.meta["total_lines"], 2);
}

#[tokio::test]
async fn paging_numbers_the_lines_it_actually_returned() {
    let w = Workspace::new();
    w.file("a.txt", &(1..=10).map(|i| format!("line {i}\n")).collect::<String>());

    let out = w.read(serde_json::json!({"path": "a.txt", "offset": 3, "limit": 2})).await;

    assert!(out.content.starts_with("     4\tline 4\n     5\tline 5\n"), "{}", out.content);
    assert!(out.content.contains("5 more lines"), "{}", out.content);
    assert!(out.content.contains("offset=5"), "it must say where to continue: {}", out.content);
    assert!(out.truncated);
}

#[tokio::test]
async fn reading_past_the_end_says_how_long_the_file_is() {
    let w = Workspace::new();
    w.file("a.txt", "only one line\n");

    let out = w.read(serde_json::json!({"path": "a.txt", "offset": 500})).await;

    assert!(out.is_error, "an empty answer would be indistinguishable from an empty file");
    assert!(out.content.contains("has 1 line(s)"), "{}", out.content);
    assert!(out.content.contains("offset 500"), "{}", out.content);
}

#[tokio::test]
async fn one_enormous_line_is_cut_to_the_output_budget() {
    let w = Workspace::new();
    // The shape of a minified bundle: `limit` counts lines, so a line-based cap
    // alone would paste the whole thing into context.
    w.file("bundle.js", &"x".repeat(2 * 1024 * 1024));

    let out = w.read(serde_json::json!({"path": "bundle.js"})).await;

    assert!(out.content.len() <= w.ctx.max_output_bytes, "returned {} bytes", out.content.len());
    assert!(out.truncated, "a cut line is a truncated read");
    assert!(
        out.content.contains("more bytes on this line"),
        "{}",
        &out.content[..200.min(out.content.len())]
    );
    assert_eq!(out.full_bytes, 2 * 1024 * 1024, "what was there is still reported");
}

#[tokio::test]
async fn many_long_lines_stop_at_the_budget_and_say_where_to_resume() {
    let w = Workspace::new();
    let line = "y".repeat(10_000);
    w.file("wide.txt", &(0..100).map(|_| format!("{line}\n")).collect::<String>());

    let out = w.read(serde_json::json!({"path": "wide.txt"})).await;

    assert!(out.content.len() <= w.ctx.max_output_bytes);
    let shown = out.meta["returned_lines"].as_u64().unwrap();
    assert!(shown > 0 && shown < 100, "some but not all: {shown}");
    assert!(out.content.contains(&format!("offset={shown}")), "{}", out.content.lines().last().unwrap());
}

#[tokio::test]
async fn a_binary_file_is_named_rather_than_pasted() {
    let w = Workspace::new();
    w.file("a.bin", "text\0with a nul");

    let out = w.read(serde_json::json!({"path": "a.bin"})).await;

    assert!(out.is_error);
    assert!(out.content.contains("looks binary"), "{}", out.content);
    assert_eq!(out.meta["binary"], true);
}

#[tokio::test]
async fn an_empty_file_reads_as_empty_rather_than_as_an_error() {
    let w = Workspace::new();
    w.file("empty.txt", "");

    let out = w.read(serde_json::json!({"path": "empty.txt"})).await;

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(out.meta["total_lines"], 0);
}

#[tokio::test]
async fn writing_creates_the_parent_directories() {
    let w = Workspace::new();

    let out = files::WriteFile
        .call(&w.ctx, &serde_json::json!({"path": "a/b/c.txt", "content": "hello"}))
        .await
        .unwrap();

    assert!(!out.is_error, "{}", out.content);
    assert_eq!(std::fs::read_to_string(w.dir.path().join("a/b/c.txt")).unwrap(), "hello");
}

#[tokio::test]
async fn listing_marks_directories_and_honours_gitignore() {
    let w = Workspace::new();
    w.file(".gitignore", "target/\n");
    w.file("src/main.rs", "fn main() {}");
    w.file("target/debug/huge", "x");

    let out = w.list(serde_json::json!({"path": "."})).await;

    assert!(out.content.contains("src/"), "{}", out.content);
    assert!(out.content.contains("src/main.rs"), "{}", out.content);
    assert!(!out.content.contains("target"), "an ignored path must not be listed: {}", out.content);
}

#[tokio::test]
async fn listing_caps_and_says_how_much_it_left_out() {
    let w = Workspace::new();
    for i in 0..50 {
        w.file(&format!("f{i}.txt"), "x");
    }

    let out = w.list(serde_json::json!({"path": ".", "limit": 10})).await;

    assert!(out.truncated);
    assert!(out.content.contains("40 more entries"), "{}", out.content);
    assert_eq!(out.meta["entries"], 50);
}

/// `limit: 0` asked for no lines and got an answer saying to call again from
/// where it stopped — which is where it started.
#[tokio::test]
async fn a_page_of_no_lines_is_not_a_page() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());

    let out = files::ReadFile.call(&ctx, &serde_json::json!({ "path": "f.txt", "limit": 0 })).await.unwrap();

    assert!(!out.content.contains("offset=0"), "an answer that says to repeat the call: {}", out.content);
    assert!(out.content.contains('c'), "it pages rather than refusing, so it answers: {}", out.content);
}

/// A model that mistypes a tool name has already spent a step; `unknown tool
/// "read_fil"` spends the next one too.
#[tokio::test]
async fn an_unknown_tool_names_the_ones_it_might_have_meant() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let tools = rook_tools::ToolBox::standard();

    let said = tools.call(&ctx, "read_fil", &serde_json::json!({})).await.unwrap_err().to_string();
    assert!(said.contains("read_fil"), "{said}");
    assert!(said.contains("read_file"), "the one it meant: {said}");

    let unrelated = tools.call(&ctx, "zzzzzzzzzz", &serde_json::json!({})).await.unwrap_err().to_string();
    assert!(!unrelated.contains("did you mean"), "nothing is close, so nothing is offered: {unrelated}");
}
