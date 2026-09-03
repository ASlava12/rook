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
    // Asked of the crate rather than spelled out: on Windows the one in place
    // is `rust-analyzer.exe`, and a test that wrote the unix name read a
    // Windows runner as having installed nothing.
    let installed = rook_core::install::current("rust-analyzer");
    assert!(installed.starts_with(home.path()), "under the state directory: {}", installed.display());
    assert_eq!(std::fs::read(&installed).unwrap(), payload, "in place: {}", installed.display());
}

/// A directory of stand-ins first on PATH: an `npm` that creates the shim
/// under the prefix it was given, and a `go` that puts `gopls` into `GOBIN`.
/// Each writes what it was asked, so the test can read the command back.
#[cfg(unix)]
fn fake_tools() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let npm = r#"#!/bin/sh
all="$*"
prefix=""
while [ $# -gt 0 ]; do
  case "$1" in --prefix) prefix="$2"; shift ;; esac
  shift
done
echo "$all" > "$prefix/asked"
mkdir -p "$prefix/node_modules/.bin"
printf '#!/bin/sh\necho ts\n' > "$prefix/node_modules/.bin/typescript-language-server"
chmod +x "$prefix/node_modules/.bin/typescript-language-server"
"#;
    let go = r#"#!/bin/sh
mkdir -p "$GOBIN"
printf '#!/bin/sh\necho gopls\n' > "$GOBIN/gopls"
chmod +x "$GOBIN/gopls"
echo "$@" > "$GOBIN/asked"
"#;
    for (name, body) in [("npm", npm), ("go", go)] {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// PATH with `tools` first, restored on drop. PATH is read when the command
/// is spawned, inside the future, so it has to stay set until the await.
#[cfg(unix)]
struct ToolsFirst(std::ffi::OsString);

#[cfg(unix)]
impl ToolsFirst {
    fn new(tools: &std::path::Path) -> Self {
        let was = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(tools.to_path_buf()).chain(std::env::split_paths(&was));
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        Self(was)
    }
}

#[cfg(unix)]
impl Drop for ToolsFirst {
    fn drop(&mut self) {
        unsafe { std::env::set_var("PATH", &self.0) };
    }
}

/// npm installs under a prefix of ours, with scripts off, and the shim it
/// leaves under `node_modules/.bin` is what `current` points at.
#[cfg(unix)]
#[tokio::test]
async fn a_node_server_is_installed_under_our_prefix_with_no_scripts_run() {
    let _one = one_at_a_time().await;
    let tools = fake_tools();
    let into = tempfile::tempdir().unwrap();
    let env = rook_skills::Environment::bare("linux", "x86_64", "0.1.0").with_tool("npm", "10");

    let installer = Installer::at("http://127.0.0.1:1".into(), into.path().to_path_buf()).unwrap();
    let _first = ToolsFirst::new(tools.path());
    let done = installer.install(&rook_core::install::TYPESCRIPT, &env).await.unwrap();
    let current = into.path().join("typescript-language-server").join("current");
    assert_eq!(done.path, current.join("node_modules").join(".bin").join("typescript-language-server"));
    assert!(done.path.is_file());
    let asked = std::fs::read_to_string(current.join("asked")).unwrap();
    assert!(asked.contains("--ignore-scripts"), "scripts off: {asked}");
    assert!(asked.contains("typescript-language-server@latest"), "{asked}");
    assert!(done.verified.contains("no install scripts"), "{}", done.verified);
}

/// gopls is built from source into `GOBIN`, which is our directory.
#[cfg(unix)]
#[tokio::test]
async fn a_go_server_is_built_into_our_directory() {
    let _one = one_at_a_time().await;
    let tools = fake_tools();
    let into = tempfile::tempdir().unwrap();
    let env = rook_skills::Environment::bare("linux", "x86_64", "0.1.0").with_language("go", "1.25");

    let installer = Installer::at("http://127.0.0.1:1".into(), into.path().to_path_buf()).unwrap();
    let _first = ToolsFirst::new(tools.path());
    let done = installer.install(&rook_core::install::GOPLS, &env).await.unwrap();
    let current = into.path().join("gopls").join("current");
    assert_eq!(done.path, current.join("gopls"));
    let asked = std::fs::read_to_string(current.join("asked")).unwrap();
    assert!(asked.contains("golang.org/x/tools/gopls@latest"), "{asked}");
    assert!(done.verified.contains("checksum database"), "{}", done.verified);
}

/// A tool this machine does not have is said so, before anything runs.
#[tokio::test]
async fn a_recipe_whose_toolchain_is_missing_says_which_one() {
    let into = tempfile::tempdir().unwrap();
    let env = rook_skills::Environment::bare("linux", "x86_64", "0.1.0");
    let installer = Installer::at("http://127.0.0.1:1".into(), into.path().to_path_buf()).unwrap();
    let refused = installer.install(&rook_core::install::GOPLS, &env).await.unwrap_err();
    assert!(refused.contains("`go`"), "{refused}");
    assert!(!into.path().join("gopls").exists(), "and nothing was made");
}

/// Read-only means nothing may change the machine, so there is nothing to
/// ask: a question whose every answer the policy then refuses is a wasted one.
#[tokio::test]
async fn at_read_only_nobody_is_asked_and_the_question_is_left_open() {
    let _one = one_at_a_time().await;
    let home = tempfile::tempdir().unwrap();
    let (_workspace, rook) = a_rust_workspace(home.path());
    let session = rook.start_session("s").unwrap();

    struct Never;
    #[async_trait::async_trait]
    impl rook_tools::ask::Asker for Never {
        async fn ask(&self, q: &[rook_tools::ask::Question]) -> Vec<rook_tools::ask::Answer> {
            panic!("nobody should have been asked: {:?}", q.iter().map(|q| &q.question).collect::<Vec<_>>())
        }
    }
    let mut agent = AgentLoop::new(&rook, Arc::new(Says("ok")), session);
    agent.policy.set_stance(rook_tools::policy::Stance::ReadOnly);
    agent.ask_via(Arc::new(Never));
    let outcome = agent.run("hello").await.unwrap();

    assert_eq!(outcome.open_questions.len(), 1, "{:?}", outcome.open_questions);
    assert!(outcome.open_questions[0].contains("read-only"), "{:?}", outcome.open_questions);
    assert!(!home.path().join("servers").join("rust-analyzer").exists());
}

/// An install that fails explains itself in its last lines, after however
/// much progress it printed. A reader that kept the head reported the noise.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_install_reports_the_reason_at_the_end_of_what_it_printed() {
    use std::os::unix::fs::PermissionsExt;
    let _one = one_at_a_time().await;
    let tools = tempfile::tempdir().unwrap();
    let npm = tools.path().join("npm");
    std::fs::write(
        &npm,
        "#!/bin/sh\ni=0\nwhile [ $i -lt 4000 ]; do echo \"npm progress line $i, nothing to see\"; i=$((i+1)); done\n\
         echo 'npm ERR! the real reason: EACCES' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let into = tempfile::tempdir().unwrap();
    let env = rook_skills::Environment::bare("linux", "x86_64", "0.1.0").with_tool("npm", "10");

    let installer = Installer::at("http://127.0.0.1:1".into(), into.path().to_path_buf()).unwrap();
    let _first = ToolsFirst::new(tools.path());
    let refused = installer.install(&rook_core::install::TYPESCRIPT, &env).await.unwrap_err();

    assert!(refused.contains("the real reason: EACCES"), "the last line is the one that matters:\n{refused}");
    assert!(refused.contains("progress line 0,"), "and the first is kept too");
    assert!(refused.len() < 80 << 10, "bounded: {} bytes", refused.len());
}

/// clangd ships a zip with a versioned top directory and needs the tree beside
/// its binary; it is picked by the beginning and end of its name, because the
/// version is in the middle.
#[tokio::test]
async fn a_zipped_server_is_picked_by_prefix_unpacked_whole_and_put_in_place() {
    use std::io::Write;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    // Named as the real archive names it on this platform: `clangd.exe` on
    // Windows, where the installer looks for exactly that.
    let inside = if cfg!(windows) { "clangd_22.1.6/bin/clangd.exe" } else { "clangd_22.1.6/bin/clangd" };
    w.start_file(inside, stored).unwrap();
    w.write_all(b"#!/bin/sh\necho clangd\n").unwrap();
    w.start_file("clangd_22.1.6/lib/clang/22/include/stddef.h", stored).unwrap();
    w.write_all(b"").unwrap();
    let bytes = Arc::new(w.finish().unwrap().into_inner());
    let api = github("clangd-linux-22.1.6.zip", bytes.clone(), sha256_of(&bytes)).await;
    let into = tempfile::tempdir().unwrap();

    let done = Installer::at(api, into.path().to_path_buf())
        .unwrap()
        .install(&rook_core::install::CLANGD, &here())
        .await
        .unwrap();

    let current = into.path().join("clangd").join("current");
    assert_eq!(done.path, rook_core::install::CLANGD.binary_in(&current), "asked of the recipe, not spelled");
    assert!(done.path.starts_with(current.join("bin")), "{}", done.path.display());
    assert!(done.path.is_file());
    assert!(
        current.join("lib").join("clang").join("22").join("include").join("stddef.h").exists(),
        "the tree it needs"
    );
    assert!(done.verified.contains("clangd-linux-22.1.6.zip"), "{}", done.verified);
}

/// A server fetched once is a server that is a year old a year later.
/// `update` fetches again whatever is in place and says which tag moved.
#[tokio::test]
async fn update_fetches_again_what_is_in_place_and_says_what_moved() {
    let payload = b"#!/bin/sh\necho v1\n".to_vec();
    let gz = Arc::new(gzipped(&payload));
    let api = github("rust-analyzer-x86_64-unknown-linux-gnu.gz", gz.clone(), sha256_of(&gz)).await;
    let into = tempfile::tempdir().unwrap();
    let installer = Installer::at(api, into.path().to_path_buf()).unwrap();
    installer.install(&RUST_ANALYZER, &here()).await.unwrap();
    assert_eq!(installer.installed().len(), 1, "one server in place, with its tag on record");

    // The same release again: nothing moved, and it says so.
    let report = installer.update(&here()).await;
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].0, "rust-analyzer");
    assert_eq!(report[0].1.as_deref(), Ok("already at 2026-01-01"));

    // A directory with no recipe is not a server, whatever it is called.
    std::fs::create_dir_all(into.path().join("notes")).unwrap();
    assert_eq!(installer.installed().len(), 1);
}
