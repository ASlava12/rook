use async_trait::async_trait;
use rook_core::{Config, Rook, agent::AgentLoop, shell_undo::Request as Undo};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

static TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn open(root: &std::path::Path, config: Config) -> Rook {
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
fn config() -> Config {
    let mut config = Config::default();
    config.shell_undo.enabled = true;
    config.memory.enabled = false;
    config.web.enabled = false;
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    config.storage.gc_grace_secs = 0;
    config
}
struct Shell {
    command: String,
    background: bool,
    called: Mutex<bool>,
}
#[async_trait]
impl Provider for Shell {
    fn id(&self) -> &str {
        "fixture/shell-undo"
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
                    id: "shell".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":self.command,"background":self.background}),
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
async fn run(
    rook: &Rook,
    owner: u128,
    command: &str,
    background: bool,
    jobs: Option<Arc<rook_tools::jobs::Jobs>>,
) {
    let mut agent = AgentLoop::new(
        rook,
        Arc::new(Shell { command: command.into(), background, called: Mutex::new(false) }),
        owner,
    );
    agent.tool_ctx.jobs = jobs;
    let result = agent.run("Run the fixture command once").await.unwrap();
    assert!(result.tools_called.contains(&"run_command".into()));
}
fn prepare(rook: &Rook, owner: u128, id: &str, paths: &[&str]) -> Value {
    rook.shell_undo_request(
        owner,
        Undo::Prepare { receipt: id.into(), paths: paths.iter().map(|s| (*s).into()).collect() },
    )
    .unwrap()
}
fn restore(rook: &Rook, owner: u128, id: &str, paths: &[&str], token: &str) -> rook_core::Result<Value> {
    rook.shell_undo_request(
        owner,
        Undo::Restore {
            receipt: id.into(),
            paths: paths.iter().map(|s| (*s).into()).collect(),
            token: token.into(),
        },
    )
}
fn id(rook: &Rook, owner: u128) -> String {
    rook.shell_undo_request(owner, Undo::List).unwrap()["items"][0]["id"].as_str().unwrap().into()
}
fn command() -> &'static str {
    if cfg!(windows) {
        "echo changed>old.txt & del deleted.txt & echo new>new.txt"
    } else {
        "printf 'changed\\n' > old.txt; rm deleted.txt; printf 'new\\n' > new.txt"
    }
}
#[tokio::test]
async fn actual_shell_edits_creation_and_deletion_restore_preexisting_user_edits_after_restart_and_gc() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let settings = config();
    let rook = open(root.path(), settings.clone());
    std::fs::write(rook.workspace.join("old.txt"), "user-local-edit\n").unwrap();
    std::fs::write(rook.workspace.join("deleted.txt"), "retained original\n").unwrap();
    let owner = rook.start_session("shell undo").unwrap();
    run(&rook, owner, command(), false, None).await;
    assert!(!rook.workspace.join("deleted.txt").exists());
    assert!(rook.workspace.join("new.txt").exists());
    let receipt = id(&rook, owner);
    let inspected = rook.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap();
    assert_eq!(inspected["state"], "finished");
    assert_eq!(inspected["changed_paths"].as_array().unwrap().len(), 3);
    drop(rook);
    let rook = open(root.path(), settings);
    rook.collect_garbage(false).unwrap();
    let paths = ["old.txt", "deleted.txt", "new.txt"];
    let prepared = prepare(&rook, owner, &receipt, &paths);
    let done = restore(&rook, owner, &receipt, &paths, prepared["token"].as_str().unwrap()).unwrap();
    assert_eq!(done["restored"].as_array().unwrap().len(), 3);
    assert_eq!(std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap(), "user-local-edit\n");
    assert_eq!(std::fs::read_to_string(rook.workspace.join("deleted.txt")).unwrap(), "retained original\n");
    assert!(!rook.workspace.join("new.txt").exists());
    rook.store.delete_session(owner).unwrap();
    assert!(rook.store.kv_get(&format!("shell-undo/{owner:032x}")).unwrap().is_none());
}
#[tokio::test]
async fn a_changed_file_after_preview_refuses_the_entire_restore_without_overwriting_any_path() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    std::fs::write(rook.workspace.join("old.txt"), "original\n").unwrap();
    std::fs::write(rook.workspace.join("deleted.txt"), "original deleted\n").unwrap();
    let owner = rook.start_session("restore conflict").unwrap();
    run(&rook, owner, command(), false, None).await;
    let receipt = id(&rook, owner);
    let paths = ["old.txt", "deleted.txt", "new.txt"];
    let prepared = prepare(&rook, owner, &receipt, &paths);
    std::fs::write(rook.workspace.join("old.txt"), "later external edit\n").unwrap();
    let error = restore(&rook, owner, &receipt, &paths, prepared["token"].as_str().unwrap()).unwrap_err();
    assert!(error.to_string().contains("conflict"));
    assert_eq!(std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap(), "later external edit\n");
    assert!(!rook.workspace.join("deleted.txt").exists());
    assert!(rook.workspace.join("new.txt").exists());
}
#[tokio::test]
async fn a_reached_snapshot_byte_budget_prevents_the_real_command_from_starting() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut settings = config();
    settings.shell_undo.file_bytes = 1024;
    let rook = open(root.path(), settings);
    std::fs::write(rook.workspace.join("large.txt"), vec![b'x'; 1025]).unwrap();
    assert!(
        std::fs::metadata(rook.workspace.join("large.txt")).unwrap().len()
            > rook.config.shell_undo.file_bytes as u64
    );
    let owner = rook.start_session("bounded shell").unwrap();
    run(&rook, owner, "echo should-not-run > marker.txt", false, None).await;
    assert!(!rook.workspace.join("marker.txt").exists());
    assert_eq!(rook.shell_undo_request(owner, Undo::List).unwrap()["items"], json!([]));
    assert!(
        rook.transcript(owner, 0, 64, 8192)
            .unwrap()
            .iter()
            .any(|entry| entry.body.contains("command did not start"))
    );
}
#[cfg(unix)]
#[tokio::test]
async fn a_background_job_keeps_its_preimage_and_cannot_restore_while_it_can_still_write() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut settings = config();
    settings.shell_undo.exclude.push("release".into());
    let rook = open(root.path(), settings);
    std::fs::write(rook.workspace.join("old.txt"), "before background\n").unwrap();
    let owner = rook.start_session("background shell").unwrap();
    let jobs = rook_core::agent::jobs_for(&rook.config);
    run(
        &rook,
        owner,
        "while [ ! -e release ]; do sleep 0.02; done; printf 'after background\\n' > old.txt",
        true,
        Some(jobs.clone()),
    )
    .await;
    let receipt = id(&rook, owner);
    assert_eq!(
        rook.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap()["state"],
        "background"
    );
    assert!(
        rook.shell_undo_request(
            owner,
            Undo::Prepare { receipt: receipt.clone(), paths: vec!["old.txt".into()] }
        )
        .is_err()
    );
    std::fs::write(rook.workspace.join("release"), "release\n").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if rook.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap()["state"]
                == "finished"
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("actual background completion must finalize its shell snapshot");
    let prepared = prepare(&rook, owner, &receipt, &["old.txt"]);
    restore(&rook, owner, &receipt, &["old.txt"], prepared["token"].as_str().unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap(), "before background\n");
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_retains_preimages_and_requires_inspected_recovery_before_selected_restore() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = Arc::new(open(root.path(), config()));
    std::fs::write(rook.workspace.join("old.txt"), "before cancellation\n").unwrap();
    let owner = rook.start_session("cancel real command").unwrap();
    let task = tokio::spawn({
        let rook = rook.clone();
        async move {
            run(
                &rook,
                owner,
                "printf '%s' $$ > owned-pid; printf 'changed' > old.txt; sleep 60; printf 'late' > late.txt",
                false,
                None,
            )
            .await;
        }
    });
    let pid = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap() == "changed"
                && let Ok(pid) = std::fs::read_to_string(rook.workspace.join("owned-pid"))
                && let Ok(pid) = pid.parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let group = rook_contain::Group::holding(Some(pid));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while group.alive() == Some(true) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(group.alive(), Some(false), "the actual owned command group must be terminal");
    assert!(!rook.workspace.join("late.txt").exists());
    let receipt = id(&rook, owner);
    let observed = rook.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap();
    assert_eq!(observed["state"], "interrupted");
    assert!(observed["after_source"].is_null());
    let note = "inspected changed old.txt and the owned PID; the entire process group is gone";
    let recover = || Undo::Recover { receipt: receipt.clone(), note: note.into() };
    assert!(
        rook.shell_undo_request(owner, recover()).is_err(),
        "unknown execution effects require acknowledgement first"
    );
    let executions = rook.execution(owner).unwrap();
    assert_eq!(executions[0].unknown.len(), 1);
    rook.acknowledge_operation(owner, &executions[0].unknown[0].id, note).unwrap();
    let recovered = rook.shell_undo_request(owner, recover()).unwrap();
    assert_eq!(recovered["state"], "recovered", "recovery is an observation, never a successful completion");
    let prepared = prepare(&rook, owner, &receipt, &["old.txt"]);
    restore(&rook, owner, &receipt, &["old.txt"], prepared["token"].as_str().unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap(), "before cancellation\n");
}

#[tokio::test]
async fn creation_and_deletion_can_restore_two_changed_paths_with_a_one_file_snapshot_budget() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut settings = config();
    settings.shell_undo.max_files = 1;
    let rook = open(root.path(), settings);
    std::fs::write(rook.workspace.join("deleted.txt"), "original").unwrap();
    let owner = rook.start_session("bounded union").unwrap();
    let command = if cfg!(windows) {
        "del deleted.txt & echo new>new.txt"
    } else {
        "rm deleted.txt; printf 'new' > new.txt"
    };
    run(&rook, owner, command, false, None).await;
    let receipt = id(&rook, owner);
    let paths = ["deleted.txt", "new.txt"];
    let prepared = prepare(&rook, owner, &receipt, &paths);
    restore(&rook, owner, &receipt, &paths, prepared["token"].as_str().unwrap()).unwrap();
    let report = rook.shell_undo_request(owner, Undo::Inspect { receipt }).unwrap();
    assert_eq!(report["restored_paths"].as_array().unwrap().len(), 2);
    assert_eq!(report["restore_attempts"][0]["state"], "finished");
}

#[cfg(unix)]
#[tokio::test]
async fn a_partial_restore_persists_completed_and_pending_files_and_resumes_only_after_inspection() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let dir = rook.workspace.join("directory");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(rook.workspace.join("old.txt"), "original").unwrap();
    std::fs::write(dir.join("deleted.txt"), "original nested").unwrap();
    let owner = rook.start_session("partial restore").unwrap();
    run(&rook, owner, "printf 'changed' > old.txt; rm directory/deleted.txt", false, None).await;
    let receipt = id(&rook, owner);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // A privileged test runner ignores directory permissions, so it cannot
    // exercise this failure. The precondition is verified on ordinary users.
    if std::fs::write(dir.join("probe"), "probe").is_ok() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let paths = ["old.txt", "directory/deleted.txt"];
    let prepared = prepare(&rook, owner, &receipt, &paths);
    assert!(restore(&rook, owner, &receipt, &paths, prepared["token"].as_str().unwrap()).is_err());
    assert_eq!(std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap(), "original");
    let report = rook.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap();
    assert_eq!(report["restore_attempts"][0]["state"], "partial");
    assert_eq!(report["restore_attempts"][0]["completed"], json!(["old.txt"]));
    assert_eq!(report["restore_attempts"][0]["pending"], "directory/deleted.txt");
    assert!(
        rook.shell_undo_request(
            owner,
            Undo::Prepare { receipt: receipt.clone(), paths: vec!["directory/deleted.txt".into()] }
        )
        .is_err()
    );
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    rook.shell_undo_request(
        owner,
        Undo::Recover {
            receipt: receipt.clone(),
            note: "old.txt restored, nested file absent; directory permissions repaired".into(),
        },
    )
    .unwrap();
    let prepared = prepare(&rook, owner, &receipt, &["directory/deleted.txt"]);
    restore(&rook, owner, &receipt, &["directory/deleted.txt"], prepared["token"].as_str().unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("deleted.txt")).unwrap(), "original nested");
}

struct Delegating {
    messages: Mutex<std::collections::VecDeque<Message>>,
}
#[async_trait]
impl Provider for Delegating {
    fn id(&self) -> &str {
        "fixture/delegated-shell-undo"
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
        } else {
            self.messages
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Message::assistant("VERDICT: unproven"))
        };
        let stop_reason =
            if message.tool_calls.is_empty() { StopReason::EndTurn } else { StopReason::ToolUse };
        Ok(Response { message, stop_reason, usage: Default::default(), model: self.id().into() })
    }
}
#[tokio::test]
async fn a_real_delegated_shell_retains_its_child_owned_preimage_and_restores_user_edits() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    std::fs::write(rook.workspace.join("old.txt"), "user edit before delegation").unwrap();
    let owner = rook.start_session("delegate shell edit").unwrap();
    let call = |name: &str, arguments| Message {
        tool_calls: vec![ToolCall { id: name.into(), name: name.into(), arguments }],
        ..Message::assistant("")
    };
    let command = if cfg!(windows) { "echo delegated>old.txt" } else { "printf 'delegated' > old.txt" };
    let provider = Arc::new(Delegating {
        messages: Mutex::new(std::collections::VecDeque::from([
            call("delegate", json!({"tasks":["run the shell edit once"]})),
            call("run_command", json!({"command":command})),
            Message::assistant("child ran the command"),
            Message::assistant("parent reports child result"),
        ])),
    });
    let mut agent = AgentLoop::new(&rook, provider, owner);
    let outcome = agent.run("Delegate the fixture edit").await.unwrap();
    let children: Vec<_> = outcome
        .delegated
        .iter()
        .map(|id| rook_store::parse_session_id(id).unwrap())
        .filter(|id| {
            !rook.shell_undo_request(*id, Undo::List).unwrap()["items"].as_array().unwrap().is_empty()
        })
        .collect();
    assert_eq!(
        children.len(),
        1,
        "only the actual shell worker owns a preimage; auxiliary checking may also create children"
    );
    let child = children[0];
    assert_eq!(rook.store.get_session(child).unwrap().unwrap().parent, Some(owner));
    assert!(
        rook.shell_undo_request(owner, Undo::List).unwrap()["items"].as_array().unwrap().is_empty(),
        "the parent never executed the shell itself"
    );
    assert!(std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap().contains("delegated"));
    let receipt = id(&rook, child);
    let prepared = prepare(&rook, child, &receipt, &["old.txt"]);
    assert!(
        restore(&rook, owner, &receipt, &["old.txt"], prepared["token"].as_str().unwrap()).is_err(),
        "receipt IDs never grant another session ownership"
    );
    restore(&rook, child, &receipt, &["old.txt"], prepared["token"].as_str().unwrap()).unwrap();
    assert_eq!(
        std::fs::read_to_string(rook.workspace.join("old.txt")).unwrap(),
        "user edit before delegation"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_shell_receipt_observer_does_not_keep_background_processes_alive_after_the_frontend_closes() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    std::fs::write(rook.workspace.join("old.txt"), "original").unwrap();
    let owner = rook.start_session("frontend lifetime").unwrap();
    let jobs = Arc::new(rook_tools::jobs::Jobs::new(2, 8192));
    run(
        &rook,
        owner,
        "printf 'changed' > old.txt; sleep 60; printf 'late' > late.txt",
        true,
        Some(jobs.clone()),
    )
    .await;
    let job = jobs.get("job001").unwrap();
    assert!(job.exit_code.is_none());
    let group = rook_contain::Group::holding(job.group);
    assert_eq!(group.alive(), Some(true), "the actual background group is live before closure");
    let weak = Arc::downgrade(&jobs);
    drop(jobs);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while weak.upgrade().is_some() || group.alive() == Some(true) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let receipt = id(&rook, owner);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let report = rook.shell_undo_request(owner, Undo::Inspect { receipt: receipt.clone() }).unwrap();
            if report["state"] == "interrupted" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!rook.workspace.join("late.txt").exists());
}
