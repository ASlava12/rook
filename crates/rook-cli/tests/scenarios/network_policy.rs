#![cfg(target_os = "macos")]
use super::*;
use serde_json::json;
#[test]
fn domain_containment_decisions_are_retained_by_real_cli_and_daemon_sessions() {
    rook_llm::init_tls();
    let target = fixture_model::Server::new(|_, _| {
        Some(("application/json", r#"{"result":"DOMAIN_RECEIPT"}"#.into()))
    });
    let command = format!("/usr/bin/curl -fsS --max-time 10 {}/receipt", target.url);
    let address = reqwest::Url::parse(&target.url).unwrap();
    let port = address.port().unwrap();
    let server = fixture_model::Server::new(move |_, request| {
        if request["stream"] != true {
            return Some(("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}).to_string()));
        }
        let messages = request["messages"].as_array().unwrap();
        let read = messages.iter().any(|m| {
            m["role"] == "tool" && m["content"].as_str().is_some_and(|s| s.contains("DOMAIN_RECEIPT"))
        });
        let checking = messages.iter().any(|m| {
            m["content"].as_str().is_some_and(|s| s.contains("You are checking a claim somebody else made"))
        });
        let (delta, finish) = if read {
            (
                json!({"content":if checking {"DOMAIN_RECEIPT\nVERDICT: holds"} else {"DOMAIN_RECEIPT"}}),
                "stop",
            )
        } else {
            let tools = request["tools"].as_array().unwrap();
            assert!(!tools.iter().any(|t| {
                ["web_fetch", "docs", "find_skill"].contains(&t["function"]["name"].as_str().unwrap())
            }));
            (
                json!({"tool_calls":[{"index":0,"id":"network-read","type":"function","function":{"name":"run_command","arguments":json!({"command":command}).to_string()}}]}),
                "tool_calls",
            )
        };
        let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":8,"completion_tokens":3}});
        Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
    });
    let fixture = Rook::new();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\nmax_steps=5\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\nisolate='required'\n[sandbox.network_policy]\nenabled=true\nallow=['127.0.0.1:{port}']\n[models.local]\napi='openai'\nmodel='fixture'\nurl='{}/v1'\ncontext_window=128000\n",server.url));
    let one = one_at_a_time();
    let local = fixture.json(&["run", "Read the permitted endpoint"]);
    let local_session = local["session"].as_str().unwrap().to_owned();
    assert!(local["outcome"]["reply"].as_str().unwrap().contains("DOMAIN_RECEIPT"));
    let history = fixture.json(&["session", "history", &local_session]).to_string();
    assert!(
        history.contains("rook:network-policy:v1")
            && history.contains("seatbelt_proxy_only")
            && history.contains("pinned HTTP destination"),
        "{history}"
    );
    drop(one);
    let daemon = Daemon::start(&fixture);
    let remote = fixture.json(&["run", "Read the permitted endpoint"]);
    assert!(remote["reply"].as_str().unwrap().contains("DOMAIN_RECEIPT"), "{remote}");
    for owner in [&local_session, remote["session"].as_str().unwrap()] {
        let history = fixture.json(&["session", "history", owner]).to_string();
        assert!(
            history.contains("rook:network-policy:v1")
                && history.contains("tool_call_id")
                && history.contains("pinned HTTP destination"),
            "{history}"
        );
    }
    drop(daemon);
}
