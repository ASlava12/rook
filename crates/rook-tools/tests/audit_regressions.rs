#![cfg(unix)]
use rook_tools::policy::{Decision, Policy, Risk, Stance};
use rook_tools::{
    Tool, ToolContext,
    exec::RunCommand,
    files::{EditFile, WriteFile},
    jobs::Jobs,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATE.lock().await
}
#[tokio::test]
async fn a_dangling_symlink_cannot_create_an_outside_file() {
    let d = tempfile::tempdir().unwrap();
    let workspace = d.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = d.path().join("outside.txt");
    std::os::unix::fs::symlink(&outside, workspace.join("link")).unwrap();
    let ctx = ToolContext::new(workspace);
    assert!(WriteFile.call(&ctx, &json!({"path":"link","content":"wrong"})).await.is_err());
    assert!(!outside.exists());
}
#[tokio::test]
async fn an_edit_batch_refuses_aliases_before_writing() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("file.txt"), "alpha beta").unwrap();
    let ctx = ToolContext::new(d.path().to_path_buf());
    let result = EditFile
        .call(
            &ctx,
            &json!({"files":[
                {"path":"file.txt","edits":[{"old":"alpha","new":"ALPHA"}]},
                {"path":"./file.txt","edits":[{"old":"beta","new":"BETA"}]}
            ]}),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(d.path().join("file.txt")).unwrap(), "alpha beta");
}
#[tokio::test]
async fn a_job_can_be_stopped_after_closing_its_output() {
    let _one = one_at_a_time().await;
    let d = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(1, 4096);
    let id = jobs.start("exec >/dev/null 2>&1; touch ready; sleep 60", d.path(), None).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !d.path().join("ready").exists() {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(jobs.stop(&id));
    assert!(jobs.wait(&id, Duration::from_secs(10)).await.unwrap().exit_code.is_some());
}
#[test]
fn shell_write_operators_and_executing_options_require_approval() {
    let (policy, _) = Policy::compile(Stance::Assist, &[r"/^(cat|find|rg|git)\b/".into()], &[], &[]);
    for command in [
        "cat /dev/null > important.txt",
        "find . -exec touch changed {} +",
        "find . -delete",
        "rg --pre=sh hi",
        "git diff --output=file",
    ] {
        assert_eq!(policy.decide(&Risk::Execute(command.into())), Decision::Ask, "{command}");
    }
    assert_eq!(policy.decide(&Risk::Execute("cat file.txt".into())), Decision::Allow);
}
struct SyntheticSecret;
impl rook_tools::Secrets for SyntheticSecret {
    fn value(&self, _: &str) -> Option<String> {
        Some("synthetic-private-value".into())
    }
}
#[tokio::test]
async fn a_secret_bearing_command_does_not_spill_raw_output() {
    let _one = one_at_a_time().await;
    let d = tempfile::tempdir().unwrap();
    let mut ctx = ToolContext::new(d.path().to_path_buf());
    ctx.max_output_bytes = 100;
    ctx.spill_dir = Some(d.path().join("output"));
    ctx.max_spill_bytes = 4096;
    ctx.secrets = Some(Arc::new(SyntheticSecret));
    let result = RunCommand.call(&ctx, &json!({"command":"printf '%s' \"$ROOK_SECRET_TEST\"; i=0; while [ $i -lt 200 ]; do printf x; i=$((i+1)); done", "secrets":["test"]})).await.unwrap();
    assert!(!result.is_error, "{}", result.content);
    let path = result.meta["output_file"].as_str().expect("long output is retained after redaction");
    let kept = std::fs::read_to_string(path).unwrap();
    assert!(kept.contains("${secret}"), "{kept}");
    assert!(!kept.contains("synthetic-private-value"), "raw secrets must never reach disk");
    assert_eq!(result.meta["output_complete"], true);
}
#[tokio::test]
async fn closing_output_does_not_disable_the_command_deadline() {
    let _one = one_at_a_time().await;
    let d = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(d.path().to_path_buf());
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        RunCommand.call(&ctx, &json!({"command":"exec >/dev/null 2>&1; sleep 60", "timeout_secs":1})),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.is_error);
    assert_eq!(result.meta.get("timed_out"), Some(&json!(true)));
}

#[tokio::test]
async fn moving_a_symlink_cannot_move_its_target_or_replace_another_link() {
    use rook_tools::files::MoveFile;
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("target"), "original").unwrap();
    std::os::unix::fs::symlink("target", d.path().join("link")).unwrap();
    let ctx = ToolContext::new(d.path().to_path_buf());
    let moved = MoveFile.call(&ctx, &json!({"from":"link", "to":"moved"})).await.unwrap();
    assert!(moved.is_error);
    assert!(std::fs::symlink_metadata(d.path().join("link")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(d.path().join("target")).unwrap(), "original");
    assert!(!d.path().join("moved").exists());
    std::os::unix::fs::symlink("absent", d.path().join("dangling")).unwrap();
    let moved = MoveFile.call(&ctx, &json!({"from":"target", "to":"dangling"})).await.unwrap();
    assert!(moved.is_error);
    assert!(d.path().join("target").exists());
    assert_eq!(std::fs::read_link(d.path().join("dangling")).unwrap(), std::path::Path::new("absent"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocked_file_reader_leaves_the_async_runtime_available() {
    let d = tempfile::tempdir().unwrap();
    let fifo = d.path().join("pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // Safety: a terminated pathname that stays alive until mkfifo returns.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (release, wait) = std::sync::mpsc::channel();
    let write_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        // Release the old blocking implementation too, so a regression fails
        // the assertion instead of hanging the test process forever.
        let _ = wait.recv_timeout(std::time::Duration::from_secs(5));
        std::fs::write(write_path, "received").unwrap();
    });
    let ctx = ToolContext::new(d.path().to_path_buf());
    let reading = ctx.read_text(&fifo);
    tokio::pin!(reading);
    let timer_ran = tokio::select! {
        biased;
        _ = &mut reading => false,
        _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => true,
    };
    let _ = release.send(());
    if timer_ran {
        assert_eq!(reading.await.unwrap(), "received");
    }
    writer.join().unwrap();
    assert!(timer_ran, "a disk read must not block the only executor thread");
}

#[tokio::test]
async fn relative_disk_paths_have_an_explicit_contract_error() {
    let d = tempfile::tempdir().unwrap();
    let mut ctx = ToolContext::new(d.path().to_path_buf());
    ctx.allow_outside_workspace = true;
    let error = ctx.read_text(std::path::Path::new("relative.txt")).await.unwrap_err().to_string();
    assert!(error.contains("absolute path"), "{error}");
}
