//! The agent loop, exercised against a scripted provider.
//!
//! No network and no model: the point is the loop's own behaviour — tool
//! dispatch, skill loading, what reaches the session log, and what the system
//! prompt actually contains.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rook_core::agent::AgentLoop;
use rook_core::{Config, Rook};
use rook_llm::{LlmError, Message, Provider, Request, Response, Role, StopReason, ToolCall, Usage};
use rook_skills::{Environment, SkillIndex, SkillSource};
use rook_store::Store;

/// Replays a fixed sequence of responses and records what it was asked.
struct ScriptedProvider {
    script: Mutex<Vec<Response>>,
    seen: Arc<Mutex<Vec<Request>>>,
}

impl ScriptedProvider {
    fn new(script: Vec<Response>) -> Self {
        Self { script: Mutex::new(script), seen: Arc::new(Mutex::new(Vec::new())) }
    }

    /// Handle on what the provider was asked, readable after it is boxed away.
    fn share(&self) -> Arc<Mutex<Vec<Request>>> {
        self.seen.clone()
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
        usage: Usage { input_tokens: 100, output_tokens: 20, ..Default::default() },
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
            cache: false,
        },
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 100, output_tokens: 20, ..Default::default() },
        model: "scripted".into(),
    }
}

struct Fixture {
    _store_dir: tempfile::TempDir,
    _skill_dir: tempfile::TempDir,
    workspace: tempfile::TempDir,
    rook: Rook,
}

/// Anything reached through `paths::` — the user skills directory, the config
/// file — lands in the developer's real `~/.rook` unless this is set, and it is
/// one variable for the whole process.
fn redirect_home() {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = HOME.get_or_init(|| tempfile::tempdir().unwrap());
    unsafe { std::env::set_var("ROOK_HOME", dir.path()) };
}

fn fixture() -> Fixture {
    redirect_home();
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
    let provider = Arc::new(ScriptedProvider::new(vec![reply("done")]));
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
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
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

    let provider = Arc::new(ScriptedProvider::new(vec![
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
    let provider = Arc::new(ScriptedProvider::new(vec![
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
    let provider = Arc::new(ScriptedProvider::new(vec![
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
    let provider = Arc::new(ScriptedProvider::new(vec![
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

    let provider = Arc::new(ScriptedProvider::new(vec![
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
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("loop").unwrap();

    // A model that only ever asks for another tool call.
    let script = (0..10).map(|_| call("list_dir", serde_json::json!({}))).collect();
    let provider = Arc::new(ScriptedProvider::new(script));
    let mut agent = AgentLoop::new(&rook, provider, session);
    let outcome = agent.run("go forever").await.unwrap();

    assert_eq!(outcome.steps, 3);
    assert_eq!(outcome.stopped, "max_steps");
}

#[tokio::test]
async fn a_mutating_tool_is_checkpointed_and_a_rewind_undoes_it() {
    let f = fixture();
    let target = f.workspace.path().join("notes.txt");
    std::fs::write(&target, "original\n").unwrap();
    let session = f.rook.start_session("edit").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "notes.txt", "content": "rewritten\n" })),
        reply("done"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("rewrite notes.txt").await.unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "rewritten\n");

    let entries = f.rook.transcript(session, 0, 100, 4096).unwrap();
    let checkpoint = entries.iter().find(|e| e.kind == "checkpoint").expect("mutation must checkpoint");
    assert!(checkpoint.body.contains("notes.txt"), "{}", checkpoint.body);

    let report = f.rook.rewind(session, checkpoint.seq, true).unwrap();
    assert_eq!(report.files_restored, 1);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original\n");
    assert_eq!(report.events_kept, checkpoint.seq, "the fork keeps the prefix before the rewind point");

    // The original session is untouched: a rewind must not destroy history.
    let original = f.rook.transcript(session, 0, 100, 4096).unwrap();
    assert_eq!(original.len(), entries.len());
}

#[tokio::test]
async fn a_rewind_deletes_a_file_the_agent_created() {
    let f = fixture();
    let created = f.workspace.path().join("new.txt");
    let session = f.rook.start_session("create").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "new.txt", "content": "hello\n" })),
        reply("created"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("create new.txt").await.unwrap();
    assert!(created.exists());

    let seq = f
        .rook
        .transcript(session, 0, 100, 4096)
        .unwrap()
        .iter()
        .find(|e| e.kind == "checkpoint")
        .unwrap()
        .seq;
    let report = f.rook.rewind(session, seq, true).unwrap();
    assert_eq!(report.files_removed, 1);
    assert!(!created.exists(), "a file that did not exist before the turn must be removed");
}

#[tokio::test]
async fn a_read_only_tool_does_not_checkpoint() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("r.txt"), "x").unwrap();
    let session = f.rook.start_session("read").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({ "path": "r.txt" })),
        reply("read"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.run("read it").await.unwrap();

    let entries = f.rook.transcript(session, 0, 100, 4096).unwrap();
    assert!(!entries.iter().any(|e| e.kind == "checkpoint"));
}

#[tokio::test]
async fn context_usage_separates_what_is_live_from_what_is_merely_stored() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("big.txt"), "z".repeat(40_000)).unwrap();
    let session = f.rook.start_session("usage").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "big.txt", "content": "small\n" })),
        reply("done"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("shrink it").await.unwrap();

    let usage = f.rook.context_usage(session, 128_000).unwrap();
    assert!(usage.by_kind.iter().any(|(k, _)| k == "checkpoint"));
    assert!(
        usage.live_tokens < usage.logged_tokens,
        "the 40 KB checkpoint is storage, not context: live {} vs logged {}",
        usage.live_tokens,
        usage.logged_tokens
    );
    assert!(usage.compact_at < usage.window);
}

#[tokio::test]
async fn a_second_turn_carries_the_first_one_with_it() {
    let f = fixture();
    let session = f.rook.start_session("continuity").unwrap();

    let first = Arc::new(ScriptedProvider::new(vec![reply("your name is Ada")]));
    AgentLoop::new(&f.rook, first, session).run("remember my name").await.unwrap();

    let second = ScriptedProvider::new(vec![reply("Ada")]);
    let seen = second.share();
    AgentLoop::new(&f.rook, Arc::new(second), session).run("what is my name?").await.unwrap();

    let request = seen.lock().unwrap().last().cloned().expect("the provider must have been called");
    let roles: Vec<_> = request.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::System, Role::User, Role::Assistant, Role::User],
        "the second turn must replay the first"
    );
    assert_eq!(request.messages[1].content, "remember my name");
    assert_eq!(request.messages[2].content, "your name is Ada");
    assert_eq!(request.messages[3].content, "what is my name?");
}

#[tokio::test]
async fn replayed_tool_calls_keep_their_results_paired() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("p.txt"), "payload").unwrap();
    let session = f.rook.start_session("tools").unwrap();

    let first = Arc::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({ "path": "p.txt" })),
        reply("read it"),
    ]));
    AgentLoop::new(&f.rook, first, session).run("read p.txt").await.unwrap();

    let second = ScriptedProvider::new(vec![reply("still here")]);
    let seen = second.share();
    AgentLoop::new(&f.rook, Arc::new(second), session).run("and now?").await.unwrap();

    let request = seen.lock().unwrap().last().cloned().unwrap();
    let call_msg = request
        .messages
        .iter()
        .find(|m| !m.tool_calls.is_empty())
        .expect("the replayed history must contain the tool call");
    let result_msg = request.messages.iter().find(|m| m.role == Role::Tool).expect("and its result");
    assert_eq!(
        result_msg.tool_call_id.as_deref(),
        Some(call_msg.tool_calls[0].id.as_str()),
        "a tool result must reference the call it answers, or the provider rejects the request"
    );
    assert_eq!(call_msg.tool_calls[0].name, "read_file");
}

#[tokio::test]
async fn the_agent_can_remember_and_recall_across_sessions() {
    let f = fixture();
    let first = f.rook.start_session("learn").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call(
            "remember",
            serde_json::json!({ "text": "this project uses tabs, not spaces", "tags": ["style"] }),
        ),
        reply("noted"),
    ]));
    let outcome = AgentLoop::new(&f.rook, provider, first).run("we use tabs here").await.unwrap();
    assert_eq!(outcome.facts_learned.len(), 1);

    // A different session must see it — that is the whole point of memory.
    let second = f.rook.start_session("apply").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), second)
        .run("what indentation style should I use?")
        .await
        .unwrap();

    let sent: String =
        seen.lock().unwrap().last().cloned().unwrap().messages.iter().map(|m| m.content.clone()).collect();
    assert!(sent.contains("tabs, not spaces"), "recalled memory must reach the model:\n{sent}");
}

#[tokio::test]
async fn irrelevant_memory_stays_out_of_the_prompt() {
    let f = fixture();
    f.rook
        .remember(rook_core::Fact::new("the deploy key lives in 1password", rook_core::Scope::Global), None)
        .unwrap();
    f.rook
        .remember(rook_core::Fact::new("prefer ripgrep over grep", rook_core::Scope::Global), None)
        .unwrap();

    let session = f.rook.start_session("s").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("how do I deploy?").await.unwrap();

    let sent: String =
        seen.lock().unwrap().last().cloned().unwrap().messages.iter().map(|m| m.content.clone()).collect();
    assert!(sent.contains("deploy key"), "the matching fact should be recalled");
    assert!(!sent.contains("ripgrep"), "an unrelated fact must not ride along:\n{sent}");
}

#[tokio::test]
async fn a_pinned_fact_is_always_present() {
    let f = fixture();
    let mut fact = rook_core::Fact::new("never force-push to main", rook_core::Scope::Global);
    fact.pinned = true;
    f.rook.remember(fact, None).unwrap();

    let session = f.rook.start_session("s").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session)
        .run("something entirely unrelated to git")
        .await
        .unwrap();

    let sent: String =
        seen.lock().unwrap().last().cloned().unwrap().messages.iter().map(|m| m.content.clone()).collect();
    assert!(sent.contains("force-push"), "a pinned fact ignores relevance:\n{sent}");
}

#[tokio::test]
async fn remembering_the_same_thing_twice_does_not_duplicate_it() {
    let f = fixture();
    let fact = || rook_core::Fact::new("the build needs a C compiler", rook_core::Scope::Global);
    use rook_core::memory::Learned;
    assert_eq!(f.rook.remember(fact(), None).unwrap(), Learned::New);
    assert_eq!(
        f.rook.remember(fact().with_tags(vec!["build".into()]), None).unwrap(),
        Learned::Merged,
        "the repeat folds into the fact rather than making a second one"
    );

    let book = f.rook.memory().unwrap();
    assert_eq!(book.facts.len(), 1);
    assert_eq!(book.facts[0].tags, vec!["build"], "the repeat should merge its tags in");
}

#[tokio::test]
async fn memory_keeps_its_history_and_can_say_what_changed() {
    let f = fixture();
    f.rook
        .remember(rook_core::Fact::new("first thing", rook_core::Scope::Global), Some("one".into()))
        .unwrap();
    // A version is stamped in whole seconds, so a boundary between two of them
    // written in the same second does not exist to be asked about.
    let after_first = rook_store::now_unix();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    f.rook
        .remember(rook_core::Fact::new("second thing", rook_core::Scope::Global), Some("two".into()))
        .unwrap();
    f.rook.forget("first thing", Some("three".into())).unwrap();

    let history = f.rook.memory_history().unwrap();
    assert_eq!(history.len(), 3, "every change is a version");
    assert_eq!(history[0].note.as_deref(), Some("three"), "newest first");

    let changes = f.rook.memory_since(after_first).unwrap();
    let learned: Vec<_> = changes.iter().filter(|(c, _)| *c == rook_core::memory::Change::Learned).collect();
    let forgotten: Vec<_> =
        changes.iter().filter(|(c, _)| *c == rook_core::memory::Change::Forgotten).collect();
    assert_eq!(learned.len(), 1);
    assert_eq!(learned[0].1.text, "second thing");
    assert_eq!(forgotten.len(), 1);
    assert_eq!(forgotten[0].1.text, "first thing");
}

#[tokio::test]
async fn project_facts_do_not_leak_into_other_workspaces() {
    let f = fixture();
    let here = f.rook.workspace.display().to_string();
    f.rook
        .remember(rook_core::Fact::new("this repo deploys on fridays", rook_core::Scope::Project(here)), None)
        .unwrap();
    f.rook
        .remember(
            rook_core::Fact::new("elsewhere deploys on mondays", rook_core::Scope::Project("/other".into())),
            None,
        )
        .unwrap();

    let recalled = f.rook.recall("when does it deploy", 500).unwrap();
    assert_eq!(recalled.len(), 1, "{recalled:?}");
    assert!(recalled[0].text.contains("fridays"));
}

#[tokio::test]
async fn a_recalled_fact_says_why_it_matched() {
    let f = fixture();
    let fact = rook_core::Fact::new("run the migrations before deploying", rook_core::Scope::Global)
        .with_tags(vec!["deploy".into()]);
    f.rook.remember(fact, None).unwrap();

    let book = f.rook.memory().unwrap();
    let hits = rook_core::memory::search(book.facts.iter(), "how do I deploy");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].matched.contains(&"#deploy".to_string()), "{:?}", hits[0].matched);
    assert!(hits[0].score >= 2.0, "a tag hit should outrank a bare word");
}

#[tokio::test]
async fn a_delegated_task_runs_in_its_own_session_and_returns_only_its_conclusion() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("big.txt"), "a".repeat(50_000)).unwrap();
    let parent = f.rook.start_session("parent").unwrap();

    // The parent delegates; the child reads a large file and reports a summary.
    let provider = ScriptedProvider::new(vec![
        call("delegate", serde_json::json!({ "task": "survey big.txt and report its size" })),
        call("read_file", serde_json::json!({ "path": "big.txt" })),
        reply("big.txt is 50 kB of the letter a"),
        reply("the survey says it is 50 kB"),
    ]);
    let seen = provider.share();
    let outcome =
        AgentLoop::new(&f.rook, Arc::new(provider), parent).run("how big is big.txt?").await.unwrap();

    assert_eq!(outcome.delegated.len(), 1, "the child session should be reported");
    assert_eq!(outcome.reply, "the survey says it is 50 kB");

    let child = rook_store::parse_session_id(&outcome.delegated[0]).unwrap();
    let child_meta = f.rook.store.get_session(child).unwrap().unwrap();
    assert_eq!(child_meta.parent, Some(parent), "the child must be linked to its parent");
    assert!(child_meta.tags.contains(&"subtask".to_string()));

    // The 50 kB read happened in the child and never entered the parent's context.
    let parent_body: String =
        f.rook.transcript(parent, 0, usize::MAX, 100_000).unwrap().iter().map(|e| e.body.clone()).collect();
    assert!(!parent_body.contains(&"a".repeat(1000)), "the child's bulk must stay out of the parent");
    assert!(parent_body.contains("50 kB"), "but its conclusion must come back");

    let child_body: String =
        f.rook.transcript(child, 0, usize::MAX, 100_000).unwrap().iter().map(|e| e.body.clone()).collect();
    assert!(child_body.contains(&"a".repeat(1000)), "the detail is still readable in the child");

    // The child started from nothing: its first message is the task, not the parent's prompt.
    let child_request = seen.lock().unwrap()[1].clone();
    assert_eq!(child_request.messages[1].content, "survey big.txt and report its size");
    assert!(!child_request.messages.iter().any(|m| m.content.contains("how big is big.txt?")));
}

#[tokio::test]
async fn delegation_stops_nesting_at_the_depth_limit() {
    let f = fixture();
    let session = f.rook.start_session("deep").unwrap();

    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.depth = rook_core::agent::MAX_DEPTH;
    agent.run("go").await.unwrap();

    let requests = seen.lock().unwrap();
    let offered: Vec<&str> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        !offered.contains(&"delegate"),
        "an agent at the depth limit must not be offered delegation: {offered:?}"
    );
}

#[tokio::test]
async fn a_child_can_inherit_the_recent_conversation_when_asked() {
    let f = fixture();
    let parent = f.rook.start_session("parent").unwrap();
    f.rook.log(parent, rook_store::EventKind::UserMessage, "", "we are migrating to redb").unwrap();
    f.rook.log(parent, rook_store::EventKind::AssistantMessage, "m", "understood").unwrap();

    let provider = ScriptedProvider::new(vec![
        call("delegate", serde_json::json!({ "task": "check the migration", "context": "recent" })),
        reply("checked"),
        reply("done"),
    ]);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), parent).run("verify it").await.unwrap();

    let child = rook_store::parse_session_id(&outcome.delegated[0]).unwrap();
    let inherited: String =
        f.rook.transcript(child, 0, usize::MAX, 10_000).unwrap().iter().map(|e| e.body.clone()).collect();
    assert!(inherited.contains("migrating to redb"), "the inherited context should be there");
    let _ = seen;
}

#[tokio::test]
async fn an_unattended_run_refuses_what_it_cannot_get_approved() {
    let f = fixture();
    let session = f.rook.start_session("unattended").unwrap();
    let target = f.workspace.path().join("out.txt");

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "out.txt", "content": "x" })),
        reply("could not write"),
    ]));
    AgentLoop::new(&f.rook, provider, session).run("write out.txt").await.unwrap();

    assert!(!target.exists(), "nothing may run unreviewed when nothing can review it");
    let entries = f.rook.transcript(session, 0, usize::MAX, 4096).unwrap();
    let refusal = entries.iter().find(|e| e.kind == "tool-result").unwrap();
    assert!(refusal.body.contains("refused"), "{}", refusal.body);
    assert!(refusal.body.contains("--yes"), "the refusal must say how to proceed: {}", refusal.body);
}

#[tokio::test]
async fn allowing_everything_not_denied_lets_the_turn_through() {
    let f = fixture();
    let session = f.rook.start_session("approved").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "out.txt", "content": "x" })),
        reply("written"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("write out.txt").await.unwrap();

    assert_eq!(std::fs::read_to_string(f.workspace.path().join("out.txt")).unwrap(), "x");
}

#[tokio::test]
async fn the_deny_list_holds_even_with_everything_else_allowed() {
    let f = fixture();
    let mut config = Config::default();
    config.sandbox.deny = vec!["/rm -rf/".into()];
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("deny")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("deny").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("run_command", serde_json::json!({ "command": "rm -rf /tmp/whatever" })),
        reply("refused"),
    ]));
    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("clean up").await.unwrap();

    let entries = rook.transcript(session, 0, usize::MAX, 4096).unwrap();
    let result = entries.iter().find(|e| e.kind == "tool-result").unwrap();
    assert!(result.body.contains("refused"), "{}", result.body);
}

/// A provider that answers tool-free turns with canned text and records what it
/// was asked, so the summarisation call can be inspected.
fn long_session(f: &Fixture, turns: usize) -> u128 {
    let session = f.rook.start_session("long").unwrap();
    for i in 0..turns {
        f.rook
            .log(
                session,
                rook_store::EventKind::UserMessage,
                "",
                &format!("question {i}: {}", "x".repeat(400)),
            )
            .unwrap();
        f.rook
            .log(
                session,
                rook_store::EventKind::AssistantMessage,
                "m",
                &format!("answer {i}: {}", "y".repeat(400)),
            )
            .unwrap();
    }
    session
}

#[tokio::test]
async fn compaction_summarises_and_later_turns_start_from_the_summary() {
    let f = fixture();
    let session = long_session(&f, 40);

    let provider = ScriptedProvider::new(vec![
        reply("## Goal\nanswer the questions\n\n## Done\nanswered 0..39"),
        reply("carrying on"),
    ]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    let outcome = agent.run("and now?").await.unwrap();
    assert_eq!(outcome.compactions, 1, "a session this long must compact");

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "one summarisation call, then the turn itself");
    assert!(
        requests[0].messages[0].content.contains("## Goal"),
        "the first call is the summarisation, with its own instructions"
    );

    let turn = &requests[1];
    let carried: String = turn.messages.iter().map(|m| m.content.clone()).collect();
    assert!(carried.contains("answered 0..39"), "the summary must be carried into the turn");
    assert!(
        !carried.contains("question 0"),
        "the compacted span must not be carried; only the recent tail should remain"
    );
}

#[tokio::test]
async fn the_compaction_survives_into_a_later_process() {
    let f = fixture();
    let session = long_session(&f, 40);

    let first = ScriptedProvider::new(vec![reply("## Done\nthe important fact is 42"), reply("ok")]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(first), session);
    agent.set_window_for_test(4_000);
    agent.run("first").await.unwrap();

    // A fresh loop, as a later invocation would build: no compaction of its own,
    // but it must still start from the recorded summary.
    let second = ScriptedProvider::new(vec![reply("still going")]);
    let seen = second.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(second), session).run("second").await.unwrap();
    assert_eq!(outcome.compactions, 0, "the summary is already recorded; do not redo it");

    let carried: String =
        seen.lock().unwrap().last().cloned().unwrap().messages.iter().map(|m| m.content.clone()).collect();
    assert!(carried.contains("the important fact is 42"));
    assert!(!carried.contains("question 0"), "and the span it replaced stays out");
}

#[tokio::test]
async fn a_failed_summary_does_not_wedge_the_turn() {
    let f = fixture();
    let session = long_session(&f, 40);

    // The script runs out on the summarisation call, so the provider errors.
    let provider = ScriptedProvider::new(vec![]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    let result = agent.run("go").await;

    assert!(result.is_err(), "the turn itself still needs a provider");
    let entries = f.rook.transcript(session, 0, usize::MAX, 4096).unwrap();
    let compaction = entries.iter().find(|e| e.kind == "compaction").expect("compaction was recorded");
    assert!(
        compaction.body.contains("through_seq"),
        "with a position in it, or it frees nothing and the next turn does this again: {}",
        compaction.body
    );
    assert!(compaction.body.contains("could not be summarised"), "{}", compaction.body);
}

#[tokio::test]
async fn context_usage_counts_only_what_survives_compaction() {
    let f = fixture();
    let session = long_session(&f, 40);
    let before = f.rook.context_usage(session, 128_000).unwrap();

    let provider = ScriptedProvider::new(vec![reply("## Done\nshort"), reply("ok")]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    agent.run("go").await.unwrap();

    let after = f.rook.context_usage(session, 128_000).unwrap();
    assert!(
        after.live_tokens < before.live_tokens / 2,
        "compaction should cut what a turn carries: {} -> {}",
        before.live_tokens,
        after.live_tokens
    );
    assert!(after.logged_tokens > after.live_tokens, "and the log keeps everything");
    assert_eq!(after.compactions, 1);
}

fn hooked(f: &Fixture, hooks: Vec<rook_core::hooks::HookConfig>) -> Rook {
    let config = Config { hooks, ..Default::default() };
    Rook::from_parts(
        Store::open(f._store_dir.path().join(format!("h{}", hooks_seed()))).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    )
}

fn hooks_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn hook(event: rook_core::hooks::Event, command: &str) -> rook_core::hooks::HookConfig {
    rook_core::hooks::HookConfig { event, command: command.into(), timeout_secs: 10, ..Default::default() }
}

#[tokio::test]
async fn a_pre_tool_hook_can_block_a_call_the_policy_would_have_allowed() {
    let f = fixture();
    let rook = hooked(
        &f,
        vec![rook_core::hooks::HookConfig {
            matches: Some("write_file".into()),
            ..hook(rook_core::hooks::Event::PreTool, r#"echo '{"decision":"deny","reason":"frozen"}'"#)
        }],
    );
    let session = rook.start_session("blocked").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "out.txt", "content": "x" })),
        reply("blocked"),
    ]));
    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("write it").await.unwrap();

    assert!(!f.workspace.path().join("out.txt").exists());
    let entries = rook.transcript(session, 0, usize::MAX, 4096).unwrap();
    let refusal = entries.iter().find(|e| e.kind == "tool-result").unwrap();
    assert!(refusal.body.contains("frozen"), "the hook's reason must reach the model: {}", refusal.body);
}

#[tokio::test]
async fn a_hook_cannot_unlock_what_the_deny_list_forbids() {
    let f = fixture();
    let mut config = Config::default();
    config.sandbox.deny = vec!["/rm -rf/".into()];
    config.hooks = vec![hook(rook_core::hooks::Event::PreTool, r#"echo '{"decision":"allow"}'"#)];
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("hook-deny")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("deny").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("run_command", serde_json::json!({ "command": "rm -rf /tmp/x" })),
        reply("refused"),
    ]));
    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("clean").await.unwrap();

    let entries = rook.transcript(session, 0, usize::MAX, 4096).unwrap();
    let result = entries.iter().find(|e| e.kind == "tool-result").unwrap();
    assert!(result.body.contains("refused"), "{}", result.body);
    assert!(!result.body.contains("allow"), "an approval must never beat a denial");
}

#[tokio::test]
async fn a_post_tool_hook_appends_what_it_prints_to_the_result() {
    let f = fixture();
    let rook = hooked(&f, vec![hook(rook_core::hooks::Event::PostTool, "echo 'formatted with prettier'")]);
    let session = rook.start_session("post").unwrap();
    std::fs::write(f.workspace.path().join("a.txt"), "hello").unwrap();

    let provider =
        ScriptedProvider::new(vec![call("read_file", serde_json::json!({ "path": "a.txt" })), reply("read")]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&rook, Arc::new(provider), session);
    agent.allow_everything_not_denied();
    agent.run("read a.txt").await.unwrap();

    let sent: String =
        seen.lock().unwrap().last().cloned().unwrap().messages.iter().map(|m| m.content.clone()).collect();
    assert!(sent.contains("formatted with prettier"), "the hook's output must reach the model");
}

#[tokio::test]
async fn a_hook_that_fails_blocks_the_call_it_was_guarding() {
    let f = fixture();
    let rook = hooked(&f, vec![hook(rook_core::hooks::Event::PreTool, "exit 3")]);
    let session = rook.start_session("failing").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "out.txt", "content": "x" })),
        reply("ok"),
    ]));
    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("write").await.unwrap();

    assert!(
        !f.workspace.path().join("out.txt").exists(),
        "a guard that cannot run must not be taken as approval"
    );
}

#[tokio::test]
async fn a_prompt_hook_can_refuse_the_turn_and_add_context() {
    let f = fixture();
    let rook = hooked(
        &f,
        vec![rook_core::hooks::HookConfig {
            matches: Some("secret".into()),
            ..hook(rook_core::hooks::Event::Prompt, r#"echo '{"decision":"deny","reason":"not that"}'"#)
        }],
    );
    let session = rook.start_session("refused").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![reply("never reached")]));
    let error = AgentLoop::new(&rook, provider, session).run("tell me the secret").await.unwrap_err();
    assert!(error.to_string().contains("not that"), "{error}");
    assert!(rook.transcript(session, 0, usize::MAX, 100).unwrap().is_empty(), "nothing should be logged");
}

#[tokio::test]
async fn a_session_start_hook_contributes_to_the_system_prompt() {
    let f = fixture();
    let rook = hooked(&f, vec![hook(rook_core::hooks::Event::SessionStart, "echo 'this repo pins nightly'")]);
    let session = rook.start_session("start").unwrap();

    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&rook, Arc::new(provider), session).run("hello").await.unwrap();

    let system = seen.lock().unwrap().last().cloned().unwrap().messages[0].content.clone();
    assert!(system.contains("this repo pins nightly"), "{system}");
}

#[tokio::test]
async fn a_hook_matcher_keeps_it_off_calls_it_does_not_care_about() {
    let f = fixture();
    let rook = hooked(
        &f,
        vec![rook_core::hooks::HookConfig {
            matches: Some("/^run_command$/".into()),
            ..hook(rook_core::hooks::Event::PreTool, r#"echo '{"decision":"deny","reason":"no shell"}'"#)
        }],
    );
    let session = rook.start_session("matcher").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "fine.txt", "content": "x" })),
        reply("done"),
    ]));
    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("write").await.unwrap();

    assert!(f.workspace.path().join("fine.txt").exists(), "the matcher should have excluded this call");
}

/// A provider that records when each call started and finished, so overlap is
/// observable rather than assumed.
struct TimedProvider {
    script: Mutex<Vec<Response>>,
    spans: Arc<Mutex<Vec<(std::time::Instant, std::time::Instant)>>>,
    delay: std::time::Duration,
}

#[async_trait]
impl Provider for TimedProvider {
    fn id(&self) -> &str {
        "timed/test"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, _request: Request) -> rook_llm::Result<Response> {
        let started = std::time::Instant::now();
        tokio::time::sleep(self.delay).await;
        let response = {
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                return Err(LlmError::Other("out of script".into()));
            }
            script.remove(0)
        };
        self.spans.lock().unwrap().push((started, std::time::Instant::now()));
        Ok(response)
    }
}

#[tokio::test]
async fn several_sub_tasks_run_at_the_same_time() {
    let f = fixture();
    let session = f.rook.start_session("fanout").unwrap();

    // One delegation of three tasks: the parent's two calls plus three children.
    let mut script =
        vec![call("delegate", serde_json::json!({ "tasks": ["check a", "check b", "check c"] }))];
    script.extend((0..3).map(|i| reply(&format!("finding {i}"))));
    script.push(reply("all three checked"));

    let delay = std::time::Duration::from_millis(200);
    let spans = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(TimedProvider { script: Mutex::new(script), spans: spans.clone(), delay });

    let outcome = AgentLoop::new(&f.rook, provider, session).run("check three things").await.unwrap();

    assert_eq!(outcome.delegated.len(), 3, "every sub-task should report its session");
    assert!(outcome.reply.contains("all three checked"));

    // Overlapping spans, not elapsed time. A wall-clock bound measures the
    // machine: five 200ms calls exceed a one-second budget on a loaded box while
    // still running concurrently, which is the failure this test used to report.
    let spans = spans.lock().unwrap();
    let overlapping =
        spans.iter().enumerate().any(|(i, a)| spans.iter().skip(i + 1).any(|b| a.0 < b.1 && b.0 < a.1));
    assert!(overlapping, "no two model calls overlapped, so nothing actually ran in parallel");
}

#[tokio::test]
async fn the_parallel_limit_is_respected() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.max_parallel_subagents = 1;
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("serial")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("serial").unwrap();

    let mut script = vec![call("delegate", serde_json::json!({ "tasks": ["a", "b"] }))];
    script.extend((0..2).map(|i| reply(&format!("done {i}"))));
    script.push(reply("both"));

    let spans = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(TimedProvider {
        script: Mutex::new(script),
        spans: spans.clone(),
        delay: std::time::Duration::from_millis(80),
    });
    AgentLoop::new(&rook, provider, session).run("two things").await.unwrap();

    let spans = spans.lock().unwrap();
    let overlapping =
        spans.iter().enumerate().any(|(i, a)| spans.iter().skip(i + 1).any(|b| a.0 < b.1 && b.0 < a.1));
    assert!(!overlapping, "a limit of one must serialise them");
}

#[tokio::test]
async fn one_failing_sub_task_does_not_lose_the_others() {
    let f = fixture();
    let session = f.rook.start_session("partial").unwrap();

    // Two children, then the script runs dry so the third fails.
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("delegate", serde_json::json!({ "tasks": ["one", "two", "three"] })),
        reply("first answer"),
        reply("second answer"),
    ]));
    let outcome = AgentLoop::new(&f.rook, provider, session).run("three things").await;

    // The parent's own follow-up call also has no script left, so the turn ends
    // in error — but the tool result must already carry what did succeed.
    let entries = f.rook.transcript(session, 0, usize::MAX, 8000).unwrap();
    let result = entries
        .iter()
        .find(|e| e.kind == "tool-result")
        .expect("the delegation must have reported something");
    assert!(result.body.contains("answer"), "successful children must be reported: {}", result.body);
    assert!(result.body.contains("failed"), "and the failure named: {}", result.body);
    let _ = outcome;
}

#[tokio::test]
async fn a_single_task_still_works_unchanged() {
    let f = fixture();
    let session = f.rook.start_session("one").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("delegate", serde_json::json!({ "task": "look at one thing" })),
        reply("looked"),
        reply("reported"),
    ]));
    let outcome = AgentLoop::new(&f.rook, provider, session).run("look").await.unwrap();
    assert_eq!(outcome.delegated.len(), 1);
    assert_eq!(outcome.reply, "reported");
}

#[tokio::test]
async fn an_aside_sees_the_conversation_but_never_joins_it() {
    let f = fixture();
    let session = f.rook.start_session("aside").unwrap();

    let first = Arc::new(ScriptedProvider::new(vec![reply("I used exponential backoff")]));
    AgentLoop::new(&f.rook, first, session).run("add retries").await.unwrap();

    let asking = ScriptedProvider::new(vec![reply("because the server rate-limits")]);
    let seen = asking.share();
    let answer =
        AgentLoop::new(&f.rook, Arc::new(asking), session).aside("why backoff?", |_| {}).await.unwrap();
    assert_eq!(answer, "because the server rate-limits");

    let request = seen.lock().unwrap().last().cloned().unwrap();
    let carried: String = request.messages.iter().map(|m| m.content.clone()).collect();
    assert!(carried.contains("add retries"), "an aside must see the conversation");
    assert!(request.tools.is_empty(), "and must be given no tools to act with");

    // The next real turn must not see the aside at all.
    let next = ScriptedProvider::new(vec![reply("ok")]);
    let seen = next.share();
    AgentLoop::new(&f.rook, Arc::new(next), session).run("carry on").await.unwrap();
    let carried: String =
        seen.lock().unwrap().last().cloned().unwrap().messages.iter().map(|m| m.content.clone()).collect();
    assert!(!carried.contains("why backoff?"), "the aside leaked into the conversation:\n{carried}");
    assert!(!carried.contains("rate-limits"), "and so did its answer");
}

#[tokio::test]
async fn an_aside_is_still_recorded_for_the_transcript() {
    let f = fixture();
    let session = f.rook.start_session("aside").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("forty-two")]));
    AgentLoop::new(&f.rook, provider, session).aside("what is it?", |_| {}).await.unwrap();

    let note = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .find(|e| e.label == "btw")
        .expect("an aside must be auditable even though the model never sees it again");
    assert!(note.body.contains("what is it?"));
    assert!(note.body.contains("forty-two"));
}

#[tokio::test]
async fn the_session_goal_reaches_the_prompt_and_survives_the_session() {
    let f = fixture();
    let session = f.rook.start_session("goal").unwrap();
    f.rook.set_goal(session, "  make the parser handle CRLF  ").unwrap();
    assert_eq!(f.rook.goal(session).unwrap().as_deref(), Some("make the parser handle CRLF"));

    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("start").await.unwrap();

    let system = seen.lock().unwrap().last().cloned().unwrap().messages[0].content.clone();
    assert!(system.contains("make the parser handle CRLF"), "{system}");

    // Another session must not inherit it.
    let other = f.rook.start_session("other").unwrap();
    assert_eq!(f.rook.goal(other).unwrap(), None);
}

#[tokio::test]
async fn planning_is_asked_for_in_the_prompt_and_never_as_a_tool() {
    let f = fixture();
    let session = f.rook.start_session("plan").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("do a big thing").await.unwrap();

    let request = seen.lock().unwrap().last().cloned().unwrap();
    assert!(request.messages[0].content.contains("say the plan"), "the instruction is the mechanism");
    assert!(
        request.messages[0].content.contains("Do not keep a checklist"),
        "checklist bookkeeping is the cost this avoids"
    );
    let offered: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
    for name in ["todo", "todo_write", "update_plan", "plan"] {
        assert!(!offered.contains(&name), "a planning tool was offered: {offered:?}");
    }
}

#[tokio::test]
async fn planning_can_be_turned_off() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.plan_first = false;
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("noplan")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("plain").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&rook, Arc::new(provider), session).run("go").await.unwrap();

    let system = seen.lock().unwrap().last().cloned().unwrap().messages[0].content.clone();
    assert!(!system.contains("say the plan"));
}

#[tokio::test]
async fn a_delegated_child_shares_the_parent_language_servers() {
    let f = fixture();
    let session = f.rook.start_session("share").unwrap();
    let parent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![])), session);

    let pool = rook_core::agent::servers_for(&f.rook);
    let mut owner = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![])), session);
    owner.servers = pool.clone();

    assert!(!Arc::ptr_eq(&parent.servers, &pool), "a loop that was not given one builds its own");
    assert!(
        Arc::ptr_eq(&owner.servers, &pool),
        "and a front end that keeps a pool must be able to hand it over — \
         otherwise every turn restarts the language servers"
    );
}

#[tokio::test]
async fn the_system_prompt_does_not_vary_with_the_prompt() {
    let f = fixture();
    f.rook.remember(rook_core::Fact::new("deploys run on fridays", rook_core::Scope::Global), None).unwrap();
    let session = f.rook.start_session("stable").unwrap();

    let provider = ScriptedProvider::new(vec![reply("a"), reply("b")]);
    let seen = provider.share();
    let provider = Arc::new(provider);
    AgentLoop::new(&f.rook, provider.clone(), session).run("when do we deploy?").await.unwrap();
    AgentLoop::new(&f.rook, provider, session).run("what colour is the sky?").await.unwrap();

    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests[0].messages[0].content, requests[1].messages[0].content,
        "anything that varies per turn belongs after the cached prefix, not in the system block"
    );
    assert!(
        !requests[0].messages[0].content.contains("fridays"),
        "recalled memory varies with the prompt and must not sit at the front"
    );
    let carried: String = requests[0].messages.iter().map(|m| m.content.clone()).collect();
    assert!(carried.contains("fridays"), "but it must still reach the model");
}

#[tokio::test]
async fn tools_are_advertised_in_a_stable_order() {
    let f = fixture();
    let session = f.rook.start_session("order").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("go").await.unwrap();

    let names: Vec<String> = seen.lock().unwrap()[0].tools.iter().map(|t| t.name.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "tools render first; a reordered list invalidates the whole prefix");
}

#[tokio::test]
async fn the_conversation_so_far_is_marked_as_a_cacheable_prefix() {
    let f = fixture();
    let session = f.rook.start_session("cache").unwrap();
    let first = Arc::new(ScriptedProvider::new(vec![reply("noted")]));
    AgentLoop::new(&f.rook, first, session).run("remember this").await.unwrap();

    let second = ScriptedProvider::new(vec![reply("ok")]);
    let seen = second.share();
    AgentLoop::new(&f.rook, Arc::new(second), session).run("and now").await.unwrap();

    let messages = seen.lock().unwrap().last().cloned().unwrap().messages;
    let marked: Vec<usize> = messages.iter().enumerate().filter(|(_, m)| m.cache).map(|(i, _)| i).collect();
    assert!(
        marked.contains(&(messages.len() - 2)),
        "the turn before the newest one is where the prefix ends: {marked:?} of {}",
        messages.len()
    );
    assert!(!messages.last().unwrap().cache, "the newest turn is not a stable prefix");
}

#[tokio::test]
async fn a_sub_agent_runs_at_lower_effort_than_its_parent() {
    let f = fixture();
    let session = f.rook.start_session("effort").unwrap();
    let provider = ScriptedProvider::new(vec![
        call("delegate", serde_json::json!({ "task": "look something up" })),
        reply("found it"),
        reply("reported"),
    ]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("go").await.unwrap();

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests[0].effort, Some(rook_llm::Effort::High), "the parent uses the configured effort");
    assert_eq!(
        requests[1].effort,
        Some(rook_llm::Effort::Low),
        "a bounded errand does not need the parent's depth"
    );
}

#[tokio::test]
async fn the_configured_effort_reaches_the_request() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.effort = "max".into();
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("effort")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("e").unwrap();
    let provider = ScriptedProvider::new(vec![reply("ok")]);
    let seen = provider.share();
    AgentLoop::new(&rook, Arc::new(provider), session).run("go").await.unwrap();

    assert_eq!(seen.lock().unwrap()[0].effort, Some(rook_llm::Effort::Max));
}

fn transcript(f: &Fixture, title: &str, lines: &[(rook_store::EventKind, &str)]) -> u128 {
    let session = f.rook.start_session(title).unwrap();
    for (kind, body) in lines {
        f.rook.log(session, *kind, "", body).unwrap();
    }
    session
}

#[tokio::test]
async fn search_finds_a_line_across_sessions_and_ranks_the_best_one_first() {
    use rook_store::EventKind::{AssistantMessage, UserMessage};
    let f = fixture();
    transcript(
        &f,
        "old work",
        &[
            (UserMessage, "can you look at the CSV importer"),
            (AssistantMessage, "the importer trims whitespace before parsing"),
        ],
    );
    transcript(
        &f,
        "parser work",
        &[
            (UserMessage, "the parser mishandles CRLF"),
            (
                AssistantMessage,
                "fixed: the parser now splits on CRLF and on LF, so the parser is line-ending agnostic",
            ),
        ],
    );

    let found = f.rook.search("parser CRLF", &Default::default()).unwrap();
    assert!(!found.hits.is_empty(), "the words are right there");
    assert!(
        found.hits[0].snippet.contains("splits on CRLF"),
        "the line that dwells on the terms should outrank one that mentions them once: {:?}",
        found.hits[0].snippet
    );
    assert!(found.hits.iter().all(|h| !h.snippet.contains("whitespace")));
}

#[tokio::test]
async fn a_repeated_body_is_matched_once_and_reported_at_every_position() {
    use rook_store::EventKind::ToolResult;
    let f = fixture();
    let session = f.rook.start_session("repeats").unwrap();
    for _ in 0..5 {
        f.rook.log(session, ToolResult, "read_file", "the distinctive marker line").unwrap();
    }

    let found = f.rook.search("distinctive marker", &Default::default()).unwrap();
    assert_eq!(found.hits.len(), 5, "every position that references it is a hit");
    assert_eq!(found.objects_scanned, 1, "but content addressing means it is decompressed and matched once");
}

#[tokio::test]
async fn search_can_be_narrowed_to_one_session() {
    use rook_store::EventKind::UserMessage;
    let f = fixture();
    let wanted = transcript(&f, "a", &[(UserMessage, "the migration is nearly done")]);
    transcript(&f, "b", &[(UserMessage, "the migration was reverted")]);

    let options = rook_core::search::Search { session: Some(wanted), ..Default::default() };
    let found = f.rook.search("migration", &options).unwrap();
    assert_eq!(found.hits.len(), 1);
    assert!(found.hits[0].snippet.contains("nearly done"));
}

#[tokio::test]
async fn an_empty_query_finds_nothing_rather_than_everything() {
    let f = fixture();
    transcript(&f, "s", &[(rook_store::EventKind::UserMessage, "anything at all")]);
    for query in ["", "   ", "the and of"] {
        let found = f.rook.search(query, &Default::default()).unwrap();
        assert!(found.hits.is_empty(), "{query:?} matched: it is all noise words");
    }
}

#[tokio::test]
async fn a_bounded_scan_says_when_it_stopped_early() {
    let f = fixture();
    let session = f.rook.start_session("many").unwrap();
    for i in 0..40 {
        f.rook
            .log(session, rook_store::EventKind::UserMessage, "", &format!("entry {i} about widgets"))
            .unwrap();
    }

    let options = rook_core::search::Search { budget: 10, ..Default::default() };
    let found = f.rook.search("widgets", &options).unwrap();
    assert!(found.truncated, "a scan that hit its budget must say so, not look complete");
    assert!(found.objects_scanned <= 10);
}

#[tokio::test]
async fn a_session_can_show_exactly_what_it_changed() {
    use rook_core::changes::Change;
    let f = fixture();
    std::fs::write(f.workspace.path().join("kept.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(f.workspace.path().join("edited.txt"), "alpha\nbeta\n").unwrap();
    let session = f.rook.start_session("edits").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "edited.txt", "content": "alpha\nGAMMA\n" })),
        call("write_file", serde_json::json!({ "path": "created.txt", "content": "new\n" })),
        // Touched and put back: the diff must not claim it changed.
        call("write_file", serde_json::json!({ "path": "kept.txt", "content": "one\ntwo\nthree\n" })),
        reply("done"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("make the edits").await.unwrap();

    let changes = f.rook.changes(session, true).unwrap();
    let by_name = |name: &str| {
        changes.files.iter().find(|c| c.path.ends_with(name)).unwrap_or_else(|| panic!("{name} missing"))
    };

    assert_eq!(by_name("edited.txt").change, Change::Modified);
    assert_eq!(by_name("edited.txt").lines_added, 1);
    assert_eq!(by_name("edited.txt").lines_removed, 1);
    assert!(by_name("edited.txt").diff.as_ref().unwrap().contains("+GAMMA"));

    assert_eq!(by_name("created.txt").change, Change::Added);
    assert_eq!(by_name("created.txt").lines_added, 1);

    assert_eq!(
        by_name("kept.txt").change,
        Change::Unchanged,
        "a file written back identically was not changed by the session"
    );
    assert_eq!(changes.touched(), 2);
}

#[tokio::test]
async fn a_deleted_file_shows_as_removed() {
    let f = fixture();
    let target = f.workspace.path().join("doomed.txt");
    std::fs::write(&target, "here\n").unwrap();
    let session = f.rook.start_session("delete").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "doomed.txt", "content": "here\n" })),
        reply("touched"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("touch it").await.unwrap();
    std::fs::remove_file(&target).unwrap();

    let changes = f.rook.changes(session, false).unwrap();
    assert_eq!(changes.files[0].change, rook_core::changes::Change::Removed);
}

#[tokio::test]
async fn the_earliest_checkpoint_is_the_baseline_not_the_latest() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("f.txt"), "original\n").unwrap();
    let session = f.rook.start_session("twice").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "f.txt", "content": "first pass\n" })),
        call("write_file", serde_json::json!({ "path": "f.txt", "content": "second pass\n" })),
        reply("done"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("edit twice").await.unwrap();

    let diff = f.rook.changes(session, true).unwrap().files[0].diff.clone().unwrap();
    assert!(diff.contains("-original"), "the baseline is before the agent touched it:\n{diff}");
    assert!(diff.contains("+second pass"));
    assert!(!diff.contains("first pass"), "its own intermediate state is not a change it made");
}

#[tokio::test]
async fn a_binary_file_is_reported_as_changed_without_a_diff() {
    let f = fixture();
    let target = f.workspace.path().join("data.bin");
    std::fs::write(&target, [0u8, 1, 2, 3]).unwrap();
    let session = f.rook.start_session("binary").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "data.bin", "content": "text now" })),
        reply("done"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("overwrite").await.unwrap();

    let file = f.rook.changes(session, true).unwrap().files[0].clone();
    assert_eq!(file.change, rook_core::changes::Change::Modified);
    assert!(file.diff.is_none(), "rendering a binary diff into a terminal helps nobody");
}

#[tokio::test]
async fn a_second_compaction_keeps_what_the_first_one_summarised() {
    let f = fixture();
    let session = long_session(&f, 40);

    let first = ScriptedProvider::new(vec![reply("## Done\nthe API key lives in 1password"), reply("ok")]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(first), session);
    agent.set_window_for_test(4_000);
    let outcome = agent.run("first").await.unwrap();
    assert_eq!(outcome.compactions, 1);

    // Fill it up again so a second compaction has to happen.
    for i in 0..40 {
        f.rook
            .log(session, rook_store::EventKind::UserMessage, "", &format!("filler {i}: {}", "z".repeat(400)))
            .unwrap();
    }

    let second = ScriptedProvider::new(vec![reply("## Done\nfiller happened"), reply("ok")]);
    let seen = second.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(second), session);
    agent.set_window_for_test(4_000);
    let outcome = agent.run("second").await.unwrap();
    assert_eq!(outcome.compactions, 1, "it should have compacted again");

    let summarised: String = seen.lock().unwrap()[0].messages.iter().map(|m| m.content.clone()).collect();
    assert!(
        summarised.contains("1password"),
        "the second summarisation must be given the first summary, or what it covered is lost:\n{summarised:.600}"
    );

    // What the second summary *says* is the model's business; a scripted one
    // returns canned text. What is asserted here is the material it was given,
    // which is the part this code is responsible for.
    assert!(summarised.contains("filler"), "the raw span must be there too, not only the carried summary");
    assert!(
        summarised.find("1password") < summarised.find("filler"),
        "the carried summary comes first, so trimming takes the raw span and never it"
    );
}

#[tokio::test]
async fn a_skill_the_agent_writes_is_there_for_the_next_turn() {
    let f = fixture();
    let session = f.rook.start_session("authoring").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call(
            "write_skill",
            serde_json::json!({
                "name": "cross-compile-freebsd",
                "description": "Cross-compile for FreeBSD, which needs a C sysroot.",
                "body": "Install the sysroot first; `zstd-sys` and `ring` will not build without it.",
                "requires": { "language": { "rust": ">=1.85" } }
            }),
        ),
        reply("written down"),
    ]));

    // Writing a skill changes how every later session behaves, so it asks like
    // any other write.
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("remember how to do that").await.unwrap();

    assert_eq!(outcome.skills_written, ["cross-compile-freebsd"]);

    // The next turn's prompt, not this one's: the point of writing it down.
    let later = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply("ok")])), session);
    assert!(
        later.system_prompt().contains("cross-compile-freebsd"),
        "a written skill must reach the catalog without restarting"
    );
    assert!(f.rook.skill_history("cross-compile-freebsd").unwrap().len() == 1, "and be versioned");
}

#[tokio::test]
async fn a_skill_that_will_not_write_says_why_instead_of_failing_the_turn() {
    let f = fixture();
    let session = f.rook.start_session("authoring").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_skill", serde_json::json!({ "name": "../escape", "description": "x", "body": "y" })),
        reply("understood"),
    ]));

    let outcome = AgentLoop::new(&f.rook, provider, session).run("write a skill").await.unwrap();

    assert!(outcome.skills_written.is_empty());
    assert_eq!(outcome.reply, "understood", "the model gets to react rather than the turn dying");
}

/// Pseudo-tool schemas are on every request just as the toolbox's are, and are
/// easier to miss because they are written inline rather than measured by
/// `rook-tools`' example.
#[test]
fn the_pseudo_tool_schemas_stay_within_a_budget() {
    let f = fixture();
    let session = f.rook.start_session("budget").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
    let agent = AgentLoop::new(&f.rook, provider, session);

    let cost = |t: &rook_llm::ToolSpec| {
        (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4)
    };
    let total: usize = agent.tool_specs().iter().map(cost).sum();

    assert!(
        total < 800,
        "the advertised schemas cost ~{total} tokens on every request: {:?}",
        agent.tool_specs().iter().map(|t| (t.name.clone(), cost(t))).collect::<Vec<_>>()
    );
}

#[test]
fn a_lazily_advertised_tool_can_still_be_called() {
    let f = fixture();
    let session = f.rook.start_session("lazy").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
    let agent = AgentLoop::new(&f.rook, provider, session);
    assert!(f.rook.config.agent.lazy_tools, "this is the default, and what it costs is the point");

    for spec in agent.tool_specs() {
        let properties = spec.parameters["properties"].as_object().unwrap();
        assert!(
            !properties.is_empty(),
            "{} advertises no arguments, so the model would have to guess them",
            spec.name
        );
        for (name, schema) in properties {
            assert!(schema.get("type").is_some(), "{}.{name} has no type", spec.name);
        }
    }
}

/// Every disclosure flag must change what goes on the wire. `lazy_tools` and
/// `lazy_skills` both shipped as fields nothing read — a config knob that does
/// nothing is worse than one that is missing, because it is documented.
fn loop_for(f: &Fixture) -> AgentLoop<'_> {
    let session = f.rook.start_session("default").unwrap();
    AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply("ok")])), session)
}

fn with_config(f: &Fixture, name: &str, config: Config) -> Rook {
    let (skills, _) = SkillIndex::discover(&[(f._skill_dir.path().to_path_buf(), SkillSource::User)]);
    Rook::from_parts(
        Store::open(f._store_dir.path().join(name)).unwrap(),
        config,
        f.rook.env().clone(),
        skills,
        PathBuf::from(f.workspace.path()),
    )
}

#[test]
fn turning_off_lazy_skills_puts_the_bodies_in_the_prompt() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.lazy_skills = false;
    let rook = with_config(&f, "eager-skills", config);
    let session = rook.start_session("s").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));

    let eager = AgentLoop::new(&rook, provider, session).system_prompt();
    let lazy = loop_for(&f).system_prompt();

    assert!(eager.contains("Always greet in the user's own language"), "inline: {eager}");
    assert!(!lazy.contains("Always greet"), "and lazily it must not be");
    assert!(lazy.contains("greeting: How to greet"), "lazily it is a card");
}

#[test]
fn turning_off_lazy_tools_restores_the_argument_descriptions() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.lazy_tools = false;
    let rook = with_config(&f, "eager-tools", config);
    let session = rook.start_session("s").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));

    let eager = AgentLoop::new(&rook, provider, session).tool_specs();
    let read = |specs: &[rook_llm::ToolSpec]| {
        specs.iter().find(|s| s.name == "read_file").unwrap().parameters.to_string()
    };

    assert!(read(&eager).contains("description"), "eager schemas carry their guidance");
    assert!(!read(&loop_for(&f).tool_specs()).contains("description"), "lazy ones do not");
}

#[tokio::test]
async fn remembering_something_already_said_names_the_older_fact() {
    let f = fixture();
    let session = f.rook.start_session("memory").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("remember", serde_json::json!({"text": "deployments go through staging first"})),
        call("remember", serde_json::json!({"text": "deployments first go through staging"})),
        reply("noted"),
    ]));

    AgentLoop::new(&f.rook, provider.clone(), session).run("remember both").await.unwrap();

    let said = provider.share();
    let requests = said.lock().unwrap();
    let last = requests.last().unwrap().messages.iter().rev().find(|m| m.role == Role::Tool).unwrap();
    assert!(last.content.contains("close to ["), "the restatement must be flagged: {}", last.content);
    assert!(last.content.contains("forget"), "and say what to do about it: {}", last.content);
}

#[test]
fn the_catalog_names_only_what_the_model_can_act_on() {
    let f = fixture();
    let prompt = loop_for(&f).system_prompt();

    let catalog = prompt.split("## Skills").nth(1).unwrap();
    assert!(catalog.contains("- greeting:"), "{catalog}");
    assert!(
        !catalog.contains("1.0.0"),
        "load_skill takes a name and resolve picks the version, so a version here is \
         cost with no effect: {catalog}"
    );
}

#[tokio::test]
async fn a_post_tool_hook_is_given_the_facts_the_tool_measured() {
    let f = fixture();
    // Echo the payload's meta back as context, which is the only way a hook can
    // show what it was handed.
    let rook = hooked(
        &f,
        vec![hook(
            rook_core::hooks::Event::PostTool,
            r#"python3 -c "import json,sys; p=json.load(sys.stdin); print(json.dumps({'context': f\"meta={p['meta']} error={p['is_error']}\"}))""#,
        )],
    );
    let session = rook.start_session("meta").unwrap();
    std::fs::write(f.workspace.path().join("notes.txt"), "one\ntwo\nthree\n").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({"path": "notes.txt"})),
        reply("read it"),
    ]));
    AgentLoop::new(&rook, provider.clone(), session).run("read the notes").await.unwrap();

    let sent = provider.share();
    let requests = sent.lock().unwrap();
    let result = requests.last().unwrap().messages.iter().rev().find(|m| m.role == Role::Tool).unwrap();
    assert!(result.content.contains("total_lines"), "the hook saw no meta: {}", result.content);
    assert!(result.content.contains("error=False"), "nor the error flag: {}", result.content);
}

#[tokio::test]
async fn readonly_stops_the_agent_writing_a_skill_as_well_as_a_file() {
    let f = fixture();
    let mut config = Config::default();
    config.sandbox.mode = rook_tools::policy::Mode::ReadOnly;
    let rook = with_config(&f, "readonly-skill", config);
    let session = rook.start_session("readonly").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_skill", serde_json::json!({"name": "sneaky", "description": "d", "body": "b"})),
        call("write_file", serde_json::json!({"path": "sneaky.txt", "content": "x"})),
        reply("done"),
    ]));
    let outcome = AgentLoop::new(&rook, provider, session).run("go").await.unwrap();

    assert!(outcome.skills_written.is_empty(), "readonly means nothing changes the machine");
    assert_eq!(rook.skills().versions_of("sneaky").len(), 0, "and nothing reached the disk");
    assert!(!f.workspace.path().join("sneaky.txt").exists());
}

#[tokio::test]
async fn a_skill_is_still_written_when_the_policy_allows_it() {
    let f = fixture();
    let mut config = Config::default();
    config.sandbox.mode = rook_tools::policy::Mode::Auto;
    let rook = with_config(&f, "auto-skill", config);
    let session = rook.start_session("auto").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_skill", serde_json::json!({"name": "allowed", "description": "d", "body": "b"})),
        reply("done"),
    ]));
    let outcome = AgentLoop::new(&rook, provider, session).run("go").await.unwrap();

    assert_eq!(outcome.skills_written, ["allowed"], "gating it must not disable it");
    assert_eq!(rook.skills().versions_of("allowed").len(), 1, "and it reached the disk");
}

#[tokio::test]
async fn a_turn_reports_a_tool_finishing_as_well_as_starting() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("a.txt"), "x\n").unwrap();
    let session = f.rook.start_session("progress").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({"path": "a.txt"})),
        call("read_file", serde_json::json!({"path": "missing.txt"})),
        reply("done"),
    ]));

    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    let mut seen: Vec<String> = Vec::new();
    agent
        .run_with("read them", |progress| match progress {
            rook_core::agent::Progress::Delta(rook_llm::Delta::ToolCall(c)) => {
                seen.push(format!("start {}", c.name))
            }
            rook_core::agent::Progress::ToolDone { name, failed } => {
                seen.push(format!("done {name} failed={failed}"))
            }
            _ => {}
        })
        .await
        .unwrap();

    assert_eq!(
        seen,
        ["start read_file", "done read_file failed=false", "start read_file", "done read_file failed=true",],
        "every call is followed by the report that it finished, and by how"
    );
}

#[tokio::test]
async fn a_loaded_skill_names_the_files_bundled_with_it() {
    let f = fixture();
    let dir = f._skill_dir.path().join("greeting");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/check.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.join("references/spec.md"), "spec").ok();
    let (skills, _) =
        SkillIndex::discover(&[(f._skill_dir.path().to_path_buf(), rook_skills::SkillSource::User)]);
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("bundled")).unwrap(),
        Config::default(),
        f.rook.env().clone(),
        skills,
        PathBuf::from(f.workspace.path()),
    );

    let session = rook.start_session("bundled").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("load_skill", serde_json::json!({"name": "greeting"})),
        reply("read it"),
    ]));
    AgentLoop::new(&rook, provider.clone(), session).run("greet someone").await.unwrap();

    let sent = provider.share();
    let requests = sent.lock().unwrap();
    let loaded = requests.last().unwrap().messages.iter().rev().find(|m| m.role == Role::Tool).unwrap();

    assert!(loaded.content.contains("Always greet"), "the body is still there: {}", loaded.content);
    assert!(loaded.content.contains("scripts/check.sh"), "a bundled script must be named");
    assert!(
        loaded.content.contains(dir.canonicalize().unwrap().to_str().unwrap())
            || loaded.content.contains(dir.to_str().unwrap()),
        "and its directory, or the path in the body cannot be followed: {}",
        loaded.content
    );
}

#[tokio::test]
async fn a_skill_that_is_only_a_markdown_file_gains_nothing() {
    let f = fixture();
    let session = f.rook.start_session("plain").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("load_skill", serde_json::json!({"name": "greeting"})),
        reply("read it"),
    ]));
    AgentLoop::new(&f.rook, provider.clone(), session).run("greet someone").await.unwrap();

    let sent = provider.share();
    let requests = sent.lock().unwrap();
    let loaded = requests.last().unwrap().messages.iter().rev().find(|m| m.role == Role::Tool).unwrap();

    assert!(!loaded.content.contains("Bundled with"), "most skills pay nothing: {}", loaded.content);
}

#[tokio::test]
async fn compaction_summarises_only_what_the_model_was_shown() {
    let f = fixture();
    let session = f.rook.start_session("noise").unwrap();
    // A conversation, and the bookkeeping the log keeps alongside it.
    // The bookkeeping goes first, so it lands in the span that gets summarised
    // rather than in the live tail — where it would be excluded anyway and the
    // test would prove nothing.
    f.rook
        .log(session, rook_store::EventKind::Note, "write_skill", "wrote skill \"unrelated-bookkeeping\"")
        .unwrap();
    f.rook
        .log(session, rook_store::EventKind::Error, "load_skill", "could not load skill \"missing\"")
        .unwrap();
    f.rook
        .log(session, rook_store::EventKind::Checkpoint, "before", r#"{"root":"/tmp/ws","files":{}}"#)
        .unwrap();
    // Enough that the live tail cannot hold it all, or there is nothing to
    // summarise and the test proves nothing either.
    for i in 0..40 {
        let question = format!("question {i} about the parser. {}", "detail ".repeat(80));
        f.rook.log(session, rook_store::EventKind::UserMessage, "prompt", &question).unwrap();
        f.rook.log(session, rook_store::EventKind::AssistantMessage, "m", &format!("answer {i}")).unwrap();
    }

    let provider = Arc::new(ScriptedProvider::new(vec![reply("a summary"), reply("done")]));
    AgentLoop::new(&f.rook, provider.clone(), session).compact_now().await;

    let sent = provider.share();
    let requests = sent.lock().unwrap();
    let material = &requests[0].messages.last().unwrap().content;

    assert!(material.contains("about the parser"), "the conversation must be in it");
    assert!(!material.contains("unrelated-bookkeeping"), "an aside is not conversation: {material}");
    assert!(!material.contains("could not load skill"), "nor is an error: {material}");
    assert!(!material.contains("\"root\""), "nor a checkpoint manifest: {material}");
}

#[tokio::test]
async fn what_context_reports_is_what_a_turn_actually_carries() {
    let f = fixture();
    let session = f.rook.start_session("cost").unwrap();
    f.rook.log(session, rook_store::EventKind::UserMessage, "prompt", "the question").unwrap();
    f.rook.log(session, rook_store::EventKind::AssistantMessage, "m", "the answer").unwrap();

    let before = f.rook.context_usage(session, 100_000).unwrap().live_tokens;

    // Bookkeeping the model never sees: an aside, a failed load, a manifest.
    f.rook.log(session, rook_store::EventKind::Note, "aside", &"noise ".repeat(500)).unwrap();
    f.rook.log(session, rook_store::EventKind::Error, "load_skill", &"more ".repeat(500)).unwrap();
    f.rook.log(session, rook_store::EventKind::Checkpoint, "before", &"manifest ".repeat(500)).unwrap();

    let after = f.rook.context_usage(session, 100_000).unwrap();

    assert_eq!(after.live_tokens, before, "none of that reaches a turn, so none of it is its cost");
    assert!(after.logged_tokens > after.live_tokens, "but it is still what the store holds");
}

#[tokio::test]
async fn the_reported_cost_matches_the_request_that_gets_built() {
    let f = fixture();
    let session = f.rook.start_session("cost").unwrap();
    f.rook.log(session, rook_store::EventKind::UserMessage, "prompt", "the question").unwrap();
    f.rook.log(session, rook_store::EventKind::Note, "aside", &"noise ".repeat(500)).unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
    let mut agent = AgentLoop::new(&f.rook, provider.clone(), session);
    let reported = f.rook.context_usage(session, 100_000).unwrap().live_tokens;
    agent.run("go").await.unwrap();

    let sent = provider.share();
    let carried: usize = sent.lock().unwrap()[0]
        .messages
        .iter()
        .skip(1) // the system prompt is not part of the log
        .map(|m| rook_core::context::estimate_tokens(&m.content))
        .sum();

    // The prompt just run is logged too, so the request carries a little more.
    assert!(carried <= reported + 20, "context says {reported} tokens and the request carries {carried}");
}

#[tokio::test]
async fn a_delegation_reports_each_sub_task_as_it_lands() {
    let f = fixture();
    let session = f.rook.start_session("fanout").unwrap();
    let mut script =
        vec![call("delegate", serde_json::json!({ "tasks": ["check a", "check b", "check c"] }))];
    script.extend((0..3).map(|i| reply(&format!("finding {i}"))));
    script.push(reply("all three checked"));

    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let mut reported: Vec<String> = Vec::new();
    agent
        .run_with("check three things", |progress| {
            if let rook_core::agent::Progress::Delegated { task, done, total } = progress {
                reported.push(format!("{done}/{total} {task}"));
            }
        })
        .await
        .unwrap();

    assert_eq!(reported.len(), 3, "one report per sub-task: {reported:?}");
    assert!(reported[0].starts_with("1/3"), "counted as they land: {reported:?}");
    assert!(reported[2].starts_with("3/3"), "{reported:?}");
    let mut named: Vec<&str> = reported.iter().map(|r| &r[4..]).collect();
    named.sort();
    assert_eq!(named, ["check a", "check b", "check c"], "each names its own task");
}

#[tokio::test]
async fn the_report_keeps_the_order_the_tasks_were_asked_in() {
    let f = fixture();
    let session = f.rook.start_session("fanout").unwrap();
    let mut script = vec![call("delegate", serde_json::json!({ "tasks": ["first", "second"] }))];
    script.extend(["answer to first", "answer to second"].map(reply));
    script.push(reply("done"));

    let outcome = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session)
        .run("two things")
        .await
        .unwrap();

    // The sub-tasks finish in whatever order they finish; the report must not.
    let transcript = f.rook.transcript(session, 0, 200, 4000).unwrap();
    let report = transcript.iter().rev().find(|e| e.label == "delegate").unwrap();
    let first = report.body.find("### first").unwrap();
    let second = report.body.find("### second").unwrap();
    assert!(first < second, "{}", report.body);
    assert_eq!(outcome.delegated.len(), 2);
}

/// A checkpoint that fails takes the session's undo with it: `session rewind`
/// restores from these, so a file edited without one is edited for good. That
/// was a line in the log file, where neither the model nor the user was looking.
#[tokio::test]
async fn an_edit_that_could_not_be_checkpointed_says_so_where_it_will_be_read() {
    let f = fixture();
    let big = f.workspace.path().join("fixture.json");
    std::fs::write(&big, "x".repeat(9 << 20)).unwrap();

    let session = f.rook.start_session("").unwrap();
    let provider = ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "fixture.json", "content": "{}" })),
        reply("done"),
    ]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.allow_everything_not_denied();
    agent.run("shrink it").await.unwrap();

    let transcript = f.rook.transcript(session, 0, 100, 4096).unwrap();
    let told_the_model = transcript.iter().find(|e| e.kind.contains("result")).expect("the call happened");
    assert!(
        told_the_model.body.contains("cannot undo this one"),
        "the model has to know an edit is final: {}",
        told_the_model.body
    );
    assert!(
        transcript.iter().any(|e| e.label == "checkpoint"),
        "and the session says it, so `session show` does too"
    );
    assert_eq!(std::fs::read_to_string(&big).unwrap(), "{}", "the edit still happened");
}

/// A sub-task can run for minutes, and the parent only heard when it landed —
/// so the counter sat still, which reads the same as a hang.
#[tokio::test]
async fn a_sub_task_says_what_it_is_doing_before_it_is_done() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("a.txt"), "one\n").unwrap();
    let session = f.rook.start_session("watching").unwrap();

    let script = vec![
        call("delegate", serde_json::json!({ "tasks": ["look at a.txt"] })),
        call("read_file", serde_json::json!({ "path": "a.txt" })),
        reply("it says one"),
        reply("done"),
    ];

    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let mut doing: Vec<String> = Vec::new();
    agent
        .run_with("look", |progress| {
            if let rook_core::agent::Progress::Delegating { task, tool } = progress {
                doing.push(format!("{task}: {tool}"));
            }
        })
        .await
        .unwrap();

    assert_eq!(doing, ["look at a.txt: read_file"], "the parent sees the child working: {doing:?}");
}

/// A hook that never answers must not become an approval by default. It is
/// bounded by its own `timeout_secs`, and the timeout is a failure like any
/// other — which for `pre_tool` means no.
#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_that_hangs_refuses_rather_than_waits_or_allows() {
    let f = fixture();
    let rook = hooked(
        &f,
        vec![rook_core::hooks::HookConfig {
            timeout_secs: 1,
            ..hook(rook_core::hooks::Event::PreTool, "sleep 30")
        }],
    );
    let session = rook.start_session("hung hook").unwrap();
    let started = std::time::Instant::now();

    let script =
        vec![call("write_file", serde_json::json!({ "path": "a.txt", "content": "hi" })), reply("refused")];
    let mut agent = AgentLoop::new(&rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("write it").await.unwrap();

    assert!(started.elapsed() < std::time::Duration::from_secs(20), "it waited the hook out");
    assert!(
        !f.workspace.path().join("a.txt").exists(),
        "a hook that could not answer is not an answer of yes"
    );
}

/// codex #41118 propagates the parent's trusted skills to a delegated worker.
/// Here a child resolves against the same index and the same environment, so
/// what the parent could load the child can — including a skill written during
/// the run, since the index is behind a lock rather than copied.
#[tokio::test]
async fn a_sub_task_can_load_the_skills_its_parent_could() {
    let f = fixture();
    let session = f.rook.start_session("delegating").unwrap();

    let script = vec![
        call("delegate", serde_json::json!({ "tasks": ["greet someone"] })),
        call("load_skill", serde_json::json!({ "name": "greeting" })),
        reply("greeted"),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("delegate a greeting").await.unwrap();

    let child = outcome.delegated.first().expect("a sub-task ran");
    let child = rook_store::parse_session_id(child).unwrap();
    let loaded = f.rook.transcript(child, 0, 100, 4096).unwrap();
    assert!(
        loaded.iter().any(|e| e.kind == "skill" && e.label.starts_with("greeting@")),
        "the child loaded the parent's skill, at a version: {:?}",
        loaded.iter().map(|e| (&e.kind, &e.label)).collect::<Vec<_>>()
    );
}

/// Answers the turn and refuses the summarisation, which is the shape of a
/// transient provider failure at exactly the wrong moment.
struct NoSummaries(ScriptedProvider);

#[async_trait]
impl Provider for NoSummaries {
    fn id(&self) -> &str {
        "scripted/no-summaries"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        if request.messages.first().is_some_and(|m| m.content.contains("compacting an agent")) {
            return Err(LlmError::Other("the summariser is down".into()));
        }
        self.0.complete(request).await
    }
}

/// A failed summary was recorded as a compaction whose body was a sentence
/// rather than a position. Nothing could read a position out of it, so the span
/// was never dropped: the next turn compacted again, and the one after that,
/// each adding an event and freeing nothing.
#[tokio::test]
async fn a_compaction_whose_summary_failed_still_moves_the_session_on() {
    let f = fixture();
    let session = long_session(&f, 40);

    let provider = NoSummaries(ScriptedProvider::new(vec![reply("carrying on")]));
    let seen = provider.0.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    agent.run("and now?").await.unwrap();

    let (from, summary) = f.rook.last_compaction(session).unwrap();
    assert!(from > 0, "the position was recorded, so the span is behind us");
    let summary = summary.expect("something stands in for the span");
    assert!(summary.contains("could not be summarised"), "{summary}");
    assert!(summary.contains("session show"), "and where to read what it stood for: {summary}");

    let turn = seen.lock().unwrap().clone();
    let carried: String = turn[0].messages.iter().map(|m| m.content.clone()).collect();
    assert!(!carried.contains("question 0"), "the span is not carried: {carried}");
}

/// A session with nothing worth compacting recorded a compaction anyway, which
/// poisoned the position it was supposed to describe.
#[tokio::test]
async fn nothing_worth_compacting_records_no_compaction() {
    let f = fixture();
    let session = f.rook.start_session("short").unwrap();

    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply("hi")])), session);
    agent.set_window_for_test(4_000);
    agent.compact_now().await;

    assert_eq!(f.rook.last_compaction(session).unwrap(), (0, None), "nothing was compacted");
    let entries = f.rook.transcript(session, 0, 100, 512).unwrap();
    assert!(entries.iter().all(|e| e.kind != "compaction"), "{entries:?}");
}

/// A live model filled both fields of every call with the same instruction —
/// differing only in whether the function name wore backticks — and both were
/// taken: every sub-task ran twice, for twice the tokens and twice the wait,
/// with one of each pair thrown away. Three files became six sub-agents.
#[tokio::test]
async fn the_same_task_asked_for_twice_is_one_sub_task() {
    let f = fixture();
    let session = f.rook.start_session("duplicated").unwrap();

    let script = vec![
        call(
            "delegate",
            serde_json::json!({
                "task": "check `a.py` for the empty-list bug",
                "tasks": ["check a.py for the empty-list bug"],
            }),
        ),
        reply("a.py divides by zero"),
        reply("one file, one answer"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("check it").await.unwrap();

    assert_eq!(outcome.delegated.len(), 1, "one task said twice is one task: {:?}", outcome.delegated);
}

#[tokio::test]
async fn a_list_of_tasks_is_run_in_full() {
    let f = fixture();
    let session = f.rook.start_session("several").unwrap();

    let mut script =
        vec![call("delegate", serde_json::json!({ "tasks": ["check a.py", "check b.py", "check c.py"] }))];
    script.extend((0..3).map(|i| reply(&format!("finding {i}"))));
    script.push(reply("all three"));

    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("check them").await.unwrap();

    assert_eq!(outcome.delegated.len(), 3, "{:?}", outcome.delegated);
}

#[tokio::test]
async fn a_bare_task_still_works_on_its_own() {
    let f = fixture();
    let session = f.rook.start_session("one").unwrap();

    let script =
        vec![call("delegate", serde_json::json!({ "task": "check a.py" })), reply("checked"), reply("done")];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("check it").await.unwrap();

    assert_eq!(outcome.delegated.len(), 1, "the single-task shape is still accepted");
}

/// The parameter was an enum of two words, and a live model filled it with the
/// file it had just read — expecting the sub-task to start with it. The value
/// was dropped and the child read the same file again, a step the parent had
/// already paid for.
#[tokio::test]
async fn context_the_parent_writes_out_reaches_the_sub_task() {
    let f = fixture();
    let session = f.rook.start_session("handing over").unwrap();

    let script = vec![
        call(
            "delegate",
            serde_json::json!({
                "tasks": ["say what is wrong with it"],
                "context": "a.py contains: def mean(xs): return sum(xs) / len(xs)",
            }),
        ),
        reply("it divides by zero on an empty list"),
        reply("passed on"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("check a.py").await.unwrap();

    let child = rook_store::parse_session_id(&outcome.delegated[0]).unwrap();
    let handed = f.rook.transcript(child, 0, 100, 4096).unwrap();
    assert!(
        handed.iter().any(|e| e.label == "inherited" && e.body.contains("def mean")),
        "the child starts with what the parent already knew: {:?}",
        handed.iter().map(|e| (&e.kind, &e.label)).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn recent_is_still_the_word_for_the_conversation_so_far() {
    let f = fixture();
    let session = f.rook.start_session("carrying").unwrap();
    f.rook.log(session, rook_store::EventKind::UserMessage, "user", "the build is broken").unwrap();

    let script = vec![
        call("delegate", serde_json::json!({ "tasks": ["look into it"], "context": "recent" })),
        reply("looked"),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("why").await.unwrap();

    let child = rook_store::parse_session_id(&outcome.delegated[0]).unwrap();
    let handed = f.rook.transcript(child, 0, 100, 4096).unwrap();
    assert!(
        handed.iter().any(|e| e.label == "inherited" && e.body.contains("the build is broken")),
        "{handed:?}"
    );
}
