//! Running a command somewhere the user can see it.
//!
//! The tool has to report the same thing either way — exit code, output, and
//! whether it was cut — because the model cannot tell where the command ran and
//! should not have to.

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use rook_tools::{Ran, Terminals, Tool, ToolContext, exec::RunCommand};

struct Panel {
    ran: Mutex<Vec<String>>,
    answer: Ran,
}

#[async_trait]
impl Terminals for Panel {
    async fn run(&self, command: &str, _cwd: &Path, _limit: usize) -> rook_tools::Result<Ran> {
        self.ran.lock().unwrap().push(command.to_string());
        Ok(Ran {
            output: self.answer.output.clone(),
            exit_code: self.answer.exit_code,
            truncated: self.answer.truncated,
            timed_out: self.answer.timed_out,
        })
    }
}

fn with(answer: Ran) -> (tempfile::TempDir, ToolContext, std::sync::Arc<Panel>) {
    let dir = tempfile::tempdir().unwrap();
    let panel = std::sync::Arc::new(Panel { ran: Default::default(), answer });
    let mut ctx = ToolContext::new(dir.path().to_path_buf());
    ctx.terminals = Some(panel.clone());
    (dir, ctx, panel)
}

fn ran(output: &str, exit_code: i32) -> Ran {
    Ran { output: output.into(), exit_code, truncated: false, timed_out: false }
}

#[tokio::test]
async fn the_command_goes_to_the_panel_and_its_answer_comes_back() {
    let (_d, ctx, panel) = with(ran("42 tests passed\n", 0));

    let out = RunCommand.call(&ctx, &serde_json::json!({"command": "make test"})).await.unwrap();

    assert_eq!(panel.ran.lock().unwrap().as_slice(), ["make test"]);
    assert!(!out.is_error);
    assert_eq!(out.content, "exit 0\n42 tests passed\n");
}

#[tokio::test]
async fn a_failing_command_is_an_error_wherever_it_ran() {
    let (_d, ctx, _) = with(ran("undefined reference\n", 2));

    let out = RunCommand.call(&ctx, &serde_json::json!({"command": "make"})).await.unwrap();

    assert!(out.is_error);
    assert!(out.content.starts_with("exit 2\n"), "{}", out.content);
    assert_eq!(out.meta["exit_code"], 2);
}

#[tokio::test]
async fn a_timeout_says_so_rather_than_reporting_an_exit() {
    let (_d, ctx, _) = with(Ran { output: String::new(), exit_code: -1, truncated: false, timed_out: true });

    let out =
        RunCommand.call(&ctx, &serde_json::json!({"command": "sleep 900", "timeout_secs": 1})).await.unwrap();

    assert!(out.is_error);
    assert_eq!(out.meta["timed_out"], true);
    assert!(out.content.contains("timed out after 1s"), "{}", out.content);
}

#[tokio::test]
async fn truncation_by_the_panel_is_reported_as_truncation() {
    let (_d, ctx, _) =
        with(Ran { output: "…the tail\n".into(), exit_code: 0, truncated: true, timed_out: false });

    let out = RunCommand.call(&ctx, &serde_json::json!({"command": "yes"})).await.unwrap();

    assert!(out.truncated, "the model is told, even though the cut was not ours to make");
}

#[tokio::test]
async fn a_cwd_outside_the_workspace_never_reaches_the_panel() {
    let (_d, ctx, panel) = with(ran("", 0));
    let outside = tempfile::tempdir().unwrap();

    let err = RunCommand
        .call(&ctx, &serde_json::json!({"command": "pwd", "cwd": outside.path().display().to_string()}))
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("outside the workspace"), "{err}");
    assert!(panel.ran.lock().unwrap().is_empty(), "the boundary is checked first");
}
