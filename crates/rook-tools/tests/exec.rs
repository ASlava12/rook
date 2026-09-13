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
    // Measured on the output, which is what the budget governs — not on the
    // advisories appended after it. `[whole output: …]` carries a temp path, and
    // the one that says a command left something running is two hundred
    // characters and fires under load: this asserted on all three together and
    // failed at 828 bytes on a loaded runner, which reads as a broken cap and
    // was a lingering subshell.
    let output = out.content.split("\n[whole output:").next().unwrap_or(&out.content);
    let output = output.split("\n(").next().unwrap_or(output);
    assert!(output.len() <= 700, "the budget still holds: {} bytes", output.len());
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

/// The ends are what the model is shown, and they are also all it had: a line
/// that is neither first nor last was discarded as it streamed.
#[cfg(unix)]
#[tokio::test]
async fn the_middle_of_a_runaway_output_is_kept_where_the_shell_can_reach_it() {
    let (_d, mut ctx) = ctx();
    let kept = tempfile::tempdir().unwrap();
    ctx.max_output_bytes = 2048;
    ctx.spill_dir = Some(kept.path().to_path_buf());
    ctx.max_spill_bytes = 8 << 20;

    let out = run(
        &ctx,
        serde_json::json!({
            "command": "echo FIRST; yes padding | head -c 200000; echo NEEDLE_IN_THE_MIDDLE;                         yes padding | head -c 200000; echo LAST",
            "timeout_secs": 120
        }),
    )
    .await;

    assert!(out.truncated, "the output has to exceed the cap or there is no middle to lose");
    assert!(
        !out.content.contains("NEEDLE_IN_THE_MIDDLE"),
        "and the middle is not in the reply: {}",
        out.content
    );

    let path = out.meta.get("output_file").and_then(|p| p.as_str()).expect("the reply names the file");
    assert!(out.content.contains(path), "and says so where the model will read it: {}", out.content);
    let whole = std::fs::read_to_string(path).unwrap();
    assert!(whole.contains("NEEDLE_IN_THE_MIDDLE"), "the middle is there");
    assert!(whole.contains("FIRST") && whole.contains("LAST"), "and so are the ends");
}

/// A command that prints without end must not fill the disk instead of memory.
#[cfg(unix)]
#[tokio::test]
async fn what_is_kept_of_a_runaway_output_is_itself_bounded() {
    let (_d, mut ctx) = ctx();
    let kept = tempfile::tempdir().unwrap();
    ctx.max_output_bytes = 1024;
    ctx.spill_dir = Some(kept.path().to_path_buf());
    ctx.max_spill_bytes = 64 * 1024;

    let out =
        run(&ctx, serde_json::json!({"command": "yes padding | head -c 4000000", "timeout_secs": 120})).await;

    assert!(
        out.full_bytes >= 4_000_000,
        "the command printed {} bytes, far more than the cap",
        out.full_bytes
    );
    let path = out.meta.get("output_file").and_then(|p| p.as_str()).unwrap();
    let size = std::fs::metadata(path).unwrap().len();
    assert!(size <= 64 * 1024, "the kept copy must stop at the cap, and it is {size} bytes");
    assert!(out.content.contains("max_spill_bytes"), "and must say it stopped: {}", out.content);
}

/// Naming a file that holds exactly what is already on screen sends the model
/// off to read something it has.
#[cfg(unix)]
#[tokio::test]
async fn an_output_that_fits_is_not_also_written_to_a_file() {
    let (_d, mut ctx) = ctx();
    let kept = tempfile::tempdir().unwrap();
    ctx.spill_dir = Some(kept.path().to_path_buf());
    ctx.max_spill_bytes = 8 << 20;

    let out = run(&ctx, serde_json::json!({"command": "echo small"})).await;

    assert!(!out.truncated, "nothing was left out");
    assert!(!out.meta.contains_key("output_file"), "so nothing is named: {:?}", out.meta);
    // And nothing is left behind: a copy of every `echo` ever run is the
    // accumulator the cap exists to prevent.
    assert_eq!(std::fs::read_dir(kept.path()).unwrap().count(), 0, "and no file remains");
}

/// The command that ran until the timeout is the one whose output is most worth
/// having, and the ends of it are the least of it.
#[cfg(unix)]
#[tokio::test]
async fn a_timed_out_command_still_says_where_the_whole_of_its_output_is() {
    let (_d, mut ctx) = ctx();
    let kept = tempfile::tempdir().unwrap();
    ctx.max_output_bytes = 1024;
    ctx.spill_dir = Some(kept.path().to_path_buf());
    ctx.max_spill_bytes = 8 << 20;

    let out = run(
        &ctx,
        serde_json::json!({
            "command": "yes padding | head -c 100000; echo NEEDLE; sleep 30",
            "timeout_secs": 3
        }),
    )
    .await;

    assert_eq!(out.meta.get("timed_out"), Some(&serde_json::json!(true)), "{}", out.content);
    let path = out.meta.get("output_file").and_then(|p| p.as_str()).expect("it names the file");
    assert!(std::fs::read_to_string(path).unwrap().contains("NEEDLE"), "which holds what it printed");
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

/// Ported from cline (#13817, and their #12417 behind it): the shell exits and
/// a child it backgrounded keeps the inherited pipe open, so the read never
/// reaches EOF. Waiting only for the output made a command that finished in
/// milliseconds arrive as one that had to be killed at the timeout — the answer
/// an interactive terminal never gives, because a prompt comes back while the
/// background job keeps printing.
#[cfg(unix)]
#[tokio::test]
async fn a_command_that_leaves_something_running_finishes_when_it_finishes() {
    let (_d, ctx) = ctx();
    // A duration this platform's `sleep` accepts, made unique by the fraction:
    // a marker built as a huge integer is rejected outright by BSD `sleep`,
    // and the test then asserted against its usage message.
    //
    // And nowhere near `771771`, which the timeout test above waits to
    // disappear from the process table: sharing a prefix with it meant that
    // test saw this one's sleep and reported a command that had outlived its
    // timeout, while this one's cleanup killed that test's sleep. Two tests
    // reaching into the same process table need names that cannot match.
    let marker = format!("881881.{}", std::process::id());
    let started = std::time::Instant::now();
    // The timeout is far longer than the sleep this backgrounds, so a timeout
    // cannot be what ends the call — and the sleep outlives the call, which is
    // the precondition: the pipe is still held when the answer is written.
    let out = run(
        &ctx,
        serde_json::json!({
            // Not redirected: inheriting this call's pipes is the whole
            // scenario, and `>/dev/null` on the child is how the first
            // version of this test quietly tested nothing.
            "command": format!("echo done; sleep {marker} &"),
            "timeout_secs": 60
        }),
    )
    .await;
    let took = started.elapsed();

    assert!(!out.is_error, "the command succeeded: {}", out.content);
    assert!(out.content.starts_with("exit 0\n"), "{}", out.content);
    assert!(out.content.contains("done"), "and what it printed is kept: {}", out.content);
    assert!(took < std::time::Duration::from_secs(30), "and it did not wait out the timeout: {took:?}");
    // Said, because it changes what the output means: there will be no more of
    // it, and something is still running.
    assert!(out.content.contains("still running"), "{}", out.content);

    // The precondition, checked after the fact: something really was holding
    // the pipe while the answer was written.
    assert_eq!(sleepers(&marker), 1, "the backgrounded child outlives the call");
    std::process::Command::new("pkill").args(["-f", &format!("sleep {marker}")]).status().ok();
}

/// The same shape without a background child: an ordinary command is never
/// told that something it started is still running.
///
/// No deadline here, deliberately. The first version asserted that this
/// answered within a second, and the answer it was really testing — that the
/// grace was not paid — is what the note says. On a machine running the whole
/// suite the second claim failed while the first held, which is the wrong way
/// round: the timing is not the claim, the note is.
#[cfg(unix)]
#[tokio::test]
async fn an_ordinary_command_is_never_told_it_left_something_running() {
    let (_d, ctx) = ctx();
    let out = run(&ctx, serde_json::json!({"command": "echo quick", "timeout_secs": 60})).await;

    assert!(out.content.contains("quick"), "{}", out.content);
    assert!(!out.content.contains("still running"), "nothing was left running: {}", out.content);
}

/// `ssh` takes a password from a terminal and from nowhere else — not from an
/// argument, not from the environment — so a secret reaches it only through the
/// helper OpenSSH already asks for. Standing in for ssh here, because a test
/// that reaches a host tests the host: what is claimed is that the program
/// named by `SSH_ASKPASS` prints the value, and that the value is in no argument.
#[cfg(unix)]
#[tokio::test]
async fn a_secret_reaches_a_program_that_only_reads_a_terminal() {
    struct One(&'static str, &'static str);
    impl rook_tools::Secrets for One {
        fn value(&self, name: &str) -> Option<String> {
            (name == self.0).then(|| self.1.to_string())
        }
    }

    let (_d, mut ctx) = ctx();
    ctx.secrets = Some(std::sync::Arc::new(One("ssh_prod", "hunter2-and-then-some")));
    let out = run(
        &ctx,
        serde_json::json!({
            // What ssh does with the helper, done by hand: run it and read
            // what it prints.
            "command": "\"$SSH_ASKPASS\"",
            "secrets": ["ssh_prod"]
        }),
    )
    .await;

    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("hunter2-and-then-some"), "the helper hands it over: {}", out.content);
    // And OpenSSH is told to use it without a terminal, or it never asks.
    let told =
        run(&ctx, serde_json::json!({ "command": "echo $SSH_ASKPASS_REQUIRE", "secrets": ["ssh_prod"] }))
            .await;
    assert!(told.content.contains("force"), "{}", told.content);
}

/// The helper is a file with a line of shell in it, not a file with a password
/// in it — and it is gone when the command is.
#[cfg(unix)]
#[tokio::test]
async fn the_askpass_helper_holds_no_value_and_does_not_outlive_the_command() {
    struct One;
    impl rook_tools::Secrets for One {
        fn value(&self, _: &str) -> Option<String> {
            Some("hunter2-and-then-some".into())
        }
    }

    let (_d, mut ctx) = ctx();
    ctx.secrets = Some(std::sync::Arc::new(One));
    let out = run(
        &ctx,
        serde_json::json!({ "command": "cat \"$SSH_ASKPASS\"; echo AT $SSH_ASKPASS", "secrets": ["x"] }),
    )
    .await;

    assert!(!out.content.contains("hunter2"), "the script carries no value: {}", out.content);
    assert!(out.content.contains("ROOK_SECRET_X"), "it prints the variable it inherits: {}", out.content);

    let at = out.content.split("AT ").nth(1).unwrap_or_default().trim().to_string();
    assert!(!at.is_empty(), "{}", out.content);
    assert!(!std::path::Path::new(&at).exists(), "the helper outlived the command: {at}");
}

/// A command that timed out while still printing and one that timed out in
/// silence are told apart.
///
/// The message invites the model to pass a larger `timeout_secs`, and for a
/// command that was working that is the right answer. For one waiting on a
/// prompt nobody is at, a lock, or a host that will not reply, it buys the same
/// wait a second time — and a model told only "it timed out" takes the
/// invitation either way.
///
/// The quiet one is not killed early on that evidence, on purpose: a single
/// large crate compiles for minutes without printing a line, and a mechanism
/// that cannot tell that from a wedge would kill real work. The judgement is
/// put to the agent, with what it needs to make it.
#[tokio::test]
async fn a_command_that_timed_out_says_whether_it_was_working_or_waiting() {
    let (_d, ctx) = ctx();

    let quiet = run(&ctx, serde_json::json!({ "command": "sleep 30", "timeout_secs": 2 })).await;
    let quiet = quiet.content;
    assert!(quiet.contains("timed out"), "{quiet}");
    assert!(
        quiet.contains("printed nothing"),
        "a command that said nothing at all was waiting, and the message says so: {quiet}"
    );

    let talking = run(
        &ctx,
        serde_json::json!({
            "command": "for i in 1 2 3 4 5 6; do echo working; sleep 0.3; done",
            "timeout_secs": 1
        }),
    )
    .await;
    let talking = talking.content;
    assert!(talking.contains("timed out"), "{talking}");
    assert!(
        !talking.contains("printed nothing"),
        "a command still printing when the clock ran out was working: {talking}"
    );
    assert!(talking.contains("working"), "and what it printed is in the message: {talking}");
}
