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

/// `#[cfg(unix)]` for the command, not for the claim: `;` does not separate
/// commands in `cmd.exe` and `>&2` is spelled differently. What is asserted —
/// that stderr is kept and labelled — is platform-independent Rust.
#[cfg(unix)]
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

/// Unix-only for `yes` and `head`, which is how four megabytes get produced
/// without writing a file. The cap they exercise is in shared code.
#[cfg(unix)]
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

/// Unix-only for its `for` loop and `$(seq)`. The truncation itself is
/// asserted on every platform by `the_middle_is_what_goes_when_output_is_elided`.
#[cfg(unix)]
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

    // One `echo`, not three chained with `;`: `cmd.exe` does not chain on `;`,
    // so there it would echo the rest of the line as text — and every assertion
    // below would still hold, on output that never came from three commands.
    let out = run(&ctx, serde_json::json!({"command": "echo one two three"})).await;

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

/// Unix-only for `yes` and `head`, which is how eight megabytes get produced
/// without writing a file. The bound it proves is in shared code.
#[cfg(unix)]
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

/// stdout was drained to EOF before stderr was read at all. A command that
/// fills the stderr pipe buffer — a build with warnings does it easily — blocks
/// writing to it, so it never finishes writing stdout, so the drain never ends.
#[cfg(unix)]
#[tokio::test]
async fn a_command_that_writes_a_lot_to_stderr_does_not_deadlock() {
    let (_dir, ctx) = ctx();
    let started = std::time::Instant::now();

    let out = run(
        &ctx,
        serde_json::json!({
            "command": "head -c 200000 /dev/zero | tr '\\0' 'x' >&2; echo done",
            "timeout_secs": 20
        }),
    )
    .await;

    assert!(started.elapsed() < std::time::Duration::from_secs(10), "it deadlocked until the timeout");
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("done"), "stdout survived: {}", out.content);
}

/// A build that times out has usually printed the very thing worth reading, and
/// the whole capture was dropped with the future that held it.
#[cfg(unix)]
#[tokio::test]
async fn a_timeout_keeps_what_the_command_had_already_printed() {
    let (_dir, ctx) = ctx();

    let out = run(&ctx, serde_json::json!({ "command": "echo starting; sleep 30", "timeout_secs": 1 })).await;

    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("starting"), "what it printed first is the useful part: {}", out.content);
    assert!(out.content.contains("timeout_secs"), "and it says how to allow longer: {}", out.content);
}

/// The shell gets the line as written.
///
/// On Windows a command is handed to `cmd /C`, and quoting it for the C runtime
/// — which is what `Command::arg` does — escapes an embedded `"` as `\"`. Nothing
/// in `cmd.exe` reads that: the backslash arrives as a backslash, and a command
/// with a quotation mark in it, which is most of them, runs as something else.
#[tokio::test]
async fn a_quoted_command_reaches_the_shell_unmangled() {
    let (_d, ctx) = ctx();
    let out = run(&ctx, serde_json::json!({"command": "echo \"quoted words\""})).await;

    assert!(out.content.contains("quoted words"), "{}", out.content);
    assert!(
        !out.content.contains("\\\""),
        "the shell was handed C-runtime escaping it does not read: {}",
        out.content
    );
}

/// The tail carries how a run ended and the head carries a compiler's first
/// error, which is the one that caused the rest. Asserted here rather than only
/// through a shell loop, so the claim is checked on every platform.
#[test]
fn the_middle_is_what_goes_when_output_is_elided() {
    let text = format!("THE_START\n{}\nTHE_END\n", "filler line\n".repeat(4_000));
    assert!(text.len() > 40_000, "the input has to exceed the budget below: {} bytes", text.len());

    let elided = rook_tools::elide_middle(&text, 2_000);

    assert!(elided.len() < 2_500, "the budget is what bounds it: {} bytes", elided.len());
    assert!(elided.starts_with("THE_START"), "the head is kept: {elided:.80}");
    assert!(elided.trim_end().ends_with("THE_END"), "and so is the tail");
    assert!(elided.contains("elided from the middle"), "and the gap says so");
}
