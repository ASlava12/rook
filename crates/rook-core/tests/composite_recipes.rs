use rook_core::{Config, Rook, agent::AgentLoop, composite_recipes::Request as Inspect};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
struct Fixture {
    // Own the workspace until every engine and receipt user has finished.
    _root: tempfile::TempDir,
    rook: Rook,
    session: u128,
}
impl Fixture {
    fn new(main: &str, a: &str, b: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".rook/recipes")).unwrap();
        for (name, source) in [("main", main), ("a", a), ("b", b)] {
            std::fs::write(root.path().join(format!(".rook/recipes/{name}.toml")), source).unwrap();
        }
        let mut config = Config::default();
        config.memory.enabled = false;
        config.web.enabled = false;
        config.agent.install_servers = false;
        config.agent.one_script = false;
        config.agent.plan_first = false;
        config.agent.max_steps = 8;
        config.agent.max_turn_tokens = 65536;
        config.agent.max_parallel_subagents = 2;
        config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
        let rook = Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "0.11.2"),
            rook_skills::SkillIndex::default(),
            root.path().into(),
        );
        let session = rook.start_session("recipe parent").unwrap();
        Self { _root: root, rook, session }
    }
    fn call(&self) -> Value {
        self.rook.composite_request(self.session, Inspect::List { after: None }).unwrap()["calls"][0].clone()
    }
    fn details(&self) -> Value {
        self.rook
            .composite_request(
                self.session,
                Inspect::Inspect { id: self.call()["id"].as_str().unwrap().into() },
            )
            .unwrap()
    }
}
const MAIN: &str =
    "version=1\nprompt='ROOT_RECIPE'\n[subrecipes.a]\npath='a.toml'\n[subrecipes.b]\npath='b.toml'\n";
const A: &str = "version=1\nprompt='SUB_A return a symbolic output marker'\n";
const B: &str = "version=1\nprompt='SUB_B input {{value}}'\n[parameters.value]\ndefault='none'\n";
struct Script {
    plan: Value,
    called: AtomicBool,
    requests: Mutex<Vec<Request>>,
    barrier: Option<tokio::sync::Barrier>,
    fail_a: bool,
    error_a: bool,
    workspace: Option<std::path::PathBuf>,
}
impl Script {
    fn new(plan: Value) -> Self {
        Self {
            plan,
            called: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            barrier: None,
            fail_a: false,
            error_a: false,
            workspace: None,
        }
    }
}
#[async_trait::async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "fixture/composite"
    }
    fn context_window(&self) -> usize {
        128000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let classifier = request.messages.first().is_some_and(|m| m.content.starts_with("Classify whether"));
        let text = request.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
        {
            let mut requests = self.requests.lock().unwrap();
            assert!(requests.len() < 256);
            requests.push(request.clone());
        }
        let root = request.tools.iter().any(|t| t.name == "run_subrecipes");
        if !root && text.contains("SUB_A") && self.error_a {
            return Err(rook_llm::LlmError::Decode(
                "response/cost coverage lost in the owned fixture".into(),
            ));
        }
        let message = if classifier {
            Message::assistant(r#"{"action":"finish"}"#)
        } else if root && !self.called.swap(true, Ordering::SeqCst) {
            if let Some(workspace) = &self.workspace {
                std::fs::write(
                    workspace.join(".rook/recipes/a.toml"),
                    "version=1\nprompt='CHANGED_AFTER_ADMISSION'\n",
                )
                .unwrap();
            }
            Message {
                tool_calls: vec![ToolCall {
                    id: "compose-one".into(),
                    name: "run_subrecipes".into(),
                    arguments: self.plan.clone(),
                }],
                ..Message::assistant("")
            }
        } else if !root && text.contains("SUB_A") {
            assert!(
                !text.contains("PARENT_ONLY_SECRET"),
                "separate child context inherited the parent request"
            );
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            if self.fail_a {
                Message {
                    tool_calls: vec![ToolCall {
                        id: "limit-read".into(),
                        name: "read_file".into(),
                        arguments: json!({"path":".rook/recipes/a.toml"}),
                    }],
                    ..Message::assistant("")
                }
            } else {
                Message::assistant("OUT_A")
            }
        } else if !root && text.contains("SUB_B") {
            assert!(!text.contains("PARENT_ONLY_SECRET"));
            if self.plan["mode"] != "parallel" {
                assert!(text.contains("OUT_A"), "dependent stage lacks the earlier completed output: {text}");
            }
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            Message::assistant("OUT_B")
        } else {
            Message::assistant("The requested stage reports are available.")
        };
        Ok(Response {
            message,
            usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            stop_reason: StopReason::EndTurn,
            model: self.id().into(),
        })
    }
}
fn options() -> rook_proto::TurnOptions {
    rook_proto::TurnOptions {
        recipe: Some(rook_proto::RecipeInvocation { path: "main".into(), parameters: BTreeMap::new() }),
        ..Default::default()
    }
}
async fn run(f: &Fixture, model: Arc<Script>) -> rook_core::Result<rook_core::agent::TurnOutcome> {
    let mut agent = AgentLoop::new(&f.rook, model, f.session);
    agent.options = options();
    agent.run("PARENT_ONLY_SECRET").await
}

#[tokio::test]
async fn recording_override_is_inherited_by_frozen_child_recipes() {
    for enabled in [false, true] {
        let mut f = Fixture::new(MAIN, A, B);
        f.rook.config.telemetry.model_calls.enabled = !enabled;
        let model =
            Arc::new(Script::new(json!({"mode":"sequential","stages":[{"id":"first","recipe":"a"}]})));
        let mut agent = AgentLoop::new(&f.rook, model, f.session);
        agent.options = options();
        agent.options.record_model_calls = Some(enabled);
        let result = agent.run("Inspect the selected recipe.").await.unwrap();
        assert!(!result.delegated.is_empty());
        for id in std::iter::once(f.session)
            .chain(result.delegated.iter().map(|id| rook_store::parse_session_id(id).unwrap()))
        {
            let count = f
                .rook
                .store
                .events(id, 0, 256)
                .unwrap()
                .iter()
                .filter(|e| e.record.label == "rook:model-call-input:v1")
                .count();
            let purposes: Vec<_> = f
                .rook
                .store
                .events(id, 0, 256)
                .unwrap()
                .iter()
                .filter(|e| e.record.label == "rook:model-call-input:v1")
                .map(|e| {
                    let row: Value =
                        serde_json::from_slice(&f.rook.store.get(&e.record.body).unwrap()).unwrap();
                    row["purpose"].clone()
                })
                .collect();
            assert_eq!(
                count > 0,
                enabled,
                "recording selection was lost: session={id}, parent={}, enabled={enabled}, purposes={purposes:?}",
                f.session
            );
        }
    }
}
#[tokio::test]
async fn sequential_subrecipes_keep_frozen_definitions_separate_contexts_output_sources_and_child_lineage() {
    let f = Fixture::new(MAIN, A, B);
    let mut model = Script::new(
        json!({"mode":"sequential","stages":[{"id":"first","recipe":"a"},{"id":"second","recipe":"b","inputs":[{"stage":"first","parameter":"value"}]}]}),
    );
    model.workspace = Some(f.rook.workspace.clone());
    let model = Arc::new(model);
    let result = run(&f, model.clone()).await.unwrap();
    assert!(result.tools_called.iter().any(|t| t == "run_subrecipes"));
    assert!(result.delegated.len() >= 2, "{result:?}");
    let details = f.details();
    assert_eq!(details["state"], "completed", "{details}");
    assert_eq!(details["stages"][1]["parameters"]["value"], "OUT_A");
    for child in &result.delegated {
        let id = rook_store::parse_session_id(child).unwrap();
        assert_eq!(f.rook.store.get_session(id).unwrap().unwrap().parent, Some(f.session));
    }
    let first = details["stages"][0]["output"].as_u64().unwrap();
    let read = f
        .rook
        .composite_request(
            f.session,
            Inspect::Read { id: details["id"].as_str().unwrap().into(), seq: first, offset: 0 },
        )
        .unwrap();
    use base64::Engine;
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(read["bytes_base64"].as_str().unwrap()).unwrap(),
        b"OUT_A"
    );
    assert!(
        !model
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content.contains("CHANGED_AFTER_ADMISSION")))
    );
}
#[tokio::test]
async fn parallel_subrecipes_reach_independent_model_requests_together_and_keep_ordered_receipts() {
    let f = Fixture::new(MAIN, A, B);
    let mut model = Script::new(
        json!({"mode":"parallel","stages":[{"id":"one","recipe":"a"},{"id":"two","recipe":"b"}]}),
    );
    model.barrier = Some(tokio::sync::Barrier::new(2));
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), run(&f, Arc::new(model)))
        .await
        .unwrap()
        .unwrap();
    assert!(result.delegated.len() >= 2);
    let details = f.details();
    assert_eq!(details["state"], "completed");
    assert_eq!(details["stages"][0]["id"], "one");
    assert_eq!(details["stages"][1]["id"], "two");
}
#[tokio::test]
async fn a_limited_stage_keeps_its_failure_and_prevents_dependent_stage_dispatch() {
    let f = Fixture::new(MAIN, &format!("{A}[limits]\nsteps=1\n"), B);
    let mut model = Script::new(
        json!({"stages":[{"id":"one","recipe":"a"},{"id":"two","recipe":"b","inputs":[{"stage":"one","parameter":"value"}]}]}),
    );
    model.fail_a = true;
    run(&f, Arc::new(model)).await.unwrap();
    let details = f.details();
    assert_eq!(details["state"], "failed", "{details}");
    assert_eq!(details["stages"][0]["state"], "failed");
    assert_eq!(details["stages"][1]["state"], "skipped");
    assert!(details["stages"][1]["child"].is_null());
}
#[tokio::test]
async fn cycles_nesting_and_reached_source_byte_bounds_fail_before_any_model_or_child() {
    for main in ["version=1\nprompt='root'\n[subrecipes.loop]\npath='main.toml'\n", MAIN] {
        let mut f = Fixture::new(main, A, B);
        if main == MAIN {
            f.rook.config.composite_recipes.max_depth = 0;
        }
        let model = Arc::new(Script::new(json!({})));
        assert!(run(&f, model.clone()).await.is_err());
        assert!(model.requests.lock().unwrap().is_empty());
        assert_eq!(f.rook.store.list_sessions().unwrap().len(), 1);
    }
    let source = format!("version=1\nprompt='{}'\n", "x".repeat(4096));
    let mut f = Fixture::new(&source, A, B);
    f.rook.config.composite_recipes.source_bytes = 4096;
    assert!(source.len() > 4096);
    let model = Arc::new(Script::new(json!({})));
    assert!(run(&f, model.clone()).await.is_err());
    assert!(model.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn fixed_parameters_cannot_be_replaced_and_parallel_dependencies_create_no_child() {
    for (main, plan) in [
        (
            "version=1\nprompt='ROOT_RECIPE'\n[subrecipes.b]\npath='b.toml'\n[subrecipes.b.values]\nvalue='fixed'\n",
            json!({"stages":[{"id":"one","recipe":"b","parameters":{"value":"changed"}}]}),
        ),
        (
            MAIN,
            json!({"mode":"parallel","stages":[{"id":"one","recipe":"a"},{"id":"two","recipe":"b","inputs":[{"stage":"one","parameter":"value"}]}]}),
        ),
    ] {
        let f = Fixture::new(main, A, B);
        run(&f, Arc::new(Script::new(plan))).await.unwrap();
        let actual = f
            .rook
            .store
            .list_sessions()
            .unwrap()
            .iter()
            .filter(|meta| {
                f.rook
                    .store
                    .events(meta.id, 0, 1)
                    .unwrap()
                    .first()
                    .is_some_and(|e| e.record.label == "recipe stage inputs")
            })
            .count();
        assert_eq!(actual, 0, "a refused call admitted a subrecipe child");
    }
}

#[cfg(unix)]
struct CancelModel {
    root: std::path::PathBuf,
    called: AtomicBool,
}
#[cfg(unix)]
#[async_trait::async_trait]
impl Provider for CancelModel {
    fn id(&self) -> &str {
        "fixture/recipe-cancel"
    }
    fn context_window(&self) -> usize {
        128000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let root = request.tools.iter().any(|t| t.name == "run_subrecipes");
        let message = if root && !self.called.swap(true, Ordering::SeqCst) {
            Message {
                tool_calls: vec![ToolCall {
                    id: "cancel-call".into(),
                    name: "run_subrecipes".into(),
                    arguments: json!({"stages":[{"id":"owned","recipe":"a"}]}),
                }],
                ..Message::assistant("")
            }
        } else {
            assert!(!self.root.join("cancel-reached").exists(), "a canceled child was replayed");
            Message {
                tool_calls: vec![ToolCall {
                    id: "owned-command".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":"printf '%s' \"$$\" > cancel-pid; touch cancel-reached; sleep 60"}),
                }],
                ..Message::assistant("")
            }
        };
        Ok(Response {
            message,
            usage: Default::default(),
            stop_reason: StopReason::EndTurn,
            model: self.id().into(),
        })
    }
}
#[cfg(unix)]
#[tokio::test]
async fn canceling_a_real_child_command_retains_interruption_and_reopens_without_replaying_it() {
    let f = Fixture::new(MAIN, A, B);
    let model = Arc::new(CancelModel { root: f.rook.workspace.clone(), called: AtomicBool::new(false) });
    let mut agent = AgentLoop::new(&f.rook, model, f.session);
    agent.options = options();
    let mut running = Box::pin(agent.run("Run the owned cancellation fixture"));
    let reached = async {
        while !f.rook.workspace.join("cancel-reached").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    tokio::select! {result=&mut running=>panic!("the actual owned command ended before cancellation: {result:?}"),result=tokio::time::timeout(std::time::Duration::from_secs(60),reached)=>result.unwrap()};
    drop(running);
    drop(agent);
    let details = f.details();
    assert_eq!(details["state"], "interrupted", "{details}");
    assert_eq!(details["stages"][0]["state"], "interrupted");
    let child = details["stages"][0]["child"].as_str().unwrap().to_owned();
    let id = details["id"].as_str().unwrap().to_owned();
    assert!(f.rook.execution(f.session).unwrap().iter().any(|e| !e.unknown.is_empty()));
    let pid = std::fs::read_to_string(f.rook.workspace.join("cancel-pid")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(std::time::Instant::now() < deadline, "the owned command process survived cancellation");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let config = f.rook.config.clone();
    let workspace = f.rook.workspace.clone();
    let session = f.session;
    drop(f.rook);
    let rook = Rook::from_parts(
        rook_store::Store::open(f._root.path().join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        workspace,
    );
    let reopened = rook.composite_request(session, Inspect::Inspect { id: id.clone() }).unwrap();
    assert_eq!(reopened["state"], "interrupted");
    assert_eq!(reopened["stages"][0]["child"], child);
    assert!(rook.composite_request(session, Inspect::Forget { id: id.clone() }).is_err());
    let resolved = rook
        .composite_request(
            session,
            Inspect::Resolve {
                id: id.clone(),
                note: "Observed the owned process stop; inspect the original execution separately".into(),
            },
        )
        .unwrap();
    assert_eq!(resolved["replayed"], false);
    assert_eq!(resolved["original_execution_acknowledgement"], false);
    assert!(rook.execution(session).unwrap().iter().any(|e| !e.unknown.is_empty()));
    rook.composite_request(session, Inspect::Forget { id }).unwrap();
    assert!(rook.store.get_session(rook_store::parse_session_id(&child).unwrap()).unwrap().is_some());
}

#[tokio::test]
async fn source_pages_refuse_replacement_events_under_a_stale_restored_receipt() {
    let f = Fixture::new(MAIN, A, B);
    run(&f, Arc::new(Script::new(json!({"stages":[{"id":"one","recipe":"a"}]})))).await.unwrap();
    let details = f.details();
    let seq = details["stages"][0]["output"].as_u64().unwrap();
    let id = details["id"].as_str().unwrap().to_owned();
    let key = format!("composite-recipes/{:032x}", f.session);
    let saved = f.rook.store.kv_get(&key).unwrap().unwrap();
    let meta = f.rook.store.get_session(f.session).unwrap().unwrap();
    f.rook.store.delete_session(f.session).unwrap();
    let replacement =
        rook_store::SessionMeta::new(f.session, meta.title, meta.workspace, rook_store::now_unix());
    f.rook.store.create_session(&replacement).unwrap();
    for _ in 0..=seq {
        f.rook.log(f.session, rook_store::EventKind::Note, "replacement", "Different source").unwrap();
    }
    f.rook.store.kv_set(&key, &saved).unwrap();
    assert!(f.rook.composite_request(f.session, Inspect::Read { id, seq, offset: 0 }).is_err());
}

#[tokio::test]
async fn child_token_allowances_do_not_multiply_the_remaining_parent_budget() {
    let mut f = Fixture::new(MAIN, A, B);
    f.rook.config.agent.max_turn_tokens = 25;
    let model = Arc::new(Script::new(
        json!({"stages":[{"id":"one","recipe":"a"},{"id":"two","recipe":"b","inputs":[{"stage":"one","parameter":"value"}]}]}),
    ));
    let result = run(&f, model.clone()).await.unwrap();
    assert!(
        result.input_tokens + result.output_tokens >= 25,
        "fixture did not reach the actual parent spend ceiling"
    );
    let details = f.details();
    assert_eq!(details["stages"][0]["state"], "failed");
    assert!(details["stages"][0]["error"].as_str().unwrap().contains("budget"));
    assert_eq!(details["stages"][1]["state"], "skipped");
    assert!(details["stages"][1]["child"].is_null());
    assert!(
        !model
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.messages.iter().any(|m| m.content.contains("SUB_B")))
    );
}

struct Nested {
    root: AtomicBool,
    inner: AtomicBool,
}
#[async_trait::async_trait]
impl Provider for Nested {
    fn id(&self) -> &str {
        "fixture/nested-recipes"
    }
    fn context_window(&self) -> usize {
        128000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let text = request.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
        let can_run = request.tools.iter().any(|t| t.name == "run_subrecipes");
        let message = if request.messages.first().is_some_and(|m| m.content.starts_with("Classify whether")) {
            Message::assistant(r#"{"action":"finish"}"#)
        } else if can_run && text.contains("INNER_RECIPE") && !self.inner.swap(true, Ordering::SeqCst) {
            Message {
                tool_calls: vec![ToolCall {
                    id: "inner-call".into(),
                    name: "run_subrecipes".into(),
                    arguments: json!({"stages":[{"id":"leaf","recipe":"leaf"}]}),
                }],
                ..Message::assistant("")
            }
        } else if can_run && !text.contains("INNER_RECIPE") && !self.root.swap(true, Ordering::SeqCst) {
            Message {
                tool_calls: vec![ToolCall {
                    id: "root-call".into(),
                    name: "run_subrecipes".into(),
                    arguments: json!({"stages":[{"id":"inner","recipe":"inner"}]}),
                }],
                ..Message::assistant("")
            }
        } else {
            Message::assistant("NESTED_STAGE_OUTPUT")
        };
        Ok(Response {
            message,
            stop_reason: StopReason::EndTurn,
            usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            model: self.id().into(),
        })
    }
}
#[tokio::test]
async fn nested_subrecipes_retain_each_composition_and_leaf_session_under_its_actual_parent() {
    let mut f = Fixture::new(
        "version=1\nprompt='ROOT_RECIPE'\n[subrecipes.inner]\npath='a.toml'\n",
        "version=1\nprompt='INNER_RECIPE'\n[subrecipes.leaf]\npath='b.toml'\n",
        "version=1\nprompt='LEAF_RECIPE'\n",
    );
    f.rook.config.composite_recipes.max_depth = 2;
    let model = Arc::new(Nested { root: AtomicBool::new(false), inner: AtomicBool::new(false) });
    let mut agent = AgentLoop::new(&f.rook, model.clone(), f.session);
    agent.options = options();
    agent.run("Run the selected nested recipe").await.unwrap();
    assert!(model.root.load(Ordering::SeqCst) && model.inner.load(Ordering::SeqCst));
    let root = f.details();
    assert_eq!(root["state"], "completed");
    let inner = rook_store::parse_session_id(root["stages"][0]["child"].as_str().unwrap()).unwrap();
    let list = f.rook.composite_request(inner, Inspect::List { after: None }).unwrap();
    let nested = f
        .rook
        .composite_request(inner, Inspect::Inspect { id: list["calls"][0]["id"].as_str().unwrap().into() })
        .unwrap();
    assert_eq!(nested["state"], "completed");
    let leaf = rook_store::parse_session_id(nested["stages"][0]["child"].as_str().unwrap()).unwrap();
    assert_eq!(f.rook.store.get_session(leaf).unwrap().unwrap().parent, Some(inner));
    assert_eq!(f.rook.store.get_session(inner).unwrap().unwrap().parent, Some(f.session));
}

#[tokio::test]
async fn unproven_child_spend_stops_the_family_instead_of_minting_another_model_allowance() {
    let main=MAIN.replacen("prompt='ROOT_RECIPE'","prompt='ROOT_RECIPE'\noutput='must-not-repair.json'\noutput_schema={type='object',required=['result']}",1);
    let f = Fixture::new(&main, A, B);
    let mut model = Script::new(
        json!({"stages":[{"id":"one","recipe":"a"},{"id":"two","recipe":"b","inputs":[{"stage":"one","parameter":"value"}]}]}),
    );
    model.error_a = true;
    let model = Arc::new(model);
    let result = run(&f, model.clone()).await.unwrap();
    assert_eq!(result.stopped, "recovery", "{result:?}");
    assert!(result.open_questions.iter().any(|q| q.contains("complete cost report")));
    let details = f.details();
    assert_eq!(details["state"], "interrupted");
    assert!(details["stages"][1]["child"].is_null());
    let requests = model.requests.lock().unwrap();
    assert!(
        requests.last().unwrap().messages.iter().any(|m| m.content.contains("SUB_A")),
        "another model request ran after cost coverage was lost"
    );
    assert!(f.rook.execution(f.session).unwrap().iter().any(|e| !e.unknown.is_empty()));
}
