use async_trait::async_trait;
use rook_core::{Config, Rook, agent::AgentLoop, shell_undo::Request as Undo};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
static TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct Shell {
    command: String,
    called: Mutex<bool>,
    timeout: u64,
    background: bool,
}
#[async_trait]
impl Provider for Shell {
    fn id(&self) -> &str {
        "fixture/container-engine"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let message = if request
            .messages
            .first()
            .is_some_and(|m| m.content.starts_with("Classify whether an assistant"))
        {
            Message::assistant(r#"{"action":"finish"}"#)
        } else if request.tools.iter().any(|tool| tool.name == "run_command") && !*self.called.lock().unwrap()
        {
            *self.called.lock().unwrap() = true;
            Message {
                tool_calls: vec![ToolCall {
                    id: "actual-container".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":self.command,"timeout_secs":self.timeout,"background":self.background}),
                }],
                ..Message::assistant("")
            }
        } else {
            Message::assistant("VERDICT: unproven")
        };
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Default::default(), model: self.id().into() })
    }
}
fn engine(root: &std::path::Path) -> Rook {
    let mut config = Config::default();
    config.memory.enabled = false;
    config.web.enabled = false;
    config.shell_undo.enabled = true;
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    config.sandbox.network = false;
    config.execution_environment=serde_json::from_value(json!({"selected":"box","profiles":{"box":{"kind":"container","image":"alpine:3","memory_mb":128,"pids":64}}})).unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        workspace,
    )
}
async fn run(engine: &Rook, session: u128, command: &str) {
    let mut agent = AgentLoop::new(
        engine,
        Arc::new(Shell {
            command: command.into(),
            called: Mutex::new(false),
            timeout: 30,
            background: false,
        }),
        session,
    );
    let result = agent.run("Run this fixture command once").await.unwrap();
    assert!(result.tools_called.contains(&"run_command".into()));
}
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn native_engine_retains_original_commands_certifies_effects_and_preserves_shell_undo() {
    let _serial = TESTS.lock().await;
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-container-engine-").tempdir_in(base).unwrap();
    let engine = engine(root.path());
    let owner = engine.start_session("container effects").unwrap();
    std::fs::write(engine.workspace.join("before.txt"), "original user edit").unwrap();
    let command = "cat before.txt; printf changed > before.txt; printf new > created.txt; sleep 60 >/dev/null 2>&1 & echo $! > descendant";
    run(&engine, owner, command).await;
    assert_eq!(std::fs::read_to_string(engine.workspace.join("before.txt")).unwrap(), "changed");
    let entries = engine.transcript(owner, 0, 128, 16384).unwrap();
    assert!(
        entries.iter().any(|e| e.body.contains(command)),
        "ordinary journal must retain the original command"
    );
    assert!(
        entries.iter().any(|e| e
            .tool_details
            .as_ref()
            .is_some_and(|d| d.text().contains("container box") && d.text().contains("resource"))),
        "ordinary tool result must retain the actual transport resource"
    );
    let receipts = engine.shell_undo_request(owner, Undo::List).unwrap();
    let receipt = receipts["items"][0]["id"].as_str().unwrap().to_owned();
    let view = engine.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap();
    assert_eq!(view["state"], "finished", "{view}");
    let paths = vec!["before.txt".into(), "created.txt".into(), "descendant".into()];
    let preview = engine
        .shell_undo_request(owner, Undo::Prepare { receipt: receipt.clone(), paths: paths.clone() })
        .unwrap();
    engine
        .shell_undo_request(
            owner,
            Undo::Restore { receipt, paths, token: preview["token"].as_str().unwrap().into() },
        )
        .unwrap();
    assert_eq!(std::fs::read_to_string(engine.workspace.join("before.txt")).unwrap(), "original user edit");
    assert!(!engine.workspace.join("created.txt").exists());
    let selected =
        engine.environment_request(owner, rook_core::environments::Request::Inspect).await.unwrap();
    assert_eq!(selected["observed"]["running"], true);
    let preview =
        engine.environment_request(owner, rook_core::environments::Request::PrepareRelease).await.unwrap();
    let release = engine
        .environment_request(
            owner,
            rook_core::environments::Request::Release {
                token: preview["token"].as_str().unwrap().into(),
                note: "Inspected completed container and restored source files".into(),
            },
        )
        .await
        .unwrap();
    let child = rook_store::parse_session_id(release["session"].as_str().unwrap()).unwrap();
    assert_eq!(engine.store.get_session(child).unwrap().unwrap().parent, Some(owner));
    let recovery = engine.execution(child).unwrap();
    assert_eq!(recovery[0].status, "complete");
}
#[tokio::test]
async fn execution_management_refuses_foreign_and_deleted_session_owners() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut engine = engine(root.path());
    engine.config.execution_environment = Default::default();
    let owner = engine.start_session("owner").unwrap();
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    let scoped = engine.for_workspace(foreign);
    assert!(scoped.environment_request(owner, rook_core::environments::Request::Inspect).await.is_err());
    let local: Value =
        engine.environment_request(owner, rook_core::environments::Request::Inspect).await.unwrap();
    assert_eq!(local["state"], "local");
    engine.store.delete_session(owner).unwrap();
    assert!(engine.environment_request(owner, rook_core::environments::Request::Inspect).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn reached_remote_stop_failure_is_an_unknown_execution_and_requires_observed_release() {
    let _serial = TESTS.lock().await;
    use rook_core::environments::Request as Environment;
    use std::os::unix::fs::PermissionsExt;
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-container-unknown-").tempdir_in(base).unwrap();
    let mut engine = engine(root.path());
    let launcher = root.path().join("docker-fixture");
    std::fs::write(&launcher, "#!/bin/sh\nif test \"$1\" = kill; then exit 1; fi\nexec docker \"$@\"\n")
        .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let rook_tools::environments::Profile::Container { program, .. } =
        engine.config.execution_environment.profiles.get_mut("box").unwrap()
    else {
        panic!()
    };
    *program = launcher.display().to_string();
    let owner = engine.start_session("unknown container effects").unwrap();
    let mut agent = AgentLoop::new(
        &engine,
        Arc::new(Shell {
            command: "printf started > started; sleep 60".into(),
            called: Mutex::new(false),
            timeout: 1,
            background: false,
        }),
        owner,
    );
    let _result = agent.run("Run the command once").await.unwrap();
    drop(agent);
    assert!(engine.workspace.join("started").exists());
    let receipts = engine.execution(owner).unwrap();
    assert_eq!(receipts[0].unknown.len(), 1, "a failed remote stop cannot become a known completed effect");
    assert_eq!(receipts[0].unknown[0].tool, "run_command");
    assert_eq!(receipts[0].status, "needs_review");
    let inspected = engine.environment_request(owner, Environment::Inspect).await.unwrap();
    assert_eq!(inspected["saved"]["status"], "interrupted_unknown");
    assert_eq!(inspected["observed"]["running"], true, "fault must reach an actual running container");
    let undo = engine.shell_undo_request(owner, Undo::List).unwrap();
    let id = undo["items"][0]["id"].as_str().unwrap().to_owned();
    let source = engine.shell_undo_request(owner, Undo::Inspect { receipt: id }).unwrap();
    assert_eq!(source["state"], "uncertain");
    let prepared = engine.environment_request(owner, Environment::PrepareRelease).await.unwrap();
    let released = engine
        .environment_request(
            owner,
            Environment::Release {
                token: prepared["token"].as_str().unwrap().into(),
                note: "Observed actual remote processes after stop transport failure".into(),
            },
        )
        .await
        .unwrap();
    let child = rook_store::parse_session_id(released["session"].as_str().unwrap()).unwrap();
    assert!(
        engine
            .transcript(child, 0, 64, 16384)
            .unwrap()
            .iter()
            .any(|entry| entry.label == "environment-release-source"
                && entry.body.contains(inspected["observed"]["id"].as_str().unwrap())),
        "release source must survive before the resource is removed"
    );
    assert_eq!(
        engine.execution(owner).unwrap()[0].unknown.len(),
        1,
        "resource release cannot silently acknowledge the original command"
    );
    engine
        .acknowledge_operation(
            owner,
            &receipts[0].unknown[0].id,
            "Inspected changed files and released the exact owned running container",
        )
        .unwrap();
    assert!(engine.execution(owner).unwrap()[0].unknown.is_empty());
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "needs actual Docker and installed alpine:3"]
async fn failed_background_cleanup_remains_unknown_across_a_new_agent_turn() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = TESTS.lock().await;
    let base = std::env::var_os("ROOK_ENV_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = tempfile::Builder::new().prefix("rook-container-background-").tempdir_in(base).unwrap();
    let mut engine = engine(root.path());
    let launcher = root.path().join("docker-fixture");
    std::fs::write(&launcher, "#!/bin/sh\nif test \"$1\" = kill; then exit 1; fi\nexec docker \"$@\"\n")
        .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let rook_tools::environments::Profile::Container { program, .. } =
        engine.config.execution_environment.profiles.get_mut("box").unwrap()
    else {
        panic!()
    };
    *program = launcher.display().to_string();
    let owner = engine.start_session("unknown background container").unwrap();
    let jobs = Arc::new(rook_tools::jobs::Jobs::new(2, 2048));
    let mut agent = AgentLoop::new(
        &engine,
        Arc::new(Shell {
            command: "printf started > started; sleep 60".into(),
            called: Mutex::new(false),
            timeout: 30,
            background: true,
        }),
        owner,
    );
    agent.tool_ctx.jobs = Some(jobs.clone());
    agent.run("Run once in the background").await.unwrap();
    drop(agent);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !engine.workspace.join("started").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let job = jobs.list()[0].id.clone();
    assert!(jobs.stop(&job));
    jobs.wait(&job, std::time::Duration::from_secs(10)).await.unwrap();
    assert_eq!(jobs.get(&job).unwrap().processes_stopped, Some(false));
    let mut next = AgentLoop::new(
        &engine,
        Arc::new(Shell {
            command: "printf forbidden > forbidden".into(),
            called: Mutex::new(false),
            timeout: 30,
            background: false,
        }),
        owner,
    );
    next.tool_ctx.jobs = Some(jobs.clone());
    let result = next.run("Start the next mutable command").await.unwrap();
    assert_eq!(result.stopped, "recovery");
    assert!(!engine.workspace.join("forbidden").exists());
    drop(next);
    let state = engine.execution(owner).unwrap();
    assert_eq!(state[0].unknown.len(), 1);
    assert_eq!(state[0].unknown[0].job.as_deref(), Some(job.as_str()));
    let view = engine.environment_request(owner, rook_core::environments::Request::Inspect).await.unwrap();
    assert_eq!(view["observed"]["running"], true);
}

#[cfg(unix)]
#[path = "../../rook-tools/tests/support/ssh.rs"]
mod ssh_fixture;
#[cfg(unix)]
fn ssh_engine(server: &ssh_fixture::Server) -> Rook {
    let context = server.context();
    let mut config = Config {
        execution_environment: context.execution.as_ref().unwrap().settings.clone(),
        ..Default::default()
    };
    config.memory.enabled = false;
    config.web.enabled = false;
    config.shell_undo.enabled = true;
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    Rook::from_parts(
        rook_store::Store::open(server.root.path().join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        context.workspace.clone(),
    )
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn native_ssh_timeout_keeps_unpublished_remote_files_unknown_even_after_certified_process_stop() {
    let _serial = TESTS.lock().await;
    let server = ssh_fixture::Server::start();
    let engine = ssh_engine(&server);
    let owner = engine.start_session("SSH deadline").unwrap();
    let mut agent = AgentLoop::new(
        &engine,
        Arc::new(Shell {
            command: "printf partial > result; sleep 60".into(),
            called: Mutex::new(false),
            timeout: 1,
            background: false,
        }),
        owner,
    );
    let _ = agent.run("Run this fixture command once").await;
    let receipts = engine.execution(owner).unwrap();
    assert_eq!(
        receipts[0].unknown.len(),
        1,
        "stopping remote processes must not resolve unpublished remote files"
    );
    let blocked =
        engine.environment_request(owner, rook_core::environments::Request::PrepareRecover).await.unwrap();
    assert!(
        engine
            .environment_request(
                owner,
                rook_core::environments::Request::Recover {
                    token: blocked["token"].as_str().unwrap().into(),
                    note: "Unknown execution is not acknowledged".into(),
                }
            )
            .await
            .is_err()
    );
    assert!(!engine.workspace.join("result").exists());
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn native_ssh_engine_preserves_original_commands_metadata_shell_undo_and_resource_reuse_across_turns() {
    let _serial = TESTS.lock().await;
    let server = ssh_fixture::Server::start();
    let engine = ssh_engine(&server);
    let owner = engine.start_session("SSH engine").unwrap();
    std::fs::write(engine.workspace.join("before"), "user edit").unwrap();
    run(&engine, owner, "printf remote > before; printf added > created").await;
    assert_eq!(std::fs::read_to_string(engine.workspace.join("before")).unwrap(), "remote");
    let entries = engine.transcript(owner, 0, 128, 8192).unwrap();
    assert!(
        entries.iter().any(|entry| entry
            .tool_details
            .as_ref()
            .is_some_and(|details| details.text().contains("ssh lab")))
    );
    assert!(entries.iter().any(|entry| entry.body.contains("printf remote > before")));
    let first = engine.environment_request(owner, rook_core::environments::Request::Inspect).await.unwrap();
    let scoped = engine.for_workspace(engine.workspace.clone());
    run(&scoped, owner, "cat before").await;
    let second = scoped.environment_request(owner, rook_core::environments::Request::Inspect).await.unwrap();
    assert_eq!(
        first["observed"]["id"], second["observed"]["id"],
        "workspace seeds share the actual retained SSH resource"
    );
    let receipts = engine.shell_undo_request(owner, Undo::List).unwrap();
    let receipt = receipts["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["changed_paths"].as_array().is_some_and(|paths| !paths.is_empty()))
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .unwrap_or_else(|| receipts["items"][0]["id"].as_str().unwrap().into());
    let inspected = engine.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap();
    assert_eq!(inspected["state"], "finished");
    let paths = vec!["before".into(), "created".into()];
    let prepared = engine
        .shell_undo_request(owner, Undo::Prepare { receipt: receipt.clone(), paths: paths.clone() })
        .unwrap();
    engine
        .shell_undo_request(
            owner,
            Undo::Restore { receipt, paths, token: prepared["token"].as_str().unwrap().into() },
        )
        .unwrap();
    assert_eq!(std::fs::read_to_string(engine.workspace.join("before")).unwrap(), "user edit");
    assert!(!engine.workspace.join("created").exists());
}
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires owned SSH fixture"]
async fn native_ssh_cancellation_retains_unknown_execution_and_recovery_requires_acknowledgement_without_command_replay()
 {
    use rook_core::environments::Request as Environment;
    let _serial = TESTS.lock().await;
    let server = ssh_fixture::Server::start();
    let engine = Arc::new(ssh_engine(&server));
    let owner = engine.start_session("SSH cancellation").unwrap();
    std::fs::write(engine.workspace.join("before"), "original").unwrap();
    let running = engine.clone();
    let task = tokio::spawn(async move {
        let mut agent = AgentLoop::new(
            &running,
            Arc::new(Shell {
                command: "printf partial > before; sleep 60".into(),
                called: Mutex::new(false),
                timeout: 60,
                background: false,
            }),
            owner,
        );
        agent.run("Run once").await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if engine
            .environment_request(owner, Environment::Inspect)
            .await
            .is_ok_and(|view| view["observed"]["running"] == true)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    task.abort();
    let _ = task.await;
    let execution = engine.execution(owner).unwrap();
    assert_eq!(execution[0].unknown.len(), 1);
    let view = engine.environment_request(owner, Environment::Inspect).await.unwrap();
    assert_eq!(view["observed"]["state"]["stopped"], true);
    assert_eq!(std::fs::read_to_string(engine.workspace.join("before")).unwrap(), "original");
    let preview = engine.environment_request(owner, Environment::PrepareRecover).await.unwrap();
    let request = || Environment::Recover {
        token: preview["token"].as_str().unwrap().into(),
        note: "Inspected actual stopped SSH command and retained file versions".into(),
    };
    assert!(
        engine.environment_request(owner, request()).await.is_err(),
        "ordinary unknown-effect acknowledgement precedes workspace publication"
    );
    engine
        .acknowledge_operation(
            owner,
            &execution[0].unknown[0].id,
            "Inspected stopped proxy and selected its partial file result",
        )
        .unwrap();
    let recovered = engine.environment_request(owner, request()).await.unwrap();
    assert_eq!(recovered["replayed"], false);
    assert_eq!(std::fs::read_to_string(engine.workspace.join("before")).unwrap(), "partial");
    let child = rook_store::parse_session_id(recovered["session"].as_str().unwrap()).unwrap();
    assert_eq!(engine.store.get_session(child).unwrap().unwrap().parent, Some(owner));
    assert!(
        engine
            .transcript(child, 0, 64, 8192)
            .unwrap()
            .iter()
            .any(|entry| entry.label == "environment-recover-source")
    );
    assert!(
        engine.environment_request(owner, request()).await.is_err(),
        "review tokens cannot replay a completed recovery"
    );
}
