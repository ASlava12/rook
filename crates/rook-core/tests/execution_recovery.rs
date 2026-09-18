//! Kill a real writer between the effect and its receipt, then reopen its store.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rook_core::agent::AgentLoop;
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, ToolSpec, Usage};

struct Script(Mutex<bool>);
#[async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "test/recovery"
    }
    fn context_window(&self) -> usize {
        32_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let classifier = request.messages.iter().any(|m| m.content.starts_with("Classify whether"));
        let mut called = self.0.lock().unwrap();
        let mut message = Message::assistant(if classifier { r#"{"action":"finish"}"# } else { "done" });
        let stop_reason = if !classifier && !*called {
            *called = true;
            message.tool_calls.push(ToolCall {
                id: "effect".into(),
                name: "effect".into(),
                arguments: serde_json::json!({}),
            });
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        };
        Ok(Response { message, stop_reason, usage: Usage::default(), model: "test/recovery".into() })
    }
}

struct Effect {
    wait: bool,
}
#[async_trait]
impl rook_tools::Tool for Effect {
    fn name(&self) -> &str {
        "effect"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "effect".into(),
            description: "Append an external effect marker".into(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        }
    }
    fn risk(&self, _: &serde_json::Value) -> rook_tools::policy::Risk {
        rook_tools::policy::Risk::Write(vec!["effect-marker".into()])
    }
    async fn call(
        &self,
        ctx: &rook_tools::ToolContext,
        _: &serde_json::Value,
    ) -> rook_tools::Result<rook_tools::ToolOutcome> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ctx.workspace.join("effect-marker"))
            .unwrap();
        file.write_all(b"x").unwrap();
        file.sync_all().unwrap();
        if self.wait {
            std::future::pending::<()>().await;
        }
        Ok(rook_tools::ToolOutcome::ok("effect happened"))
    }
}

#[tokio::test]
async fn recovery_child() {
    let Ok(mode) = std::env::var("ROOK_RECOVERY_CHILD") else { return };
    let workspace = PathBuf::from(std::env::var_os("ROOK_RECOVERY_WORKSPACE").unwrap());
    let rook = rook_core::Rook::open(Some(workspace.clone())).unwrap();
    let session = if mode == "crash" {
        let id = rook.start_session("interrupted command").unwrap();
        std::fs::write(workspace.join("session"), rook_store::format_session_id(id)).unwrap();
        id
    } else {
        rook.session_named(&std::fs::read_to_string(workspace.join("session")).unwrap()).unwrap()
    };
    if mode == "resume" {
        let recovered = rook.execution(session).unwrap();
        assert_eq!(recovered[0].status, "interrupted");
        assert_eq!(recovered[0].unknown.len(), 1);
        let mut agent = AgentLoop::new(&rook, Arc::new(Script(Mutex::new(false))), session);
        agent.tools.register(Arc::new(Effect { wait: false }));
        agent.allow_everything_not_denied();
        let outcome = agent.run("continue the old task").await.unwrap();
        assert_eq!(outcome.stopped, "recovery");
        std::fs::write(
            workspace.join("recovered.json"),
            serde_json::to_vec(&rook.execution(session).unwrap()).unwrap(),
        )
        .unwrap();
    } else {
        let mut agent = AgentLoop::new(&rook, Arc::new(Script(Mutex::new(false))), session);
        agent.tools.register(Arc::new(Effect { wait: true }));
        agent.allow_everything_not_denied();
        agent.run("perform one operation").await.unwrap();
        panic!("the child must be killed while the effect has no receipt");
    }
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_killed_process_leaves_a_durable_unknown_result_that_resume_does_not_repeat() {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[agent]\ninstall_servers = false\n").unwrap();
    let launch = |mode| {
        Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "recovery_child", "--nocapture"])
                .env("ROOK_HOME", home.path())
                .env("ROOK_RECOVERY_WORKSPACE", workspace.path())
                .env("ROOK_RECOVERY_CHILD", mode)
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut first = launch("crash");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !workspace.path().join("effect-marker").exists() {
        assert!(first.0.try_wait().unwrap().is_none(), "the child exited before the effect");
        assert!(std::time::Instant::now() < deadline, "the effect did not start");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Wait for the actual durable marker, not just its directory entry.
    while std::fs::read(workspace.path().join("effect-marker")).unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    first.0.kill().unwrap();
    assert!(!first.0.wait().unwrap().success());
    let mut second = launch("resume");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = second.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "restart did not finish");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(std::fs::read(workspace.path().join("effect-marker")).unwrap(), b"x");
    let receipts: serde_json::Value =
        serde_json::from_slice(&std::fs::read(workspace.path().join("recovered.json")).unwrap()).unwrap();
    assert_eq!(receipts[0]["unknown"].as_array().unwrap().len(), 1);
    assert_eq!(receipts[0]["unknown"][0]["tool"], "effect");
}
