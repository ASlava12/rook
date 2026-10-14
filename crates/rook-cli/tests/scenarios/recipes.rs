use super::fixture_model::Server;
use super::*;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn native_recipe_cli_runs_real_stages_and_inspects_identical_local_http_restart_sources() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let root_called = Arc::new(AtomicBool::new(false));
    let called = root_called.clone();
    let model = Server::new(move |_, request| {
        {
            let mut seen = seen.lock().unwrap();
            assert!(seen.len() < 128);
            seen.push(request.clone());
        }
        let text = request["messages"].to_string();
        let classifier = text.contains("Classify whether an assistant");
        let checker = text.contains("VERDICT: holds");
        let tool_result =
            request["messages"].as_array().is_some_and(|m| m.iter().any(|m| m["role"] == "tool"));
        let root = request["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["function"]["name"] == "run_subrecipes"));
        if request["stream"] == true {
            let (delta, finish) = if checker && !tool_result {
                (
                    json!({"tool_calls":[{"index":0,"id":"independent-source-read","type":"function","function":{"name":"read_file","arguments":json!({"path":"recipe-native-result"}).to_string()}}]}),
                    "tool_calls",
                )
            } else if checker {
                (json!({"content":"VERDICT: holds"}), "stop")
            } else if root && !called.swap(true, Ordering::SeqCst) {
                (
                    json!({"tool_calls":[{"index":0,"id":"native-composition","type":"function","function":{"name":"run_subrecipes","arguments":json!({"stages":[{"id":"write","recipe":"a"},{"id":"read","recipe":"b","inputs":[{"stage":"write","parameter":"value"}]}]}).to_string()}}]}),
                    "tool_calls",
                )
            } else if !root && text.contains("SUB_A") && !tool_result {
                (
                    json!({"tool_calls":[{"index":0,"id":"actual-stage-write","type":"function","function":{"name":"write_file","arguments":json!({"path":"recipe-native-result","content":"NATIVE_A_OUT"}).to_string()}}]}),
                    "tool_calls",
                )
            } else if !root && text.contains("SUB_A") {
                (json!({"content":"NATIVE_A_OUT"}), "stop")
            } else if !root && text.contains("SUB_B") && !tool_result {
                assert!(text.contains("NATIVE_A_OUT"), "native dependent stage lacks original output");
                (
                    json!({"tool_calls":[{"index":0,"id":"actual-stage-read","type":"function","function":{"name":"read_file","arguments":json!({"path":"recipe-native-result"}).to_string()}}]}),
                    "tool_calls",
                )
            } else if !root && text.contains("SUB_B") {
                (json!({"content":"NATIVE_B_OUT"}), "stop")
            } else {
                (json!({"content":"NATIVE_A_OUT and NATIVE_B_OUT were produced."}), "stop")
            };
            let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
            Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
        } else if classifier {
            Some(("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string()))
        } else {
            Some(("application/json", json!({"data":[]}).to_string()))
        }
    });
    let fixture = Rook::new();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\n[models.local]\napi='openai'\nmodel='test'\nurl='{}/v1'\ncontext_window=128000\n",model.url));
    std::fs::create_dir_all(fixture.workspace.path().join(".rook/recipes")).unwrap();
    for (name, source) in [
        (
            "main",
            "version=1\nprompt='ROOT_RECIPE'\n[subrecipes.a]\npath='a.toml'\n[subrecipes.b]\npath='b.toml'\n",
        ),
        ("a", "version=1\nprompt='SUB_A write the exact marker and return it'\n"),
        (
            "b",
            "version=1\nprompt='SUB_B read the result for {{value}} and return NATIVE_B_OUT'\n[parameters.value]\ndescription='Prior stage output'\n",
        ),
    ] {
        std::fs::write(fixture.workspace.path().join(format!(".rook/recipes/{name}.toml")), source).unwrap();
    }
    fixture.ok(&["run", "--recipe", "main", "Run the selected pipeline"]);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("recipe-native-result")).unwrap(),
        "NATIVE_A_OUT"
    );
    let session = {
        let store = rook_store::Store::open(fixture.home.path().join("store")).unwrap();
        store
            .list_sessions()
            .unwrap()
            .iter()
            .find_map(|m| {
                store
                    .kv_get(&format!("composite-recipes/{:032x}", m.id))
                    .unwrap()
                    .map(|_| rook_store::format_session_id(m.id))
            })
            .expect("actual CLI run has a composite receipt")
    };
    let listed = fixture.json(&["session", "recipes", &session]);
    let id = listed["calls"][0]["id"].as_str().unwrap().to_owned();
    let inspect = json!({"action":"inspect","id":id}).to_string();
    let local = fixture.json(&["session", "recipes", &session, &inspect]);
    assert_eq!(local["state"], "completed", "{local}");
    let seq = local["stages"][0]["output"].as_u64().unwrap();
    let read = json!({"action":"read","id":id,"seq":seq,"offset":0}).to_string();
    let source = fixture.json(&["session", "recipes", &session, &read]);
    let daemon = Daemon::start(&fixture);
    assert_eq!(fixture.json(&["session", "recipes", &session, &inspect]), local);
    assert_eq!(fixture.json(&["session", "recipes", &session, &read]), source);
    drop(daemon);
    let restarted = Daemon::start(&fixture);
    assert_eq!(fixture.json(&["session", "recipes", &session, &read]), source);
    drop(restarted);
    let writes = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| {
            r["stream"] == true
                && r["messages"].to_string().contains("SUB_A")
                && !r["messages"].as_array().is_some_and(|m| m.iter().any(|m| m["role"] == "tool"))
        })
        .count();
    assert_eq!(writes, 1, "inspection/restart replayed the original stage");
}
