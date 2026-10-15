use rook_mcp::{
    Server, ServerConfig,
    elicitation::{ElicitationAction, ElicitationOrigin, ElicitationResponse, Elicitor, Interaction},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}
fn config(mode: &str) -> ServerConfig {
    ServerConfig {
        name: mode.into(),
        command: env!("CARGO_BIN_EXE_mcp-mock").into(),
        args: vec![mode.into()],
        startup_timeout_secs: 5,
        call_timeout_secs: 5,
        ..Default::default()
    }
}
struct Person {
    action: ElicitationAction,
    origins: Mutex<Vec<ElicitationOrigin>>,
}
#[async_trait::async_trait]
impl Elicitor for Person {
    async fn elicit(
        &self,
        origin: &ElicitationOrigin,
        params: &Value,
    ) -> std::result::Result<ElicitationResponse, String> {
        assert!(params["message"].is_string());
        self.origins.lock().unwrap().push(origin.clone());
        Ok(ElicitationResponse { action: self.action, content: Some(json!({"name":"a private answer"})) })
    }
}

#[tokio::test]
async fn server_requests_with_colliding_numeric_and_string_ids_do_not_consume_client_responses() {
    let _serial = serial().await;
    for mode in ["elicitation", "elicitation-string"] {
        let server = Server::connect(&config(mode), &Default::default()).await.unwrap();
        let person = Arc::new(Person { action: ElicitationAction::Accept, origins: Mutex::new(Vec::new()) });
        let answer = server
            .call_tool_interactive(
                "collect",
                &json!({}),
                &Interaction { elicitor: person.clone(), trace: None },
            )
            .await
            .unwrap();
        assert_eq!(answer.to_text(), "accept");
        {
            let origins = person.origins.lock().unwrap();
            assert_eq!(origins.len(), 1);
            assert_eq!(origins[0].server, mode);
            assert_eq!(origins[0].method, "tools/call");
        }
        server.shutdown().await;
    }
}

#[tokio::test]
async fn decline_strips_accidental_values_and_missing_user_input_cancels() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("response.json");
    let mut config = config("elicitation");
    config.env.insert("MCP_MOCK_RECEIPT".into(), path.display().to_string());
    let server = Server::connect(&config, &Default::default()).await.unwrap();
    let person = Arc::new(Person { action: ElicitationAction::Decline, origins: Mutex::new(Vec::new()) });
    assert_eq!(
        server
            .call_tool_interactive("collect", &json!({}), &Interaction { elicitor: person, trace: None })
            .await
            .unwrap()
            .to_text(),
        "decline"
    );
    let response: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(response["result"].get("content").is_none());
    assert!(!response.to_string().contains("a private answer"));
    assert_eq!(server.call_tool("collect", &json!({})).await.unwrap().to_text(), "cancel");
    server.shutdown().await;
}

struct Waiting {
    entered: tokio::sync::Notify,
    dropped: Arc<std::sync::atomic::AtomicBool>,
    connection: std::sync::atomic::AtomicU64,
}
#[async_trait::async_trait]
impl Elicitor for Waiting {
    async fn elicit(
        &self,
        origin: &ElicitationOrigin,
        _: &Value,
    ) -> std::result::Result<ElicitationResponse, String> {
        struct End(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for End {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        let _end = End(self.dropped.clone());
        self.connection.store(origin.connection, std::sync::atomic::Ordering::Release);
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancelled_invocations_drop_their_dialogs_and_a_new_owner_gets_a_new_connection() {
    let _serial = serial().await;
    let server = Arc::new(Server::connect(&config("elicitation"), &Default::default()).await.unwrap());
    let waiting = Arc::new(Waiting {
        entered: Default::default(),
        dropped: Default::default(),
        connection: Default::default(),
    });
    let turn = tokio::spawn({
        let server = server.clone();
        let waiting = waiting.clone();
        async move {
            server
                .call_tool_interactive("collect", &json!({}), &Interaction { elicitor: waiting, trace: None })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), waiting.entered.notified()).await.unwrap();
    turn.abort();
    assert!(turn.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !waiting.dropped.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let person = Arc::new(Person { action: ElicitationAction::Accept, origins: Mutex::new(Vec::new()) });
    assert_eq!(
        server
            .call_tool_interactive(
                "collect",
                &json!({}),
                &Interaction { elicitor: person.clone(), trace: None }
            )
            .await
            .unwrap()
            .to_text(),
        "accept"
    );
    assert_ne!(
        person.origins.lock().unwrap()[0].connection,
        waiting.connection.load(std::sync::atomic::Ordering::Acquire)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn duplicate_and_flooded_server_requests_retire_the_connection_without_ambiguous_answers() {
    let _serial = serial().await;
    for mode in ["elicitation-duplicate", "elicitation-flood"] {
        let server = Server::connect(&config(mode), &Default::default()).await.unwrap();
        let waiting = Arc::new(Waiting {
            entered: Default::default(),
            dropped: Default::default(),
            connection: Default::default(),
        });
        assert!(
            server
                .call_tool_interactive("collect", &json!({}), &Interaction { elicitor: waiting, trace: None })
                .await
                .is_err()
        );
        server.shutdown().await;
    }
}

struct HttpFixture {
    config: ServerConfig,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl HttpFixture {
    async fn new() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = ServerConfig {
            name: "http-forms".into(),
            url: Some(format!("http://{}/mcp", listener.local_addr().unwrap())),
            call_timeout_secs: 10,
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            type Replies = Arc<Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<Value>>>>;
            let replies: Replies = Default::default();
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let (mut socket,_)=accepted.unwrap();let replies=replies.clone();
                        connections.spawn(async move {
                            let mut raw=Vec::new();
                            let request:Value=loop {
                                let mut buf=[0;2048];let n=socket.read(&mut buf).await.unwrap();
                                if n==0 {return;}
                                assert!(raw.len()+n<=65536);raw.extend_from_slice(&buf[..n]);
                                let Some(split)=raw.windows(4).position(|w|w==b"\r\n\r\n") else {continue};
                                let head=String::from_utf8_lossy(&raw[..split]);
                                let len:usize=head.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").map(|n|n.trim().parse().unwrap())).unwrap_or(0);
                                if raw.len()<split+4+len {continue;}
                                break serde_json::from_slice(&raw[split+4..split+4+len]).unwrap();
                            };
                            if request.get("method").is_none() {
                                let key=request["id"].to_string();
                                let waiting=replies.lock().unwrap().remove(&key);
                                if let Some(waiting)=waiting {let _=waiting.send(request["result"].clone());}
                                socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();return;
                            }
                            let id=&request["id"];
                            if id.is_null() {socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();return;}
                            let method=request["method"].as_str().unwrap();
                            if method=="tools/call" || method=="resources/read" {
                                // Deliberately collide server and client numeric IDs.
                                let server_id=id.clone();let (tx,rx)=tokio::sync::oneshot::channel();
                                assert!(replies.lock().unwrap().insert(server_id.to_string(),tx).is_none());
                                let message=json!({"jsonrpc":"2.0","id":server_id,"method":"elicitation/create","params":{"message":"Name","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}});
                                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {message}\n\n").as_bytes()).await.unwrap();
                                let answer=tokio::time::timeout(Duration::from_secs(15),rx).await.unwrap().unwrap();
                                let text=answer["content"]["name"].as_str().unwrap_or_else(||answer["action"].as_str().unwrap());
                                let result=if method=="resources/read" {json!({"contents":[{"uri":"doc:one","text":text}]})} else {json!({"content":[{"type":"text","text":text}]})};
                                let end=json!({"jsonrpc":"2.0","id":id,"result":result});
                                let _=socket.write_all(format!("data: {end}\n\n").as_bytes()).await;return;
                            }
                            let result=match method {
                                "initialize"=>{assert!(request["params"]["capabilities"]["elicitation"]["form"].is_object());assert!(request["params"]["capabilities"]["elicitation"]["url"].is_object());json!({"protocolVersion":"2025-11-25","serverInfo":{"name":"forms","version":"1"},"capabilities":{"tools":{},"resources":{}}})},
                                "tools/list"=>json!({"tools":[{"name":"collect","description":"Collect","inputSchema":{"type":"object"}}]}),
                                _=>panic!("unexpected request {method}"),
                            };
                            let body=json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                        });
                    }
                    Some(result)=connections.join_next(),if !connections.is_empty()=>{result.unwrap();}
                }
            }
        });
        Self { config, task }
    }
}
struct Named(&'static str, Arc<tokio::sync::Barrier>);
#[async_trait::async_trait]
impl Elicitor for Named {
    async fn elicit(
        &self,
        _: &ElicitationOrigin,
        _: &Value,
    ) -> std::result::Result<ElicitationResponse, String> {
        self.1.wait().await;
        Ok(ElicitationResponse { action: ElicitationAction::Accept, content: Some(json!({"name":self.0})) })
    }
}
#[tokio::test]
async fn concurrent_http_streams_keep_each_answer_in_its_own_call_even_with_colliding_ids() {
    let fixture = HttpFixture::new().await;
    let server = Server::connect(&fixture.config, &Default::default()).await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let one = Interaction { elicitor: Arc::new(Named("first-owner", barrier.clone())), trace: None };
    let two = Interaction { elicitor: Arc::new(Named("second-owner", barrier)), trace: None };
    let arguments = json!({});
    let pair = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            server.call_tool_interactive("collect", &arguments, &one),
            server.read_resource_interactive("doc:one", &two)
        )
    })
    .await
    .unwrap();
    assert_eq!(pair.0.unwrap().to_text(), "first-owner");
    assert_eq!(pair.1.unwrap().contents[0].text.as_deref(), Some("second-owner"));
    server.shutdown().await;
}
