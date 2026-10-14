use super::fixture_model::Server;
use super::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[test]
fn genuine_acp_executor_writes_through_cli_and_websocket_and_retains_sources_after_restart() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let model = Server::new(move |_, request| {
        {
            let mut records = seen.lock().unwrap();
            assert!(records.len() < 64);
            records.push(request.clone());
        }
        let text = request["messages"].to_string();
        let tools = request["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
        if request["stream"] == true {
            let checker = text.contains("VERDICT: holds");
            let (delta, finish) = if checker && !tools {
                (
                    json!({"tool_calls":[{"index":0,"id":"check-source","type":"function","function":{"name":"read_file","arguments":json!({"path":"real-acp.txt"}).to_string()}}]}),
                    "tool_calls",
                )
            } else if checker {
                (json!({"content":"VERDICT: holds"}), "stop")
            } else if text.contains("ROOT_EXTERNAL") && !tools {
                (
                    json!({"tool_calls":[{"index":0,"id":"external-errand","type":"function","function":{"name":"delegate","arguments":json!({"task":"CHILD_NATIVE write real-acp.txt with GENUINE_ACP and report","executor":"rook"}).to_string()}}]}),
                    "tool_calls",
                )
            } else if text.contains("CHILD_NATIVE") && !tools {
                (
                    json!({"tool_calls":[{"index":0,"id":"actual-foreign-write","type":"function","function":{"name":"write_file","arguments":json!({"path":"real-acp.txt","content":"GENUINE_ACP\n"}).to_string()}}]}),
                    "tool_calls",
                )
            } else {
                (json!({"content":"GENUINE_ACP was written."}), "stop")
            };
            let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
            Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
        } else if text.contains("Classify whether an assistant") {
            Some(("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string()))
        } else {
            Some(("application/json", json!({"data":[]}).to_string()))
        }
    });
    let fixture = Rook::new();
    let foreign = tempfile::tempdir().unwrap();
    let common = format!(
        "[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\n[models.local]\napi='openai'\nmodel='test'\nurl='{}/v1'\ncontext_window=128000\n",
        model.url
    );
    let foreign_config =
        common.replace("context_window=128000", "context_window=128000\nkey='secret:foreign-api'");
    std::fs::write(foreign.path().join("config.toml"), &foreign_config).unwrap();
    std::fs::write(
        foreign.path().join("secrets.toml"),
        "[foreign-api]\nsource='env:ROOK_TEST_ACP_ENV_TOKEN'\n",
    )
    .unwrap();
    std::fs::write(
        fixture.home.path().join("secrets.toml"),
        "[profile-token]\nvalue='private-profile-token'\n",
    )
    .unwrap();
    let profile = json!({"command":env!("CARGO_BIN_EXE_rook"),"args":["acp"],"trusted":true,"env":{"ROOK_HOME":foreign.path().to_str().unwrap(),"ROOK_TEST_ACP_ENV_TOKEN":"secret:profile-token"}});
    fixture.write_config(&common);
    let mut config = rook_core::Config::load_from(fixture.home.path().join("config.toml")).unwrap();
    config.acp_executor.profiles.insert("rook".into(), serde_json::from_value(profile).unwrap());
    fixture.write_config(&config.as_written().unwrap());
    let cli = fixture.run(&["run", "ROOT_EXTERNAL CLI"]);
    let report = format!("{}{}", String::from_utf8_lossy(&cli.stdout), String::from_utf8_lossy(&cli.stderr));
    assert!(!cli.status.success(), "unverified external usage cannot report verified completion");
    assert!(report.contains("subrecipe_cost_unknown") && report.contains("GENUINE_ACP"), "{report}");
    assert!(!report.contains("private-profile-token"), "{report}");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("real-acp.txt")).unwrap(),
        "GENUINE_ACP\n"
    );
    let (parent, child, source) = saved_child(&fixture);
    assert!(source.to_string().contains("GENUINE_ACP"));
    let local = fixture.json(&["session", "show", &child]);
    let daemon = Daemon::start(&fixture);
    assert_eq!(fixture.json(&["session", "show", &child]), local);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let url=daemon.address.replacen("http://","ws://",1)+"/api/chat";
        let (mut socket,_)=tokio_tungstenite::connect_async(url).await.unwrap();
        socket.send(tokio_tungstenite::tungstenite::Message::Text(json!({"type":"prompt","session":null,"text":"ROOT_EXTERNAL API","id":"owned-acp-api","options":{}}).to_string().into())).await.unwrap();
        let done=tokio::time::timeout(std::time::Duration::from_secs(60),async {
            while let Some(message)=socket.next().await {
                let message=message.unwrap();if !message.is_text(){continue;}
                let event:Value=serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_ne!(event["type"],"failed","{event}");
                if event["type"]=="done" {return event;}
            }
            panic!("chat ended without a terminal outcome");
        }).await.unwrap();
        assert_eq!(done["stopped"],"subrecipe_cost_unknown","the outer engine must not invent an external price or an extra allowance: {done}");
        assert_eq!(done["delegated"].as_array().unwrap().len(),1);
        socket.close(None).await.unwrap();
    });
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("real-acp.txt")).unwrap(),
        "GENUINE_ACP\n"
    );
    let remote = fixture.json(&["session", "show", &child]);
    assert_eq!(remote, local);
    drop(daemon);
    let restarted = Daemon::start(&fixture);
    assert_eq!(fixture.json(&["session", "show", &child]), local);
    assert!(fixture.ok(&["session", "ls"]).contains("CHILD_NATIVE"));
    drop(restarted);
    let store = rook_store::Store::open(fixture.home.path().join("store")).unwrap();
    assert_eq!(
        store.get_session(rook_store::parse_session_id(&child).unwrap()).unwrap().unwrap().parent,
        Some(rook_store::parse_session_id(&parent).unwrap())
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["stream"] == true
                && r["messages"].to_string().contains("ROOT_EXTERNAL")
                && !r["messages"].to_string().contains("CHILD_NATIVE"))
            .count(),
        2,
        "source inspection/restart must never replay a foreign prompt"
    );
}

fn saved_child(fixture: &Rook) -> (String, String, Value) {
    let store = rook_store::Store::open(fixture.home.path().join("store")).unwrap();
    let sessions = store.list_sessions().unwrap();
    let child = sessions.iter().find(|m| m.model == "acp/rook").unwrap();
    let parent = rook_store::format_session_id(child.parent.unwrap());
    let id = rook_store::format_session_id(child.id);
    let events = store.events(child.id, 0, 128).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.record.kind == rook_store::EventKind::ToolCall && e.record.label == "write_file")
            .count(),
        1
    );
    let reply = events.iter().find(|e| e.record.kind == rook_store::EventKind::AssistantMessage).unwrap();
    let source = Value::String(String::from_utf8(store.get(&reply.record.body).unwrap()).unwrap());
    (parent, id, source)
}
