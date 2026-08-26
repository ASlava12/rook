//! The agent loop, exercised against a scripted provider.
//!
//! No network and no model: the point is the loop's own behaviour — tool
//! dispatch, skill loading, what reaches the session log, and what the system
//! prompt actually contains.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use rook_llm::{LlmError, Message, Provider, Request, Response, Role, StopReason, ToolCall, Usage};
use rook_skills::{Environment, SkillIndex, SkillSource};
use rook_store::Store;

/// Replays a fixed sequence of responses and records what it was asked.
struct ScriptedProvider {
    script: Mutex<Vec<Response>>,
    seen: Mutex<Vec<Request>>,
}

impl ScriptedProvider {
    fn new(script: Vec<Response>) -> Self {
        Self { script: Mutex::new(script), seen: Mutex::new(Vec::new()) }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted/test"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.seen.lock().unwrap().push(request);
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            return Err(LlmError::Other("the script ran out of responses".into()));
        }
        Ok(script.remove(0))
    }
}

fn reply(text: &str) -> Response {
    Response {
        message: Message::assistant(text),
        stop_reason: StopReason::EndTurn,
        usage: Usage { input_tokens: 100, output_tokens: 20 },
        model: "scripted".into(),
    }
}

fn call(name: &str, args: serde_json::Value) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall { id: "call_1".into(), name: name.into(), arguments: args }],
            tool_call_id: None,
        },
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 100, output_tokens: 20 },
        model: "scripted".into(),
    }
}

struct Fixture {
    _store_dir: tempfile::TempDir,
    _skill_dir: tempfile::TempDir,
    workspace: tempfile::TempDir,
    rook: Rook,
}

fn fixture() -> Fixture {
    let store_dir = tempfile::tempdir().unwrap();
    let skill_dir = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();

    let dir = skill_dir.path().join("greeting");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: greeting\ndescription: How to greet someone properly.\nversion: 1.0.0\n---\n\
         Always greet in the user's own language.\n",
    )
    .unwrap();

    let blocked = skill_dir.path().join("windows-only");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(
        blocked.join("SKILL.md"),
        "---\nname: windows-only\ndescription: Windows things.\nversion: 1.0.0\n\
         requires:\n  os: [windows]\n---\nbody\n",
    )
    .unwrap();

    let (skills, errors) = SkillIndex::discover(&[(skill_dir.path().to_path_buf(), SkillSource::User)]);
    assert!(errors.is_empty(), "{errors:?}");

    let store = Store::open(store_dir.path()).unwrap();
    let env = Environment::bare("linux", "x86_64", "0.1.0").with_language("rust", "1.97.1");
    let rook = Rook::from_parts(store, Config::default(), env, skills, PathBuf::from(workspace.path()));
    Fixture { _store_dir: store_dir, _skill_dir: skill_dir, workspace, rook }
}

#[tokio::test]
async fn a_plain_turn_is_logged_end_to_end() {
    let f = fixture();
    let session = f.rook.start_session("test").unwrap();
    let provider = Box::new(ScriptedProvider::new(vec![reply("done")]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);

    let outcome = agent.run("say hello").await.unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(outcome.steps, 1);
    assert_eq!(outcome.input_tokens, 100);

    let entries = f.rook.transcript(session, 0, 100, 4096).unwrap();
    let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["user", "assistant"], "both sides of the turn must be in the log");
    assert_eq!(entries[0].body, "say hello");
}

#[tokio::test]
async fn the_system_prompt_carries_the_environment_and_skill_cards_not_bodies() {
    let f = fixture();
    let session = f.rook.start_session("test").unwrap();
    let provider = Box::new(ScriptedProvider::new(vec![reply("ok")]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    let prompt = agent.system_prompt();

    assert!(prompt.contains("os: linux"), "{prompt}");
    assert!(prompt.contains("gnu userland"), "{prompt}");
    assert!(prompt.contains("rust 1.97.1"), "the detected toolchain belongs in the prompt");

    assert!(prompt.contains("greeting"), "an applicable skill must be advertised");
    assert!(
        !prompt.contains("Always greet in the user's own language"),
        "the skill *body* must not be in the prompt — that is the whole point of cards"
    );
    assert!(!prompt.contains("windows-only"), "a skill blocked by this environment must not be offered");

    let _ = agent.run("hi").await.unwrap();
}

#[tokio::test]
async fn a_tool_call_runs_and_both_halves_reach_the_log() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("hello.txt"), "line one\nline two\n").unwrap();
    let session = f.rook.start_session("test").unwrap();

    let provider = Box::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({ "path": "hello.txt" })),
        reply("the file has two lines"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    let outcome = agent.run("read hello.txt").await.unwrap();

    assert_eq!(outcome.steps, 2);
    assert_eq!(outcome.tools_called, vec!["read_file"]);
    assert_eq!(outcome.reply, "the file has two lines");

    let entries = f.rook.transcript(session, 0, 100, 8192).unwrap();
    let kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["user", "tool-call", "tool-result", "assistant"]);
    assert!(entries[2].body.contains("line two"), "{}", entries[2].body);
}

#[tokio::test]
async fn load_skill_pulls_a_body_in_on_demand() {
    let f = fixture();
    let session = f.rook.start_session("test").unwrap();
    let provider = Box::new(ScriptedProvider::new(vec![
        call("load_skill", serde_json::json!({ "name": "greeting" })),
        reply("greeted"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    let outcome = agent.run("greet the user").await.unwrap();

    assert_eq!(outcome.skills_loaded, vec!["greeting@1.0.0"]);
    let entries = f.rook.transcript(session, 0, 100, 8192).unwrap();
    let skill_entry = entries.iter().find(|e| e.kind == "skill").expect("a skill load must be logged");
    assert!(skill_entry.body.contains("Always greet in the user's own language"));
}

#[tokio::test]
async fn a_skill_blocked_by_the_environment_explains_itself_rather_than_404ing() {
    let f = fixture();
    let session = f.rook.start_session("test").unwrap();
    let provider = Box::new(ScriptedProvider::new(vec![
        call("load_skill", serde_json::json!({ "name": "windows-only" })),
        reply("understood"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    let outcome = agent.run("do a windows thing").await.unwrap();

    assert!(outcome.skills_loaded.is_empty());
    let entries = f.rook.transcript(session, 0, 100, 8192).unwrap();
    let failure = entries
        .iter()
        .find(|e| e.kind == "error")
        .expect("a skill that failed to load must be visible in the transcript");
    assert!(
        failure.body.contains("os"),
        "the model must be told *why*, not just that it failed: {}",
        failure.body
    );
    assert!(failure.body.contains("windows-only"), "{}", failure.body);
}

#[tokio::test]
async fn an_unknown_tool_is_reported_to_the_model_not_fatal() {
    let f = fixture();
    let session = f.rook.start_session("test").unwrap();
    let provider = Box::new(ScriptedProvider::new(vec![
        call("no_such_tool", serde_json::json!({})),
        reply("recovered"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    let outcome = agent.run("try something").await.unwrap();

    assert_eq!(outcome.reply, "recovered", "the loop must survive a bad tool name");
    let entries = f.rook.transcript(session, 0, 100, 8192).unwrap();
    let result = entries.iter().find(|e| e.kind == "tool-result").unwrap();
    assert!(result.body.contains("unknown tool"), "{}", result.body);
}

#[tokio::test]
async fn a_repeated_tool_result_is_stored_once() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("same.txt"), "x".repeat(4096)).unwrap();
    let session = f.rook.start_session("test").unwrap();

    let provider = Box::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({ "path": "same.txt" })),
        call("read_file", serde_json::json!({ "path": "same.txt" })),
        reply("read twice"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.run("read it twice").await.unwrap();

    let stats = f.rook.stats().unwrap();
    let results = stats.per_kind.iter().find(|k| k.kind == "tool-result").unwrap();
    assert_eq!(results.objects, 1, "identical tool output must not be stored twice");
    assert!(stats.dedup_saved_hint > 0, "and the saving should be visible in the stats");
}

#[tokio::test]
async fn the_loop_stops_at_max_steps_instead_of_spinning() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.max_steps = 3;
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("second")).unwrap(),
        config,
        f.rook.env.clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("loop").unwrap();

    // A model that only ever asks for another tool call.
    let script = (0..10).map(|_| call("list_dir", serde_json::json!({}))).collect();
    let provider = Box::new(ScriptedProvider::new(script));
    let mut agent = AgentLoop::new(&rook, provider, session);
    let outcome = agent.run("go forever").await.unwrap();

    assert_eq!(outcome.steps, 3);
    assert_eq!(outcome.stopped, "max_steps");
}
