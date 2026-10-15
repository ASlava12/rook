//! Resource discovery/read from real local and daemon clients, then API/reopen.
use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[test]
fn mcp_resources_run_through_native_clients_and_survive_daemon_history_and_reopen() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let server = fixture_model::Server::new(move |path, request| {
        log.lock().unwrap().push((path.to_owned(), request.clone()));
        if path == "/mcp" {
            if request["id"].is_null() {
                return Some(("application/json", String::new()));
            }
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"native-documents","version":"1"},"capabilities":{"resources":{}}})
                }
                "resources/list" => json!({"resources":[{"uri":"document:receipt","name":"receipt"}]}),
                "resources/read" => {
                    assert_eq!(request["params"]["uri"], "document:receipt");
                    json!({"contents":[{"uri":"document:receipt","mimeType":"text/plain","text":"NATIVE_RESOURCE_RECEIPT_26"}]})
                }
                other => panic!("resource-only server received {other}"),
            };
            return Some((
                "application/json",
                json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string(),
            ));
        }
        if request["stream"] == true {
            let messages = request["messages"].as_array().unwrap();
            let read = messages.iter().any(|m| {
                m["role"] == "tool"
                    && m["content"].as_str().is_some_and(|s| s.contains("NATIVE_RESOURCE_RECEIPT_26"))
            });
            let listed = messages.iter().any(|m| {
                m["role"] == "tool" && m["content"].as_str().is_some_and(|s| s.contains("document:receipt"))
            });
            let (delta, finish) = if read {
                (json!({"content":"The document contains NATIVE_RESOURCE_RECEIPT_26."}), "stop")
            } else {
                let (name, args) = if listed {
                    ("read_mcp_resource", json!({"server":"documents","uri":"document:receipt"}))
                } else {
                    ("list_mcp_resources", json!({"server":"documents"}))
                };
                assert!(
                    request["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == name),
                    "tool must be natively offered: {request}"
                );
                (
                    json!({"tool_calls":[{"index":0,"id":format!("native-{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
                    "tool_calls",
                )
            };
            let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":3}});
            Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
        } else if request["messages"].to_string().contains("Classify whether an assistant") {
            Some(("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}).to_string()))
        } else {
            Some(("application/json", json!({"data":[]}).to_string()))
        }
    });
    let fixture = Rook::new();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\nmax_steps=8\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\n[models.local]\napi='openai'\nmodel='test'\nurl='{}/v1'\ncontext_window=128000\n[[mcp]]\nname='documents'\nurl='{}/mcp'\n",server.url,server.url));
    let one = one_at_a_time();
    let local = fixture.json(&["run", "Read the document through MCP resource discovery"]);
    let first = local["session"].as_str().unwrap().to_owned();
    assert!(local["outcome"]["reply"].as_str().unwrap().contains("NATIVE_RESOURCE_RECEIPT_26"));
    drop(one);
    let daemon = Daemon::start(&fixture);
    let remote = fixture.json(&["run", "Read the document through MCP resource discovery"]);
    let second = remote["session"].as_str().unwrap().to_owned();
    assert!(remote["outcome"]["reply"].as_str().unwrap().contains("NATIVE_RESOURCE_RECEIPT_26"));
    let connections = fixture.json(&["mcp", "status"]);
    assert_eq!(connections["servers"][0]["resources"], true);
    assert_eq!(connections["servers"][0]["tools"], 0);
    assert!(fixture.ok(&["mcp", "status"]).contains("resources"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for session in [&first, &second] {
        let saved = fixture.json(&["session", "history", session]).to_string();
        assert!(
            saved.contains("read_mcp_resource")
                && saved.contains("document:receipt")
                && saved.contains("NATIVE_RESOURCE_RECEIPT_26"),
            "{saved}"
        );
        let page: Value = runtime.block_on(async {
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("{}/api/sessions/{session}/history", daemon.address))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap()
        });
        assert!(page.to_string().contains("document:receipt"));
        let context = fixture.json(&["session", "context", session]);
        assert!(context.to_string().contains("read_mcp_resource"));
    }
    drop(daemon);
    for session in [&first, &second] {
        assert!(
            fixture.json(&["session", "history", session]).to_string().contains("NATIVE_RESOURCE_RECEIPT_26")
        );
    }
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|(_, r)| r["method"] == "resources/read").count(),
        2,
        "history/context/reopen must not repeat resource reads"
    );
    assert!(!requests.iter().any(|(_, r)| r["method"] == "tools/list"));
}
