use super::*;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(mut respond: impl FnMut(&str, Value) -> Option<(&'static str, String)> + Send + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halt = stop.clone();
        let thread = std::thread::spawn(move || {
            while !halt.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
                let mut bytes = Vec::new();
                let (path, request) = loop {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).unwrap();
                    assert!(n > 0 && bytes.len() + n <= 2 * 1024 * 1024);
                    bytes.extend_from_slice(&chunk[..n]);
                    let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                    let head = String::from_utf8_lossy(&bytes[..at]);
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + len {
                        break (
                            head.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned(),
                            if len == 0 {
                                Value::Null
                            } else {
                                serde_json::from_slice(&bytes[at + 4..at + 4 + len]).unwrap()
                            },
                        );
                    }
                };
                if let Some((mime, body)) = respond(&path, request) {
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            }
        });
        Self { url, stop, thread: Some(thread) }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let done = thread.join();
            if !std::thread::panicking() {
                assert!(done.is_ok());
            }
        }
    }
}
struct ReleaseOnDrop(Arc<AtomicBool>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
fn input(id: i64, topic: i64, user: i64, text: &str) -> Value {
    json!({"update_id":id,"message":{"message_id":id,"chat":{"id":-10},"message_thread_id":topic,"from":{"id":user,"is_bot":false},"text":text}})
}
fn wait(mut condition: impl FnMut() -> bool, label: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "{label}");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
#[test]
fn telegram_http_routes_followups_live_approvals_stop_and_uncertain_results_survive_restart_without_replay() {
    rook_llm::init_tls();
    let incoming = Arc::new(Mutex::new(Vec::<Value>::new()));
    let outgoing = Arc::new(Mutex::new(Vec::<Value>::new()));
    let lost = Arc::new(AtomicBool::new(false));
    let (updates, sends, lose) = (incoming.clone(), outgoing.clone(), lost.clone());
    let bot = Server::new(move |path, request| {
        assert!(path.starts_with("/bot123:fixture-telegram-token/"));
        let body = if path.ends_with("/getMe") {
            json!({"ok":true,"result":{"id":123}})
        } else if path.ends_with("/getUpdates") {
            std::thread::sleep(std::time::Duration::from_millis(25));
            let offset = request["offset"].as_i64().unwrap();
            json!({"ok":true,"result":updates.lock().unwrap().iter().filter(|u|u["update_id"].as_i64().unwrap()>=offset).take(32).cloned().collect::<Vec<_>>()})
        } else if path.ends_with("/sendMessage") {
            let mut kept = sends.lock().unwrap();
            assert!(kept.len() < 128);
            kept.push(request.clone());
            if request["text"].as_str().is_some_and(|t| t == "TELEGRAM_NATIVE_42 was written.")
                && !lose.swap(true, Ordering::SeqCst)
            {
                return None;
            }
            json!({"ok":true,"result":{"message_id":kept.len()}})
        } else if path.ends_with("/answerCallbackQuery") {
            json!({"ok":true,"result":true})
        } else {
            panic!("unexpected bot method")
        };
        Some(("application/json", body.to_string()))
    });
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = observed.clone();
    let release = Arc::new(AtomicBool::new(false));
    let released = release.clone();
    let model = Server::new(move |_, request| {
        requests.lock().unwrap().push(request.clone());
        if request["stream"] == true {
            let messages = request["messages"].to_string();
            if messages.contains("WAIT_FOR_STOP") {
                while !released.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            let tool_result =
                request["messages"].as_array().is_some_and(|ms| ms.iter().any(|m| m["role"] == "tool"));
            let (delta, finish) = if tool_result {
                (json!({"content":"TELEGRAM_NATIVE_42 was written."}), "stop")
            } else {
                (
                    json!({"tool_calls":[{"index":0,"id":"bot-command","type":"function","function":{"name":"run_command","arguments":json!({"command":if cfg!(windows) {
                        "powershell -NoProfile -NonInteractive -Command \"[IO.File]::WriteAllText('telegram-result','TELEGRAM_NATIVE_42')\""
                    } else {
                        "printf TELEGRAM_NATIVE_42 > telegram-result"
                    }}).to_string()}}]}),
                    "tool_calls",
                )
            };
            let frame = json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
            Some(("text/event-stream", format!("data: {frame}\n\ndata: [DONE]\n\n")))
        } else if request["messages"].to_string().contains("Classify whether an assistant") {
            Some(("application/json",json!({"choices":[{"message":{"role":"assistant","content":"{\"action\":\"finish\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}).to_string()))
        } else {
            Some(("application/json", json!({"data":[]}).to_string()))
        }
    });
    let _release_on_drop = ReleaseOnDrop(release.clone());
    let fixture = Rook::new();
    // Canonical routing must still reach the daemon opened through an alias.
    #[cfg(unix)]
    let daemon_workspace = {
        let alias = fixture.home.path().join("workspace-alias");
        std::os::unix::fs::symlink(fixture.workspace.path(), &alias).unwrap();
        assert_ne!(alias, alias.canonicalize().unwrap());
        alias
    };
    #[cfg(not(unix))]
    let daemon_workspace = fixture.workspace.path().to_path_buf();
    fixture.write_config(&format!("[agent]\nmodel='local'\ninstall_servers=false\none_script=false\nplan_first=false\n[memory]\nenabled=false\n[web]\nenabled=false\n[sandbox]\nstance='assist'\n[models.local]\napi='openai'\nmodel='test'\nurl='{}/v1'\ncontext_window=128000\n[telegram]\nenabled=true\nsecret='bot'\napi_url='{}'\nworkspace={}\nallowed_users=[1,2]\nallowed_chats=[-10]\nmax_turn_secs=120\n",model.url,bot.url,serde_json::to_string(fixture.workspace.path().to_str().unwrap()).unwrap()));
    let mut vault = rook_core::Vault::load_from(fixture.home.path().join("secrets.toml")).unwrap();
    vault.keep("bot", "123:fixture-telegram-token").unwrap();
    incoming.lock().unwrap().extend([
        input(1, 7, 99, "untrusted"),
        input(2, 7, 1, "Write TELEGRAM_NATIVE_42 to telegram-result"),
    ]);
    let daemon = Daemon::start_in(&fixture, &daemon_workspace);
    wait(
        || outgoing.lock().unwrap().iter().any(|m| m.get("reply_markup").is_some()),
        "live tool approval was never published",
    );
    assert!(
        !fixture.workspace.path().join("telegram-result").exists(),
        "command waits for an actual live approval"
    );
    let routes = fixture.json(&["telegram", r#"{"action":"routes","after":null}"#]);
    assert_eq!(routes["routes"].as_array().unwrap().len(), 1);
    let session = routes["routes"][0]["session"].as_str().unwrap().to_owned();
    incoming.lock().unwrap().push(input(3, 7, 1, "Keep the same output file; this is a follow-up."));
    wait(
        || {
            fixture.json(&["telegram", r#"{"action":"inputs","after":null}"#])["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["update"] == 3 && i["state"] == "handled")
        },
        "follow-up was not retained while waiting",
    );
    let token = outgoing
        .lock()
        .unwrap()
        .iter()
        .find_map(|m| m["reply_markup"]["inline_keyboard"][0][0]["callback_data"].as_str().map(str::to_owned))
        .unwrap();
    let callback = |id, user| json!({"update_id":id,"callback_query":{"id":format!("query-{id}"),"from":{"id":user,"is_bot":false},"data":token,"message":{"chat":{"id":-10},"message_thread_id":7}}});
    incoming.lock().unwrap().push(callback(4, 2));
    wait(
        || {
            fixture.json(&["telegram", r#"{"action":"inputs","after":null}"#])["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["update"] == 4 && i["state"] == "failed")
        },
        "wrong-owner callback was not refused",
    );
    assert!(!fixture.workspace.path().join("telegram-result").exists());
    incoming.lock().unwrap().push(callback(5, 1));
    wait(
        || {
            std::fs::read_to_string(fixture.workspace.path().join("telegram-result"))
                .is_ok_and(|text| text == "TELEGRAM_NATIVE_42")
        },
        "authorized approval never released the ordinary tool",
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("telegram-result")).unwrap(),
        "TELEGRAM_NATIVE_42"
    );
    let result_id = format!("result-{session}-");
    let mut delivery = Value::Null;
    wait(
        || {
            let page = fixture.json(&["telegram", r#"{"action":"outbox","after":null}"#]);
            if let Some(i) = page["outbox"].as_array().unwrap().iter().find(|i| {
                i["id"].as_str().is_some_and(|s| s.starts_with(&result_id)) && i["state"] == "uncertain"
            }) {
                delivery = i.clone();
                true
            } else {
                false
            }
        },
        "lost response was not retained as an uncertain delivery",
    );
    drop(daemon);
    let restarted = Daemon::start_in(&fixture, &daemon_workspace);
    incoming.lock().unwrap().push(input(6, 8, 1, "/status"));
    wait(
        || {
            fixture.json(&["telegram", r#"{"action":"routes","after":null}"#])["routes"]
                .as_array()
                .unwrap()
                .len()
                == 2
        },
        "new topic did not create its own session",
    );
    assert_eq!(
        outgoing.lock().unwrap().iter().filter(|m| m["text"] == "TELEGRAM_NATIVE_42 was written.").count(),
        1,
        "restart must not resend an uncertain response"
    );
    let retry=json!({"action":"retry","id":delivery["id"],"attempts":delivery["attempts"],"note":"Fixture observed the lost response; deliberately allow one duplicate"}).to_string();
    fixture.json(&["telegram", &retry]);
    let inspect = json!({"action":"inspect","id":delivery["id"]}).to_string();
    wait(|| fixture.json(&["telegram", &inspect])["state"] == "sent", "reviewed retry never completed");
    assert_eq!(
        outgoing.lock().unwrap().iter().filter(|m| m["text"] == "TELEGRAM_NATIVE_42 was written.").count(),
        2
    );
    incoming.lock().unwrap().push(input(7, 7, 1, "WAIT_FOR_STOP"));
    wait(
        || {
            observed
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["stream"] == true && r["messages"].to_string().contains("WAIT_FOR_STOP"))
        },
        "second turn never reached a live model wait",
    );
    incoming.lock().unwrap().push(input(8, 7, 1, "/stop"));
    wait(
        || {
            fixture.json(&["telegram", r#"{"action":"inputs","after":null}"#])["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["update"] == 8 && i["state"] == "handled")
        },
        "stop was not applied to the current turn",
    );
    release.store(true, Ordering::SeqCst);
    let remote = fixture.json(&["telegram", &inspect]);
    drop(restarted);
    assert_eq!(fixture.json(&["telegram", &inspect]), remote);
    let engine = rook_core::Rook::from_parts(
        rook_store::Store::open(fixture.home.path().join("store")).unwrap(),
        rook_core::Config::default(),
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        fixture.workspace.path().into(),
    );
    let entries = engine.store.events(rook_store::parse_session_id(&session).unwrap(), 0, 1000).unwrap();
    let user: Vec<_> = entries
        .iter()
        .filter(|e| e.record.kind == rook_store::EventKind::UserMessage)
        .map(|e| String::from_utf8(engine.store.get(&e.record.body).unwrap()).unwrap())
        .collect();
    assert_eq!(user.iter().filter(|t| t.contains("this is a follow-up")).count(), 1);
    assert!(!user.iter().any(|t| t == "untrusted" || t == "/stop"));
}
