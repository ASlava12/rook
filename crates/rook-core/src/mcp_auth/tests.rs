#![cfg(test)]

use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Default)]
struct Behavior {
    fallback: bool,
    oidc: u8,
    wrong_issuer: bool,
    wrong_resource: bool,
    no_pkce: bool,
    huge_metadata: bool,
    deny_catalog: bool,
    metadata_down: bool,
}
struct Seen {
    path: String,
    headers: String,
    body: String,
}
struct Fixture {
    config: ServerConfig,
    store: Store,
    root: tempfile::TempDir,
    requests: Arc<Mutex<Vec<Seen>>>,
    behavior: Arc<Mutex<Behavior>>,
    challenge: Arc<Mutex<String>>,
    refreshes: Arc<AtomicUsize>,
    drop_refresh: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let root = tempfile::tempdir().unwrap();
        let store = Store { directory: root.path().join("auth"), limits: Settings::default() };
        let config = ServerConfig {
            name: "private".into(),
            url: Some(format!("{base}/mcp")),
            oauth: rook_mcp::oauth::OAuthConfig { client_id: "public-rook".into(), ..Default::default() },
            ..Default::default()
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let behavior = Arc::new(Mutex::new(Behavior::default()));
        let challenge = Arc::new(Mutex::new(String::new()));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let drop_refresh = Arc::new(AtomicBool::new(false));
        let (seen, flags, expected, refreshed, drop_reply) =
            (requests.clone(), behavior.clone(), challenge.clone(), refreshes.clone(), drop_refresh.clone());
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let (base, seen, flags, expected, refreshed, drop_reply) = (base.clone(), seen.clone(), flags.clone(), expected.clone(), refreshed.clone(), drop_reply.clone());
                        clients.spawn(async move {
                            let mut bytes = Vec::new(); let mut chunk = [0;4096];
                            let (end, length) = loop {
                                let n = socket.read(&mut chunk).await.unwrap(); if n == 0 { return; }
                                assert!(n <= 65536usize.saturating_sub(bytes.len())); bytes.extend_from_slice(&chunk[..n]);
                                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let headers = String::from_utf8_lossy(&bytes[..end]);
                                    let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                                    break (end+4,length);
                                }
                            };
                            while bytes.len() < end+length {
                                let n=socket.read(&mut chunk).await.unwrap(); if n==0 {return;}
                                assert!(n <= 65536usize.saturating_sub(bytes.len())); bytes.extend_from_slice(&chunk[..n]);
                            }
                            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                            let path = headers.lines().next().unwrap().split(' ').nth(1).unwrap().to_string();
                            let body = String::from_utf8(bytes[end..end+length].to_vec()).unwrap();
                            { let mut seen = seen.lock().unwrap(); assert!(seen.len()<256); seen.push(Seen { path: path.clone(), headers: headers.clone(), body: body.clone() }); }
                            let (status, extra, answer) = {
                                let flags = flags.lock().unwrap();
                                let resource = || json!({"resource":format!("{base}/{}",if flags.wrong_resource {"other"} else {"mcp"}),"authorization_servers":[format!("{base}/issuer/tenant")],"scopes_supported":["fallback"]});
                                let metadata = || json!({"issuer":format!("{base}/{}",if flags.wrong_issuer {"wrong"} else {"issuer/tenant"}),
                                    "authorization_endpoint":format!("{base}/authorize"), "token_endpoint":format!("{base}/token"),
                                    "registration_endpoint":format!("{base}/register"), "token_endpoint_auth_methods_supported":["none"],
                                    "authorization_response_iss_parameter_supported":true,
                                    "code_challenge_methods_supported":if flags.no_pkce {vec!["plain"]} else {vec!["S256"]}});
                                match path.as_str() {
                                    "/mcp" => {
                                        if !headers.to_ascii_lowercase().contains("authorization: bearer access-") {
                                            let challenge = if flags.fallback { String::new() } else { format!("WWW-Authenticate: Basic realm=\"other\", Bearer resource_metadata=\"{base}/resource\", scope=\"files:read\"\r\n") };
                                            (401, challenge, json!({"error":"auth required"}).to_string())
                                        } else {
                                            let request: Value = serde_json::from_str(&body).unwrap();
                                            let result = match request["method"].as_str().unwrap() {
                                                "initialize" => json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
                                                "notifications/initialized" => json!({}),
                                                "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"},"description":"test"}]}),
                                                "tools/call" => json!({"content":[{"type":"text","text":"worked"}]}),
                                                other => panic!("unexpected method {other}"),
                                            };
                                            if request.get("id").is_none() { (202,String::new(),String::new()) }
                                            else if flags.deny_catalog && request["method"]=="tools/list" { (200,String::new(),json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-1,"message":"SECRET_TOKEN_ECHO"}}).to_string()) }
                                            else { (200,String::new(),json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string()) }
                                        }
                                    },
                                    "/resource" | "/.well-known/oauth-protected-resource" => {
                                        if flags.metadata_down { (503,String::new(),"unavailable".into()) }
                                        else if flags.huge_metadata { (200,String::new(),"x".repeat(8192)) }
                                        else { (200,String::new(),resource().to_string()) }
                                    },
                                    "/.well-known/oauth-protected-resource/mcp" => (404,String::new(),String::new()),
                                    "/.well-known/oauth-authorization-server/issuer/tenant" if flags.oidc == 0 => (200,String::new(),metadata().to_string()),
                                    "/.well-known/openid-configuration/issuer/tenant" if flags.oidc == 1 => (200,String::new(),metadata().to_string()),
                                    "/issuer/tenant/.well-known/openid-configuration" if flags.oidc == 2 => (200,String::new(),metadata().to_string()),
                                    "/register" => {
                                        let request: Value = serde_json::from_str(&body).unwrap();
                                        assert_eq!(request["token_endpoint_auth_method"],"none");
                                        assert_eq!(request["response_types"],json!(["code"]));
                                        (201,String::new(),json!({"client_id":"dynamic-rook","token_endpoint_auth_method":"none"}).to_string())
                                    },
                                    "/token" => {
                                        let fields = pairs(&body);
                                        assert_eq!(fields["resource"],format!("{base}/mcp"));
                                        let refresh = fields["grant_type"]=="refresh_token";
                                        if refresh {
                                            refreshed.fetch_add(1,Ordering::SeqCst);
                                            assert_eq!(fields["refresh_token"],"refresh-one");
                                            if drop_reply.load(Ordering::SeqCst) { return; }
                                        } else {
                                            assert_eq!(fields["code"],"code-one");
                                            assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(fields["code_verifier"].as_bytes())),*expected.lock().unwrap());
                                        }
                                        (200,String::new(),json!({"access_token":if refresh {"access-two"} else {"access-one"},"refresh_token":if refresh {"refresh-two"} else {"refresh-one"},"token_type":"Bearer","expires_in":3600,"scope":"files:read"}).to_string())
                                    },
                                    _ => (404,String::new(),String::new()),
                                }
                            };
                            let response = format!("HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{answer}", answer.len());
                            let _=socket.write_all(response.as_bytes()).await;
                        });
                    },
                    Some(result) = clients.join_next() => { result.unwrap(); },
                }
            }
        });
        Self { config, store, root, requests, behavior, challenge, refreshes, drop_refresh, task }
    }
    async fn authorize(&self) -> std::result::Result<(), &'static str> {
        let expected = self.challenge.clone();
        let issuer = self.config.url.as_deref().unwrap().replace("/mcp", "/issuer/tenant");
        let (send, receive) = tokio::sync::oneshot::channel::<String>();
        let browser = tokio::spawn(async move {
            let url = Url::parse(&receive.await.unwrap()).unwrap();
            let fields: BTreeMap<_, _> =
                url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
            assert_eq!(fields["code_challenge_method"], "S256");
            assert_eq!(fields["scope"], "files:read");
            assert_ne!(fields["state"], fields["code_challenge"]);
            *expected.lock().unwrap() = fields["code_challenge"].clone();
            let mut callback = Url::parse(&fields["redirect_uri"]).unwrap();
            callback
                .query_pairs_mut()
                .append_pair("code", "code-one")
                .append_pair("state", &fields["state"])
                .append_pair("iss", &issuer);
            reqwest::Client::new().get(callback).send().await.unwrap().status()
        });
        let result = login_at(&self.config, &rook_llm::Proxy::default(), self.store.clone(), |url| {
            let _ = send.send(url.into());
        })
        .await;
        let status = browser.await.unwrap();
        assert_eq!(status.is_success(), result.is_ok());
        result
    }
    fn expire(&self) {
        let mut entries = self.store.read().unwrap();
        entries[0].grant.expires_at = Some(1);
        self.store.write(&entries).unwrap();
    }
    fn source(&self) -> Arc<dyn TokenSource> {
        credentials_from(&self.config, &rook_llm::Proxy::default(), self.store.clone()).unwrap().unwrap()
    }
}
fn pairs(body: &str) -> BTreeMap<String, String> {
    let mut url = Url::parse("http://127.0.0.1/").unwrap();
    url.set_query(Some(body));
    url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

#[tokio::test]
async fn native_login_uses_pkce_then_checks_the_full_catalog_and_keeps_tokens_private() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    let entries = f.store.read().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].grant.access_token, "access-one");
    assert_eq!(f.source().token().await.unwrap(), "access-one");
    let contents = std::fs::read_to_string(f.store.directory.join("credentials.json")).unwrap();
    assert!(contents.contains("refresh-one"));
    assert!(!contents.contains("code_verifier"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.store.directory.join("credentials.json")).unwrap().permissions().mode()
                & 0o777,
            0o600
        );
        assert_eq!(std::fs::metadata(&f.store.directory).unwrap().permissions().mode() & 0o777, 0o700);
    }
    for request in f.requests.lock().unwrap().iter().filter(|r| r.path != "/mcp") {
        assert!(!request.headers.to_lowercase().contains("authorization:"));
        assert!(!request.headers.contains("access-one"));
    }
    let versions: Vec<_> = f
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.path == "/mcp")
        .filter_map(|r| {
            let body: Value = serde_json::from_str(&r.body).unwrap();
            (body["method"] == "initialize").then(|| body["params"]["protocolVersion"].clone())
        })
        .collect();
    assert!(versions.len() >= 2, "both discovery and authenticated handshake were exercised");
    assert!(versions.iter().all(|v| v == rook_mcp::PROTOCOL_VERSION));
    assert!(f.root.path().is_dir());
}

#[tokio::test]
async fn concurrent_refresh_rotates_once_and_updates_the_live_http_connection() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    f.expire();
    let first = f.source();
    let second = f.source();
    let (a, b) = tokio::join!(first.token(), second.token());
    assert_eq!(a.unwrap(), "access-two");
    assert_eq!(b.unwrap(), "access-two");
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(f.store.read().unwrap()[0].grant.refresh_token.as_deref(), Some("refresh-two"));
    let server = rook_mcp::Server::connect_with_token(&f.config, &rook_llm::Proxy::default(), Some(first))
        .await
        .unwrap();
    assert_eq!(server.call_tool("echo", &json!({})).await.unwrap().to_text(), "worked");
    assert!(
        f.requests.lock().unwrap().iter().any(
            |r| r.path == "/mcp" && r.headers.to_lowercase().contains("authorization: bearer access-two")
        )
    );
}

#[tokio::test]
async fn lost_refresh_response_leaves_a_durable_marker_and_is_never_replayed() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    f.expire();
    f.drop_refresh.store(true, Ordering::SeqCst);
    assert!(f.source().token().await.is_err());
    assert!(f.store.read().unwrap()[0].refreshing);
    f.drop_refresh.store(false, Ordering::SeqCst);
    assert!(f.source().token().await.unwrap_err().contains("previous OAuth refresh"));
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn discovery_outage_does_not_consume_a_refresh_token_or_require_a_new_login() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    f.expire();
    f.behavior.lock().unwrap().metadata_down = true;
    assert!(f.source().token().await.is_err());
    assert!(!f.store.read().unwrap()[0].refreshing);
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 0);
    f.behavior.lock().unwrap().metadata_down = false;
    assert_eq!(f.source().token().await.unwrap(), "access-two");
}

#[tokio::test]
async fn account_replacement_does_not_change_an_existing_turns_identity() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    let old = f.source();
    let mut grant = f.store.read().unwrap()[0].grant.clone();
    grant.access_token = "access-new-account".into();
    f.store.save(&f.config, grant).await.unwrap();
    assert_eq!(old.token().await.unwrap(), "access-one");
    assert_eq!(f.source().token().await.unwrap(), "access-new-account");
    f.store.write(&[]).unwrap();
    assert!(old.token().await.unwrap_err().contains("removed"));
}

#[tokio::test]
async fn refused_catalog_preserves_previously_stored_credentials_and_hides_error_bodies() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    let before = std::fs::read(f.store.directory.join("credentials.json")).unwrap();
    f.behavior.lock().unwrap().deny_catalog = true;
    let error = f.authorize().await.unwrap_err();
    assert!(error.contains("catalog"));
    assert!(!error.contains("SECRET"));
    assert_eq!(before, std::fs::read(f.store.directory.join("credentials.json")).unwrap());
}

#[tokio::test]
async fn discovery_supports_resource_root_fallback_and_all_oidc_locations() {
    for mode in [1, 2] {
        let f = Fixture::new().await;
        {
            let mut flags = f.behavior.lock().unwrap();
            flags.fallback = true;
            flags.oidc = mode;
        }
        let proxy = rook_llm::Proxy::default();
        let found = Client::new(&f.config, &proxy).unwrap().discover().await.unwrap();
        assert_eq!(found.scopes, ["fallback"]);
        let requests = f.requests.lock().unwrap();
        let paths: Vec<_> = requests.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            &paths[..4],
            [
                "/mcp",
                "/.well-known/oauth-protected-resource/mcp",
                "/.well-known/oauth-protected-resource",
                "/.well-known/oauth-authorization-server/issuer/tenant"
            ]
        );
        assert!(paths.contains(&"/.well-known/openid-configuration/issuer/tenant"));
        if mode == 2 {
            assert!(paths.contains(&"/issuer/tenant/.well-known/openid-configuration"));
        }
    }
}

#[tokio::test]
async fn discovery_rejects_wrong_issuer_wrong_resource_missing_pkce_and_oversize_metadata() {
    for mode in 0..4 {
        let mut f = Fixture::new().await;
        f.config.oauth.max_response_bytes = 4096;
        {
            let mut flags = f.behavior.lock().unwrap();
            match mode {
                0 => flags.wrong_issuer = true,
                1 => flags.wrong_resource = true,
                2 => flags.no_pkce = true,
                _ => flags.huge_metadata = true,
            }
        }
        let proxy = rook_llm::Proxy::default();
        let result = Client::new(&f.config, &proxy).unwrap().discover().await;
        let error = result.err().unwrap();
        assert!(
            error.contains(match mode {
                0 => "issuer",
                1 => "resource",
                2 => "S256",
                _ => "max_response_bytes",
            }),
            "{error}"
        );
        if mode == 3 {
            assert!(8192 > f.config.oauth.max_response_bytes);
        }
        assert!(!f.requests.lock().unwrap().iter().any(|r| r.path == "/token"));
    }
}

#[tokio::test]
async fn invalid_callback_state_or_issuer_never_reaches_the_token_endpoint() {
    let f = Fixture::new().await;
    let proxy = rook_llm::Proxy::default();
    let client = Client::new(&f.config, &proxy).unwrap();
    for mode in 0..4 {
        let (pending, url) = client.prepare("http://127.0.0.1:9999/callback".into()).await.unwrap();
        let fields: BTreeMap<_, _> =
            Url::parse(&url).unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let mut callback = Url::parse(&fields["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("code", "code-one")
            .append_pair("state", if mode == 0 { "wrong" } else { &fields["state"] });
        if mode != 1 {
            callback.query_pairs_mut().append_pair(
                "iss",
                if mode == 2 { "https://wrong.example" } else { &pending.discovery.issuer.issuer },
            );
        }
        if mode == 3 {
            callback.query_pairs_mut().append_pair("state", &fields["state"]);
        }
        assert!(client.exchange(pending, callback.as_str()).await.is_err());
    }
    assert!(!f.requests.lock().unwrap().iter().any(|r| r.path == "/token"));
}

#[tokio::test]
async fn empty_client_id_registers_a_public_client_before_opening_authorization() {
    let mut f = Fixture::new().await;
    f.config.oauth.client_id.clear();
    f.authorize().await.unwrap();
    assert_eq!(f.store.read().unwrap()[0].grant.client_id, "dynamic-rook");
    let requests = f.requests.lock().unwrap();
    let registration = requests.iter().find(|r| r.path == "/register").unwrap();
    let body: Value = serde_json::from_str(&registration.body).unwrap();
    assert_eq!(body["grant_types"], json!(["authorization_code", "refresh_token"]));
}

#[tokio::test]
async fn credential_entry_and_byte_limits_fail_without_overwriting_a_previous_grant() {
    let mut f = Fixture::new().await;
    f.authorize().await.unwrap();
    let grant = f.store.read().unwrap()[0].grant.clone();
    let mut another = f.config.clone();
    another.name = "another".into();
    f.store.limits.oauth_max_entries = 1;
    assert_eq!(f.store.read().unwrap().len(), f.store.limits.oauth_max_entries);
    let before = std::fs::read(f.store.directory.join("credentials.json")).unwrap();
    assert!(f.store.save(&another, grant.clone()).await.unwrap_err().contains("full"));
    assert_eq!(before, std::fs::read(f.store.directory.join("credentials.json")).unwrap());
    f.store.limits.oauth_max_entries = 2;
    f.store.limits.oauth_max_bytes = 65536;
    let mut large = grant;
    large.access_token = "a".repeat(16384);
    large.refresh_token = Some("r".repeat(16384));
    f.store.save(&f.config, large.clone()).await.unwrap();
    let before = std::fs::read(f.store.directory.join("credentials.json")).unwrap();
    assert!(before.len() * 2 > f.store.limits.oauth_max_bytes, "fixture must cross the byte cap");
    assert!(f.store.save(&another, large).await.unwrap_err().contains("oauth_max_bytes"));
    assert_eq!(before, std::fs::read(f.store.directory.join("credentials.json")).unwrap());
}

#[tokio::test]
async fn a_changed_issuer_is_rejected_before_sending_the_stored_refresh_token() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    f.expire();
    let mut entries = f.store.read().unwrap();
    entries[0].grant.issuer = "https://previous-issuer.example".into();
    f.store.write(&entries).unwrap();
    let error = f.source().token().await.unwrap_err();
    assert!(error.contains("issuer/resource changed"));
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 0);
    assert!(!f.store.read().unwrap()[0].refreshing);
}

#[test]
fn oauth_urls_and_configuration_refuse_downgrades_and_unbounded_values() {
    for bad in [
        "http://public.example/mcp",
        "https://user:password@example.com/mcp",
        "https://example.com/mcp#fragment",
        "file:///etc/passwd",
    ] {
        assert!(protocol::safe_url(bad).is_err());
    }
    let mut config = ServerConfig { url: Some("https://public.example/mcp".into()), ..Default::default() };
    config.oauth.max_response_bytes = usize::MAX;
    assert!(Client::new(&config, &rook_llm::Proxy::default()).is_err());
}

#[tokio::test]
async fn browser_wait_expires_and_closes_the_native_callback_without_storing_credentials() {
    let mut f = Fixture::new().await;
    f.config.oauth.login_timeout_secs = 30;
    let config = f.config.clone();
    let store = f.store.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    let work = tokio::spawn(async move {
        login_at(&config, &rook_llm::Proxy::default(), store, |url| {
            let _ = send.send(url.to_owned());
        })
        .await
    });
    let url = receive.await.unwrap();
    let parsed = Url::parse(&url).unwrap();
    let redirect = parsed.query_pairs().find(|(k, _)| k == "redirect_uri").unwrap().1.into_owned();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    assert!(work.await.unwrap().unwrap_err().contains("expired"));
    tokio::time::resume();
    assert!(reqwest::Client::new().get(redirect).send().await.is_err());
    assert!(f.store.read().unwrap().is_empty());
}

#[tokio::test]
async fn a_short_lived_access_token_without_refresh_remains_usable_until_its_expiry() {
    let f = Fixture::new().await;
    f.authorize().await.unwrap();
    let mut entries = f.store.read().unwrap();
    let issued = now();
    entries[0].grant.expires_at = Some(issued + 15);
    entries[0].grant.refresh_token = None;
    assert!(entries[0].grant.usable(issued + 14));
    assert!(!entries[0].grant.usable(issued + 15));
    f.store.write(&entries).unwrap();
    assert_eq!(f.source().token().await.unwrap(), "access-one");
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 0);
    f.expire();
    assert!(f.source().token().await.is_err());
}

#[tokio::test]
async fn a_frontend_owned_callback_expires_without_exchanging_or_saving_a_grant() {
    let f = Fixture::new().await;
    let proxy = rook_llm::Proxy::default();
    let login = Login::begin_at(
        &f.config,
        &proxy,
        f.store.clone(),
        "https://rook.example/mcp-oauth-callback.html".into(),
    )
    .await
    .unwrap();
    let url = Url::parse(login.url()).unwrap();
    let fields: BTreeMap<_, _> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let mut callback = Url::parse(&fields["redirect_uri"]).unwrap();
    callback.query_pairs_mut().append_pair("code", "code-one").append_pair("state", &fields["state"]);
    assert!(login.accepts(callback.as_str()));
    assert!(!login.accepts("https://rook.example/mcp-oauth-callback.html?state=wrong"));
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(f.config.oauth.login_timeout_secs + 1)).await;
    assert!(login.is_expired());
    assert!(!login.accepts(callback.as_str()));
    assert!(login.complete(callback.as_str()).await.unwrap_err().contains("expired"));
    assert!(f.store.read().unwrap().is_empty());
    assert!(!f.requests.lock().unwrap().iter().any(|r| r.path == "/token"));
}

#[tokio::test]
async fn browser_callback_uses_the_same_pkce_and_verification_as_native_login() {
    let f = Fixture::new().await;
    for redirect in
        ["https://rook.example/mcp-oauth-callback.html", "http://localhost:8080/mcp-oauth-callback.html"]
    {
        let login = Login::begin_at(&f.config, &rook_llm::Proxy::default(), f.store.clone(), redirect.into())
            .await
            .unwrap();
        assert!(login.matches_config(&f.config));
        let mut changed = f.config.clone();
        changed.headers.insert("x-project".into(), "different".into());
        assert!(!login.matches_config(&changed));
        let url = Url::parse(login.url()).unwrap();
        let fields: BTreeMap<_, _> =
            url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        *f.challenge.lock().unwrap() = fields["code_challenge"].clone();
        let mut callback = Url::parse(redirect).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("code", "code-one")
            .append_pair("state", &fields["state"])
            .append_pair("iss", &f.config.url.as_ref().unwrap().replace("/mcp", "/issuer/tenant"));
        login.complete(callback.as_str()).await.unwrap();
        assert_eq!(f.source().token().await.unwrap(), "access-one");
    }
}
