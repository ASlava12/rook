//! Running commands. The guards here are the ones that keep a single turn from
//! taking down the machine, so each is asserted rather than assumed.

use rook_tools::{Tool, ToolContext, ToolOutcome, exec::RunCommand};

fn ctx() -> (tempfile::TempDir, ToolContext) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    (dir, ctx)
}

async fn run(ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
    RunCommand.call(ctx, &args).await.unwrap()
}

/// Counts only real `sleep` processes: matching a whole command line would also
/// match this test's own, which is how the first attempt at this fooled itself.
#[cfg(unix)]
fn sleepers(marker: &str) -> usize {
    let out = std::process::Command::new("ps").args(["-Ao", "command="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("sleep ") && line.contains(marker))
        .count()
}

#[tokio::test]
async fn a_command_reports_its_exit_code_and_output() {
    let (_d, ctx) = ctx();
    let out = run(&ctx, serde_json::json!({"command": "echo hello"})).await;

    assert!(!out.is_error);
    assert!(out.content.starts_with("exit 0\n"), "{}", out.content);
    assert!(out.content.contains("hello"));
}

#[tokio::test]
async fn a_failing_command_is_an_error_with_its_stderr() {
    let (_d, ctx) = ctx();
    let out = run(&ctx, serde_json::json!({"command": "echo oops >&2; exit 3"})).await;

    assert!(out.is_error);
    assert!(out.content.starts_with("exit 3\n"), "{}", out.content);
    assert!(out.content.contains("--- stderr ---"), "{}", out.content);
    assert!(out.content.contains("oops"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_timeout_kills_the_whole_process_tree_not_just_the_shell() {
    let (_d, ctx) = ctx();
    // Backgrounded so the shell forks rather than execs: killing the shell alone
    // would leave this running, which is what the claim "was killed" would then
    // be lying about.
    let out = run(&ctx, serde_json::json!({"command": "sleep 771771 & wait", "timeout_secs": 1})).await;

    assert!(out.is_error);
    assert_eq!(out.meta["timed_out"], true);
    assert!(out.content.contains("was killed"), "{}", out.content);

    for _ in 0..50 {
        if sleepers("771771") == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    let _ = std::process::Command::new("pkill").args(["-f", "771771"]).status();
    panic!("the command outlived the timeout that said it had been killed");
}

#[tokio::test]
async fn output_larger_than_memory_would_allow_is_bounded_while_it_is_read() {
    let (_d, mut ctx) = ctx();
    ctx.max_output_bytes = 4096;

    let out =
        run(&ctx, serde_json::json!({"command": "yes abcdefghij | head -c 4000000", "timeout_secs": 60}))
            .await;

    assert!(out.content.len() < 20_000, "kept {} bytes for a 4 KiB cap", out.content.len());
    assert!(out.truncated);
    assert_eq!(out.full_bytes, 4_000_000, "what the command actually produced is still reported");
    assert!(out.content.contains("elided"), "{}", &out.content[..120.min(out.content.len())]);
}

#[tokio::test]
async fn both_ends_survive_truncation_and_the_middle_goes() {
    let (_d, mut ctx) = ctx();
    ctx.max_output_bytes = 512;

    let out = run(
        &ctx,
        serde_json::json!({"command": "echo THE_START; for i in $(seq 1 4000); do echo line $i; done; echo THE_END"}),
    )
    .await;

    assert!(out.truncated);
    assert!(out.content.contains("THE_START"), "a compiler's first error is at the head");
    assert!(out.content.contains("THE_END"), "a run says how it ended at the tail");
    assert!(out.content.contains("elided from the middle"), "{}", &out.content[..200]);
    assert!(!out.content.contains("line 2000\n"), "the middle is what goes");
    assert!(out.content.len() <= 700, "the budget still holds: {} bytes", out.content.len());
}

#[tokio::test]
async fn output_that_fits_is_not_touched() {
    let (_d, mut ctx) = ctx();
    ctx.max_output_bytes = 4096;

    let out = run(&ctx, serde_json::json!({"command": "echo one; echo two; echo three"})).await;

    assert!(!out.truncated);
    assert!(!out.content.contains("elided"), "{}", out.content);
    assert!(out.content.contains("one") && out.content.contains("three"));
}

#[tokio::test]
async fn a_cwd_outside_the_workspace_is_refused() {
    let (_d, ctx) = ctx();
    let outside = tempfile::tempdir().unwrap();

    let err = RunCommand
        .call(&ctx, &serde_json::json!({"command": "pwd", "cwd": outside.path().display().to_string()}))
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("outside the workspace"), "{err}");
}

#[tokio::test]
async fn a_runaway_command_costs_bounded_memory_and_still_shows_both_ends() {
    let (_d, mut ctx) = ctx();
    ctx.max_output_bytes = 2048;

    let out = run(
        &ctx,
        serde_json::json!({
            "command": "echo FIRST_LINE; yes padding-padding-padding | head -c 8000000; echo; echo LAST_LINE",
            "timeout_secs": 120
        }),
    )
    .await;

    assert!(out.content.contains("FIRST_LINE"), "the head survived eight megabytes");
    assert!(out.content.contains("LAST_LINE"), "so did the tail");
    assert!(out.content.len() < 8_000, "and the reply stayed small: {} bytes", out.content.len());
    assert!(out.full_bytes > 8_000_000, "what was produced is still reported");
}
