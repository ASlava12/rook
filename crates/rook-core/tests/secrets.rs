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
        if request.messages.first().is_some_and(|m| m.content.starts_with("Classify whether an assistant")) {
            let mut verdict = reply(r#"{"action":"finish"}"#);
            verdict.usage = Usage::default();
            return Ok(verdict);
        }
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
            images: Vec::new(),
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
    // Printed on purpose: the value reaches the command, and what comes back
    // must still not carry it. The command is per platform and the claim is
    // not — `cmd.exe` does not expand `$VAR` and has no `printf`, so the unix
    // spelling ran and printed nothing, and the test failed on the assertion
    // about redaction while proving nothing about it.
    let echo_it = match cfg!(windows) {
        true => "echo %ROOK_SECRET_SSH_PROD%",
        false => "printf '%s' \"$ROOK_SECRET_SSH_PROD\"",
    };
    let provider = Arc::new(Scripted(
        Mutex::new(vec![
            call("run_command", serde_json::json!({ "command": echo_it, "secrets": ["ssh_prod"] })),
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
    // What a command that ran leaves, rather than a word out of its own text.
    // `hello` was the test for this and stopped being one when the goal check
    // began to be shown the turn's own record: the call is in that record, so
    // the word is there whether or not anything ran. `exit ` is the first line
    // of every result `run_command` returns and appears nowhere else.
    assert!(!handed.contains("exit 0"), "and the command did not run:\n{handed}");
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

/// Ported from cline #13716: a key pasted with a byte-order mark or a
/// zero-width space is kept corrupted and then answers 401 exactly as a wrong
/// key does — and here nothing prints a value, so the one way to tell the
/// difference is gone.
#[test]
fn a_value_pasted_with_what_a_paste_carries_is_kept_without_it() {
    let (dir, mut vault) = alone();
    // A byte-order mark in front, a zero-width space inside the trailing
    // whitespace, and a newline at the end: what a copy out of a browser or a
    // word processor actually looks like.
    vault.keep("pasted", "\u{FEFF}  hunter2-and-then-some \u{200B}\n").unwrap();

    let text = std::fs::read_to_string(dir.path().join("secrets.toml")).unwrap();
    assert!(text.contains("hunter2-and-then-some"), "{text}");
    assert!(!text.contains('\u{FEFF}'), "the mark is not part of the key: {text:?}");
    assert!(!text.contains('\u{200B}'), "and neither is the zero-width space: {text:?}");
    assert!(!text.contains("some \""), "nor the whitespace around it: {text:?}");

    // And nothing that is only invisible is kept at all: an empty secret that
    // looks set is the same failure one step later.
    let mut vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    let refused = vault.keep("empty", "\u{FEFF} \u{200B}\n").unwrap_err().to_string();
    assert!(refused.contains("invisible characters"), "{refused}");
}

/// The same for a value that comes from somewhere else: a variable exported by
/// a script keeps its trailing newline, and a manager that prints a banner
/// prints it however it likes.
#[test]
fn a_value_from_another_source_is_cleaned_the_same_way() {
    let (dir, mut vault) = alone();
    unsafe { std::env::set_var("ROOK_TEST_TRAILING", "value-with-a-newline\n") };
    vault.refer("trailing", &Source::Env("ROOK_TEST_TRAILING".into())).unwrap();

    let vault = Vault::load_from(dir.path().join("secrets.toml")).unwrap();
    let handed = rook_tools::Secrets::value(&vault, "trailing").unwrap();

    assert_eq!(handed, "value-with-a-newline", "the newline is not part of the value");
}

struct PrivatePerson {
    outcome: rook_proto::SecretStatus,
    requests: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl rook_tools::ask::Asker for PrivatePerson {
    async fn ask(&self, _: &[rook_tools::ask::Question]) -> Vec<rook_tools::ask::Answer> {
        panic!("private values must never use ordinary questions");
    }
    fn supports_private_input(&self) -> bool {
        true
    }
    async fn request_secret(
        &self,
        prompt: rook_proto::SecretPrompt,
        sink: Arc<dyn rook_tools::private_input::SecretSink>,
    ) -> rook_proto::SecretStatus {
        self.requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(!serde_json::to_string(&prompt).unwrap().contains(PASSWORD));
        match self.outcome {
            rook_proto::SecretStatus::Saved => sink.save(PASSWORD),
            status => status,
        }
    }
}

#[tokio::test]
async fn a_privately_requested_secret_is_usable_in_the_same_turn_and_redacted_everywhere() {
    let (_w, rook) = fixture("private-saved");
    let session = rook.start_session("request a missing credential").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let echo = if cfg!(windows) {
        "echo %ROOK_SECRET_PRIVATE_SAVED%"
    } else {
        "printf '%s' \"$ROOK_SECRET_PRIVATE_SAVED\""
    };
    let provider = Arc::new(Scripted(
        Mutex::new(vec![
            call(
                "request_secret",
                serde_json::json!({"name":"private_saved","reason":"Authenticate the requested command"}),
            ),
            call("run_command", serde_json::json!({"command":echo,"secrets":["private_saved"]})),
            reply("done"),
        ]),
        seen.clone(),
    ));
    let person =
        Arc::new(PrivatePerson { outcome: rook_proto::SecretStatus::Saved, requests: Default::default() });
    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.ask_via(person.clone());
    agent.run("Use a missing named credential, requesting it privately").await.unwrap();
    assert_eq!(person.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    let handed: String =
        seen.lock().unwrap().iter().flat_map(|r| r.messages.clone()).map(|m| m.content).collect();
    assert!(handed.contains("saved") && handed.contains("${secret}"), "{handed}");
    assert!(!handed.contains(PASSWORD));
    let logged: String =
        rook.transcript(session, 0, 200, 8000).unwrap().iter().map(|e| e.body.clone()).collect();
    assert!(!logged.contains(PASSWORD));
    let receipt = rook
        .store
        .kv_get(&format!("private-input/{}", rook_store::format_session_id(session)))
        .unwrap()
        .unwrap();
    let receipt = String::from_utf8(receipt).unwrap();
    assert!(receipt.contains("saved"));
    assert!(!receipt.contains(PASSWORD));
    assert_eq!(rook_tools::Secrets::value(&Vault::load().unwrap(), "private_saved").unwrap(), PASSWORD);
}

#[tokio::test]
async fn refusal_and_no_answer_are_final_for_the_ongoing_request_without_retrying_the_person() {
    use rook_proto::SecretStatus;
    for status in [
        SecretStatus::Declined,
        SecretStatus::Skipped,
        SecretStatus::Expired,
        SecretStatus::Cancelled,
        SecretStatus::Unavailable,
    ] {
        let label = serde_json::to_value(status).unwrap().as_str().unwrap().to_owned();
        let (_w, rook) = fixture(&format!("private-{label}"));
        let session = rook.start_session("respect refusal").unwrap();
        let name = format!("private_{label}");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ask = || {
            call(
                "request_secret",
                serde_json::json!({"name":name,"reason":"Only needed for an optional authenticated check"}),
            )
        };
        let provider = Arc::new(Scripted(
            Mutex::new(vec![ask(), ask(), reply("Continue independent work")]),
            seen.clone(),
        ));
        let person = Arc::new(PrivatePerson { outcome: status, requests: Default::default() });
        let mut agent = AgentLoop::new(&rook, provider, session);
        agent.ask_via(person.clone());
        agent.run("Do the work; I may not supply credentials").await.unwrap();
        assert_eq!(
            person.requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{label} must not ask again"
        );
        let handed: String =
            seen.lock().unwrap().iter().flat_map(|r| r.messages.clone()).map(|m| m.content).collect();
        assert!(handed.contains(&label));
        assert!(!Vault::load().unwrap().named().iter().any(|s| s.name == name));
        let receipt = rook
            .store
            .kv_get(&format!("private-input/{}", rook_store::format_session_id(session)))
            .unwrap()
            .unwrap();
        assert!(String::from_utf8(receipt).unwrap().contains(&label));
    }
}

#[test]
fn stale_vault_snapshots_preserve_other_saved_credentials_and_parse_errors_are_private() {
    let (dir, mut a) = alone();
    let path = dir.path().join("secrets.toml");
    let mut b = Vault::load_from(path.clone()).unwrap();
    a.keep("one", "first").unwrap();
    b.keep("two", "second").unwrap();
    a.forget("one").unwrap();
    assert_eq!(
        Vault::load_from(path.clone()).unwrap().named().iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        ["two"]
    );
    std::fs::write(&path, format!("invalid secret {PASSWORD} = [")).unwrap();
    let error = Vault::load_from(path).err().unwrap().to_string();
    assert!(!error.contains(PASSWORD), "parse errors must not quote file content");
}
