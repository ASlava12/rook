//! Dedicated remote transport and native-loop fixtures.
#![cfg(test)]

use crate::Result;
use crate::remote_agents::{DOMAIN, Envelope, Operation, Peer, Settings, TOOL, signature};
use crate::{
    Config, Rook, Vault,
    inbound::{Delivery, Route},
    work::managed,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
const SECRET: &str = "remote-agent-fixture-private-key";

struct Network {
    address: String,
    rook: Arc<Rook>,
    halt: Arc<AtomicBool>,
    tamper: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Network {
    fn new(rook: Rook, vault: Vault, lose_first_ack: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let rook = Arc::new(rook);
        let server = rook.clone();
        let halt = Arc::new(AtomicBool::new(false));
        let stopping = halt.clone();
        let tamper = Arc::new(AtomicBool::new(false));
        let corrupt = tamper.clone();
        let thread = std::thread::spawn(move || {
            let mut lose = lose_first_ack;
            while !stopping.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
                let mut data = Vec::new();
                let (head, body) = loop {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0 && data.len() + n <= 128 * 1024);
                    data.extend_from_slice(&chunk[..n]);
                    let Some(at) = data.windows(4).position(|b| b == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&data[..at]).to_string();
                    let size = head
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if data.len() >= at + 4 + size {
                        break (head, data[at + 4..at + 4 + size].to_vec());
                    }
                };
                assert!(head.starts_with("POST /api/a2a/alpha HTTP/1.1"));
                let header = |name: &str| {
                    head.lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                                .map(|(_, v)| v.trim())
                        })
                        .unwrap()
                };
                let result = server.agent_http_reply(
                    Delivery {
                        route: "alpha",
                        id: header("x-rook-delivery"),
                        timestamp: header("x-rook-timestamp"),
                        signature: header("x-rook-signature"),
                        body: &body,
                    },
                    &vault,
                );
                if lose {
                    assert!(result.is_ok());
                    lose = false;
                    continue;
                }
                let (status, mut bytes, signature) = match result {
                    Ok((bytes, signature)) => (200, bytes, signature),
                    Err(error) => {
                        (400, json!({"error":error.to_string()}).to_string().into_bytes(), String::new())
                    }
                };
                if corrupt.swap(false, Ordering::SeqCst) {
                    bytes[0] = b'[';
                }
                let body = String::from_utf8(bytes).unwrap();
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nx-rook-response-signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self { address, rook, halt, tamper, thread: Some(thread) }
    }
}
impl Drop for Network {
    fn drop(&mut self) {
        self.halt.store(true, Ordering::SeqCst);
        let result = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            assert!(result.is_ok());
        }
    }
}
fn connect(rook: &mut Rook, network: &Network) {
    rook.config.remote_agents.peers.insert(
        "worker".into(),
        Peer {
            url: network.address.clone(),
            route: "alpha".into(),
            secret: "auth".into(),
            description: "independent computer".into(),
            allow_http: false,
        },
    );
}

#[tokio::test]
async fn independent_stores_exchange_real_http_and_a_lost_ack_retry_admits_one_remote_task() {
    let (_remote_root, remote, vault) = fixture();
    let network = Network::new(remote, vault, true);
    let (_local_root, mut local, vault) = fixture();
    connect(&mut local, &network);
    let session = local.start_session("local sender").unwrap();
    let operation = Operation::Submit { text: "REMOTE_HTTP_TASK".into() };
    let failure = local
        .remote_agent_request(&vault, session, "worker", Some("job-one"), operation.clone())
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("retry only the same"));
    let ack =
        local.remote_agent_request(&vault, session, "worker", Some("job-one"), operation).await.unwrap();
    assert_eq!(ack["response"]["duplicate"], true);
    assert_eq!(network.rook.pending_events().unwrap().len(), 1);
    assert!(local.pending_events().unwrap().is_empty());
    let id = ack["response"]["id"].as_str().unwrap().to_string();
    network.rook.launch_event(&id).unwrap();
    let sent = local
        .remote_agent_request(
            &vault,
            session,
            "worker",
            Some("message-one"),
            Operation::Message { id: id.clone(), text: "REMOTE_HTTP_UPDATE".into() },
        )
        .await
        .unwrap();
    assert!(sent["response"]["message"]["text"].as_str().unwrap().contains("REMOTE_HTTP_UPDATE"));
    let status =
        local.remote_agent_request(&vault, session, "worker", None, Operation::Status { id }).await.unwrap();
    assert_eq!(status["response"]["work"]["status"], "queued");
    network.tamper.store(true, Ordering::SeqCst);
    let tampered =
        local.remote_agent_request(&vault, session, "worker", None, Operation::Info).await.unwrap_err();
    assert!(tampered.to_string().contains("response signature"), "{tampered}");
}

#[tokio::test]
async fn a_signed_peer_response_is_still_bounded_while_its_bytes_arrive() {
    let (_remote_root, mut remote, vault) = fixture();
    remote.config.inbound.routes.get_mut("alpha").unwrap().task = format!("x{}", "\n".repeat(16382));
    let info = send(&remote, &vault, "alpha", "measure", Operation::Info).unwrap();
    let encoded = serde_json::to_vec(&json!({"protocol":"rook-a2a-v1","result":info})).unwrap();
    assert!(encoded.len() > 32768, "the fixture must exceed the client response admission bound");
    let network = Network::new(remote, vault, false);
    let (_local_root, mut local, vault) = fixture();
    connect(&mut local, &network);
    local.config.remote_agents.response_bytes = 32768;
    let session = local.start_session("bounded remote response").unwrap();
    let failure =
        local.remote_agent_request(&vault, session, "worker", None, Operation::Info).await.unwrap_err();
    assert!(failure.to_string().contains("response_bytes while arriving"), "{failure}");
}

#[tokio::test]
async fn native_remote_tool_polls_verified_pending_work_and_bounds_new_tasks_across_the_turn() {
    struct Approved;
    #[async_trait::async_trait]
    impl rook_tools::policy::Approver for Approved {
        async fn ask(
            &self,
            _: &str,
            _: &rook_tools::policy::Risk,
            _: Option<&str>,
        ) -> rook_tools::policy::Approval {
            rook_tools::policy::Approval::Once
        }
    }
    struct Workflow(std::sync::Mutex<(usize, Option<String>)>);
    #[async_trait::async_trait]
    impl rook_llm::Provider for Workflow {
        fn id(&self) -> &str {
            "fixture/remote-agent"
        }
        fn context_window(&self) -> usize {
            32768
        }
        async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            let text = request.messages.last().unwrap().content.as_str();
            let classifier =
                request.messages.iter().any(|m| m.content.contains("Classify whether an assistant"));
            let mut state = self.0.lock().unwrap();
            let args = if classifier {
                None
            } else {
                match state.0 {
                    0 => Some(
                        json!({"action":"submit","peer":"worker","request":"first","text":"WAIT_FOR_REMOTE"}),
                    ),
                    1..=4 => {
                        if state.1.is_none() {
                            let source: Value = serde_json::from_str(text).unwrap();
                            let result: Value =
                                serde_json::from_str(source["rook_source"]["content"].as_str().unwrap())
                                    .unwrap();
                            state.1 = Some(result["response"]["id"].as_str().unwrap().to_string());
                        }
                        Some(json!({"action":"status","peer":"worker","id":state.1}))
                    }
                    5 => Some(
                        json!({"action":"submit","peer":"worker","request":"second","text":"SHOULD_NOT_RUN"}),
                    ),
                    6 => {
                        assert!(text.contains("remote task allowance"), "{text}");
                        None
                    }
                    _ => panic!("unexpected remote tool step: {text}"),
                }
            };
            if !classifier {
                state.0 += 1;
            }
            let message = match args {
                Some(arguments) => rook_llm::Message {
                    tool_calls: vec![rook_llm::ToolCall {
                        id: "remote".into(),
                        name: TOOL.into(),
                        arguments,
                    }],
                    ..rook_llm::Message::assistant("")
                },
                None => rook_llm::Message::assistant(if classifier {
                    r#"{"action":"finish"}"#
                } else {
                    "remote task coordinated"
                }),
            };
            Ok(rook_llm::Response {
                stop_reason: if message.tool_calls.is_empty() {
                    rook_llm::StopReason::EndTurn
                } else {
                    rook_llm::StopReason::ToolUse
                },
                message,
                usage: Default::default(),
                model: "fixture".into(),
            })
        }
    }
    let (_remote_root, remote, vault) = fixture();
    let network = Network::new(remote, vault, false);
    let (_local_root, mut local, vault) = fixture();
    connect(&mut local, &network);
    local.config.remote_agents.max_tracked_tasks = 1;
    // AgentLoop loads the vault from the ordinary configured home. Seed only
    // this turn's transient credential, leaving process-global paths untouched.
    let session = local.start_session("native remote caller").unwrap();
    let provider = Arc::new(Workflow(std::sync::Mutex::new((0, None))));
    let mut agent = crate::agent::AgentLoop::new(&local, provider, session);
    agent.approver = Arc::new(Approved);
    agent.vault = Arc::new(vault);
    agent.tool_ctx.secrets = Some(agent.vault.clone());
    let outcome = agent.run("TEST_REMOTE_ROOT").await.unwrap();
    assert_eq!(outcome.reply, "remote task coordinated");
    assert_eq!(outcome.stopped, "end_turn");
    assert_eq!(network.rook.pending_events().unwrap().len(), 1);
}
fn fixture() -> (tempfile::TempDir, Rook, Vault) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut config = Config::default();
    config.remote_agents.enabled = true;
    config.remote_agents.routes = std::collections::BTreeSet::from(["alpha".into(), "beta".into()]);
    config.inbound.enabled = true;
    config.agent.install_servers = false;
    config.memory.enabled = false;
    config.learning.automatic = false;
    let route = Route {
        workspace: workspace.canonicalize().unwrap().display().to_string(),
        secret: "auth".into(),
        task: "Handle the authenticated remote request in operation.text within this workspace.".into(),
        autonomous: false,
        max_iterations: 2,
        max_tokens: 10000,
        max_seconds: 120,
    };
    config.inbound.routes = BTreeMap::from([("alpha".into(), route.clone()), ("beta".into(), route)]);
    let rook = Rook::from_parts(
        rook_store::Store::open(root.path().join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "test"),
        rook_skills::SkillIndex::default(),
        workspace,
    );
    let mut vault = Vault::load_from(root.path().join("secrets.toml")).unwrap();
    vault.keep("auth", SECRET).unwrap();
    (root, rook, vault)
}
fn send(rook: &Rook, vault: &Vault, route: &str, delivery: &str, operation: Operation) -> Result<Value> {
    let body =
        serde_json::to_vec(&Envelope { version: 1, sender: rook_store::format_session_id(1), operation })
            .unwrap();
    let timestamp = managed::now().to_string();
    let signed = signature(DOMAIN, route, delivery, &timestamp, &body, SECRET.as_bytes()).unwrap();
    rook.receive_agent_request(
        Delivery { route, id: delivery, timestamp: &timestamp, signature: &signed, body: &body },
        vault,
    )
    .map(|v| v["result"].clone())
}
#[test]
fn peer_authentication_binds_protocol_route_timestamp_sender_and_exact_bytes_before_effects() {
    let (_root, mut rook, vault) = fixture();
    let body = serde_json::to_vec(&Envelope {
        version: 1,
        sender: rook_store::format_session_id(1),
        operation: Operation::Submit { text: "request".into() },
    })
    .unwrap();
    let at = managed::now().to_string();
    let signed = signature(DOMAIN, "alpha", "one", &at, &body, SECRET.as_bytes()).unwrap();
    for (route, time, code, payload) in [
        ("beta", at.clone(), signed.clone(), body.clone()),
        ("alpha", "1".into(), signed.clone(), body.clone()),
        (
            "alpha",
            at.clone(),
            signature(b"rook-inbound-v1\0", "alpha", "one", &at, &body, SECRET.as_bytes()).unwrap(),
            body.clone(),
        ),
        ("alpha", at.clone(), signed.clone(), body.iter().copied().chain(*b" ").collect()),
    ] {
        assert!(
            rook.receive_agent_request(
                Delivery { route, id: "one", timestamp: &time, signature: &code, body: &payload },
                &vault
            )
            .is_err()
        );
    }
    assert!(rook.pending_events().unwrap().is_empty());
    assert!(rook.store.session_ids_after(None, 10).unwrap().is_empty());
    rook.config.remote_agents.enabled = false;
    assert!(
        rook.receive_agent_request(
            Delivery { route: "alpha", id: "one", timestamp: &at, signature: &signed, body: &body },
            &vault
        )
        .is_err()
    );
    rook.config.remote_agents.enabled = true;
    rook.config.remote_agents.request_bytes = 1024;
    assert!(send(&rook, &vault, "alpha", "large", Operation::Submit { text: "x".repeat(1200) }).is_err());
    assert!(rook.store.kv_get("inbound/v1").unwrap().is_none());
    rook.config.remote_agents.routes.remove("alpha");
    assert!(send(&rook, &vault, "alpha", "not-opted-in", Operation::Info).is_err());
    let legacy =
        signature(b"rook-inbound-v1\0", "alpha", "ordinary-event", &at, &body, SECRET.as_bytes()).unwrap();
    rook.receive_event(
        Delivery { route: "alpha", id: "ordinary-event", timestamp: &at, signature: &legacy, body: &body },
        &vault,
    )
    .unwrap();
    assert_eq!(
        rook.pending_events().unwrap().len(),
        1,
        "an ordinary webhook keeps its existing authority without exposing A2A controls"
    );
}
#[test]
fn remote_requests_survive_reopen_and_scope_messages_results_and_cancel_to_the_original_task() {
    let (root, rook, vault) = fixture();
    let first =
        send(&rook, &vault, "alpha", "submit-one", Operation::Submit { text: format!("task {SECRET}") })
            .unwrap();
    let id = first["id"].as_str().unwrap().to_string();
    assert_eq!(
        send(&rook, &vault, "alpha", "submit-one", Operation::Submit { text: format!("task {SECRET}") })
            .unwrap()["duplicate"],
        true
    );
    assert!(
        send(&rook, &vault, "alpha", "submit-one", Operation::Submit { text: "different".into() }).is_err()
    );
    let source = rook.store.events(rook_store::parse_session_id(&id).unwrap(), 0, 1).unwrap().pop().unwrap();
    assert!(!String::from_utf8(rook.store.get(&source.record.body).unwrap()).unwrap().contains(SECRET));
    let started = rook.launch_event(&id).unwrap();
    let run = started["run"]["id"].as_str().unwrap().to_string();
    let config = rook.config.clone();
    let workspace = rook.workspace.clone();
    drop(rook);
    let rook = Rook::from_parts(
        rook_store::Store::open(root.path().join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "test"),
        rook_skills::SkillIndex::default(),
        workspace,
    );
    assert!(send(&rook, &vault, "beta", "foreign", Operation::Status { id: id.clone() }).is_err());
    let operation = Operation::Message { id: id.clone(), text: format!("report {SECRET}") };
    let first = send(&rook, &vault, "alpha", "message-one", operation.clone()).unwrap();
    let again = send(&rook, &vault, "alpha", "message-one", operation).unwrap();
    assert_eq!(first["message"]["id"], again["message"]["id"]);
    let saved = managed::read(&rook, &run).unwrap();
    assert_eq!(saved.run.instructions.len(), 1);
    let note: Value = serde_json::from_str(&saved.run.instructions[0].text).unwrap();
    assert_eq!(note["rook_source"]["authority"], "data");
    assert!(!saved.run.instructions[0].text.contains(SECRET));
    assert!(
        send(
            &rook,
            &vault,
            "alpha",
            "message-one",
            Operation::Message { id: id.clone(), text: "changed".into() }
        )
        .is_err()
    );
    let reply = format!("{}{}{}", "x".repeat(4090), SECRET, "é🦀".repeat(1500));
    managed::update(&rook, &run, |saved| {
        saved.run.reply = reply.clone();
        Ok(())
    })
    .unwrap();
    let expected = vault.redact(&reply);
    let mut offset = 0;
    let mut source = None;
    let mut combined = String::new();
    loop {
        let page = send(
            &rook,
            &vault,
            "alpha",
            "result",
            Operation::Result { id: id.clone(), offset, source: source.clone() },
        )
        .unwrap();
        assert!(page["text"].as_str().unwrap().len() <= 4096);
        combined.push_str(page["text"].as_str().unwrap());
        source = Some(page["source"].as_str().unwrap().to_string());
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = next as usize;
    }
    assert_eq!(combined, expected);
    assert!(combined.len() > 4096);
    let status = send(&rook, &vault, "alpha", "status", Operation::Status { id: id.clone() }).unwrap();
    assert_eq!(status["work"]["reply_truncated"], true);
    assert_eq!(status["work"]["reply_bytes"], expected.len());
    assert!(!status.to_string().contains(SECRET));
    managed::update(&rook, &run, |saved| {
        saved.run.reply.push('!');
        Ok(())
    })
    .unwrap();
    assert!(
        send(&rook, &vault, "alpha", "stale", Operation::Result { id: id.clone(), offset: 4096, source })
            .is_err()
    );
    send(&rook, &vault, "alpha", "cancel-one", Operation::Cancel { id: id.clone() }).unwrap();
    send(&rook, &vault, "alpha", "cancel-one", Operation::Cancel { id: id.clone() }).unwrap();
    assert_eq!(managed::read(&rook, &run).unwrap().run.status, rook_proto::work::Status::Cancelled);
    managed::update(&rook, &run, |saved| {
        saved.run.generation = "replacement".into();
        Ok(())
    })
    .unwrap();
    assert!(send(&rook, &vault, "alpha", "wrong-generation", Operation::Cancel { id }).is_err());
}

#[test]
fn remote_peer_urls_and_settings_reject_untrusted_expansion() {
    let mut peer = Peer {
        url: "https://worker.example/rook".into(),
        route: "alpha".into(),
        secret: "auth".into(),
        description: String::new(),
        allow_http: false,
    };
    assert_eq!(peer.endpoint().unwrap().as_str(), "https://worker.example/rook/api/a2a/alpha");
    for url in [
        "http://192.168.1.2:8787",
        "https://u:p@worker.example",
        "https://worker.example/?q=1",
        "file:///tmp/rook",
    ] {
        peer.url = url.into();
        assert!(peer.endpoint().is_err(), "{url}");
    }
    peer.url = "http://127.0.0.1:8787".into();
    assert!(peer.endpoint().is_ok());
    peer.url = "http://192.168.1.2:8787".into();
    peer.allow_http = true;
    assert!(peer.endpoint().is_ok());
    let settings = Settings { max_tracked_tasks: 0, ..Default::default() };
    assert!(!settings.errors().is_empty());
}

#[test]
fn reply_authentication_cannot_be_replayed_for_a_different_operation_body() {
    use hmac::Mac;
    let first = rook_store::ObjectId::of(b"submit-body");
    let other = rook_store::ObjectId::of(b"cancel-body");
    let code = crate::remote_agents::reply_mac(
        "alpha",
        "same-name",
        "1700000000",
        first.as_bytes(),
        b"{}",
        SECRET.as_bytes(),
    )
    .unwrap()
    .finalize()
    .into_bytes();
    assert!(
        crate::remote_agents::reply_mac(
            "alpha",
            "same-name",
            "1700000000",
            other.as_bytes(),
            b"{}",
            SECRET.as_bytes()
        )
        .unwrap()
        .verify_slice(&code)
        .is_err()
    );
}
