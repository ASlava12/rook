//! A text-only endpoint sees page text, never PDF/base64 bytes or image blocks.
use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[test]
#[ignore = "requires local Poppler helpers"]
fn selected_pdf_pages_reach_a_text_only_model_locally_and_through_the_daemon() {
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
        let read = messages.iter().any(|m| {
            m["role"] == "tool" && m["content"].as_str().is_some_and(|s| s.contains("PDF_PAGE_TWO_RECEIPT"))
        });
        let checking = messages.iter().any(|m| {
            m["content"].as_str().is_some_and(|s| s.contains("You are checking a claim somebody else made"))
        });
        let (delta, finish) = if read {
            (
                json!({"content":if checking {"PDF_PAGE_TWO_RECEIPT\nVERDICT: holds"}else{"PDF_PAGE_TWO_RECEIPT"}}),
                "stop",
            )
        } else {
            assert!(request["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "read_pdf"));
            (
                json!({"tool_calls":[{"index":0,"id":"pdf-read","type":"function","function":{"name":"read_pdf","arguments":json!({"path":"pages.pdf","pages":"2"}).to_string()}}]}),
                "tool_calls",
            )
        };
        let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":8,"completion_tokens":3}});
        Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
    });
    let fixture = Rook::new();
    std::fs::write(fixture.workspace.path().join("pages.pdf"), include_bytes!("../fixtures/pages.pdf"))
        .unwrap();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\nmax_steps=8\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\n[models.local]\napi='openai'\nmodel='text-only'\nurl='{}/v1'\ncontext_window=128000\n",server.url));
    let one = one_at_a_time();
    let local = fixture.json(&["run", "Read the second PDF page"]);
    assert!(local["outcome"]["reply"].as_str().unwrap().contains("PDF_PAGE_TWO_RECEIPT"));
    let local_session = local["session"].as_str().unwrap().to_owned();
    drop(one);
    let daemon = Daemon::start(&fixture);
    let remote = fixture.json(&["run", "Read the second PDF page"]);
    assert!(remote["reply"].as_str().unwrap().contains("PDF_PAGE_TWO_RECEIPT"));
    for session in [&local_session, remote["session"].as_str().unwrap()] {
        let history = fixture.json(&["session", "history", session]).to_string();
        assert!(
            history.contains("read_pdf")
                && history.contains("PDF_PAGE_TWO_RECEIPT")
                && history.contains("sha256")
        );
        assert!(!history.contains("PDF_PAGE_ONE") && !history.contains("PDF_PAGE_THREE"));
    }
    drop(daemon);
    let requests = requests.lock().unwrap();
    assert!(requests.len() >= 4);
    for request in requests.iter() {
        assert!(!request.to_string().contains("%PDF-"));
        assert!(
            request["messages"].as_array().unwrap().iter().all(|m| !m["content"].is_array()),
            "text-only requests must not contain image blocks"
        );
    }
}
