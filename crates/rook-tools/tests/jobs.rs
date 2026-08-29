//! Commands left running. The guards here are the ones that stop a turn from
//! filling the machine with processes nobody is waiting on.

#![cfg(unix)]

use std::sync::Arc;

use rook_tools::jobs::{JobTool, Jobs};
use rook_tools::{Tool, ToolContext, exec::RunCommand};

fn ctx(most: usize) -> (tempfile::TempDir, ToolContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolContext::new(dir.path().to_path_buf());
    ctx.jobs = Some(Arc::new(Jobs::new(most, 64 * 1024)));
    (dir, ctx)
}

/// Waits for `wanted` to show up in the job's output, or gives up. A background
/// command has no moment at which it is done, so there is nothing else to wait
/// for — and a fixed sleep is a guess about scheduling that a loaded machine
/// makes wrong.
async fn until(ctx: &ToolContext, id: &str, wanted: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let out = JobTool.call(ctx, &serde_json::json!({ "id": id })).await.unwrap();
        if out.content.contains(wanted) {
            return out.content;
        }
        assert!(std::time::Instant::now() < deadline, "{wanted:?} never appeared in: {}", out.content);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_background_command_answers_at_once_and_keeps_printing() {
    let (_d, ctx) = ctx(4);

    let started = RunCommand
        .call(&ctx, &serde_json::json!({"command": "echo ready; sleep 30", "background": true}))
        .await
        .unwrap();

    let id = started.meta.get("job").and_then(|j| j.as_str()).expect("an id to read it by").to_string();
    let seen = until(&ctx, &id, "ready").await;
    assert!(seen.contains("running for"), "and it is still going: {seen}");

    let stopped = JobTool.call(&ctx, &serde_json::json!({"id": id, "stop": true})).await.unwrap();
    assert!(!stopped.is_error, "{}", stopped.content);
    assert!(stopped.content.contains("ready"), "what it printed survives being stopped: {}", stopped.content);
    assert!(until(&ctx, &id, "exit ").await.contains("ready"), "and it really stops");
}

/// The signal has to be kept rather than delivered to whoever happens to be
/// listening: a job stopped in the same breath as it was started has a task that
/// is not waiting yet.
#[tokio::test]
async fn a_background_command_stopped_at_once_still_stops() {
    let (_d, ctx) = ctx(4);

    let started =
        RunCommand.call(&ctx, &serde_json::json!({"command": "sleep 30", "background": true})).await.unwrap();
    let id = started.meta.get("job").and_then(|j| j.as_str()).unwrap().to_string();
    JobTool.call(&ctx, &serde_json::json!({"id": id, "stop": true})).await.unwrap();

    until(&ctx, &id, "exit ").await;
}

/// Each one is a process nobody is waiting on, so what stops a turn filling the
/// machine is a cap and not good sense.
#[tokio::test]
async fn more_background_commands_than_the_cap_are_refused_by_name() {
    let (_d, ctx) = ctx(2);
    let start = serde_json::json!({"command": "sleep 30", "background": true});

    for _ in 0..2 {
        assert!(!RunCommand.call(&ctx, &start).await.unwrap().is_error);
    }
    let refused = RunCommand.call(&ctx, &start).await.unwrap_err().to_string();

    assert!(refused.contains("max_background_jobs"), "it must name the limit: {refused}");
    assert!(refused.contains("job001"), "and which to stop: {refused}");
}

/// A dev server that outlived the agent that started it is one nobody knows to
/// stop.
#[tokio::test]
async fn the_registry_going_away_takes_the_processes_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("alive");
    let mut ctx = ToolContext::new(dir.path().to_path_buf());
    ctx.jobs = Some(Arc::new(Jobs::new(4, 4096)));

    let command = format!("while :; do touch {}; sleep 0.05; done", marker.display());
    let started =
        RunCommand.call(&ctx, &serde_json::json!({"command": command, "background": true})).await.unwrap();
    let id = started.meta.get("job").and_then(|j| j.as_str()).unwrap().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !marker.exists() {
        assert!(std::time::Instant::now() < deadline, "the loop never ran");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    ctx.jobs = None;
    assert!(JobTool.call(&ctx, &serde_json::json!({"id": id})).await.unwrap().is_error, "and it is gone");

    // Until it stops recreating the marker, rather than once: how long the kill
    // takes to land is scheduling, and a fixed wait is a guess about it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let _ = std::fs::remove_file(&marker);
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if !marker.exists() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the process kept running after the registry was dropped"
        );
    }
}

/// A turn that starts a thousand short commands would otherwise keep all
/// thousand, which is the accumulator the cap exists to prevent.
#[tokio::test]
async fn finished_background_commands_do_not_pile_up() {
    let (_d, ctx) = ctx(2);
    let jobs = ctx.jobs.clone().unwrap();

    for _ in 0..6 {
        let out =
            RunCommand.call(&ctx, &serde_json::json!({"command": "true", "background": true})).await.unwrap();
        let id = out.meta.get("job").and_then(|j| j.as_str()).unwrap().to_string();
        until(&ctx, &id, "exit 0").await;
    }
    // One more, because the pruning happens when the next one starts.
    RunCommand.call(&ctx, &serde_json::json!({"command": "true", "background": true})).await.unwrap();

    let held = jobs.list();
    assert!(held.len() <= 3, "seven ran and {} are still held: {held:?}", held.len());
    let newest = held.iter().map(|j| j.id.clone()).max().unwrap();
    assert_eq!(newest, "job007", "and it is the oldest that go: {held:?}");
}

/// The point of the wait: three commands run at once and the turn spends four
/// tool calls, not one per check. Polling costs a whole model round trip each
/// time.
#[tokio::test]
async fn several_commands_run_at_once_and_the_turn_waits_for_them() {
    let (dir, ctx) = ctx(4);

    let mut ids = Vec::new();
    for name in ["a", "b", "c"] {
        let command = format!("sleep 0.3; echo done > {}/{name}", dir.path().display());
        let out = RunCommand
            .call(&ctx, &serde_json::json!({"command": command, "background": true}))
            .await
            .unwrap();
        ids.push(out.meta.get("job").and_then(|j| j.as_str()).unwrap().to_string());
    }

    let started = std::time::Instant::now();
    for id in &ids {
        let out = JobTool.call(&ctx, &serde_json::json!({"id": id, "wait_secs": 20})).await.unwrap();
        assert_eq!(out.meta.get("running"), Some(&serde_json::json!(false)), "{}", out.content);
    }

    assert!(started.elapsed() < std::time::Duration::from_secs(1), "they ran together, not one by one");
    for name in ["a", "b", "c"] {
        assert!(dir.path().join(name).exists(), "{name} never finished");
    }
}

/// A wait that outlives what it is waiting for would hold the turn as surely as
/// a command with no timeout.
#[tokio::test]
async fn a_wait_gives_up_and_says_it_is_still_running() {
    let (_d, mut ctx) = ctx(4);
    ctx.command_timeout = std::time::Duration::from_secs(1);

    let started =
        RunCommand.call(&ctx, &serde_json::json!({"command": "sleep 30", "background": true})).await.unwrap();
    let id = started.meta.get("job").and_then(|j| j.as_str()).unwrap().to_string();

    let waited = std::time::Instant::now();
    // Asked for far longer than a command in the foreground would have been
    // given, which is the cap it is held to.
    let out = JobTool.call(&ctx, &serde_json::json!({"id": id, "wait_secs": 600})).await.unwrap();

    assert!(waited.elapsed() < std::time::Duration::from_secs(5), "it waited {:?}", waited.elapsed());
    assert_eq!(out.meta.get("running"), Some(&serde_json::json!(true)), "{}", out.content);
}

/// A front end with nowhere to keep one says so rather than running it in the
/// foreground and appearing to hang.
#[tokio::test]
async fn a_front_end_that_keeps_none_refuses_rather_than_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());

    let out =
        RunCommand.call(&ctx, &serde_json::json!({"command": "sleep 30", "background": true})).await.unwrap();

    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("timeout_secs"), "and says what to do instead: {}", out.content);
}
