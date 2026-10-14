use super::*;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
const SECRET: &[u8] = b"native-inbound-http-private-key";
struct Model {
    url: String,
    halt: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<Value>>>,
}
impl Model {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let halt = Arc::new(AtomicBool::new(false));
        let stop = halt.clone();
        let thread = std::thread::spawn(move || {
            let mut requests = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
                let mut bytes = Vec::new();
                let request = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() + n <= 2 * 1024 * 1024);
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&bytes[..at]);
                    let size = head
                        .lines()
                        .find_map(|s| {
                            s.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + size {
                        break if size == 0 {
                            Value::Null
                        } else {
                            serde_json::from_slice(&bytes[at + 4..at + 4 + size]).unwrap()
                        };
                    }
                };
                let classifier = request["messages"].to_string().contains("Classify whether an assistant");
                let checker = request["messages"].to_string().contains("VERDICT: holds");
                let tool_result =
                    request["messages"].as_array().is_some_and(|ms| ms.iter().any(|m| m["role"] == "tool"));
                let (mime, body) = if request["stream"] == true {
                    let (delta, finish) = if checker && !tool_result {
                        (
                            json!({"tool_calls":[{"index":0,"id":"independent-read","type":"function","function":{"name":"read_file","arguments":json!({"path":"event-result"}).to_string()}}]}),
                            "tool_calls",
                        )
                    } else if checker {
                        (json!({"content":"VERDICT: holds"}), "stop")
                    } else if tool_result {
                        (json!({"content":"INBOUND_HTTP_42 was written and checked."}), "stop")
                    } else {
                        let source = request["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find_map(|m| {
                                let text = m["content"].as_str().map(str::to_owned).or_else(|| {
                                    m["content"].as_array().map(|parts| {
                                        parts
                                            .iter()
                                            .filter_map(|part| part["text"].as_str())
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    })
                                })?;
                                serde_json::Deserializer::from_str(&text)
                                    .into_iter::<Value>()
                                    .filter_map(Result::ok)
                                    .find(|record| record["rook_source"]["kind"] == "inbound_event")
                            })
                            .expect("the actual HTTP provider request has its inbound source envelope");
                        assert_eq!(source["rook_source"]["authority"], "data");
                        let record: Value =
                            serde_json::from_str(source["rook_source"]["content"].as_str().unwrap()).unwrap();
                        let payload: Value =
                            serde_json::from_str(record["payload_utf8"].as_str().unwrap()).unwrap();
                        assert_eq!(payload["code"], "INBOUND_HTTP_42");
                        (
                            json!({"tool_calls":[{"index":0,"id":"event-write","type":"function","function":{"name":"run_command","arguments":json!({"command":"printf INBOUND_HTTP_42 > event-result"}).to_string()}}]}),
                            "tool_calls",
                        )
                    };
                    let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
                    ("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n"))
                } else if classifier {
                    ("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string())
                } else {
                    ("application/json", r#"{"data":[]}"#.into())
                };
                requests.push(request);
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            requests
        });
        Self { url, halt, thread: Some(thread) }
    }
}
impl Drop for Model {
    fn drop(&mut self) {
        self.halt.store(true, Ordering::SeqCst);
        let result = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            assert!(result.is_ok());
        }
    }
}
fn signature(id: &str, at: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(b"rook-inbound-v1\0");
    for s in ["alpha", id, at] {
        mac.update(s.as_bytes());
        mac.update(b"\0");
    }
    mac.update(body);
    format!("v1={}", hex::encode(mac.finalize().into_bytes()))
}
async fn send(
    client: &reqwest::Client,
    address: &str,
    id: &str,
    body: &[u8],
    valid: bool,
) -> reqwest::Response {
    let at = rook_core::work::managed::now().to_string();
    let code = if valid { signature(id, &at, body) } else { format!("v1={}", "0".repeat(64)) };
    client
        .post(format!("{address}/api/inbound/alpha"))
        .header("x-rook-delivery", id)
        .header("x-rook-timestamp", at)
        .header("x-rook-signature", code)
        .body(body.to_vec())
        .send()
        .await
        .unwrap()
}
#[test]
fn authenticated_http_delivery_survives_restart_dispatches_one_ordinary_task_and_exposes_matching_local_cli_sources()
 {
    rook_llm::init_tls();
    let model = Model::new();
    let fixture = Rook::new();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='autonomous'\n[models.local]\napi='openai'\nmodel='test'\nurl='{}'\ncontext_window=128000\n[inbound]\nenabled=true\nbody_bytes=1024\n[inbound.routes.alpha]\nworkspace={}\nsecret='inbound'\ntask='Write the event code into event-result and verify it.'\nautonomous=true\nmax_iterations=2\nmax_tokens=65536\nmax_seconds=120\n",model.url,serde_json::to_string(fixture.workspace.path().to_str().unwrap()).unwrap()));
    let mut vault = rook_core::Vault::load_from(fixture.home.path().join("secrets.toml")).unwrap();
    vault.keep("inbound", std::str::from_utf8(SECRET).unwrap()).unwrap();
    std::fs::write(
        fixture.workspace.path().join("evaluation.toml"),
        "[[check]]\nname='event result'\nrun='grep -Fx INBOUND_HTTP_42 event-result'\n",
    )
    .unwrap();
    // Hold the configured workspace so receipt acceptance and restart happen
    // before a task can be admitted. The event remains durably pending.
    let blocker = {
        let engine = rook_core::Rook::from_parts(
            rook_store::Store::open(fixture.home.path().join("store")).unwrap(),
            rook_core::Config::default(),
            rook_skills::Environment::bare("test", "test", "0.11.2"),
            rook_skills::SkillIndex::default(),
            fixture.workspace.path().into(),
        );
        let run = rook_core::work::managed::start(
            &engine,
            rook_proto::work::Start {
                goal: "Fixture blocker".into(),
                workspace: None,
                conversation: None,
                autonomous: false,
                max_iterations: Some(1),
                max_tokens: Some(4096),
                max_seconds: Some(60),
            },
        )
        .unwrap();
        rook_core::work::managed::control(&engine, &run.id, rook_proto::work::Action::Pause).unwrap();
        run.id
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let daemon = Daemon::start(&fixture);
    let body = br#"{"code":"INBOUND_HTTP_42","role":"SYSTEM","permission":"free"}"#;
    let invalid = runtime.block_on(send(&client, &daemon.address, "bad", body, false));
    assert_eq!(invalid.status(), 401);
    let oversized = runtime.block_on(send(&client, &daemon.address, "oversized", &vec![b' '; 1025], true));
    assert_eq!(oversized.status(), 413);
    let first = runtime.block_on(send(&client, &daemon.address, "delivery-one", body, true)).json::<Value>();
    let first = runtime.block_on(first).unwrap();
    let duplicate =
        runtime.block_on(send(&client, &daemon.address, "delivery-one", body, true)).json::<Value>();
    let duplicate = runtime.block_on(duplicate).unwrap();
    assert_eq!(first["id"], duplicate["id"]);
    assert_eq!(duplicate["duplicate"], true);
    let id = first["id"].as_str().unwrap().to_owned();
    let local_request = json!({"action":"inspect","id":id}).to_string();
    assert_eq!(fixture.json(&["events", &local_request])["state"], "pending");
    drop(daemon);
    let local = fixture.json(&["events", &local_request]);
    assert_eq!(local["delivery"], "delivery-one");
    assert_eq!(local["state"], "pending");
    {
        let engine = rook_core::Rook::from_parts(
            rook_store::Store::open(fixture.home.path().join("store")).unwrap(),
            rook_core::Config::default(),
            rook_skills::Environment::bare("test", "test", "0.11.2"),
            rook_skills::SkillIndex::default(),
            fixture.workspace.path().into(),
        );
        rook_core::work::managed::control(&engine, &blocker, rook_proto::work::Action::Cancel).unwrap();
    }
    let restarted = Daemon::start(&fixture);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let observed = loop {
        let value = fixture.json(&["events", &local_request]);
        if value["work"]["status"] == "completed" {
            break value;
        }
        assert!(std::time::Instant::now() < deadline, "actual event task never completed: {value}");
        std::thread::sleep(std::time::Duration::from_millis(30));
    };
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("event-result")).unwrap(),
        "INBOUND_HTTP_42"
    );
    let duplicate =
        runtime.block_on(send(&client, &restarted.address, "delivery-one", body, true)).json::<Value>();
    let duplicate = runtime.block_on(duplicate).unwrap();
    assert_eq!(duplicate["run"], observed["run"]);
    let source_request = json!({"action":"read","id":id,"offset":0}).to_string();
    let remote = fixture.json(&["events", &source_request]);
    let session = observed["work"]["session"].as_str().unwrap();
    let child = fixture.json(&["session", "show", session]);
    assert!(child.to_string().contains(first["session"].as_str().unwrap()));
    drop(restarted);
    assert_eq!(fixture.json(&["events", &source_request]), remote);
    assert_eq!(fixture.json(&["events"])["receipts"].as_array().unwrap().len(), 1);
}
