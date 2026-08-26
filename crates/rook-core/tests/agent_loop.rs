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
    let prompt = agent.system_prompt("hi");

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
        f.rook.env.clone(),
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

    let system = seen.lock().unwrap().last().cloned().unwrap().messages[0].content.clone();
    assert!(system.contains("tabs, not spaces"), "recalled memory belongs in the prompt:\n{system}");
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

    let system = seen.lock().unwrap().last().cloned().unwrap().messages[0].content.clone();
    assert!(system.contains("deploy key"), "the matching fact should be recalled");
    assert!(!system.contains("ripgrep"), "an unrelated fact must not ride along:\n{system}");
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

    let system = seen.lock().unwrap().last().cloned().unwrap().messages[0].content.clone();
    assert!(system.contains("force-push"), "a pinned fact ignores relevance:\n{system}");
}

#[tokio::test]
async fn remembering_the_same_thing_twice_does_not_duplicate_it() {
    let f = fixture();
    let fact = || rook_core::Fact::new("the build needs a C compiler", rook_core::Scope::Global);
    assert!(f.rook.remember(fact(), None).unwrap());
    assert!(!f.rook.remember(fact().with_tags(vec!["build".into()]), None).unwrap());

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
    let after_first = rook_store::now_unix();
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
        f.rook.env.clone(),
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
        compaction.body.contains("without a summary"),
        "a failed summary must be recorded as such: {}",
        compaction.body
    );
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
        f.rook.env.clone(),
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
        f.rook.env.clone(),
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
