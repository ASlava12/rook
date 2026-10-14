use rook_tools::{
    Tool, ToolContext,
    environments::{Context, Pool, Profile, Settings},
    exec::RunCommand,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
#[tokio::test]
#[ignore = "needs an available Docker engine and installed alpine:3; run explicitly"]
async fn actual_container_changes_share_the_workspace_and_reuse_one_owned_resource_across_calls() {
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-exec-").tempdir_in(&base).unwrap();
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&state).unwrap();
    std::fs::write(workspace.join("before.txt"), "user's original edit").unwrap();
    let mut ctx = ToolContext::new(workspace.clone());
    ctx.isolation.network = false;
    ctx.max_output_bytes = 2048;
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            selected: "box".into(),
            profiles: BTreeMap::from([(
                "box".into(),
                Profile::Container {
                    image: "alpine:3".into(),
                    program: "docker".into(),
                    memory_mb: 128,
                    pids: 64,
                },
            )]),
            ..Default::default()
        },
        pool: Arc::new(Pool::default()),
        state_root: state,
    }));
    let first = RunCommand
        .call(&ctx, &json!({"command": "cat before.txt; printf changed > changed.txt; pwd"}))
        .await
        .unwrap();
    assert!(!first.is_error, "{}", first.content);
    assert_eq!(std::fs::read_to_string(workspace.join("changed.txt")).unwrap(), "changed");
    assert!(first.content.contains("/workspace"));
    assert_eq!(std::fs::read_to_string(workspace.join("before.txt")).unwrap(), "user's original edit");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(workspace.join("changed.txt")).unwrap().uid(),
            unsafe { libc::geteuid() },
            "container writes must retain the operator's uid"
        );
    }
    let second = RunCommand.call(&ctx, &json!({"command":"cat changed.txt"})).await.unwrap();
    assert!(!second.is_error, "{}", second.content);
    assert_eq!(
        first.meta["execution_environment"]["resource"],
        second.meta["execution_environment"]["resource"]
    );
    let id = first.meta["execution_environment"]["resource"].as_str().unwrap().to_owned();
    drop(ctx);
    let inspected = rook_contain::quietly(&mut std::process::Command::new("docker"))
        .args(["inspect", &id])
        .output()
        .unwrap();
    assert!(!inspected.status.success(), "dropping the owned pool must release only its actual resource");
}

#[tokio::test]
#[ignore = "needs an available Docker engine and installed alpine:3; run explicitly"]
async fn cancellation_kills_the_owned_container_and_refuses_silent_resource_reuse() {
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-exec-").tempdir_in(&base).unwrap();
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&state).unwrap();
    let mut ctx = ToolContext::new(workspace.clone());
    ctx.isolation.network = false;
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            selected: "box".into(),
            profiles: BTreeMap::from([(
                "box".into(),
                Profile::Container {
                    image: "alpine:3".into(),
                    program: "docker".into(),
                    memory_mb: 128,
                    pids: 64,
                },
            )]),
            ..Default::default()
        },
        pool: Arc::new(Pool::default()),
        state_root: state.clone(),
    }));
    let ready = RunCommand.call(&ctx, &json!({"command":"printf prepared"})).await.unwrap();
    let id = ready.meta["execution_environment"]["resource"].as_str().unwrap().to_owned();
    let running = ctx.clone();
    let task = tokio::spawn(async move {
        RunCommand.call(&running, &json!({"command":"printf ready > started; sleep 60 & wait"})).await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !workspace.join("started").exists() {
        assert!(tokio::time::Instant::now() < deadline, "actual command never started");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    task.abort();
    let _ = task.await;
    let inspected = rook_contain::quietly(&mut std::process::Command::new("docker"))
        .args(["inspect", "--format", "{{.State.Running}}", &id])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&inspected.stdout).trim(),
        "false",
        "cancelled remote descendants must not keep executing"
    );
    let denied = RunCommand.call(&ctx, &json!({"command":"printf forbidden > replayed"})).await;
    assert!(denied.is_err());
    assert!(!workspace.join("replayed").exists());
    let receipts: Vec<_> =
        std::fs::read_dir(state.join("execution-environments")).unwrap().map(|p| p.unwrap().path()).collect();
    assert_eq!(receipts.len(), 1);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipts[0]).unwrap()).unwrap();
    assert_eq!(saved["status"], "interrupted_stopped");
    drop(ctx);
    assert!(
        !rook_contain::quietly(&mut std::process::Command::new("docker"))
            .args(["inspect", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[tokio::test]
#[ignore = "needs an available Docker engine and installed alpine:3; run explicitly"]
async fn background_jobs_keep_the_remote_lease_and_stop_the_owned_resource_on_request() {
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-exec-").tempdir_in(&base).unwrap();
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&state).unwrap();
    let mut ctx = ToolContext::new(workspace.clone());
    ctx.isolation.network = false;
    ctx.jobs = Some(Arc::new(rook_tools::jobs::Jobs::new(2, 2048)));
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            selected: "box".into(),
            profiles: BTreeMap::from([(
                "box".into(),
                Profile::Container {
                    image: "alpine:3".into(),
                    program: "docker".into(),
                    memory_mb: 128,
                    pids: 64,
                },
            )]),
            ..Default::default()
        },
        pool: Arc::new(Pool::default()),
        state_root: state.clone(),
    }));
    let outcome = RunCommand
        .call(&ctx, &json!({"command":"printf ready > started; sleep 60 & wait","background":true}))
        .await
        .unwrap();
    let id = outcome.meta["execution_environment"]["resource"].as_str().unwrap().to_owned();
    let job = outcome.meta["job"].as_str().unwrap().to_owned();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !workspace.join("started").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let busy = RunCommand.call(&ctx, &json!({"command":"printf forbidden > busy-write"})).await;
    assert!(busy.is_err());
    assert!(!workspace.join("busy-write").exists());
    assert!(ctx.jobs.as_ref().unwrap().stop(&job));
    while ctx.jobs.as_ref().unwrap().is_running(Some(&job)) {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let inspect = rook_contain::quietly(&mut std::process::Command::new("docker"))
        .args(["inspect", "--format", "{{.State.Running}}", &id])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&inspect.stdout).trim(), "false");
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            std::fs::read_dir(state.join("execution-environments")).unwrap().next().unwrap().unwrap().path(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(record["status"], "interrupted_stopped");
    drop(ctx);
    assert!(
        !rook_contain::quietly(&mut std::process::Command::new("docker"))
            .args(["inspect", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
}

fn container_context() -> (tempfile::TempDir, ToolContext, std::path::PathBuf) {
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-exec-").tempdir_in(base).unwrap();
    let workspace = root.path().join("workspace");
    let state = root.path().join("state");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&state).unwrap();
    let mut ctx = ToolContext::new(workspace);
    ctx.isolation.network = false;
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            selected: "box".into(),
            profiles: BTreeMap::from([(
                "box".into(),
                Profile::Container {
                    image: "alpine:3".into(),
                    program: "docker".into(),
                    memory_mb: 128,
                    pids: 64,
                },
            )]),
            ..Default::default()
        },
        pool: Arc::new(Pool::default()),
        state_root: state.clone(),
    }));
    (root, ctx, state)
}
async fn manage(
    ctx: &ToolContext,
    request: rook_tools::environments::Request,
) -> rook_tools::Result<serde_json::Value> {
    let env = ctx.execution.as_ref().unwrap();
    env.pool.request(ctx, env, request).await
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn completed_remote_command_kills_redirected_descendants_before_returning_the_resource() {
    let (_root, ctx, _state) = container_context();
    let result = RunCommand
        .call(&ctx, &json!({"command":"sleep 60 >/dev/null 2>&1 & echo $! > descendant"}))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(result.meta["processes_stopped"], true);
    let result=RunCommand.call(&ctx,&json!({"command":"pid=$(cat descendant); stat=$(ps -eo pid,stat | awk -v pid=\"$pid\" '$1==pid {print $2}'); test -z \"$stat\" || test \"${stat#Z}\" != \"$stat\""})).await.unwrap();
    assert!(!result.is_error, "a live descendant survived certified completion: {}", result.content);
}
struct Secret;
impl rook_tools::Secrets for Secret {
    fn value(&self, name: &str) -> Option<String> {
        (name == "token").then(|| "secret-value-47f3".to_owned())
    }
    fn redactions(&self) -> Vec<String> {
        vec!["secret-value-47f3".to_owned()]
    }
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn container_secrets_are_per_call_redacted_and_never_rendered_in_transport_arguments() {
    let (_root, mut ctx, _state) = container_context();
    ctx.secrets = Some(Arc::new(Secret));
    let result = RunCommand
        .call(&ctx, &json!({"command":"printf '%s' \"$ROOK_SECRET_TOKEN\"","secrets":["token"]}))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert!(!result.content.contains("secret-value-47f3"));
    assert!(!serde_json::to_string(&result.meta).unwrap().contains("secret-value-47f3"));
    let result = RunCommand.call(&ctx, &json!({"command":"test -z \"$ROOK_SECRET_TOKEN\""})).await.unwrap();
    assert!(!result.is_error, "a secret leaked into later commands: {}", result.content);
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn ordinary_background_exit_is_certified_before_quiesce_and_reuse() {
    let (_root, mut ctx, _state) = container_context();
    ctx.jobs = Some(Arc::new(rook_tools::jobs::Jobs::new(2, 2048)));
    let first =
        RunCommand.call(&ctx, &json!({"command":"printf done; exit 7","background":true})).await.unwrap();
    let jobs = ctx.jobs.as_ref().unwrap();
    let job = first.meta["job"].as_str().unwrap();
    let terminal = jobs.wait(job, std::time::Duration::from_secs(10)).await.unwrap();
    assert_eq!(terminal.exit_code, Some(7));
    jobs.quiesce(job).await.unwrap();
    let next = RunCommand.call(&ctx, &json!({"command":"printf reused"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert_eq!(
        next.meta["execution_environment"]["resource"],
        first.meta["execution_environment"]["resource"]
    );
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn timed_out_resource_is_explicitly_inspected_released_and_recreated_without_replay() {
    use rook_tools::environments::Request;
    let (_root, ctx, _state) = container_context();
    let first = RunCommand
        .call(&ctx, &json!({"command":"printf started > started; sleep 60","timeout_secs":1}))
        .await
        .unwrap();
    assert!(first.is_error);
    assert_eq!(first.meta["timed_out"], true);
    assert_eq!(first.meta["processes_stopped"], true);
    let inspected = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(inspected["saved"]["status"], "interrupted_stopped");
    assert_eq!(inspected["observed"]["running"], false);
    let prepared = manage(&ctx, Request::PrepareRelease).await.unwrap();
    let token = prepared["token"].as_str().unwrap().to_owned();
    let released = manage(
        &ctx,
        Request::Release {
            token: token.clone(),
            note: "Inspected timeout and stopped owned container".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(released["replayed"], false);
    assert!(manage(&ctx, Request::Release { token, note: "replay".into() }).await.is_err());
    let next = RunCommand.call(&ctx, &json!({"command":"printf recreated"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert_ne!(
        next.meta["execution_environment"]["resource"],
        first.meta["execution_environment"]["resource"]
    );
}

#[tokio::test]
#[ignore = "subprocess helper for the actual process-loss fixture"]
async fn process_loss_child() {
    let Some(root) = std::env::var_os("ROOK_ENV_CRASH_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let ctx = existing_context(&root);
    let result = RunCommand.call(&ctx, &json!({"command":"printf retained > persisted"})).await.unwrap();
    assert!(!result.is_error, "{}", result.content);
    std::fs::write(root.join("owned-id"), result.meta["execution_environment"]["resource"].as_str().unwrap())
        .unwrap();
    // Models a real process loss: no Rust destructor or frontend cleanup runs.
    std::process::exit(0);
}
fn existing_context(root: &std::path::Path) -> ToolContext {
    let mut ctx = ToolContext::new(root.join("workspace"));
    ctx.isolation.network = false;
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            selected: "box".into(),
            profiles: BTreeMap::from([(
                "box".into(),
                Profile::Container {
                    image: "alpine:3".into(),
                    program: "docker".into(),
                    memory_mb: 128,
                    pids: 64,
                },
            )]),
            ..Default::default()
        },
        pool: Arc::new(Pool::default()),
        state_root: root.join("state"),
    }));
    ctx
}
struct OwnedContainer(String);
impl Drop for OwnedContainer {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "--force", &self.0]).output();
    }
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn actual_process_loss_refuses_reuse_and_requires_inspected_release_after_reopening() {
    use rook_tools::environments::Request;
    let (root, ctx, _state) = container_context();
    drop(ctx);
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "process_loss_child", "--nocapture"])
        .env("ROOK_ENV_CRASH_ROOT", root.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let status =
        tokio::time::timeout(std::time::Duration::from_secs(40), child.wait()).await.unwrap().unwrap();
    assert!(status.success());
    let id = std::fs::read_to_string(root.path().join("owned-id")).unwrap();
    let _cleanup = OwnedContainer(id.clone());
    let ctx = existing_context(root.path());
    assert!(RunCommand.call(&ctx, &json!({"command":"printf forbidden > replayed"})).await.is_err());
    assert!(!ctx.workspace.join("replayed").exists());
    let view = manage(&ctx, Request::Inspect).await.unwrap();
    assert_eq!(view["saved"]["status"], "ready");
    assert_eq!(view["observed"]["running"], true);
    assert_eq!(view["observed"]["id"], id);
    let preview = manage(&ctx, Request::PrepareRelease).await.unwrap();
    manage(
        &ctx,
        Request::Release {
            token: preview["token"].as_str().unwrap().into(),
            note: "Inspected orphan after actual process exit".into(),
        },
    )
    .await
    .unwrap();
    let next = RunCommand.call(&ctx, &json!({"command":"cat persisted"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert!(next.content.contains("retained"));
    assert_ne!(next.meta["execution_environment"]["resource"], id);
}

#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn a_separate_live_pool_cannot_release_another_owners_resource() {
    use rook_tools::environments::Request;
    let (root, ctx, _state) = container_context();
    let first = RunCommand.call(&ctx, &json!({"command":"printf first"})).await.unwrap();
    assert!(!first.is_error);
    let other = existing_context(root.path());
    let view = manage(&other, Request::Inspect).await.unwrap();
    assert_eq!(view["foreign_owner_active"], true);
    assert!(manage(&other, Request::PrepareRelease).await.is_err());
    let next = RunCommand.call(&ctx, &json!({"command":"printf still-owned"})).await.unwrap();
    assert!(!next.is_error, "{}", next.content);
    assert_eq!(
        first.meta["execution_environment"]["resource"],
        next.meta["execution_environment"]["resource"]
    );
}
#[cfg(unix)]
fn fault_launcher(root: &std::path::Path, ctx: &mut ToolContext) {
    use std::os::unix::fs::PermissionsExt;
    let launcher = root.join("docker-fixture");
    let flag = format!("'{}'", root.join("refuse-kill").display().to_string().replace('\'', "'\\''"));
    std::fs::write(
        &launcher,
        format!("#!/bin/sh\nif test \"$1\" = kill && test -f {flag}; then exit 1; fi\nexec docker \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = Arc::get_mut(ctx.execution.as_mut().unwrap()).unwrap();
    let Profile::Container { program, .. } = env.settings.profiles.get_mut("box").unwrap() else { panic!() };
    *program = launcher.display().to_string();
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn failed_remote_stop_never_certifies_job_quiescence_from_the_local_client_exit() {
    let (root, mut ctx, _state) = container_context();
    fault_launcher(root.path(), &mut ctx);
    ctx.jobs = Some(Arc::new(rook_tools::jobs::Jobs::new(2, 2048)));
    let first = RunCommand
        .call(&ctx, &json!({"command":"printf started > started; sleep 60","background":true}))
        .await
        .unwrap();
    let id = first.meta["execution_environment"]["resource"].as_str().unwrap().to_owned();
    let _cleanup = OwnedContainer(id.clone());
    let job = first.meta["job"].as_str().unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ctx.workspace.join("started").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    std::fs::write(root.path().join("refuse-kill"), "fault enabled").unwrap();
    let jobs = ctx.jobs.as_ref().unwrap();
    assert!(jobs.stop(job));
    assert!(jobs.wait(job, std::time::Duration::from_secs(10)).await.unwrap().exit_code.is_some());
    assert!(
        jobs.quiesce(job).await.is_err(),
        "local Docker client cleanup cannot certify a failed remote stop"
    );
    let inspected = std::process::Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", &id])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&inspected.stdout).trim(),
        "true",
        "fault must reach the actual running container"
    );
    let env = ctx.execution.as_ref().unwrap();
    let state = env.pool.request(&ctx, env, rook_tools::environments::Request::Inspect).await.unwrap();
    assert_eq!(state["saved"]["status"], "interrupted_unknown");
}

#[tokio::test]
async fn a_not_yet_created_state_directory_inside_the_workspace_is_refused_before_any_setup_effect() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let state = workspace.join("private-state");
    assert!(!state.exists());
    let mut ctx = ToolContext::new(workspace.clone());
    ctx.execution = Some(Arc::new(Context {
        settings: Settings {
            selected: "box".into(),
            profiles: BTreeMap::from([(
                "box".into(),
                Profile::Container {
                    image: "alpine:3".into(),
                    program: "rook-fixture-launcher-that-does-not-exist".into(),
                    memory_mb: 128,
                    pids: 64,
                },
            )]),
            ..Default::default()
        },
        pool: Arc::new(Pool::default()),
        state_root: state.clone(),
    }));
    let error =
        RunCommand.call(&ctx, &json!({"command":"printf forbidden > forbidden"})).await.err().unwrap();
    assert!(error.to_string().contains("state must stay outside"), "{error}");
    assert!(!state.exists(), "refusal must precede metadata/setup-directory creation");
    assert!(!workspace.join("forbidden").exists());
}

struct RotatingSecret(std::sync::atomic::AtomicUsize);
impl rook_tools::Secrets for RotatingSecret {
    fn value(&self, name: &str) -> Option<String> {
        (name == "token").then(|| {
            format!("DYNAMIC_PRIVATE_VALUE_{}", self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        })
    }
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn a_dynamic_secret_is_resolved_once_for_admission_transport_and_redaction() {
    let (_root, mut ctx, _state) = container_context();
    let secret = Arc::new(RotatingSecret(std::sync::atomic::AtomicUsize::new(0)));
    ctx.secrets = Some(secret.clone());
    let result = RunCommand
        .call(&ctx, &json!({"command":"printf '%s' \"$ROOK_SECRET_TOKEN\"","secrets":["token"]}))
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        secret.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a dynamic helper must resolve the admitted value only once"
    );
    assert!(
        !result.content.contains("DYNAMIC_PRIVATE_VALUE_"),
        "the actually forwarded value must be masked"
    );
}
