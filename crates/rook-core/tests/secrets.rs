//! Secrets the agent can use and cannot leak.
//!
//! Every claim here is about where a value is *not*: not in what the model is
//! given, not in the session log, not in the store, and not in any listing.
//! What it is about is one thing — a command that needs a password gets one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook, Source, Vault};
use rook_llm::{LlmError, Message, Provider, Request, Response, Role, StopReason, ToolCall, Usage};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

const PASSWORD: &str = "hunter2-and-then-some";

struct Scripted(Mutex<Vec<Response>>, Arc<Mutex<Vec<Request>>>);

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &str {
        "scripted/secrets"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.1.lock().unwrap().push(request);
        let mut script = self.0.lock().unwrap();
        match script.is_empty() {
            true => Err(LlmError::Other("the script ran out".into())),
            false => Ok(script.remove(0)),
        }
    }
}

fn call(name: &str, args: serde_json::Value) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall { id: "c1".into(), name: name.into(), arguments: args }],
            tool_call_id: None,
            cache: false,
            reasoning: Vec::new(),
        },
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 10, output_tokens: 2, ..Default::default() },
        model: "scripted".into(),
    }
}

fn reply(text: &str) -> Response {
    Response {
        message: Message::assistant(text),
        stop_reason: StopReason::EndTurn,
        usage: Usage { input_tokens: 10, output_tokens: 2, ..Default::default() },
        model: "scripted".into(),
    }
}

/// One home for the whole file, set once. `ROOK_HOME` belongs to the process,
/// so a test that sets its own moves the vault out from under whichever test is
/// reading it — which is how the first version of this failed in parallel and
/// passed alone.
fn home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ROOK_HOME", dir.path()) };
        dir
    });
    dir.path()
}

/// A vault of its own, for what is about the vault rather than about a turn.
fn alone() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    (dir, vault)
}

/// An engine whose turns run without being asked about: what is being tested is
/// where a value goes, and an approval prompt nobody answers refuses the
/// command before it can go anywhere.
fn fixture(named: &str) -> (tempfile::TempDir, Rook) {
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open(home().join(format!("store-{named}"))).unwrap();
    let (skills, _) = SkillIndex::discover(&[]);
    let mut config = Config::default();
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
    let rook = Rook::from_parts(
        store,
        config,
        Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        PathBuf::from(workspace.path()),
    );
    (workspace, rook)
}

#[test]
fn a_kept_value_is_written_where_only_this_account_can_read_it() {
    let (dir, mut vault) = alone();
    vault.keep("ssh_prod", PASSWORD).unwrap();

    let path = dir.path().join("secrets.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(PASSWORD), "it is kept in the clear, and the file says so: {text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "and nobody else can read it");
    }
}

#[test]
fn a_listing_says_where_a_value_comes_from_and_never_what_it_is() {
    let (dir, mut vault) = alone();
    vault.keep("ssh_prod", PASSWORD).unwrap();
    vault.refer("npm_token", &Source::Env("ROOK_TEST_NPM".into())).unwrap();
    vault.refer("absent", &Source::Env("ROOK_TEST_NOT_SET".into())).unwrap();
    unsafe { std::env::set_var("ROOK_TEST_NPM", "from-the-environment") };

    let named = Vault::load_from(dir.path().join("secrets.toml")).unwrap().named();
    let listed = format!("{named:?}");

    assert!(!listed.contains(PASSWORD), "a listing carries no values: {listed}");
    assert!(!listed.contains("from-the-environment"), "of either kind: {listed}");
    let kept = named.iter().find(|s| s.name == "ssh_prod").unwrap();
    assert_eq!(kept.source, "kept");
    assert!(kept.resolves);
    // The difference somebody debugging needs: a name nobody set and a source
    // that is not answering look the same in a list of names.
    assert!(named.iter().find(|s| s.name == "npm_token").unwrap().resolves);
    assert!(!named.iter().find(|s| s.name == "absent").unwrap().resolves);
}

/// The whole point, end to end: a command gets the value, the model gets the
/// name, and the transcript gets neither.
#[tokio::test]
async fn a_command_is_given_the_value_and_the_transcript_is_not() {
    let (_w, rook) = fixture("used");
    Vault::load().unwrap().keep("ssh_prod", PASSWORD).unwrap();

    let session = rook.start_session("secret").unwrap();
    let seen: Arc<Mutex<Vec<Request>>> = Default::default();
    let provider = Arc::new(Scripted(
        Mutex::new(vec![
            call(
                "run_command",
                serde_json::json!({
                    // Printed on purpose: the value reaches the command, and
                    // what comes back must still not carry it.
                    "command": "printf '%s' \"$ROOK_SECRET_SSH_PROD\"",
                    "secrets": ["ssh_prod"]
                }),
            ),
            reply("done"),
        ]),
        seen.clone(),
    ));
    let out = AgentLoop::new(&rook, provider, session).run("use the password").await.unwrap();

    assert!(out.tools_called.contains(&"run_command".to_string()), "{:?}", out.tools_called);

    // What the model was given.
    let handed: String =
        seen.lock().unwrap().iter().flat_map(|r| r.messages.clone()).map(|m| m.content).collect();
    assert!(!handed.contains(PASSWORD), "the value reached the model:\n{handed}");
    assert!(handed.contains("${secret}"), "and what it prints comes back marked:\n{handed}");

    // What the store kept.
    let events = rook.transcript(session, 0, 200, 8_000).unwrap();
    let logged: String = events.iter().map(|e| e.body.clone()).collect();
    assert!(!logged.contains(PASSWORD), "the value is in the transcript:\n{logged}");
}

#[tokio::test]
async fn a_command_naming_a_secret_nobody_set_is_refused_before_it_runs() {
    let (_w, rook) = fixture("missing");
    let session = rook.start_session("missing").unwrap();
    let seen: Arc<Mutex<Vec<Request>>> = Default::default();
    let provider = Arc::new(Scripted(
        Mutex::new(vec![
            call("run_command", serde_json::json!({ "command": "echo hello", "secrets": ["nothing_here"] })),
            reply("ok"),
        ]),
        seen.clone(),
    ));
    AgentLoop::new(&rook, provider, session).run("try it").await.unwrap();

    let handed: String =
        seen.lock().unwrap().iter().flat_map(|r| r.messages.clone()).map(|m| m.content).collect();
    assert!(handed.contains("no secret"), "it says which:\n{handed}");
    assert!(handed.contains("rook secrets ls"), "and where to look:\n{handed}");
    assert!(!handed.contains("hello"), "and the command did not run:\n{handed}");
}

#[test]
fn a_name_that_could_not_be_a_variable_is_refused_rather_than_mangled() {
    let (_dir, mut vault) = alone();
    for bad in ["", "1password", "with space", "semi;colon", "$var"] {
        assert!(vault.keep(bad, "x").is_err(), "kept {bad:?}, which cannot be an environment variable");
    }
    assert!(vault.keep("ssh_prod-2", "x").is_ok());
}

#[test]
fn a_source_nothing_can_act_on_is_said_rather_than_guessed_at() {
    assert_eq!(Source::parse("env:TOKEN"), Some(Source::Env("TOKEN".into())));
    assert_eq!(Source::parse("cmd:op read op://x/y"), Some(Source::Command("op read op://x/y".into())));
    assert_eq!(Source::parse("keychain:svc/acct"), Some(Source::Keychain("svc/acct".into())));
    assert_eq!(Source::parse("TOKEN"), None, "a bare word is not a source");
    assert_eq!(Source::parse("env:"), None, "and neither is a prefix with nothing after it");
}
