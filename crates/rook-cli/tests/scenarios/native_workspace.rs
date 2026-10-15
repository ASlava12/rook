//! Native authority and session-owned recovery through both real frontends.
#![cfg(unix)]
use super::*;
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
#[test]
fn remote_files_and_owned_undo_work_locally_and_through_the_daemon_without_local_replica_writes() {
    rook_llm::init_tls();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let server = fixture_model::Server::new(move |_path, request| {
        if request["stream"] != true {
            let result = if request["messages"].to_string().contains("Classify whether an assistant") {
                json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}})
            } else {
                json!({"data":[]})
            };
            return Some(("application/json", result.to_string()));
        }
        seen.lock().unwrap().push(request.clone());
        let messages = request["messages"].as_array().unwrap();
        let checking = messages.iter().any(|m| {
            m["content"].as_str().is_some_and(|s| s.contains("You are checking a claim somebody else made"))
        });
        let results: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
        let read = results.iter().any(|m| m["content"].as_str().is_some_and(|s| s.contains("NATIVE_SOURCE")));
        let edited = results.iter().any(|m| m["content"].as_str().is_some_and(|s| s.contains("applied")));
        let (delta, finish) = if (checking && read) || edited {
            (
                json!({"content":if checking {"NATIVE_SOURCE observed\nVERDICT: holds"} else {"NATIVE_SOURCE updated"}}),
                "stop",
            )
        } else {
            let tools = request["tools"].as_array().unwrap();
            assert!(tools.iter().any(|t| t["function"]["name"] == "ssh_workspace"));
            assert!(!tools.iter().any(|t| {
                ["read_file", "edit_file", "run_command"].contains(&t["function"]["name"].as_str().unwrap())
            }));
            let args = if !checking && read {
                json!({"operation":"edit","path":"a.txt","old":"NATIVE_SOURCE before","new":"NATIVE_SOURCE after"})
            } else {
                json!({"operation":"read","path":"a.txt"})
            };
            (
                json!({"tool_calls":[{"index":0,"id":format!("native-{}",results.len()),"type":"function","function":{"name":"ssh_workspace","arguments":args.to_string()}}]}),
                "tool_calls",
            )
        };
        let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":8,"completion_tokens":3}});
        Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
    });
    let fixture = Rook::new();
    // The worker rejects symlink ancestors; macOS aliases /var to /private/var.
    let remote = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    std::fs::write(fixture.workspace.path().join("a.txt"), "WRONG_LOCAL_SOURCE").unwrap();
    let ssh = fixture.home.path().join("fixture-ssh");
    std::fs::write(&ssh, "#!/bin/sh\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\nmax_steps=8\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\n[execution_environment]\nnative_workspace=true\nselected='lab'\n[execution_environment.profiles.lab]\nkind='ssh'\nhost='trusted-fixture'\nprogram={}\nremote_workspace={}\n[models.local]\napi='openai'\nmodel='fixture'\nurl='{}/v1'\ncontext_window=128000\n",serde_json::to_string(ssh.to_str().unwrap()).unwrap(),serde_json::to_string(remote.path().to_str().unwrap()).unwrap(),server.url));
    let one = one_at_a_time();
    {
        std::fs::write(remote.path().join("a.txt"), "NATIVE_SOURCE before\n").unwrap();
        let run = fixture.json(&["run", "Read and update remote a.txt"]);
        let owner = run["session"].as_str().unwrap();
        assert_eq!(
            std::fs::read_to_string(remote.path().join("a.txt")).unwrap(),
            "NATIVE_SOURCE after\n",
            "{run}"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.path().join("a.txt")).unwrap(),
            "WRONG_LOCAL_SOURCE"
        );
        let count = requests.lock().unwrap().len();
        let list = fixture.json(&["session", "environment", owner, r#"{"action":"native_inspect"}"#]);
        let id = list["receipts"][0]["call"].as_str().unwrap();
        let inspected = fixture.json(&[
            "session",
            "environment",
            owner,
            &json!({"action":"native_inspect","id":id}).to_string(),
        ]);
        assert_eq!(inspected["receipt"]["status"], "applied");
        let undone = fixture.json(&[
            "session",
            "environment",
            owner,
            &json!({"action":"native_undo","id":id}).to_string(),
        ]);
        assert_eq!(undone["receipt"]["status"], "undone");
        assert!(undone["session"].is_string(), "durable recovery operation: {undone}");
        assert_eq!(std::fs::read_to_string(remote.path().join("a.txt")).unwrap(), "NATIVE_SOURCE before\n");
        assert_eq!(requests.lock().unwrap().len(), count, "manual recovery makes no model calls");
        let history = fixture.json(&["session", "history", owner]).to_string();
        assert!(history.contains("ssh_workspace") && history.contains("NATIVE_SOURCE"));
    }
    drop(one);
    let daemon = Daemon::start(&fixture);
    std::fs::write(remote.path().join("a.txt"), "NATIVE_SOURCE before\n").unwrap();
    let run = fixture.json(&["run", "Read and update remote a.txt"]);
    assert_eq!(
        std::fs::read_to_string(remote.path().join("a.txt")).unwrap(),
        "NATIVE_SOURCE after\n",
        "{run}"
    );
    let owner = run["session"].as_str().unwrap();
    let count = requests.lock().unwrap().len();
    let list = fixture.json(&["session", "environment", owner, r#"{"action":"native_inspect"}"#]);
    let id = list["receipts"][0]["call"].as_str().unwrap();
    let undone = fixture.json(&[
        "session",
        "environment",
        owner,
        &json!({"action":"native_undo","id":id}).to_string(),
    ]);
    assert_eq!(undone["receipt"]["status"], "undone");
    assert_eq!(std::fs::read_to_string(remote.path().join("a.txt")).unwrap(), "NATIVE_SOURCE before\n");
    assert_eq!(requests.lock().unwrap().len(), count);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("a.txt")).unwrap(),
        "WRONG_LOCAL_SOURCE"
    );
    drop(daemon);
    for request in requests.lock().unwrap().iter() {
        assert!(!request.to_string().contains("WRONG_LOCAL_SOURCE"));
        assert!(request["messages"][0]["content"].as_str().unwrap().contains("SSH-native"));
    }
}
