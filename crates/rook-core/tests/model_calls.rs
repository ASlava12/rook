//! The actual agent path: explicit turn selection, real Store and scripted model.
use async_trait::async_trait;
use rook_core::{Config, Rook, agent::AgentLoop};
use rook_llm::{Message, Provider, Request, Response, StopReason, Usage};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct FixtureModel(Arc<Mutex<Vec<Value>>>, &'static str);
#[async_trait]
impl Provider for FixtureModel {
    fn id(&self) -> &str {
        "owned-scripted"
    }
    fn dispatch_identity(&self) -> Option<rook_llm::Dispatch> {
        rook_llm::Dispatch::bounded("owned-scripted", "fixture-model", true)
    }
    fn context_window(&self) -> usize {
        65536
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let classifier = request
            .messages
            .iter()
            .any(|m| m.content.contains("Classify whether an assistant's proposed last reply"));
        self.0.lock().unwrap().push(serde_json::to_value(&request).unwrap());
        Ok(Response {
            message: Message::assistant(if classifier { r#"{"action":"finish"}"# } else { self.1 }),
            stop_reason: StopReason::EndTurn,
            usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            model: "owned-scripted".into(),
        })
    }
}

fn open(path: &std::path::Path) -> Rook {
    Rook::from_parts(
        Store::open(path.join("store")).unwrap(),
        Config::default(),
        Environment::bare("windows", "x86_64", "0.1.0"),
        SkillIndex::default(),
        path.to_path_buf(),
    )
}

#[tokio::test]
async fn actual_turn_flag_records_exact_requests_and_purposes_without_adding_model_calls() {
    let mut counts = Vec::new();
    for enabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let rook = open(root.path());
        let owner = rook.start_session("owned capture control").unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(FixtureModel(seen.clone(), "The requested inspection is complete."));
        let mut agent = AgentLoop::new(&rook, provider, owner);
        agent.options.record_model_calls = Some(enabled);
        agent.max_steps = 4;
        let outcome = agent.run("Inspect the supplied example and finish.").await.unwrap();
        assert_eq!(outcome.reply, "The requested inspection is complete.");
        let events = rook.store.events(owner, 0, 256).unwrap();
        let inputs =
            events.iter().filter(|e| e.record.label == "rook:model-call-input:v1").collect::<Vec<_>>();
        let outputs =
            events.iter().filter(|e| e.record.label == "rook:model-call-outcome:v1").collect::<Vec<_>>();
        let requests = seen.lock().unwrap();
        counts.push(requests.len());
        assert!(!requests.is_empty() && requests.len() <= 4);
        if !enabled {
            assert!(inputs.is_empty() && outputs.is_empty());
            continue;
        }
        assert_eq!(inputs.len(), requests.len());
        assert_eq!(outputs.len(), requests.len());
        let mut purposes = Vec::new();
        for (input, request) in inputs.iter().zip(requests.iter()) {
            let value: Value = serde_json::from_slice(&rook.store.get(&input.record.body).unwrap()).unwrap();
            purposes.push(value["purpose"].as_str().unwrap().to_owned());
            assert_eq!(value["request"]["value"]["tools"], request["tools"]);
            let captured = value["request"]["value"]["messages"].as_array().unwrap();
            for (actual, expected) in captured.iter().zip(request["messages"].as_array().unwrap()) {
                assert_eq!(actual["role"], expected["role"]);
                assert_eq!(actual["content"], expected["content"]);
            }
            let output = outputs.iter().find(|e| {
                let row: Value = serde_json::from_slice(&rook.store.get(&e.record.body).unwrap()).unwrap();
                row["attempt"] == value["attempt"] && row["input_seq"] == input.seq
            });
            assert!(output.is_some());
        }
        assert!(purposes.iter().any(|p| p == "main"));
        assert!(purposes.iter().any(|p| p == "completion_check"));
        let export = rook
            .conversation_export(
                owner,
                &rook_core::conversation_export::Request {
                    profile: rook_core::conversation_export::Profile::Archive,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!export.html.contains("rook:model-call-input:v1"));
        assert!(export.omitted.get("note").is_some_and(|n| *n >= inputs.len() + outputs.len()));
    }
    assert_eq!(counts[0], counts[1], "recording adds no model call");
    assert_eq!(json!(counts), json!([2, 2]));
}

#[tokio::test]
async fn actual_turn_masks_known_connection_secret_before_saved_outcome_and_dataset_selection() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let owner = rook.start_session("owned secret probe").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(FixtureModel(seen, "The result contains owned-private-token."));
    let mut agent = AgentLoop::new(&rook, provider, owner);
    agent.options.record_model_calls = Some(true);
    agent.hide_connection_secret("owned-private-token");
    agent.run("Inspect the owned example and finish.").await.unwrap();
    let records = rook
        .model_calls(
            owner,
            &rook_core::model_calls::ReadRequest {
                format: rook_core::model_calls::Format::Records,
                ..Default::default()
            },
        )
        .unwrap();
    let main =
        records.calls.iter().find(|c| c.purpose == Some(rook_core::model_route::Purpose::Main)).unwrap();
    assert!(main.output.as_ref().unwrap().known_secret_redacted);
    assert!(!main.eligible);
    assert!(main.exclusions.iter().any(|s| s == "output_was_redacted"));
    assert!(!String::from_utf8(records.bytes().unwrap()).unwrap().contains("owned-private-token"));
    assert_eq!(main.response.as_ref().unwrap()["message"]["content"], "The result contains ${secret}.");
}
