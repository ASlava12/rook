//! Fetching a language server, against a stand-in for GitHub's release API.
//!
//! What is asserted is the checking: the bytes are compared to the digest the
//! release lists as they arrive, and a mismatch installs nothing.

use std::sync::Arc;

use rook_core::agent::AgentLoop;
use rook_core::install::{Installer, RUST_ANALYZER};
use rook_llm::{Message, Provider, Request, Response, Role, StopReason};
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers like GitHub: the release API with one asset, and the asset itself.
/// `digest` is what the API claims, which need not be what is served.
async fn github(asset_name: &'static str, bytes: Arc<Vec<u8>>, digest: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let at = base.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (bytes, digest, at) = (bytes.clone(), digest.clone(), at.clone());
            tokio::spawn(async move {
                let mut scratch = [0u8; 8192];
                let n = socket.read(&mut scratch).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..n]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, kind, body): (&str, &str, Vec<u8>) = if path.ends_with("/releases/latest") {
                    let mut entry = serde_json::json!({
                        "name": asset_name,
                        "size": bytes.len(),
                        "browser_download_url": format!("{at}/download/{asset_name}"),
                    });
                    // An empty digest is a release that lists none, not one
                    // that lists an empty one.
                    if !digest.is_empty() {
                        entry["digest"] = serde_json::json!(format!("sha256:{digest}"));
                    }
                    let json = serde_json::json!({ "tag_name": "2026-01-01", "assets": [entry] });
                    ("200 OK", "application/json", json.to_string().into_bytes())
                } else if path.starts_with("/download/") {
                    ("200 OK", "application/octet-stream", (*bytes).clone())
                } else {
                    ("404 Not Found", "text/plain", b"no".to_vec())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    base
}

fn gzipped(payload: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}

fn sha256_of(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

fn here() -> rook_skills::Environment {
    rook_skills::Environment::bare("linux", "x86_64", "0.1.0")
}

#[tokio::test]
async fn a_server_is_fetched_checked_against_the_listed_digest_and_put_in_place() {
    let payload = b"#!/bin/sh\necho I am rust-analyzer\n".to_vec();
    let gz = Arc::new(gzipped(&payload));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz.clone(), sha256_of(&gz)).await;
    let into = tempfile::tempdir().unwrap();

    let done = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&RUST_ANALYZER, &here())
        .await
        .unwrap();

    assert_eq!(done.tag, "2026-01-01");
    let versioned = into.path().join("rust-analyzer").join("2026-01-01").join("rust-analyzer");
    assert_eq!(std::fs::read(&versioned).unwrap(), payload, "unpacked, not stored as the gzip");
    assert!(done.verified.contains(&sha256_of(&gz)), "says which digest it matched: {}", done.verified);
    assert!(done.unverified.contains("not reviewed"), "and what it did not check: {}", done.unverified);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(std::fs::metadata(&versioned).unwrap().permissions().mode() & 0o111, 0, "runnable");
    }
}

/// The whole point: the file the server sends is compared with the digest the
/// release lists, and a file that does not match installs nothing.
#[tokio::test]
async fn a_download_that_does_not_match_the_listed_digest_installs_nothing() {
    let gz = Arc::new(gzipped(b"not what was promised"));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz, "0".repeat(64)).await;
    let into = tempfile::tempdir().unwrap();

    let refused = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&RUST_ANALYZER, &here())
        .await
        .unwrap_err();

    assert!(refused.contains("does not match the digest"), "{refused}");
    assert!(refused.contains("nothing was installed"), "{refused}");
    assert!(!into.path().join("rust-analyzer").exists(), "not even a directory");
}

#[tokio::test]
async fn a_release_that_lists_no_digest_is_not_fetched_at_all() {
    // Served with an empty digest: the API entry says nothing to check against.
    let gz = Arc::new(gzipped(b"anything"));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz, String::new()).await;
    let into = tempfile::tempdir().unwrap();

    let refused = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&RUST_ANALYZER, &here())
        .await
        .unwrap_err();
    assert!(refused.contains("no sha256 digest"), "{refused}");
    assert!(refused.contains("nothing was fetched"), "{refused}");
}

/// The tests below set `ROOK_HOME` and `ROOK_RELEASE_API`, which are process
/// wide, so they take turns.
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATE.lock().await
}

/// Says one thing and stops.
struct Says(&'static str);

#[async_trait::async_trait]
impl Provider for Says {
    fn id(&self) -> &str {
        "scripted/says"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, _request: Request) -> rook_llm::Result<Response> {
        Ok(Response {
            message: Message {
                role: Role::Assistant,
                content: self.0.into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                cache: false,
            },
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
            model: "m".into(),
        })
    }
}

/// Answers every question with `chosen`.
struct Chooses(&'static str);

#[async_trait::async_trait]
impl rook_tools::ask::Asker for Chooses {
    async fn ask(&self, questions: &[rook_tools::ask::Question]) -> Vec<rook_tools::ask::Answer> {
        questions
            .iter()
            .map(|q| rook_tools::ask::Answer {
                question: q.question.clone(),
                chosen: q.choices.iter().find(|c| c.starts_with(self.0)).cloned().into_iter().collect(),
            })
            .collect()
    }
}

/// A workspace with a Rust file and no rust-analyzer anywhere, under a state
/// directory of its own.
fn a_rust_workspace(home: &std::path::Path) -> (tempfile::TempDir, rook_core::Rook) {
    unsafe { std::env::set_var("ROOK_HOME", home) };
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("main.rs"), "fn main() {}\n").unwrap();
    let store = rook_store::Store::open(home.join("store")).unwrap();
    let (skills, _) = rook_skills::SkillIndex::discover(&[]);
    // Named so the test does not depend on what is on this machine's PATH: a
    // configured list that serves no Rust is a Rust project with no server.
    let config = rook_core::Config {
        lsp: vec![rook_lsp::ServerConfig {
            language: "go".into(),
            command: "gopls".into(),
            extensions: vec!["go".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let rook = rook_core::Rook::from_parts(
        store,
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        workspace.path().to_path_buf(),
    );
    (workspace, rook)
}

/// Nobody there to ask is a question left for whoever reads the outcome — and
/// asked once, not once a turn.
#[tokio::test]
async fn with_nobody_to_ask_a_missing_server_is_an_open_question_asked_once() {
    let _one = one_at_a_time().await;
    let home = tempfile::tempdir().unwrap();
    let (_workspace, rook) = a_rust_workspace(home.path());
    let session = rook.start_session("s").unwrap();

    let first = AgentLoop::new(&rook, Arc::new(Says("ok")), session).run("hello").await.unwrap();
    assert_eq!(first.open_questions.len(), 1, "{:?}", first.open_questions);
    assert!(first.open_questions[0].contains("rust-analyzer"), "{:?}", first.open_questions);
    assert!(first.open_questions[0].contains("rook lsp install"), "and says what would settle it");
    assert!(!home.path().join("servers").join("rust-analyzer").exists(), "nothing was fetched");

    let second = AgentLoop::new(&rook, Arc::new(Says("ok")), session).run("again").await.unwrap();
    assert!(second.open_questions.is_empty(), "not asked twice: {:?}", second.open_questions);
}

#[tokio::test]
async fn a_person_who_says_not_now_is_a_decision_and_nothing_is_fetched() {
    let _one = one_at_a_time().await;
    let home = tempfile::tempdir().unwrap();
    let (_workspace, rook) = a_rust_workspace(home.path());
    let session = rook.start_session("s").unwrap();

    let mut agent = AgentLoop::new(&rook, Arc::new(Says("ok")), session);
    agent.ask_via(Arc::new(Chooses("not now")));
    let outcome = agent.run("hello").await.unwrap();

    assert!(outcome.open_questions.is_empty(), "{:?}", outcome.open_questions);
    assert_eq!(outcome.decisions.len(), 1, "{:?}", outcome.decisions);
    assert!(outcome.decisions[0].contains("declined"), "{:?}", outcome.decisions);
    assert!(!home.path().join("servers").join("rust-analyzer").exists());
}

/// Autonomous fetches into the state directory without asking, and says that
/// it serves from the next session: the pool is built before the first turn.
#[tokio::test]
async fn an_autonomous_turn_fetches_the_missing_server_into_the_state_directory() {
    let _one = one_at_a_time().await;
    let payload = b"#!/bin/sh\necho rust-analyzer\n".to_vec();
    let gz = Arc::new(gzipped(&payload));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz.clone(), sha256_of(&gz)).await;
    unsafe { std::env::set_var("ROOK_RELEASE_API", &api) };

    let home = tempfile::tempdir().unwrap();
    let (_workspace, rook) = a_rust_workspace(home.path());
    let session = rook.start_session("s").unwrap();
    let mut agent = AgentLoop::new(&rook, Arc::new(Says("ok")), session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("hello").await.unwrap();
    unsafe { std::env::remove_var("ROOK_RELEASE_API") };

    assert!(outcome.open_questions.is_empty(), "{:?}", outcome.open_questions);
    assert_eq!(outcome.decisions.len(), 1, "{:?}", outcome.decisions);
    assert!(outcome.decisions[0].contains("next session"), "{:?}", outcome.decisions);
    let installed = home.path().join("servers").join("rust-analyzer").join("current").join("rust-analyzer");
    assert_eq!(std::fs::read(&installed).unwrap(), payload, "in place under the state directory");
}
