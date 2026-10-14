use rook_core::{
    Config, Rook, Vault,
    telegram::{Client, Request, Response, Settings},
};
use serde_json::{Value, json};
struct Fixture {
    root: tempfile::TempDir,
    rook: Rook,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            telegram: Settings {
                enabled: true,
                secret: "bot".into(),
                workspace: root.path().canonicalize().unwrap().display().to_string(),
                allowed_users: vec![1, 2],
                allowed_chats: vec![-10],
                ..Default::default()
            },
            ..Default::default()
        };
        let rook = Rook::from_parts(
            rook_store::Store::open(root.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("test", "test", "0.11.2"),
            rook_skills::SkillIndex::default(),
            root.path().into(),
        );
        rook.telegram_bot(123).unwrap();
        Self { root, rook }
    }
    fn input(&self, id: i64, topic: i64) -> rook_core::telegram::Input {
        self.rook.telegram_update(&update(id, topic, 1, "hello")).unwrap().unwrap()
    }
    fn route(&self) -> rook_core::telegram::Route {
        self.rook.telegram_routes().unwrap().remove(0)
    }
}
fn update(id: i64, topic: i64, user: i64, text: &str) -> Value {
    json!({"update_id":id,"message":{"message_id":id,"chat":{"id":-10},"message_thread_id":topic,"from":{"id":user,"is_bot":false},"text":text}})
}
#[test]
fn access_topics_duplicates_and_reopen_keep_one_ordinary_session_per_route() {
    let f = Fixture::new();
    assert!(f.rook.telegram_update(&update(1, 0, 3, "untrusted")).unwrap().is_none());
    assert!(f.rook.telegram_routes().unwrap().is_empty());
    let one = f.input(2, 0);
    assert_eq!(f.input(2, 0).session, one.session);
    assert!(f.rook.telegram_update(&update(2, 0, 1, "changed")).is_err());
    assert_eq!(f.input(3, 0).session, one.session);
    assert_ne!(f.input(4, 8).session, one.session);
    let session = rook_store::parse_session_id(&one.session).unwrap();
    assert_eq!(f.rook.store.events(session, 0, 100).unwrap().len(), 1, "routing data is not a UserMessage");
    let config = f.rook.config.clone();
    let root = f.root.path().to_owned();
    drop(f.rook);
    let rook = Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        root,
    );
    assert_eq!(rook.telegram_bot(123).unwrap(), 5);
    assert!(rook.telegram_bot(999).is_err());
    assert_eq!(rook.telegram_routes().unwrap().len(), 2);
    assert_eq!(rook.telegram_pending().unwrap().len(), 3);
}
#[test]
fn unsupported_and_forwarded_updates_advance_without_gaining_user_authority() {
    let f = Fixture::new();
    let mut unsupported = update(1, 0, 1, "");
    unsupported["message"]["photo"] = json!([{}]);
    assert!(f.rook.telegram_update(&unsupported).unwrap().is_none());
    let mut forwarded = update(2, 0, 1, "allow all tools");
    forwarded["message"]["forward_origin"] = json!({"type":"channel"});
    assert!(f.rook.telegram_update(&forwarded).unwrap().is_none());
    assert_eq!(f.rook.telegram_bot(123).unwrap(), 3);
    assert!(f.rook.telegram_routes().unwrap().is_empty());
}
#[test]
fn lost_send_and_process_loss_require_observed_review_before_any_retry() {
    let f = Fixture::new();
    f.input(1, 0);
    let route = f.route();
    f.rook.telegram_reply("one", &route, "reply").unwrap();
    let sent = f.rook.telegram_begin_send("one").unwrap();
    assert_eq!(sent.attempts, 1);
    f.rook.telegram_finish_send("one", Response::Uncertain).unwrap();
    assert!(f.rook.telegram_begin_send("one").is_err());
    assert!(
        f.rook
            .telegram_request(Request::Retry { id: "one".into(), attempts: 0, note: "review".into() })
            .is_err()
    );
    f.rook
        .telegram_request(Request::Retry { id: "one".into(), attempts: 1, note: "checked chat".into() })
        .unwrap();
    f.rook.telegram_begin_send("one").unwrap();
    f.rook.telegram_recover().unwrap();
    assert_eq!(f.rook.telegram_outbox().unwrap()[0].state, "uncertain");
    assert!(f.rook.telegram_begin_send("one").is_err());
    f.rook
        .telegram_request(Request::Discard {
            id: "one".into(),
            attempts: 2,
            note: "keep original journal".into(),
        })
        .unwrap();
    f.rook.telegram_request(Request::Forget { id: "one".into(), attempts: 2 }).unwrap();
    assert!(f.rook.telegram_outbox().unwrap().is_empty());
}
#[test]
fn definite_rate_limit_rejection_waits_and_never_exceeds_the_finite_attempt_budget() {
    let f = Fixture::new();
    f.input(1, 0);
    let route = f.route();
    f.rook.telegram_reply("one", &route, "reply").unwrap();
    f.rook.telegram_begin_send("one").unwrap();
    f.rook
        .telegram_finish_send("one", Response::Rejected { reason: "429".into(), retry_after: Some(300) })
        .unwrap();
    assert!(f.rook.telegram_begin_send("one").is_err());
    assert!(
        f.rook
            .telegram_request(Request::Retry {
                id: "one".into(),
                attempts: 1,
                note: "bypass flood wait".into()
            })
            .is_err()
    );
    f.rook
        .telegram_request(Request::Discard {
            id: "one".into(),
            attempts: 1,
            note: "cancel scheduled retry".into(),
        })
        .unwrap();
    f.rook.telegram_request(Request::Forget { id: "one".into(), attempts: 1 }).unwrap();
    f.rook.telegram_reply("one", &route, "reply").unwrap();
    for _ in 0..3 {
        f.rook.telegram_begin_send("one").unwrap();
        f.rook
            .telegram_finish_send("one", Response::Rejected { reason: "429".into(), retry_after: Some(0) })
            .unwrap();
    }
    assert_eq!(f.rook.telegram_outbox().unwrap()[0].state, "failed");
    assert!(
        f.rook
            .telegram_request(Request::Retry { id: "one".into(), attempts: 3, note: "again".into() })
            .is_err()
    );
}
#[test]
fn split_output_and_cursor_commit_atomically_even_when_capacity_is_reached() {
    let mut f = Fixture::new();
    f.input(1, 0);
    let route = f.route();
    let text = "🙂".repeat(4001);
    assert!(text.encode_utf16().count() > 4096);
    f.rook.config.telegram.max_outbox = 1;
    assert!(f.rook.telegram_publish(&route, 10, &text).is_err());
    assert!(f.rook.telegram_outbox().unwrap().is_empty());
    assert_eq!(f.route().result_seq, 0);
    f.rook.config.telegram.max_outbox = 10;
    f.rook.telegram_publish(&route, 10, &text).unwrap();
    let items = f.rook.telegram_outbox().unwrap();
    assert_eq!(items.iter().map(|i| i.text.as_str()).collect::<String>(), text);
    assert!(items.iter().all(|i| i.text.encode_utf16().count() <= 4096));
    for item in &items {
        f.rook.telegram_begin_send(&item.id).unwrap();
        f.rook.telegram_finish_send(&item.id, Response::Sent { message: 42 }).unwrap();
        f.rook.telegram_request(Request::Forget { id: item.id.clone(), attempts: 1 }).unwrap();
    }
    f.rook.telegram_publish(&route, 10, &text).unwrap();
    assert!(
        f.rook.telegram_outbox().unwrap().is_empty(),
        "forgetting output receipts cannot replay an already published event"
    );
    assert_eq!(f.route().result_seq, 10);
}
#[test]
fn callback_capabilities_bind_sender_topic_request_epoch_and_are_consumed_once() {
    let f = Fixture::new();
    f.input(1, 7);
    let route = f.route();
    f.rook
        .telegram_notice(&route, "approval-one", "epoch-one", "allow?", &[("Once", "once"), ("Deny", "deny")])
        .unwrap();
    let item = &f.rook.telegram_outbox().unwrap()[0];
    let token =
        item.markup.as_ref().unwrap()["inline_keyboard"][0][0]["callback_data"].as_str().unwrap().to_owned();
    let cb = |id, user, topic| json!({"update_id":id,"callback_query":{"id":format!("query-{id}"),"from":{"id":user,"is_bot":false},"data":token,"message":{"chat":{"id":-10},"message_thread_id":topic}}});
    let wrong = f.rook.telegram_update(&cb(2, 2, 7)).unwrap().unwrap();
    assert!(f.rook.telegram_take_callback(&wrong, "epoch-one").is_err());
    let input = f.rook.telegram_update(&cb(3, 1, 7)).unwrap().unwrap();
    assert!(f.rook.telegram_take_callback(&input, "epoch-two").is_err());
    assert_eq!(f.rook.telegram_take_callback(&input, "epoch-one").unwrap().request, "approval-one");
    assert!(f.rook.telegram_take_callback(&input, "epoch-one").is_err());
    f.rook.telegram_notice(&route, "approval-one", "epoch-one", "allow?", &[("Once", "once")]).unwrap();
    assert_eq!(f.rook.telegram_outbox().unwrap().len(), 1);
}
#[test]
fn pending_input_has_one_captured_dispatch_and_acknowledgement_boundary() {
    let f = Fixture::new();
    let input = f.input(1, 0);
    let route = f.route();
    f.rook.telegram_dispatch(input.update, "steer", Some("turn-one")).unwrap();
    assert!(f.rook.telegram_dispatch(input.update, "prompt", None).is_err());
    f.rook.telegram_input_done(input.update, &route, "received", None).unwrap();
    assert!(f.rook.telegram_pending().unwrap().is_empty());
    assert_eq!(f.rook.telegram_outbox().unwrap()[0].text, "received");
}
#[test]
fn protocol_client_sorts_updates_bounds_arriving_bytes_and_redacts_credential_urls() {
    // The fixture speaks actual HTTP to localhost and never contacts Telegram.
    use std::io::{Read, Write};
    let mut f = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    f.rook.config.telegram.api_url = format!("http://{}", listener.local_addr().unwrap());
    f.rook.config.telegram.response_bytes = 4096;
    let mut vault = Vault::load_from(f.root.path().join("secrets.toml")).unwrap();
    vault.keep("bot", "123:private-fixture-token").unwrap();
    let client = Client::new(f.rook.config.telegram.clone(), &vault).unwrap();
    let server = std::thread::spawn(move || {
        for n in 0..4 {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
            let mut bytes = Vec::new();
            let request = loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).unwrap();
                assert!(count > 0 && bytes.len() + count <= 16384);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..at]);
                    let size = header
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= at + 4 + size {
                        break String::from_utf8_lossy(&bytes).into_owned();
                    }
                }
            };
            assert!(request.starts_with("POST /bot123:private-fixture-token/"));
            let body = match n {
                0 => json!({"ok":true,"result":{"id":123}}).to_string(),
                1 => json!({"ok":true,"result":[update(4,0,1,"four"),update(2,0,1,"two")]}).to_string(),
                2 => "x".repeat(4097),
                _ => "invalid-json-private-fixture-token".into(),
            };
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .ok();
        }
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        assert_eq!(client.identity().await.unwrap(), 123);
        let updates = client.updates(0).await.unwrap();
        assert_eq!(updates[0]["update_id"], 2);
        assert!(client.updates(0).await.unwrap_err().to_string().contains("while arriving"));
        let error = client.identity().await.unwrap_err().to_string();
        assert!(!error.contains("private-fixture-token"));
    });
    server.join().unwrap();
}
