#![cfg(unix)]
use rook_tools::{
    Tool, ToolContext,
    environments::{Context, Pool, Settings},
    exec::RunCommand,
};
use serde_json::json;
use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};
#[path = "support/ssh.rs"]
mod support;
use support::{Server, checked};
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_file_directory_replacements_round_trip_and_local_changes_upload_into_the_reused_cwd() {
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(ctx.workspace.join("file"), "old file").unwrap();
    std::fs::create_dir(ctx.workspace.join("directory")).unwrap();
    std::fs::write(ctx.workspace.join("directory/child"), "old child").unwrap();
    let first = RunCommand.call(&ctx, &json!({"command":"rm file; mkdir file; printf remote-child > file/child; rm -r directory; printf remote-file > directory"})).await.unwrap();
    assert!(!first.is_error, "{}", first.content);
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("directory")).unwrap(), "remote-file");
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("file/child")).unwrap(), "remote-child");
    std::fs::remove_file(ctx.workspace.join("directory")).unwrap();
    std::fs::create_dir(ctx.workspace.join("directory")).unwrap();
    std::fs::write(ctx.workspace.join("directory/child"), "local-child").unwrap();
    std::fs::remove_dir_all(ctx.workspace.join("file")).unwrap();
    std::fs::write(ctx.workspace.join("file"), "local-file").unwrap();
    let next =
        RunCommand.call(&ctx, &json!({"command":"cat child; cat ../file", "cwd":"directory"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert!(next.content.contains("local-childlocal-file"));
    assert_eq!(
        first.meta["execution_environment"]["resource"],
        next.meta["execution_environment"]["resource"]
    );
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_timeout_bounds_output_stops_the_remote_command_and_retains_partial_files_for_review() {
    use rook_tools::environments::Request;
    let server = Server::start();
    let mut ctx = server.context();
    ctx.max_output_bytes = 512;
    let result = RunCommand.call(&ctx, &json!({"command":"yes output | head -c 262144; printf partial > result; sleep 60", "timeout_secs":1})).await.unwrap();
    assert!(result.is_error, "{}", result.content);
    assert!(result.truncated);
    assert!(result.full_bytes >= 262144);
    assert!(
        result.content.len() < 2048,
        "the actual output cap must be reached without retaining the whole stream"
    );
    assert_eq!(result.meta["execution_uncertain"], true);
    let view = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(view["observed"]["state"]["stopped"], true);
    assert_eq!(view["observed"]["running"], false);
    assert!(!ctx.workspace.join("result").exists());
    let preview = manage(&ctx, Request::PrepareRecover).await.unwrap();
    let call = preview["call"].as_str().unwrap().to_owned();
    let source =
        manage(&ctx, Request::Read { call, which: "after".into(), path: "result".into(), offset: 0 })
            .await
            .unwrap();
    assert_eq!(source["bytes_base64"], "cGFydGlhbA==");
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_partial_writeback_recovers_only_remaining_paths_without_replaying_the_command() {
    use rook_tools::environments::Request;
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(ctx.workspace.join("a-first"), "before").unwrap();
    let blocked = ctx.workspace.join("z-blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("last"), "before").unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = RunCommand.call(&ctx, &json!({"command":"printf after > a-first; printf after > z-blocked/last; printf once >> /tmp/command-count"})).await.unwrap();
    assert!(result.is_error, "{}", result.content);
    assert_eq!(result.meta["execution_uncertain"], true);
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("a-first")).unwrap(), "after");
    assert_eq!(std::fs::read_to_string(blocked.join("last")).unwrap(), "before");
    let view = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(view["saved"]["pending"], "z-blocked/last");
    assert_eq!(view["saved"]["completed"], 1);
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let preview = manage(&ctx, Request::PrepareRecover).await.unwrap();
    let result = manage(
        &ctx,
        Request::Recover {
            token: preview["token"].as_str().unwrap().into(),
            note: "Reviewed partial publication and restored local directory permissions".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(result["replayed"], false);
    assert_eq!(std::fs::read_to_string(blocked.join("last")).unwrap(), "after");
    let count = checked(Command::new("docker").args(["exec", &server.id, "cat", "/tmp/command-count"]));
    assert_eq!(count.stdout, b"once");
}
#[tokio::test]
#[ignore = "requires Docker, openssh client and owned rook-ssh-fixture:20261014 image"]
async fn actual_ssh_mirrors_files_returns_changes_and_reuses_the_same_owned_remote_workspace() {
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(ctx.workspace.join("before.txt"), "original user edit").unwrap();
    std::fs::write(ctx.workspace.join("deleted.txt"), "old").unwrap();
    let first=RunCommand.call(&ctx,&json!({"command":"cat before.txt; printf changed > before.txt; printf created > created.txt; rm deleted.txt; pwd"})).await.unwrap();
    assert!(!first.is_error, "{}", first.content);
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("before.txt")).unwrap(), "changed");
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("created.txt")).unwrap(), "created");
    assert!(!ctx.workspace.join("deleted.txt").exists());
    assert!(first.content.contains("/tmp/rook-workspaces/.rook-exec/"));
    std::fs::write(ctx.workspace.join("before.txt"), "later user edit").unwrap();
    let next = RunCommand.call(&ctx, &json!({"command":"cat before.txt; cat created.txt"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert!(next.content.contains("later user edit"));
    assert_eq!(
        first.meta["execution_environment"]["workspace"],
        next.meta["execution_environment"]["workspace"]
    );
}
async fn manage(
    ctx: &ToolContext,
    request: rook_tools::environments::Request,
) -> rook_tools::Result<serde_json::Value> {
    let env = ctx.execution.as_ref().unwrap();
    env.pool.request(ctx, env, request).await
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_cancel_stops_owned_descendants_and_explicit_recovery_returns_observed_changes_without_replay()
 {
    use rook_tools::environments::Request;
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(ctx.workspace.join("before"), "original").unwrap();
    let running = ctx.clone();
    let task = tokio::spawn(async move {
        RunCommand
            .call(
                &running,
                &json!({"command":"printf partial > before; setsid sleep 60 & echo $! > descendant; wait"}),
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let observed = loop {
        let view = manage(&ctx, Request::Inspect).await;
        if let Ok(view) = view
            && view["observed"]["running"] == true
        {
            break view;
        }
        assert!(tokio::time::Instant::now() < deadline, "remote command never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    task.abort();
    let _ = task.await;
    let view = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(view["observed"]["state"]["stopped"], true, "cancel must stop actual adopted descendants");
    assert_eq!(
        std::fs::read_to_string(ctx.workspace.join("before")).unwrap(),
        "original",
        "cancel does not silently publish remote partial work"
    );
    assert!(RunCommand.call(&ctx, &json!({"command":"printf forbidden > replay"})).await.is_err());
    let preview = manage(&ctx, Request::PrepareRecover).await.unwrap();
    let recovered = manage(
        &ctx,
        Request::Recover {
            token: preview["token"].as_str().unwrap().into(),
            note: "Inspected stopped remote command and partial source".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(recovered["replayed"], false);
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("before")).unwrap(), "partial");
    let pid = std::fs::read_to_string(ctx.workspace.join("descendant")).unwrap();
    assert!(
        !Command::new("docker")
            .args(["exec", &server.id, "kill", "-0", pid.trim()])
            .output()
            .unwrap()
            .status
            .success(),
        "the actual detached remote descendant must be stopped"
    );
    assert!(!ctx.workspace.join("replay").exists());
    let next = RunCommand.call(&ctx, &json!({"command":"cat before"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert_eq!(next.meta["execution_environment"]["workspace"], observed["observed"]["workspace"]);
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_conflicts_preserve_later_local_edits_and_retained_sources_support_reviewed_recovery() {
    use rook_tools::environments::Request;
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(ctx.workspace.join("same"), "before").unwrap();
    std::fs::write(ctx.workspace.join("independent"), "old independent").unwrap();
    let running = ctx.clone();
    let task = tokio::spawn(async move {
        RunCommand.call(&running, &json!({"command":"printf remote > same; while test ! -f /tmp/fixture-release; do sleep 0.05; done"})).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if manage(&ctx, Request::Inspect).await.is_ok_and(|view| view["observed"]["running"] == true) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::fs::write(ctx.workspace.join("same"), "later user edit").unwrap();
    std::fs::write(ctx.workspace.join("independent"), "later independent edit").unwrap();
    checked(Command::new("docker").args(["exec", &server.id, "touch", "/tmp/fixture-release"]));
    let result = task.await.unwrap().unwrap();
    assert!(result.is_error, "writeback must reach the actual conflict");
    assert_eq!(result.meta["execution_uncertain"], true);
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("same")).unwrap(), "later user edit");
    let call = result.meta["execution_environment"]["call"].as_str().unwrap().to_owned();
    let after = manage(
        &ctx,
        Request::Read { call: call.clone(), which: "after".into(), path: "same".into(), offset: 0 },
    )
    .await
    .unwrap();
    assert_eq!(after["bytes_base64"], "cmVtb3Rl");
    let preview = manage(&ctx, Request::PrepareRecover).await.unwrap();
    assert!(
        manage(
            &ctx,
            Request::Recover {
                token: preview["token"].as_str().unwrap().into(),
                note: "conflict still exists".into()
            }
        )
        .await
        .is_err()
    );
    // The user explicitly chooses the retained before image to permit the reviewed merge.
    std::fs::write(ctx.workspace.join("same"), "before").unwrap();
    let preview = manage(&ctx, Request::PrepareRecover).await.unwrap();
    manage(
        &ctx,
        Request::Recover {
            token: preview["token"].as_str().unwrap().into(),
            note: "Reviewed both versions and selected remote result for same".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("same")).unwrap(), "remote");
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("independent")).unwrap(), "later independent edit");
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_background_jobs_own_the_pipe_and_reuse_only_after_certified_writeback() {
    let server = Server::start();
    let mut ctx = server.context();
    ctx.jobs = Some(Arc::new(rook_tools::jobs::Jobs::new(2, 2048)));
    let first = RunCommand
        .call(&ctx, &json!({"command":"printf background > result; sleep 0.3","background":true}))
        .await
        .unwrap();
    let job = first.meta["job"].as_str().unwrap();
    let terminal = ctx.jobs.as_ref().unwrap().wait(job, Duration::from_secs(20)).await.unwrap();
    assert_eq!(terminal.exit_code, Some(0), "{}", terminal.output);
    ctx.jobs.as_ref().unwrap().quiesce(job).await.unwrap();
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("result")).unwrap(), "background");
    let next = RunCommand.call(&ctx, &json!({"command":"cat result"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert_eq!(
        next.meta["execution_environment"]["resource"],
        first.meta["execution_environment"]["resource"]
    );
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_release_removes_only_the_owned_replica_and_a_later_call_creates_a_new_identity() {
    use rook_tools::environments::Request;
    let server = Server::start();
    let ctx = server.context();
    let first = RunCommand.call(&ctx, &json!({"command":"printf original > keep"})).await.unwrap();
    assert!(!first.is_error, "{}", first.content);
    let remote = first.meta["execution_environment"]["workspace"].as_str().unwrap();
    let preview = manage(&ctx, Request::PrepareRelease).await.unwrap();
    manage(
        &ctx,
        Request::Release {
            token: preview["token"].as_str().unwrap().into(),
            note: "Inspected the completed replica".into(),
        },
    )
    .await
    .unwrap();
    assert!(
        !Command::new("docker").args(["exec", &server.id, "test", "-d", remote]).status().unwrap().success()
    );
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("keep")).unwrap(), "original");
    let next = RunCommand.call(&ctx, &json!({"command":"cat keep"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert_ne!(
        first.meta["execution_environment"]["resource"],
        next.meta["execution_environment"]["resource"]
    );
}
struct DynamicSecret(std::sync::atomic::AtomicUsize);
impl rook_tools::Secrets for DynamicSecret {
    fn value(&self, name: &str) -> Option<String> {
        (name == "token").then(|| {
            format!("PRIVATE_SSH_VALUE_{}", self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        })
    }
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_secrets_use_only_the_stdin_frame_are_resolved_once_and_do_not_survive_the_call() {
    let server = Server::start();
    let mut ctx = server.context();
    let secret = Arc::new(DynamicSecret(std::sync::atomic::AtomicUsize::new(0)));
    ctx.secrets = Some(secret.clone());
    let result = RunCommand
        .call(&ctx, &json!({"command":"printf '%s' \"$ROOK_SECRET_TOKEN\"","secrets":["token"]}))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(secret.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!result.content.contains("PRIVATE_SSH_VALUE_"));
    assert!(!serde_json::to_string(&result.meta).unwrap().contains("PRIVATE_SSH_VALUE_"));
    let next = RunCommand.call(&ctx, &json!({"command":"test -z \"$ROOK_SECRET_TOKEN\""})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_rejects_an_untrusted_server_key_without_running_any_remote_or_local_fallback() {
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(server.root.path().join("known_hosts"), "").unwrap();
    let result = RunCommand.call(&ctx, &json!({"command":"printf forbidden > forbidden"})).await;
    assert!(result.is_err());
    assert!(!ctx.workspace.join("forbidden").exists());
    assert!(
        Command::new("docker")
            .args(["exec", &server.id, "test", "!", "-e", "/tmp/rook-workspaces"])
            .status()
            .unwrap()
            .success(),
        "key refusal precedes remote helper/resource effects"
    );
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn a_reached_ssh_source_byte_bound_refuses_transfer_and_command_before_remote_setup() {
    let server = Server::start();
    let mut ctx = server.context();
    let env = Arc::get_mut(ctx.execution.as_mut().unwrap()).unwrap();
    env.settings.file_bytes = 1024;
    std::fs::write(ctx.workspace.join("large"), vec![b'x'; 1025]).unwrap();
    assert!(std::fs::metadata(ctx.workspace.join("large")).unwrap().len() > env.settings.file_bytes as u64);
    let result = RunCommand.call(&ctx, &json!({"command":"printf forbidden > forbidden"})).await;
    assert!(result.is_err());
    assert!(!ctx.workspace.join("forbidden").exists());
    assert!(
        Command::new("docker")
            .args(["exec", &server.id, "test", "!", "-e", "/tmp/rook-workspaces"])
            .status()
            .unwrap()
            .success()
    );
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn ssh_retained_sources_reach_their_count_bound_and_prune_only_resolved_older_call_images() {
    use rook_tools::environments::Request;
    let server = Server::start();
    let mut ctx = server.context();
    Arc::get_mut(ctx.execution.as_mut().unwrap()).unwrap().settings.max_calls = 2;
    std::fs::write(ctx.workspace.join("keep"), "original user file").unwrap();
    let mut calls = Vec::new();
    for _ in 0..3 {
        let result = RunCommand.call(&ctx, &json!({"command":"cat keep"})).await.unwrap();
        assert!(!result.is_error, "{}", result.content);
        calls.push(result.meta["execution_environment"]["call"].as_str().unwrap().to_owned());
    }
    assert!(calls.len() > 2);
    let view = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(view["saved"]["history"].as_array().unwrap().len(), 2);
    assert!(
        manage(
            &ctx,
            Request::Read { call: calls[0].clone(), which: "before".into(), path: "keep".into(), offset: 0 }
        )
        .await
        .is_err()
    );
    assert!(
        manage(
            &ctx,
            Request::Read { call: calls[2].clone(), which: "before".into(), path: "keep".into(), offset: 0 }
        )
        .await
        .is_ok()
    );
    let state = &ctx.execution.as_ref().unwrap().state_root;
    let files =
        std::fs::read_dir(state.join("execution-environments/ssh")).unwrap().next().unwrap().unwrap().path();
    assert_eq!(std::fs::read_dir(files).unwrap().count(), 2);
}
#[tokio::test]
#[ignore = "subprocess helper for actual SSH process-loss fixture"]
async fn ssh_process_loss_child() {
    let Some(root) = std::env::var_os("ROOK_SSH_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let settings: Settings =
        serde_json::from_slice(&std::fs::read(root.join("fixture-settings.json")).unwrap()).unwrap();
    let mut ctx = ToolContext::new(root.join("workspace"));
    ctx.execution =
        Some(Arc::new(Context { settings, pool: Arc::new(Pool::default()), state_root: root.join("state") }));
    let running = ctx.clone();
    let _task = tokio::spawn(async move {
        RunCommand.call(&running, &json!({"command":"printf survived > result; sleep 60"})).await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if manage(&ctx, rook_tools::environments::Request::Inspect)
            .await
            .is_ok_and(|view| view["observed"]["running"] == true)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::process::exit(0);
}
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn actual_ssh_process_loss_keeps_remote_ownership_and_only_observed_recovery_restores_files_after_reopening()
 {
    use rook_tools::environments::Request;
    let server = Server::start();
    let ctx = server.context();
    std::fs::write(
        server.root.path().join("fixture-settings.json"),
        serde_json::to_vec(&ctx.execution.as_ref().unwrap().settings).unwrap(),
    )
    .unwrap();
    drop(ctx);
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "ssh_process_loss_child", "--nocapture"])
        .env("ROOK_SSH_CHILD_ROOT", server.root.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(40), child.wait()).await.unwrap().unwrap().success());
    let ctx = server.context();
    assert!(RunCommand.call(&ctx, &json!({"command":"printf forbidden > replay"})).await.is_err());
    assert!(!ctx.workspace.join("replay").exists());
    let view = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(view["foreign_owner_active"], false);
    if view["observed"]["state"]["terminal"] != true {
        let preview = manage(&ctx, Request::PrepareStop).await.unwrap();
        let stopped = manage(
            &ctx,
            Request::Stop {
                token: preview["token"].as_str().unwrap().into(),
                note: "Inspected actual retained proxy after process exit".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(stopped["replayed"], false);
    }
    let preview = manage(&ctx, Request::PrepareRecover).await.unwrap();
    let recovered = manage(
        &ctx,
        Request::Recover {
            token: preview["token"].as_str().unwrap().into(),
            note: "Reviewed exact remote source after lost owner".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(recovered["replayed"], false);
    assert_eq!(std::fs::read_to_string(ctx.workspace.join("result")).unwrap(), "survived");
    assert!(!ctx.workspace.join("replay").exists());
}
