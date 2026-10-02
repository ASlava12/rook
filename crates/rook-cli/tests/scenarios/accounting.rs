//! Cost coverage from real streaming/nonstreaming HTTP, local and daemon paths.
use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct Model {
    url: String,
    requests: mpsc::Receiver<Value>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
impl Model {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let (send, requests) = mpsc::sync_channel(16);
        let thread = std::thread::spawn(move || {
            let mut count = 0;
            while !halt.load(Ordering::SeqCst) && count < 32 {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                count += 1;
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(90))).unwrap();
                let mut bytes = Vec::new();
                let request: Value = loop {
                    let mut chunk = [0; 8192];
                    let Ok(n) = socket.read(&mut chunk) else { break Value::Null };
                    if n == 0 {
                        break Value::Null;
                    }
                    assert!(bytes.len() + n <= 4 * 1024 * 1024);
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(split) = bytes.windows(4).position(|s| s == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&bytes[..split]);
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse().ok())
                        })
                        .unwrap();
                    if bytes.len() < split + 4 + length {
                        continue;
                    }
                    break serde_json::from_slice(&bytes[split + 4..split + 4 + length]).unwrap();
                };
                if request.is_null() {
                    continue;
                }
                send.try_send(request.clone()).unwrap();
                if request["model"] == "unavailable" {
                    let body = r#"{"error":{"code":"insufficient_quota"}}"#;
                    let _ = write!(
                        socket,
                        "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    continue;
                }
                let streamed = request["stream"] == true;
                let repair = request["messages"].to_string().contains("Repair only the JSON representation");
                let (text, input, output, cached) = if streamed {
                    ("not JSON", 10, 5, 3)
                } else if repair {
                    (r#"{"value":7}"#, 6, 3, 0)
                } else {
                    (r#"{"action":"finish"}"#, 4, 2, 0)
                };
                let choice = if streamed {
                    json!({"index":0,"delta":{"role":"assistant","content":text},"finish_reason":"stop"})
                } else {
                    json!({"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"})
                };
                let answer = json!({"model":"server-echo","choices":[choice],"usage":{"prompt_tokens":input,"completion_tokens":output,"prompt_tokens_details":{"cached_tokens":cached}}});
                let (mime, body) = if streamed {
                    ("text/event-stream", format!("data: {answer}\n\ndata: [DONE]\n\n"))
                } else {
                    ("application/json", answer.to_string())
                };
                assert!(body.len() <= 4096);
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            assert!(count < 32, "accounting fixture connection budget exhausted");
        });
        Self { url, requests, stop, thread: Some(thread) }
    }
    fn next(&self) -> Value {
        self.requests.try_recv().unwrap()
    }
}

#[test]
fn auxiliary_pricing_and_explicit_missing_coverage_survive_reopen_and_fork_locally_and_through_daemon() {
    rook_llm::init_tls();
    for shared in [false, true] {
        for priced in [false, true] {
            let _local = (!shared).then(one_at_a_time);
            let rook = Rook::new();
            let model = Model::new();
            let rates = if priced {
                "input_usd_per_million=2.0\noutput_usd_per_million=6.0\ncache_read_usd_per_million=0.2\n"
            } else {
                ""
            };
            let settings = format!(
                "[agent]\nmodel='selected'\ninstall_servers=false\none_script=false\nplan_first=false\n[sandbox]\nmode='readonly'\n[models.selected]\napi='openai'\nmodel='unavailable'\nurl='{}'\ncontext_window=65536\n[models.backup]\napi='openai'\nmodel='physical'\nurl='{}'\ncontext_window=65536\npriority=1\n{rates}",
                model.url, model.url
            );
            rook.write_config(&settings);
            let schema = rook.workspace.path().join("answer-schema.json");
            std::fs::write(&schema, r#"{"type":"object","required":["value"],"properties":{"value":{"type":"integer"}},"additionalProperties":false}"#).unwrap();
            let mut daemon = shared.then(|| Daemon::start(&rook));
            let output =
                rook.json(&["run", "Answer with value seven.", "--output-schema", schema.to_str().unwrap()]);
            let outcome = output.get("outcome").unwrap_or(&output);
            assert_eq!(
                serde_json::from_str::<Value>(outcome["reply"].as_str().unwrap()).unwrap(),
                json!({"value":7})
            );
            let session = output["session"].as_str().unwrap();
            assert_eq!(model.next()["model"], "unavailable");
            let main = model.next();
            let check = model.next();
            let repair = model.next();
            assert_eq!(main["model"], "physical");
            assert_eq!(main["stream"], true);
            assert_ne!(check["stream"], true);
            assert_ne!(repair["stream"], true);
            assert!(check["messages"].to_string().contains("Classify whether"));
            assert!(repair["messages"].to_string().contains("Repair only"));
            assert!(model.requests.try_recv().is_err());
            let context = rook.json(&["session", "context", session]);
            let coverage = &context["cost_coverage"];
            assert_eq!(coverage["main_receipts"], 1);
            assert_eq!(coverage["auxiliary_receipts"], 2);
            assert_eq!(coverage["usage_events_without_receipt"], 0);
            assert_eq!(
                coverage["complete_accounting"], false,
                "the primary failure cannot be presented as free"
            );
            assert_eq!(context["last_response"]["receipt"]["dispatch"]["provider"], "backup");
            if priced {
                assert_eq!(coverage["priced_receipts"], 3);
                assert_eq!(coverage["unpriced_receipts"], 0);
                assert!((coverage["known_subtotal_usd"].as_f64().unwrap() - 0.0000946).abs() < 1e-15);
            } else {
                assert_eq!(coverage["unpriced_receipts"], 3);
                assert!(coverage["known_subtotal_usd"].is_null());
            }
            let text = rook.ok(&["session", "context", session]);
            assert!(text.contains("Total cost is unknown") && text.contains("retry/failure"));
            drop(daemon.take());
            let end = {
                let store = rook_store::Store::open(rook.home.path().join("store")).unwrap();
                let id = rook_store::parse_session_id(session).unwrap();
                let events = store.events(id, 0, 256).unwrap();
                let aux: Vec<Value> = events
                    .iter()
                    .filter(|e| e.record.label == "rook:model-aux:v1")
                    .map(|e| serde_json::from_slice(&store.get(&e.record.body).unwrap()).unwrap())
                    .collect();
                assert_eq!(aux.len(), 2);
                assert_eq!(aux[0]["purpose"], "completion_check");
                assert_eq!(aux[1]["purpose"], "output_repair");
                for receipt in aux.iter().map(|a| &a["receipt"]) {
                    assert_eq!(receipt["dispatch"]["provider"], "backup");
                    assert_eq!(receipt["dispatch"]["model"], "physical");
                    assert_eq!(receipt["reported_model"], "server-echo");
                    assert_eq!(receipt["complete"], true);
                    assert_eq!(receipt["usage_reported"], true);
                }
                let meta = store.get_session(id).unwrap().unwrap();
                assert_eq!(
                    (meta.tokens_in, meta.tokens_out),
                    (20, 10),
                    "receipts do not double-charge the carriers"
                );
                meta.next_seq
            };
            let new_rates =
                "input_usd_per_million=99.0\noutput_usd_per_million=99.0\ncache_read_usd_per_million=99.0\n";
            let repriced =
                if priced { settings.replace(rates, new_rates) } else { format!("{settings}{new_rates}") };
            rook.write_config(&repriced);
            daemon = shared.then(|| Daemon::start(&rook));
            assert_eq!(
                rook.json(&["session", "context", session])["cost_coverage"],
                *coverage,
                "historical rates do not follow current configuration"
            );
            let fork = rook.ok(&["session", "fork", session, "--at", &end.to_string()]);
            let child = fork.split_whitespace().last().unwrap();
            assert_eq!(rook.json(&["session", "context", child])["cost_coverage"], *coverage);
            assert!(
                rook.ok(&["session", "context", child])
                    .contains("Inherited receipts are historical, not new charges")
            );
            drop(daemon);
        }
    }
}
