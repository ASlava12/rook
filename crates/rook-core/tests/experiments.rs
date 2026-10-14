#![cfg(unix)]
use rook_core::{
    Config, Rook, Vault,
    experiments::{Contract, Direction, Request},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
static TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const EVALUATE: &str = r#"score=$(sed -n 's/.*"score":"\([0-9]*\)".*/\1/p' .rook-experiment/parameters.json)
mkdir -p .rook-experiment/artifacts
printf '{"score":%s}' "$score" > .rook-experiment/metrics.json
printf 'score=%s' "$score" > .rook-experiment/artifacts/result.txt
printf 'measured %s\n' "$score"
"#;
fn config() -> Config {
    let mut c = Config::default();
    c.memory.enabled = false;
    c.web.enabled = false;
    c.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    c.storage.gc_grace_secs = 0;
    c
}
fn open(root: &std::path::Path, c: Config) -> Rook {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        c,
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        workspace,
    )
}
async fn ask(rook: &Rook, owner: u128, request: Value) -> rook_core::Result<Value> {
    rook.experiment_request(
        owner,
        rook_core::experiments::arguments(&request.to_string())?,
        Arc::new(Vault::empty()),
    )
    .await
}
async fn create(rook: &Rook, owner: u128, command: &str) -> Value {
    rook.experiment_request(
        owner,
        Request::Create {
            title: "baseline".into(),
            hypothesis: "score one is the measured baseline".into(),
            contract: Contract {
                command: command.into(),
                metrics: BTreeMap::from([("score".into(), Direction::Maximize)]),
                artifacts: vec![".rook-experiment/artifacts/result.txt".into()],
            },
            parameters: BTreeMap::from([("score".into(), "1".into())]),
        },
        Arc::new(Vault::empty()),
    )
    .await
    .unwrap()
}
async fn run(rook: &Rook, owner: u128, node: &Value, wait: bool) -> rook_core::Result<Value> {
    let prepared = ask(rook, owner, json!({"action":"prepare","node":node["id"]})).await?;
    ask(rook, owner, json!({"action":"run","node":node["id"],"token":prepared["token"],"wait":wait})).await
}
async fn branch(rook: &Rook, owner: u128, parent: &Value, score: &str) -> Value {
    ask(rook, owner, json!({"action":"branch","parent":parent["id"],"title":format!("score {score}"),"hypothesis":"change one parameter","parameters":{"score":score}})).await.unwrap()
}
#[tokio::test]
async fn measured_branches_compare_fixed_sources_and_retain_artifacts_after_restart_and_gc() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let c = config();
    let rook = open(root.path(), c.clone());
    std::fs::write(rook.workspace.join("evaluate.sh"), EVALUATE).unwrap();
    std::fs::write(rook.workspace.join("user.txt"), "original user edit").unwrap();
    let owner = rook.start_session("experiment tree").unwrap();
    let baseline = create(&rook, owner, "sh evaluate.sh").await;
    std::fs::write(rook.workspace.join("user.txt"), "later user edit").unwrap();
    let measured = run(&rook, owner, &baseline, true).await.unwrap();
    assert_eq!(measured["runs"][0]["state"], "answered", "{measured}");
    assert_eq!(measured["runs"][0]["metrics"]["score"], 1.0);
    let replica = branch(&rook, owner, &baseline, "1").await;
    let repeated = run(&rook, owner, &replica, true).await.unwrap();
    assert_eq!(repeated["runs"][0]["metrics"], measured["runs"][0]["metrics"]);
    assert_eq!(repeated["runs"][0]["artifacts"], measured["runs"][0]["artifacts"]);
    for field in ["source", "parameters", "contract", "environment"] {
        assert_eq!(
            repeated["runs"][0][field], measured["runs"][0][field],
            "the reproduced run must use the same {field}"
        );
    }
    let child = branch(&rook, owner, &baseline, "2").await;
    let winner = run(&rook, owner, &child, true).await.unwrap();
    let loser = branch(&rook, owner, &baseline, "0").await;
    run(&rook, owner, &loser, true).await.unwrap();
    let grandchild = branch(&rook, owner, &child, "3").await;
    run(&rook, owner, &grandchild, true).await.unwrap();
    let report = ask(&rook, owner, json!({"action":"compare"})).await.unwrap();
    assert_eq!(report["rows"].as_array().unwrap().len(), 4, "other rounds are not siblings: {report}");
    assert!(
        ask(&rook, owner, json!({"action":"promote","node":loser["id"],"token":report["token"]}))
            .await
            .is_err()
    );
    let promoted = ask(&rook, owner, json!({"action":"promote","node":child["id"],"token":report["token"]}))
        .await
        .unwrap();
    assert_eq!(promoted["winner"], child["id"]);
    assert!(
        ask(&rook, owner, json!({"action":"promote","node":grandchild["id"],"token":report["token"]}))
            .await
            .is_err()
    );
    assert!(
        ask(&rook, owner, json!({"action":"capture","node":child["id"]}))
            .await
            .unwrap_err()
            .to_string()
            .contains("frozen")
    );
    assert_eq!(std::fs::read_to_string(rook.workspace.join("user.txt")).unwrap(), "later user edit");
    let run_dir = winner["runs"][0]["workspace"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(run_dir).join("user.txt")).unwrap(),
        "original user edit"
    );
    let child_id = child["id"].clone();
    let run_id = winner["runs"][0]["id"].clone();
    drop(rook);
    let rook = open(root.path(), c);
    rook.collect_garbage(false).unwrap();
    let retained = ask(&rook, owner, json!({"action":"read","node":child_id,"run":run_id,"artifact":".rook-experiment/artifacts/result.txt"})).await.unwrap();
    assert_eq!(retained["text"], "score=2");
    let metrics = ask(
        &rook,
        owner,
        json!({"action":"read","node":child_id,"run":run_id,"artifact":".rook-experiment/metrics.json"}),
    )
    .await
    .unwrap();
    assert_eq!(metrics["text"], "{\"score\":2}");
    let output = ask(&rook, owner, json!({"action":"read","node":child_id,"run":run_id})).await.unwrap();
    assert!(output["text"].as_str().unwrap().contains("measured 2"));
    rook.store.delete_session(owner).unwrap();
    assert!(rook.store.kv_get(&format!("experiments/{owner:032x}")).unwrap().is_none());
}
#[tokio::test]
async fn a_failed_command_with_fabricated_metric_files_never_becomes_an_answer() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("failed metrics").unwrap();
    let baseline = create(&rook, owner, &format!("{EVALUATE}\nexit 7")).await;
    let result = run(&rook, owner, &baseline, true).await.unwrap();
    assert_eq!(result["runs"][0]["state"], "failed", "{result}");
    assert_eq!(result["runs"][0]["exit_code"], 7);
    assert_eq!(result["answered"], false);
    assert!(ask(&rook, owner, json!({"action":"compare"})).await.is_err());
    assert!(ask(&rook, owner, json!({"action":"branch","parent":baseline["id"],"title":"invalid","hypothesis":"cannot fork a failed answer"})).await.is_err());
    assert!(
        rook_core::experiments::arguments(
            r#"{"action":"branch","parent":"x","title":"x","hypothesis":"x","contract":{"command":"other"}}"#
        )
        .is_err()
    );
}
#[tokio::test]
async fn source_mutation_is_detected_before_accepting_a_successful_command_metric() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    std::fs::write(rook.workspace.join("fixed.txt"), "fixed").unwrap();
    let owner = rook.start_session("source mutation").unwrap();
    let baseline = create(&rook, owner, &format!("{EVALUATE}\nprintf modified > fixed.txt")).await;
    let result = run(&rook, owner, &baseline, true).await.unwrap();
    assert_eq!(result["runs"][0]["state"], "failed");
    assert!(result["runs"][0]["error"].as_str().unwrap().contains("modified its frozen source"), "{result}");
    assert_eq!(std::fs::read_to_string(rook.workspace.join("fixed.txt")).unwrap(), "fixed");
}
#[tokio::test]
async fn terminal_receipt_capacity_is_admitted_before_any_execution_workspace_is_published() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut c = config();
    c.experiments.state_bytes = 65536;
    let rook = open(root.path(), c);
    for i in 0..200 {
        std::fs::write(rook.workspace.join(format!("source-file-with-long-name-{i:04}.txt")), "x").unwrap();
    }
    let owner = rook.start_session("capacity").unwrap();
    let baseline = create(&rook, owner, EVALUATE).await;
    let error = run(&rook, owner, &baseline, true).await.unwrap_err();
    assert!(error.to_string().contains("65536"), "the actual state byte cap must be reached: {error}");
    let node = ask(&rook, owner, json!({"action":"inspect","node":baseline["id"]})).await.unwrap();
    assert!(node["runs"].as_array().unwrap().is_empty());
    let source = std::path::Path::new(node["workspace"].as_str().unwrap());
    assert!(!source.parent().unwrap().join("runs").exists());
}
#[tokio::test]
async fn another_workspace_cannot_launch_a_saved_owner_contract() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("owner binding").unwrap();
    let baseline = create(&rook, owner, EVALUATE).await;
    let token =
        ask(&rook, owner, json!({"action":"prepare","node":baseline["id"]})).await.unwrap()["token"].clone();
    let other = root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let other = rook.for_workspace(other);
    assert!(
        ask(&other, owner, json!({"action":"run","node":baseline["id"],"token":token}))
            .await
            .unwrap_err()
            .to_string()
            .contains("another workspace")
    );
}
#[tokio::test]
async fn detached_cancellation_stops_actual_descendants_and_requires_unknown_effect_acknowledgement() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("cancel experiment").unwrap();
    let baseline = create(
        &rook,
        owner,
        "sleep 60 & child=$!; printf '%s' \"$child\" > child.pid; printf ready > started; wait",
    )
    .await;
    let admission = run(&rook, owner, &baseline, false).await.unwrap();
    let run_id = admission["run"].clone();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let inspect = loop {
        let inspect = ask(&rook, owner, json!({"action":"inspect","node":baseline["id"]})).await.unwrap();
        let dir = std::path::Path::new(inspect["runs"][0]["workspace"].as_str().unwrap());
        if dir.join("started").exists() {
            break inspect;
        }
        assert!(tokio::time::Instant::now() < deadline, "command never started: {inspect}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let pid = std::fs::read_to_string(
        std::path::Path::new(inspect["runs"][0]["workspace"].as_str().unwrap()).join("child.pid"),
    )
    .unwrap();
    assert_eq!(inspect["runs"][0]["live"], true);
    assert!(
        ask(
            &rook,
            owner,
            json!({"action":"recover","node":baseline["id"],"run":run_id,"note":"not yet stopped"})
        )
        .await
        .is_err()
    );
    ask(&rook, owner, json!({"action":"stop","node":baseline["id"],"run":run_id})).await.unwrap();
    loop {
        let inspect = ask(&rook, owner, json!({"action":"inspect","node":baseline["id"]})).await.unwrap();
        if inspect["runs"][0]["live"] == false {
            assert_eq!(inspect["runs"][0]["state"], "interrupted", "{inspect}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // A process may briefly remain a reaped zombie; it must never still execute.
    loop {
        let out = rook_contain::quietly(&mut std::process::Command::new("ps"))
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&out.stdout);
        if status.trim().is_empty() || status.trim().starts_with('Z') {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "descendant still live: {pid} {status}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let session = rook_store::parse_session_id(admission["session"].as_str().unwrap()).unwrap();
    assert!(
        ask(
            &rook,
            owner,
            json!({"action":"recover","node":baseline["id"],"run":run_id,"note":"processes stopped"})
        )
        .await
        .is_err()
    );
    drop(rook);
    let rook = open(root.path(), config());
    let executions = rook.execution(session).unwrap();
    assert!(!executions[0].unknown.is_empty());
    for unknown in &executions[0].unknown {
        rook.acknowledge_operation(
            session,
            &unknown.id,
            "Inspected files and verified descendant processes stopped",
        )
        .unwrap();
    }
    let result = ask(&rook, owner, json!({"action":"recover","node":baseline["id"],"run":run_id,"note":"Inspected files and verified descendant processes stopped"})).await.unwrap();
    assert_eq!(result["runs"][0]["state"], "reviewed");
    assert_eq!(result["answered"], false);
}

struct MutateMeasured(std::sync::Mutex<bool>);
#[async_trait::async_trait]
impl rook_llm::Provider for MutateMeasured {
    fn id(&self) -> &str {
        "fixture/frozen-experiment"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
        let message = if request
            .messages
            .first()
            .is_some_and(|m| m.content.starts_with("Classify whether an assistant"))
        {
            rook_llm::Message::assistant(r#"{"action":"finish"}"#)
        } else if request.tools.iter().any(|t| t.name == "run_command") && !*self.0.lock().unwrap() {
            *self.0.lock().unwrap() = true;
            rook_llm::Message {
                tool_calls: vec![rook_llm::ToolCall {
                    id: "mutate".into(),
                    name: "run_command".into(),
                    arguments: json!({"command":"printf forged > model-mutated.txt"}),
                }],
                ..rook_llm::Message::assistant("")
            }
        } else {
            rook_llm::Message::assistant("The measured source is frozen.")
        };
        let stop_reason = if message.tool_calls.is_empty() {
            rook_llm::StopReason::EndTurn
        } else {
            rook_llm::StopReason::ToolUse
        };
        Ok(rook_llm::Response { message, stop_reason, usage: Default::default(), model: self.id().into() })
    }
}
#[tokio::test]
async fn ordinary_agent_dispatch_refuses_shell_mutation_of_a_measured_node() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("frozen agent dispatch").unwrap();
    let baseline = create(&rook, owner, EVALUATE).await;
    run(&rook, owner, &baseline, true).await.unwrap();
    let child = rook.for_workspace(std::path::PathBuf::from(baseline["workspace"].as_str().unwrap()));
    let session = rook_store::parse_session_id(baseline["session"].as_str().unwrap()).unwrap();
    let mut agent = rook_core::agent::AgentLoop::new(
        &child,
        Arc::new(MutateMeasured(std::sync::Mutex::new(false))),
        session,
    );
    agent.run("Try the fixture command once").await.unwrap();
    assert!(!child.workspace.join("model-mutated.txt").exists());
    let messages = child.transcript(session, 0, 128, 4096).unwrap();
    assert!(
        messages.iter().any(|m| m.body.contains("experiment source is frozen")),
        "refusal must actually reach the model: {messages:?}"
    );
}
#[tokio::test]
async fn reached_metric_file_bounds_keep_a_successful_command_unanswered() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("bounded metrics").unwrap();
    let baseline =
        create(&rook, owner, &format!("{EVALUATE}\nhead -c 65537 /dev/zero > .rook-experiment/metrics.json"))
            .await;
    let result = run(&rook, owner, &baseline, true).await.unwrap();
    assert_eq!(result["runs"][0]["exit_code"], 0);
    assert_eq!(result["runs"][0]["state"], "failed");
    let path = std::path::Path::new(result["runs"][0]["workspace"].as_str().unwrap())
        .join(".rook-experiment/metrics.json");
    assert_eq!(
        std::fs::metadata(path).unwrap().len(),
        65537,
        "the configured read cap was actually exceeded"
    );
}
