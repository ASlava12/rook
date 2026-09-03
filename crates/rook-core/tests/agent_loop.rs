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
            reasoning: Vec::new(),
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

/// A stance is worth nothing if a sub-agent can be given more of one than the
/// turn that started it. Today they share the parent's policy, which makes
/// that true by construction — this says so out loud, so that giving children
/// a policy of their own has to keep it true.
#[tokio::test]
async fn a_sub_agent_is_never_given_more_latitude_than_the_turn_that_started_it() {
    let f = fixture();
    let mut config = Config::default();
    config.sandbox.stance = rook_tools::policy::Stance::ReadOnly;
    let rook = with_config(&f, "read-only parent", config);
    let session = rook.start_session("s").unwrap();
    std::fs::write(rook.workspace.join("notes.txt"), "original\n").unwrap();

    let provider = Arc::new(ByPrompt(vec![
        ("rewrite it", call("delegate", serde_json::json!({ "tasks": ["rewrite notes.txt"] }))),
        ("read-only", reply("I was not allowed to")),
        (
            "rewrite notes.txt",
            call("write_file", serde_json::json!({ "path": "notes.txt", "content": "changed\n" })),
        ),
        ("### rewrite", reply("it could not")),
    ]));

    let mut agent = AgentLoop::new(&rook, provider, session);
    agent.policy = rook_core::agent::policy_for(&rook.config);
    agent.run("rewrite it").await.unwrap();

    assert_eq!(
        std::fs::read_to_string(rook.workspace.join("notes.txt")).unwrap(),
        "original\n",
        "the child inherited a stance that changes nothing, and changed nothing"
    );
}

/// Autonomy is a task and its boundaries, and the boundary has to be held by
/// something other than the turn's own opinion of its work. Before an
/// autonomous turn with a goal ends, a checker asks whether the goal is met;
/// `fails` gives the turn one more go, with the reason.
#[tokio::test]
async fn an_autonomous_turn_is_checked_against_its_goal_before_it_may_end() {
    let f = fixture();
    let session = f.rook.start_session("s").unwrap();
    f.rook.set_goal(session, "notes.txt must say done").unwrap();

    // Most specific first: the failed check quotes the checker, which quotes
    // the file, so a rule keyed on the file's content would answer for both.
    let provider = Arc::new(ByPrompt(vec![
        (
            "Checked against the goal before finishing",
            call("write_file", serde_json::json!({ "path": "notes.txt", "content": "done" })),
        ),
        ("the agent has just finished a turn", call("read_file", serde_json::json!({ "path": "notes.txt" }))),
        ("half", reply("VERDICT: fails — notes.txt says half, not done")),
        ("overwrote", reply("fixed: it said half")),
        ("created", reply("done")),
        (
            "make it say done",
            call("write_file", serde_json::json!({ "path": "notes.txt", "content": "half" })),
        ),
    ]));

    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("make it say done").await.unwrap();

    assert_eq!(std::fs::read_to_string(f.workspace.path().join("notes.txt")).unwrap(), "done");
    assert_eq!(outcome.reply, "fixed: it said half", "put right and said what was wrong");
    let notes: Vec<String> = f
        .rook
        .transcript(session, 0, 200, 512)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "note" && e.label == "goal check")
        .map(|e| e.body)
        .collect();
    assert_eq!(notes.len(), 1, "checked once, and the check is on the record: {notes:?}");
    assert!(notes[0].contains("fails"), "{notes:?}");
}

/// Somebody answering the approval prompt, one way or the other.
struct Answers(rook_tools::policy::Approval);

#[async_trait]
impl rook_tools::policy::Approver for Answers {
    async fn ask(
        &self,
        _tool: &str,
        _risk: &rook_tools::policy::Risk,
        _preview: Option<&str>,
    ) -> rook_tools::policy::Approval {
        self.0.clone()
    }
}

/// A refusal nobody made is a different thing from one somebody made, and the
/// end of a run one was not watching has to say which: the first is still a
/// question, the second is settled.
#[tokio::test]
async fn what_nobody_could_answer_is_an_open_question_and_what_somebody_refused_is_a_decision() {
    let f = fixture();
    let call_then_stop = || {
        Arc::new(ScriptedProvider::new(vec![
            call("write_file", serde_json::json!({ "path": "notes.txt", "content": "x" })),
            reply("I could not"),
        ]))
    };

    let unattended = f.rook.start_session("unattended").unwrap();
    let outcome = AgentLoop::new(&f.rook, call_then_stop(), unattended).run("write it").await.unwrap();
    assert_eq!(outcome.decisions, Vec::<String>::new(), "nobody decided anything");
    assert_eq!(outcome.open_questions.len(), 1, "{:?}", outcome.open_questions);
    assert!(outcome.open_questions[0].contains("write_file"), "{:?}", outcome.open_questions);

    let attended = f.rook.start_session("attended").unwrap();
    let mut agent = AgentLoop::new(&f.rook, call_then_stop(), attended);
    agent.approver = Arc::new(Answers(rook_tools::policy::Approval::declined()));
    let outcome = agent.run("write it").await.unwrap();
    assert_eq!(outcome.open_questions, Vec::<String>::new(), "a person answered, so nothing is open");
    assert_eq!(outcome.decisions.len(), 1, "{:?}", outcome.decisions);
    assert!(outcome.decisions[0].contains("write_file"), "{:?}", outcome.decisions);
}

/// The agent may ask for more latitude; it may not take it. A person grants
/// it for the rest of the run, nobody being there leaves the question open,
/// and a stance is only ever asked up.
#[tokio::test]
async fn a_stance_is_raised_by_a_person_and_never_by_the_agent() {
    use rook_tools::policy::{Approval, Stance};
    let f = fixture();
    let asks = || {
        Arc::new(ScriptedProvider::new(vec![
            call("stance", serde_json::json!({ "to": "autonomous", "why": "to finish the migration" })),
            reply("thanks"),
        ]))
    };

    let granted = f.rook.start_session("granted").unwrap();
    let mut agent = AgentLoop::new(&f.rook, asks(), granted);
    agent.approver = Arc::new(Answers(Approval::Once));
    let outcome = agent.run("go").await.unwrap();
    assert_eq!(agent.policy.stance(), Stance::Autonomous, "granted for the rest of the run");
    assert!(outcome.decisions.iter().any(|d| d.contains("autonomous")), "{:?}", outcome.decisions);

    let nobody = f.rook.start_session("nobody").unwrap();
    let mut agent = AgentLoop::new(&f.rook, asks(), nobody);
    let outcome = agent.run("go").await.unwrap();
    assert_eq!(agent.policy.stance(), Stance::Assist, "nobody was there, so nothing changed");
    assert!(outcome.open_questions.iter().any(|q| q.contains("autonomous")), "{:?}", outcome.open_questions);

    let down = f.rook.start_session("down").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        call("stance", serde_json::json!({ "to": "readonly", "why": "safer" })),
        reply("ok"),
    ]));
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, provider, down);
    agent.approver = Arc::new(Answers(Approval::Once));
    agent.run("go").await.unwrap();
    let told = seen.lock().unwrap()[1].messages.last().unwrap().content.clone();
    assert!(told.contains("only ever asked up"), "{told}");
    assert_eq!(agent.policy.stance(), Stance::Assist);
}

/// A checker that reached for nothing is reported as unproven whatever it
/// said, and unproven is neither a pass nor a fail: it is the question the
/// person has to settle, and it must reach them.
#[tokio::test]
async fn a_goal_check_that_could_not_settle_is_an_open_question() {
    let f = fixture();
    let session = f.rook.start_session("s").unwrap();
    f.rook.set_goal(session, "notes.txt must say done").unwrap();
    let provider = Arc::new(ByPrompt(vec![
        ("the agent has just finished a turn", reply("VERDICT: holds")),
        ("created", reply("done")),
        (
            "make it say done",
            call("write_file", serde_json::json!({ "path": "notes.txt", "content": "done" })),
        ),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("make it say done").await.unwrap();

    assert_eq!(outcome.reply, "done", "an unsettled check does not cost the turn another go");
    assert_eq!(outcome.open_questions.len(), 1, "{:?}", outcome.open_questions);
    assert!(outcome.open_questions[0].contains("could not be settled"), "{:?}", outcome.open_questions);
}

/// Answers by what it was last asked rather than by position.
///
/// With a parent and its sub-agent both calling, the order they arrive in is
/// the thing under test and cannot also be the thing the script depends on — a
/// positional script makes the race decide who gets which reply.
struct ByPrompt(Vec<(&'static str, Response)>);

#[async_trait]
impl Provider for ByPrompt {
    fn id(&self) -> &str {
        "scripted/by-prompt"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let last = request.messages.last().map(|m| m.content.clone()).unwrap_or_default();
        match self.0.iter().find(|(when, _)| last.contains(when)) {
            Some((_, answer)) => Ok(answer.clone()),
            None => Err(LlmError::Other(format!("nothing to say to {last:?}"))),
        }
    }
}

/// A parent blocked on its children cannot look at them, cannot redirect one,
/// and cannot do anything else while they run. Started rather than waited for,
/// they advance during the parent's own model call.
#[tokio::test]
async fn a_turn_goes_on_while_the_sub_agents_it_started_are_still_running() {
    let f = fixture();
    let session = f.rook.start_session("s").unwrap();
    // Most specific first: the report of a task quotes the task, so a rule
    // keyed on the task alone would answer for both.
    let provider = Arc::new(ByPrompt(vec![
        ("count the files", call("delegate", serde_json::json!({ "tasks": ["tally"], "wait": false }))),
        ("started: task01", call("subagents", serde_json::json!({ "id": "task01", "wait_secs": 20 }))),
        ("there are three", reply("it says three")),
        ("tally", reply("there are three")),
    ]));

    let outcome = AgentLoop::new(&f.rook, provider, session).run("count the files").await.unwrap();

    assert_eq!(outcome.reply, "it says three");
    assert_eq!(outcome.tools_called, ["delegate", "subagents"]);
    assert_eq!(outcome.delegated.len(), 1, "the child's cost is the turn's");
}

/// The point of starting them rather than waiting: a parent that sees a child
/// going the wrong way can say so while it is still going, instead of reading
/// the finished wrong answer.
#[tokio::test]
async fn a_parent_can_redirect_a_sub_agent_that_is_still_running() {
    let f = fixture();
    std::fs::write(f.workspace.path().join("a.txt"), "contents of a").unwrap();
    let session = f.rook.start_session("s").unwrap();

    // The child reads the same file until it is told otherwise, so the remark
    // arriving is what ends it rather than a guess about scheduling.
    let provider = Arc::new(ByPrompt(vec![
        ("stop, tally instead", reply("switched to tallying")),
        (
            "started: task01",
            call("subagents", serde_json::json!({ "id": "task01", "say": "stop, tally instead" })),
        ),
        (
            "sees it at its next step",
            call("subagents", serde_json::json!({ "id": "task01", "wait_secs": 20 })),
        ),
        ("switched to tallying", reply("it switched")),
        ("redirect it", call("delegate", serde_json::json!({ "tasks": ["walk"], "wait": false }))),
        ("walk", call("read_file", serde_json::json!({ "path": "a.txt" }))),
        ("contents of a", call("read_file", serde_json::json!({ "path": "a.txt" }))),
    ]));

    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("redirect it").await.unwrap();

    assert_eq!(outcome.reply, "it switched", "the parent saw what the child did after being told");
    let child = rook_store::parse_session_id(&outcome.delegated[0]).unwrap();
    let heard: Vec<String> =
        f.rook.transcript(child, 0, 200, 512).unwrap().iter().map(|e| e.body.clone()).collect();
    assert!(
        heard.iter().any(|body| body.contains("stop, tally instead")),
        "the remark reached the child while it was running: {heard:?}"
    );
}

/// Reversibility is a property of the system, not of one session: a turn that
/// delegated the writing is a turn whose rewind has to undo it. The child works
/// in a forked session, so its checkpoints are in a log the parent's rewind was
/// not reading.
#[tokio::test]
async fn a_rewind_undoes_what_the_turn_delegated_as_well_as_what_it_did() {
    let f = fixture();
    let target = f.workspace.path().join("notes.txt");
    std::fs::write(&target, "original\n").unwrap();
    let session = f.rook.start_session("delegate an edit").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        // In order, and the child is served in the middle: the parent asks for
        // the delegation, the child does the work, and the parent then speaks.
        call("delegate", serde_json::json!({ "tasks": ["rewrite notes.txt"] })),
        call("write_file", serde_json::json!({ "path": "notes.txt", "content": "rewritten\n" })),
        reply("rewritten"),
        reply("the sub-agent did it"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("have someone rewrite notes.txt").await.unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "rewritten\n", "the child did the work");

    let kinds: Vec<String> =
        f.rook.transcript(session, 0, 200, 256).unwrap().iter().map(|e| e.kind.clone()).collect();
    assert!(!kinds.contains(&"checkpoint".to_string()), "the parent wrote nothing itself: {kinds:?}");

    let report = f.rook.rewind(session, 1, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "original\n",
        "rewinding the turn that delegated must undo what was delegated: {report:?}"
    );
}

/// The point of having a tool for it: `rm` through the shell declares no path,
/// so nothing is captured and the file is simply gone.
#[tokio::test]
async fn a_rewind_brings_back_a_file_the_agent_deleted() {
    let f = fixture();
    let target = f.workspace.path().join("dead.rs");
    std::fs::write(&target, "fn old() {}\n").unwrap();
    let session = f.rook.start_session("delete").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("delete_file", serde_json::json!({ "path": "dead.rs" })),
        reply("gone"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("drop dead.rs").await.unwrap();
    assert!(!target.exists(), "it has to have been deleted for this to be about anything");

    let entries = f.rook.transcript(session, 0, 100, 4096).unwrap();
    let checkpoint = entries.iter().find(|e| e.kind == "checkpoint").expect("a deletion checkpoints");
    let report = f.rook.rewind(session, checkpoint.seq, true).unwrap();

    assert_eq!(report.files_restored, 1);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn old() {}\n");
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

    let usage = f.rook.context_usage(session, Some(128_000)).unwrap();
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
    // The date travels beside the newest prompt and folds into it, which is why
    // the count of turns above is unchanged by it.
    assert!(request.messages[3].content.ends_with("what is my name?"), "{}", request.messages[3].content);
    assert!(request.messages[3].content.starts_with("Today is"), "{}", request.messages[3].content);
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
    assert!(
        child_request.messages[1].content.ends_with("survey big.txt and report its size"),
        "the task is the child's first turn, after the date that travels beside every prompt: {}",
        child_request.messages[1].content
    );
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
    let before = f.rook.context_usage(session, Some(128_000)).unwrap();

    let provider = ScriptedProvider::new(vec![reply("## Done\nshort"), reply("ok")]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    agent.run("go").await.unwrap();

    let after = f.rook.context_usage(session, Some(128_000)).unwrap();
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

/// A hook command that prints `json` back, spelled for the shell the platform
/// runs hooks through. A hook command is shell text, and the two shells do not
/// agree: `sh -c` strips the double quotes out of a bare argument, and `cmd /C`
/// keeps the single ones — so one spelling that looks portable is simply wrong
/// on one of them, silently, by returning a reply that will not parse.
fn prints(json: &str) -> String {
    match cfg!(windows) {
        true => format!("echo {json}"),
        false => format!("echo '{json}'"),
    }
}

/// A hook command that keeps the payload it was handed, so a test can assert on
/// the payload itself. Deliberately a shell builtin rather than an interpreter:
/// a stock Windows and a stock FreeBSD have no `python3`.
fn keeps_its_payload(at: &std::path::Path) -> String {
    let at = at.display();
    match cfg!(windows) {
        true => format!("findstr /R \"^\" > \"{at}\""),
        false => format!("cat > '{at}'"),
    }
}

/// A hook's stdout becomes context the model carries for the rest of the
/// session, and it was read whole: `cat` on the wrong file spent the window.
///
/// Asked of the hooks directly rather than through a turn, because the cap is
/// generous enough that a fixture's small window would refuse the request
/// before the assertion was reached — which is its own answer, and not this one.
#[cfg(unix)]
#[tokio::test]
async fn a_hook_that_will_not_stop_talking_is_cut_off() {
    let (hooks, bad) = rook_core::hooks::Hooks::compile(&[hook(
        rook_core::hooks::Event::SessionStart,
        // Far past what a reply may be, and cheap to produce. `head` closes the
        // pipe, so `yes` is stopped by its own reader rather than by ours.
        "yes chatter | head -c 4000000",
    )]);
    assert!(bad.is_empty(), "{bad:?}");

    let ran = std::time::Instant::now();
    let outcome = hooks.run(rook_core::hooks::Event::SessionStart, "", &serde_json::json!({})).await;
    let said = outcome.context().expect("the hook did run");

    assert!(said.starts_with("chatter"), "and what it said is kept: {}", &said[..40.min(said.len())]);
    assert!(said.len() <= 64 * 1024, "up to the cap and no further: {} bytes", said.len());
    // Reading only the first bytes would leave the writer blocked on a full pipe
    // and every hook would end at its timeout instead.
    assert!(ran.elapsed() < std::time::Duration::from_secs(5), "it took {:?}", ran.elapsed());
}

#[tokio::test]
async fn a_pre_tool_hook_can_block_a_call_the_policy_would_have_allowed() {
    let f = fixture();
    let rook = hooked(
        &f,
        vec![rook_core::hooks::HookConfig {
            matches: Some("write_file".into()),
            ..hook(rook_core::hooks::Event::PreTool, &prints(r#"{"decision":"deny","reason":"frozen"}"#))
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
    config.hooks = vec![hook(rook_core::hooks::Event::PreTool, &prints(r#"{"decision":"allow"}"#))];
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
    // Assisting, not autonomous: a read needs no approval, and an autonomous
    // turn ends with a goal check whose request is not the one being read.
    let mut agent = AgentLoop::new(&rook, Arc::new(provider), session);
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
            ..hook(rook_core::hooks::Event::Prompt, &prints(r#"{"decision":"deny","reason":"not that"}"#))
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
            ..hook(rook_core::hooks::Event::PreTool, &prints(r#"{"decision":"deny","reason":"no shell"}"#))
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

    let pool = rook_core::agent::servers_for(&f.rook.config, &f.rook.workspace);
    let mut owner = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![])), session);
    owner.servers = pool.clone();

    assert!(!Arc::ptr_eq(&parent.servers, &pool), "a loop that was not given one builds its own");
    assert!(
        Arc::ptr_eq(&owner.servers, &pool),
        "and a front end that keeps a pool must be able to hand it over — \
         otherwise every turn restarts the language servers"
    );
}

/// The convention every reference agent already reads, and this one read none.
#[tokio::test]
async fn a_projects_own_instructions_reach_the_model_under_both_names() {
    let f = fixture();
    std::fs::write(rook_core::paths::home().join("AGENTS.md"), "Prefer tabs everywhere.\n").unwrap();
    std::fs::write(f.workspace.path().join("AGENTS.md"), "This project uses spaces.\n").unwrap();

    let session = f.rook.start_session("instructed").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
    let prompt = AgentLoop::new(&f.rook, provider, session).system_prompt();

    assert!(prompt.contains("Prefer tabs everywhere"), "the user's own instructions: {prompt}");
    assert!(prompt.contains("This project uses spaces"), "and the project's: {prompt}");
    let general = prompt.find("Prefer tabs").unwrap();
    let specific = prompt.find("This project uses").unwrap();
    assert!(general < specific, "most general first, so the project has the last word: {prompt}");

    std::fs::remove_file(rook_core::paths::home().join("AGENTS.md")).unwrap();
}

/// A file in a repository is written by whoever sends the pull request, and it
/// is paid for on every single request.
#[tokio::test]
async fn instructions_a_repository_committed_cannot_spend_the_context_window() {
    let mut f = fixture();
    f.rook.config.agent.max_instructions_bytes = 64;
    let huge = format!("{}\nthe part past the limit\n", "x".repeat(4096));
    std::fs::write(f.workspace.path().join("AGENTS.md"), &huge).unwrap();
    assert!(huge.len() > f.rook.config.agent.max_instructions_bytes, "the file has to exceed the limit");

    let session = f.rook.start_session("bounded").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
    let prompt = AgentLoop::new(&f.rook, provider, session).system_prompt();

    assert!(!prompt.contains("the part past the limit"), "what was cut must not be there: {prompt}");
    assert!(prompt.contains("max_instructions_bytes"), "and the cut must be named: {prompt}");
    // Counted from the file's length: past the cap the rest is never read.
    let past = huge.len() - f.rook.config.agent.max_instructions_bytes;
    assert!(prompt.contains(&format!("{past} more bytes")), "and counted from the whole: {prompt}");

    // A byte that is not text is not a reason to follow nothing and say nothing
    // about why.
    std::fs::write(f.workspace.path().join("AGENTS.md"), b"tabs, not \xff spaces\n").unwrap();
    let prompt =
        AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply("ok")])), session).system_prompt();
    assert!(prompt.contains("tabs, not"), "{prompt}");
}

/// A turn is not a wall: somebody watching one go the wrong way could only stop
/// it and start again, losing everything it had done to say one sentence.
#[tokio::test]
async fn something_said_while_a_turn_runs_reaches_the_next_step() {
    let f = fixture();
    let session = f.rook.start_session("steered").unwrap();
    let provider = ScriptedProvider::new(vec![
        call("list_dir", serde_json::json!({ "path": "." })),
        reply("understood"),
    ]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    let said = agent.interjections.clone();

    // Said during the turn, which is the whole case: before it starts it would
    // simply be part of the prompt, and consecutive user turns are folded into
    // one anyway.
    let saying = said.clone();
    agent
        .run_with("look around", |progress| {
            if let rook_core::agent::Progress::ToolDone { .. } = progress {
                saying.say("actually, look at src/ instead");
            }
        })
        .await
        .unwrap();

    let second = &seen.lock().unwrap()[1].messages;
    let roles: Vec<_> = second.iter().map(|m| (m.role, m.content.contains("src/ instead"))).collect();
    assert!(roles.iter().any(|(_, said)| *said), "it has to reach the model: {roles:?}");
    // Between an assistant's tool call and its result, no dialect accepts a user
    // message — so it goes after the result and before the next request.
    let at = second.iter().position(|m| m.content.contains("src/ instead")).unwrap();
    assert_eq!(second[at].role, rook_llm::Role::User, "{roles:?}");
    assert_eq!(second[at - 1].role, rook_llm::Role::Tool, "it must follow the tool result: {roles:?}");
    assert_eq!(at, second.len() - 1, "and be the last thing said: {roles:?}");

    assert!(said.take().is_empty(), "and be delivered once, not at every step");
}

/// Said while the model was writing its last answer, it would sit in the queue
/// until the next prompt and be folded into it — which is not where it was put.
#[tokio::test]
async fn something_said_as_a_turn_ends_keeps_the_turn_going() {
    let f = fixture();
    let session = f.rook.start_session("steered late").unwrap();
    let provider = ScriptedProvider::new(vec![reply("all done"), reply("right, changed course")]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    let said = agent.interjections.clone();

    let saying = said.clone();
    let mut once = true;
    let outcome = agent
        .run_with("look around", |progress| {
            if let rook_core::agent::Progress::Delta(_) = progress
                && std::mem::take(&mut once)
            {
                saying.say("wait — the other directory");
            }
        })
        .await
        .unwrap();

    assert_eq!(outcome.steps, 2, "the turn went on rather than ending on the model's word");
    assert_eq!(outcome.reply, "right, changed course");
    let second = &seen.lock().unwrap()[1].messages;
    assert!(second.last().unwrap().content.contains("the other directory"), "{second:?}");
    assert!(said.take().is_empty(), "and nothing is left for the next prompt to inherit");
}

/// A call cut off at the output limit arrives with arguments that will not
/// parse. Nobody should be asked to approve running the empty string, and the
/// model should be told what it actually got wrong.
#[tokio::test]
async fn a_tool_call_whose_arguments_did_not_parse_says_that_and_asks_nobody() {
    let f = fixture();
    let session = f.rook.start_session("truncated").unwrap();
    let mut truncated = call("run_command", serde_json::json!({}));
    truncated.message.tool_calls[0].arguments = serde_json::Value::Null;

    let provider = ScriptedProvider::new(vec![truncated, reply("sorry, again")]);
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    // Refuses everything and says so, so an approval that was asked for would
    // show up as that refusal instead.
    agent.approver = Arc::new(rook_tools::policy::Unattended);

    agent.run("do something").await.unwrap();

    let said: String = f
        .rook
        .transcript(session, 0, 100, 4096)
        .unwrap()
        .iter()
        .map(|e| e.body.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(said.contains("not valid JSON"), "the model has to be told what it got wrong: {said}");
    assert!(!said.contains("needs someone to approve"), "and nobody is asked to approve it: {said}");
}

/// Two results carrying one id is a request every dialect rejects, so the
/// model's mistake would come back as an opaque error from the provider after
/// the work had already been done.
#[tokio::test]
async fn two_tool_calls_sharing_an_id_do_not_make_an_unsendable_request() {
    let f = fixture();
    let session = f.rook.start_session("duplicate ids").unwrap();
    let mut both = call("list_dir", serde_json::json!({ "path": "." }));
    let first = both.message.tool_calls[0].clone();
    both.message.tool_calls.push(rook_llm::ToolCall { id: first.id.clone(), ..first });

    let provider = ScriptedProvider::new(vec![both, reply("noted")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("look").await.unwrap();

    let second = &seen.lock().unwrap()[1].messages;
    let ids: Vec<&Option<String>> =
        second.iter().filter(|m| m.role == rook_llm::Role::Tool).map(|m| &m.tool_call_id).collect();
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "one result per id, or the next request is unsendable: {ids:?}");

    let calls = second.iter().filter(|m| m.role == rook_llm::Role::Assistant).flat_map(|m| &m.tool_calls);
    assert_eq!(calls.count(), 1, "and the message that asked must not carry the duplicate either");
    let said: String = second.iter().map(|m| m.content.clone()).collect();
    assert!(said.contains("same call id"), "the model has to be told which call was dropped: {said}");
}

/// A model can end a turn with nothing in it, and every front end renders that
/// as a hang: the prompt, then no answer and no error.
#[tokio::test]
async fn a_turn_that_said_nothing_says_that_rather_than_nothing() {
    let f = fixture();
    let session = f.rook.start_session("silent").unwrap();

    let outcome = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply("")])), session)
        .run("say something")
        .await
        .unwrap();

    assert!(outcome.reply.contains("without saying anything"), "{:?}", outcome.reply);
    assert!(outcome.reply.contains(&outcome.stopped), "and why it ended: {:?}", outcome.reply);

    // The transcript records what the model said, and it said nothing.
    let entries = f.rook.transcript(session, 0, 100, 4096).unwrap();
    assert!(entries.iter().all(|e| e.kind != "assistant"), "{entries:?}");
}

/// A session resumed a week later replayed as though it had paused for a
/// moment, and what the agent should do next often depends on which it was.
#[tokio::test]
async fn a_conversation_picked_up_days_later_says_so() {
    let f = fixture();
    let session = f.rook.start_session("resumed").unwrap();
    f.rook.log(session, rook_store::EventKind::UserMessage, "", "did you run the tests?").unwrap();

    // The store stamps an event as it is written, so an old exchange is written
    // as one: the record carries the time, and the replay reads it back.
    f.rook
        .store
        .append_event(
            session,
            rook_store::NewEvent::new(
                rook_store::EventKind::AssistantMessage,
                rook_store::Kind::Message,
                b"yes, all green",
            )
            .at(rook_store::now_unix() + 3 * 24 * 3600),
        )
        .unwrap();

    let provider = ScriptedProvider::new(vec![reply("that was a while ago")]);
    let seen = provider.share();
    AgentLoop::new(&f.rook, Arc::new(provider), session).run("and now?").await.unwrap();

    let carried: String = seen.lock().unwrap()[0].messages.iter().map(|m| m.content.clone()).collect();
    assert!(carried.contains("3 days later"), "{carried}");
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

/// The order was pinned and the list was not. A tool appearing or changing
/// between turns invalidates everything behind it just as surely as reordering
/// them, and the list is assembled from more places than the order is: the
/// toolbox, the language servers, every MCP server, and the loop's own.
#[tokio::test]
async fn the_tools_a_session_advertises_do_not_change_between_its_turns() {
    let f = fixture();
    let session = f.rook.start_session("stable tools").unwrap();
    let provider = ScriptedProvider::new(vec![reply("a"), reply("b")]);
    let seen = provider.share();
    let provider = Arc::new(provider);
    AgentLoop::new(&f.rook, provider.clone(), session).run("first").await.unwrap();
    AgentLoop::new(&f.rook, provider, session).run("second").await.unwrap();

    let requests = seen.lock().unwrap().clone();
    let rendered = |request: &rook_llm::Request| {
        request.tools.iter().map(|t| serde_json::to_string(t).unwrap()).collect::<Vec<_>>()
    };
    assert!(!rendered(&requests[0]).is_empty(), "a request with no tools would pass this saying nothing");
    assert_eq!(
        rendered(&requests[0]),
        rendered(&requests[1]),
        "a list that differs between turns invalidates the prefix behind it"
    );
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

/// The size cap was applied to bytes already in memory, which is not a cap: the
/// file had to be read whole in order to decide it would not be diffed.
#[tokio::test]
async fn a_file_too_large_to_diff_still_says_whether_it_changed() {
    let f = fixture();
    let target = f.workspace.path().join("big.bin");
    // Past the 256 KiB the diff gives up at, and poorly compressible so it is a
    // real read either way.
    let bulk: String = (0..300_000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    std::fs::write(&target, &bulk).unwrap();
    let session = f.rook.start_session("bulk").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("write_file", serde_json::json!({ "path": "big.bin", "content": bulk.clone() })),
        reply("left it alone"),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.allow_everything_not_denied();
    agent.run("rewrite it with what it already says").await.unwrap();

    // Written with the same content: hashing says so, and nothing else could
    // without holding both halves at once.
    let same = f.rook.changes(session, true).unwrap();
    assert_eq!(same.files[0].change, rook_core::changes::Change::Unchanged, "{:?}", same.files[0]);
    assert!(same.files[0].diff.is_none(), "and nothing that large is rendered");

    std::fs::write(
        &target,
        format!(
            "{bulk}and one more line
"
        ),
    )
    .unwrap();
    let moved = f.rook.changes(session, true).unwrap();
    assert_eq!(moved.files[0].change, rook_core::changes::Change::Modified, "{:?}", moved.files[0]);
    assert_eq!(moved.files[0].path, "big.bin", "and it is still named");
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

/// The loop's own tools are written inline rather than measured by
/// `rook-tools`' example, so they are the easy ones to let grow. Only those are
/// priced here: the whole list has one budget already, in `config_is_wired`,
/// and this used to be a second number for the same question — it sat under
/// its limit only because this fixture registers no `ask`.
#[test]
fn the_loops_own_tool_schemas_stay_within_a_budget() {
    use rook_core::agent::{
        DELEGATE, FIND_SKILL, FORGET, LOAD_SKILL, RECALL, REMEMBER, STANCE, SUBAGENTS, VERIFY, WRITE_SKILL,
    };
    let f = fixture();
    let session = f.rook.start_session("budget").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("ok")]));
    let agent = AgentLoop::new(&f.rook, provider, session);

    let own =
        [LOAD_SKILL, WRITE_SKILL, FIND_SKILL, REMEMBER, FORGET, RECALL, DELEGATE, SUBAGENTS, STANCE, VERIFY];
    let cost = |t: &rook_llm::ToolSpec| {
        (t.name.len() + t.description.len() + t.parameters.to_string().len()).div_ceil(4)
    };
    let priced: Vec<(String, usize)> = agent
        .tool_specs()
        .iter()
        .filter(|t| own.contains(&t.name.as_str()))
        .map(|t| (t.name.clone(), cost(t)))
        .collect();
    assert_eq!(priced.len(), own.len(), "every one of the loop's tools is advertised here: {priced:?}");
    let total: usize = priced.iter().map(|(_, c)| c).sum();

    assert!(total < 500, "the loop's own schemas cost ~{total} tokens on every request: {priced:?}");
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

/// Some OpenAI-compatible endpoints refuse a request carrying `tools` at all,
/// and the model behind one can still use them if they are in the prompt. The
/// loop above the provider must not be able to tell which happened.
#[tokio::test]
async fn tools_an_endpoint_cannot_be_sent_are_put_in_the_prompt_and_read_back() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.native_tools = false;
    let rook = with_config(&f, "no-native-tools", config);
    let session = rook.start_session("s").unwrap();

    std::fs::write(rook.workspace.join("answer.txt"), "forty-two").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        reply(
            "Let me look.\n```json\n\
             {\"tool\": \"read_file\", \"arguments\": {\"path\": \"answer.txt\"}}\n```",
        ),
        reply("it says forty-two"),
    ]));
    let seen = provider.share();

    let outcome = AgentLoop::new(&rook, provider, session).run("what is in answer.txt?").await.unwrap();

    assert_eq!(outcome.tools_called, ["read_file"], "the written call was run as a call");
    assert_eq!(outcome.reply, "it says forty-two");

    let turns = seen.lock().unwrap().clone();
    assert!(turns[0].tools.is_empty(), "nothing may be sent to an endpoint that refuses it");
    let system = &turns[0].messages[0].content;
    assert!(system.contains("read_file"), "so the tools are described instead: {system}");
    assert!(system.contains("\"tool\""), "with the shape of a call: {system}");
    assert!(
        turns[1].messages.iter().any(|m| m.content.contains("forty-two")),
        "and the result came back as a tool result"
    );
}

/// Small and quantised models sometimes finish a sentence in a script nobody
/// used. The text cannot be repaired from here — only the model knows what it
/// meant — so it is asked for the answer again.
#[tokio::test]
async fn a_reply_that_changed_writing_system_is_asked_for_again() {
    let f = fixture();
    let session = f.rook.start_session("s").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        reply("Готово, я 修复 сборку"),
        reply("Готово, я починил сборку"),
    ]));
    let seen = provider.share();

    let outcome = AgentLoop::new(&f.rook, provider, session).run("Почини сборку").await.unwrap();

    assert_eq!(outcome.reply, "Готово, я починил сборку");
    let turns = seen.lock().unwrap().clone();
    assert_eq!(turns.len(), 2, "the slip cost exactly one more turn");
    let told = &turns[1].messages.last().unwrap().content;
    assert!(told.contains("Han"), "which names the script it slipped into: {told}");
    assert!(told.contains("Cyrillic"), "and the one to go back to: {told}");
}

/// Translating, or quoting a file, or being asked about another script at all.
#[tokio::test]
async fn a_run_that_mixes_scripts_on_purpose_can_turn_the_check_off() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.one_script = false;
    let rook = with_config(&f, "mixed-scripts", config);
    let session = rook.start_session("s").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![reply("Готово, я 修复 сборку")]));
    let seen = provider.share();

    let outcome = AgentLoop::new(&rook, provider, session).run("Почини сборку").await.unwrap();

    assert_eq!(outcome.reply, "Готово, я 修复 сборку", "left exactly as the model wrote it");
    assert_eq!(seen.lock().unwrap().len(), 1, "and it cost no second turn");
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
    // The hook keeps what it was handed rather than summarising it back through
    // an interpreter, so the assertion is about the payload itself.
    let handed_to_it = f.workspace.path().join("payload.json");
    let rook = hooked(&f, vec![hook(rook_core::hooks::Event::PostTool, &keeps_its_payload(&handed_to_it))]);
    let session = rook.start_session("meta").unwrap();
    std::fs::write(f.workspace.path().join("notes.txt"), "one\ntwo\nthree\n").unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![
        call("read_file", serde_json::json!({"path": "notes.txt"})),
        reply("read it"),
    ]));
    AgentLoop::new(&rook, provider.clone(), session).run("read the notes").await.unwrap();

    let payload = std::fs::read_to_string(&handed_to_it).expect("the hook must have run at all");
    assert!(payload.contains("total_lines"), "the hook was given no meta: {payload}");
    assert!(payload.contains(r#""is_error":false"#), "nor the error flag: {payload}");
}

#[tokio::test]
async fn readonly_stops_the_agent_writing_a_skill_as_well_as_a_file() {
    let f = fixture();
    let mut config = Config::default();
    config.sandbox.stance = rook_tools::policy::Stance::ReadOnly;
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
    config.sandbox.stance = rook_tools::policy::Stance::Autonomous;
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

    let before = f.rook.context_usage(session, Some(100_000)).unwrap().live_tokens;

    // Bookkeeping the model never sees: an aside, a failed load, a manifest.
    f.rook.log(session, rook_store::EventKind::Note, "aside", &"noise ".repeat(500)).unwrap();
    f.rook.log(session, rook_store::EventKind::Error, "load_skill", &"more ".repeat(500)).unwrap();
    f.rook.log(session, rook_store::EventKind::Checkpoint, "before", &"manifest ".repeat(500)).unwrap();

    let after = f.rook.context_usage(session, Some(100_000)).unwrap();

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
    let reported = f.rook.context_usage(session, Some(100_000)).unwrap().live_tokens;
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

/// A child that ran out of steps read exactly like one that finished: the stop
/// reason was in the line, and a parent skimming five uniform blocks reads them
/// uniformly — and then answers as though the work were done.
#[tokio::test]
async fn a_sub_task_that_did_not_finish_is_not_reported_like_one_that_did() {
    let f = fixture();
    let session = f.rook.start_session("unfinished").unwrap();
    let script = vec![
        call("delegate", serde_json::json!({ "tasks": ["look around"], "max_steps": 3 })),
        // Every one of the child's steps asks for a tool, so it stops at its
        // limit rather than at an answer. Three, because a budget is never
        // below what a task needs.
        call("list_dir", serde_json::json!({ "path": "." })),
        call("list_dir", serde_json::json!({ "path": "." })),
        call("list_dir", serde_json::json!({ "path": "." })),
        // Out of steps, the child gets a last word; then the parent's.
        reply("I listed the directory three times"),
        reply("so much for that"),
    ];

    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("delegate one thing").await.unwrap();

    let transcript = f.rook.transcript(session, 0, 200, 8000).unwrap();
    let report = transcript.iter().rev().find(|e| e.label == "delegate").unwrap();
    assert!(report.body.contains("did not finish"), "{}", report.body);
    assert!(report.body.contains("max_steps"), "and why: {}", report.body);
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

/// Answers the summarisation with the summary in its reasoning channel and
/// nothing in `content`, which is what a reasoning model does when the whole
/// answer is short enough to be one thought.
struct ThinksTheSummary(ScriptedProvider);

#[async_trait]
impl Provider for ThinksTheSummary {
    fn id(&self) -> &str {
        "scripted/thinks-the-summary"
    }
    fn context_window(&self) -> usize {
        16_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        self.0.complete(request).await
    }
    async fn stream(&self, request: Request) -> rook_llm::Result<rook_llm::ResponseStream> {
        if !request.messages.first().is_some_and(|m| m.content.contains("compacting an agent")) {
            return self.0.stream(request).await;
        }
        let deltas = vec![
            Ok(rook_llm::Delta::Reasoning("they asked forty questions about the parser".into())),
            Ok(rook_llm::Delta::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: "thinker".into(),
            }),
        ];
        Ok(Box::pin(futures_util::stream::iter(deltas)))
    }
}

/// The empty `content` was read as a summariser that produced nothing, so a
/// summary that had been written was replaced by the note saying none was.
#[tokio::test]
async fn a_summary_the_model_only_thought_is_still_the_summary() {
    let f = fixture();
    let session = long_session(&f, 40);

    let provider = ThinksTheSummary(ScriptedProvider::new(vec![reply("carrying on")]));
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    agent.run("and now?").await.unwrap();

    let (from, summary) = f.rook.last_compaction(session).unwrap();
    assert!(from > 0, "the span is behind us");
    let summary = summary.expect("something stands in for the span");
    assert!(summary.contains("forty questions about the parser"), "{summary}");
    assert!(!summary.contains("could not be summarised"), "{summary}");
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

/// A span too small to summarise leaves the context exactly as full as it was,
/// so the check at the top of the next step is true again — and the step after
/// that. Each one spends a summarisation call to stand still.
#[tokio::test]
async fn a_turn_stops_compacting_once_it_stops_helping() {
    let f = fixture();
    let session = f.rook.start_session("stuck").unwrap();

    // One enormous message: compaction summarises history and cannot make a
    // single message smaller, so there is nothing it can win here.
    // Sized against the window below: over the compaction threshold of ~2,232
    // tokens and under the usable ~2,976, so the turn compacts and does not
    // refuse the request outright.
    f.rook.log(session, rook_store::EventKind::UserMessage, "user", &"x ".repeat(5_000)).unwrap();

    let script: Vec<_> = (0..6)
        .map(|i| call("list_dir", serde_json::json!({ "path": format!("d{i}") })))
        .chain([reply("done")])
        .collect();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.set_window_for_test(4_000);
    let outcome = agent.run("carry on").await.unwrap();

    assert_eq!(
        outcome.compactions, 1,
        "one attempt, and one is needed — a turn that never compacted would pass a `<= 1` \
         assertion without exercising anything"
    );
}

/// `max_steps` is a ceiling the configuration sets, and the child's value for it
/// arrives in tool arguments the model wrote. Taken at face value it is the
/// model that decides how long its own sub-agents may run.
#[tokio::test]
async fn a_child_cannot_be_given_a_bigger_step_budget_than_the_configuration_allows() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.max_steps = 3;
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("clamp")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("clamp").unwrap();

    let mut script = vec![call("delegate", serde_json::json!({ "tasks": ["go"], "max_steps": 50 }))];
    script.extend((0..8).map(|_| call("list_dir", serde_json::json!({}))));
    script.push(reply("child done"));
    script.push(reply("parent done"));

    let mut agent = AgentLoop::new(&rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("split this up").await.unwrap();

    let child = rook_store::parse_session_id(&outcome.delegated[0]).unwrap();
    let steps =
        rook.transcript(child, 0, usize::MAX, 4096).unwrap().iter().filter(|e| e.kind == "tool-call").count();
    assert!(
        steps <= 3,
        "the child took {steps} steps against a configured `max_steps = 3`, because the \
         argument asked for 50"
    );
}

/// The list of tasks comes from the model, and one delegation is as many turns
/// as it has entries. Without a ceiling on the total, a single tool call is an
/// unbounded number of model calls — and a child that delegates again multiplies
/// it, which is why the count is shared with the children rather than reset.
#[tokio::test]
async fn a_turn_cannot_start_more_sub_agents_than_the_configured_ceiling() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.max_subagents_per_turn = 2;
    let rook = Rook::from_parts(
        Store::open(f._store_dir.path().join("fleet")).unwrap(),
        config,
        f.rook.env().clone(),
        SkillIndex::default(),
        PathBuf::from(f.workspace.path()),
    );
    let session = rook.start_session("fleet").unwrap();

    let script = vec![
        call("delegate", serde_json::json!({ "tasks": ["a", "b"] })),
        reply("first child"),
        reply("second child"),
        call("delegate", serde_json::json!({ "tasks": ["c"] })),
        reply("stopped asking"),
        // Spare, so a ceiling that failed to hold runs the third child and
        // fails the count below rather than running the script dry.
        reply("unused"),
    ];
    let mut agent = AgentLoop::new(&rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("check five things").await.unwrap();

    assert_eq!(outcome.delegated.len(), 2, "the ceiling of two must actually be reached");
    let refusal = rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .expect("the second delegation must have answered something")
        .body;
    assert!(
        refusal.contains("max_subagents_per_turn"),
        "a refusal has to name the limit that was hit and what to do: {refusal}"
    );
}

/// A rewind puts back what the checkpoints hold. What was on disk instead is
/// whatever happened since — including edits the agent never saw, which no
/// checkpoint holds and which the write therefore destroys with no copy left.
#[tokio::test]
async fn a_rewind_keeps_the_state_it_is_about_to_overwrite() {
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

    // Somebody edits it by hand afterwards. Nothing captured this.
    std::fs::write(&target, "and then a person changed it\n").unwrap();

    let seq = f
        .rook
        .transcript(session, 0, 100, 4096)
        .unwrap()
        .iter()
        .find(|e| e.kind == "checkpoint")
        .unwrap()
        .seq;
    let rewound = f.rook.rewind(session, seq, true).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original\n");

    // Rewinding the fork past its own arrival is the way back.
    let fork = rook_store::parse_session_id(&rewound.session).unwrap();
    f.rook.rewind(fork, rewound.events_kept, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "and then a person changed it\n",
        "the hand edit the rewind wrote over has to still be reachable"
    );
}

/// Summarising is mechanical work, and a sub-agent is already run at low effort
/// for exactly that reason. The compaction call was the one auxiliary request
/// that still asked for whatever the provider does by default, so a turn
/// configured for deep thinking spent it on writing its own summary.
#[tokio::test]
async fn compacting_asks_for_less_thinking_than_the_turn_that_needed_it() {
    let f = fixture();
    let session = f.rook.start_session("effort").unwrap();
    // Several messages rather than one: a span of fewer than two entries has
    // nothing to summarise, and compaction would be counted as attempted
    // without a summarisation request ever being made.
    for i in 0..12 {
        let kind = match i % 2 {
            0 => rook_store::EventKind::UserMessage,
            _ => rook_store::EventKind::AssistantMessage,
        };
        f.rook.log(session, kind, "turn", &format!("{i} ").repeat(600)).unwrap();
    }

    let provider = ScriptedProvider::new(vec![reply("## Goal\nsummarised"), reply("done")]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.set_window_for_test(4_000);
    let outcome = agent.run("carry on").await.unwrap();
    assert_eq!(outcome.compactions, 1, "the turn has to compact for this to mean anything");

    let requests = seen.lock().unwrap();
    let summary = requests
        .iter()
        .find(|r| r.messages.first().is_some_and(|m| m.content.starts_with("You are compacting")))
        .expect("a summarisation request has to have been made, not merely attempted");
    assert_eq!(summary.effort, Some(rook_llm::Effort::Low));
}

/// A checker that could edit what it is judging is not checking it. The
/// instruction not to is one the model weighs against everything else; a tool it
/// was never handed is not.
#[tokio::test]
async fn a_checker_is_given_no_way_to_edit_what_it_is_judging() {
    let f = fixture();
    let session = f.rook.start_session("verify").unwrap();
    std::fs::write(f.workspace.path().join("notes.txt"), "as written\n").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "notes.txt says 'as written'" })),
        // The child's turn: it reaches for the tool it does not have, then
        // commits to a verdict.
        call("write_file", serde_json::json!({ "path": "notes.txt", "content": "fixed\n" })),
        reply("read it instead\n\nVERDICT: holds"),
        reply("checked"),
    ];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("prove it").await.unwrap();

    assert_eq!(outcome.delegated.len(), 1, "the check runs in a session of its own");
    assert_eq!(
        std::fs::read_to_string(f.workspace.path().join("notes.txt")).unwrap(),
        "as written\n",
        "the checker must not have been able to change what it was judging"
    );

    let offered_to_child: Vec<String> = {
        let requests = seen.lock().unwrap();
        let child = requests
            .iter()
            .find(|r| r.messages.iter().any(|m| m.content.contains("You are checking a claim")))
            .expect("the checker's own request must be among what the provider was asked");
        child.tools.iter().map(|t| t.name.clone()).collect()
    };
    assert!(
        !offered_to_child.iter().any(|n| n == "write_file"),
        "and never offered one: {offered_to_child:?}"
    );
    assert!(offered_to_child.iter().any(|n| n == "run_command"), "but must keep what settles a claim");
    for way_round in ["write_skill", "delegate", "remember", "edit_file"] {
        assert!(
            !offered_to_child.iter().any(|n| n == way_round),
            "{way_round} is another way to change things: {offered_to_child:?}"
        );
    }
}

/// "It looks fine" is what a model says when it has read something and run
/// nothing. A check that will not commit is reported as unchecked rather than
/// passed.
#[tokio::test]
async fn a_check_that_will_not_commit_is_not_a_pass() {
    let f = fixture();
    let session = f.rook.start_session("hedge").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "the build is clean" })),
        reply("seems reasonable to me"),
        // Asked once to finish, and still will not.
        reply("still seems fine"),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("prove it").await.unwrap();

    let result = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(result.contains("unchecked"), "a hedge must not read as a pass: {result}");
    assert_eq!(outcome.delegated.len(), 1);
}

/// The point of asking a second agent is to get past the first one's memory. A
/// verdict reached without reaching for anything is that memory with a label on
/// it, and it is the shape a fabricated check takes.
#[tokio::test]
async fn a_verdict_reached_without_touching_anything_is_not_a_check() {
    let f = fixture();
    let session = f.rook.start_session("recall").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "the release shipped on the fourth" })),
        reply("I recall that it did.\n\nVERDICT: holds"),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    let outcome = agent.run("check it").await.unwrap();

    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("unproven"), "a recollection must not read as a pass: {said}");
    assert!(said.contains("reached for nothing"), "and must say why: {said}");
    // A small model reads the last line. The checker's `holds` is not left
    // there to be read as the answer; the ruling is.
    assert!(said.trim_end().ends_with("nothing was run or read to settle it"), "{said}");
    assert!(!said.contains("VERDICT: holds"), "the discounted line is not quoted: {said}");
    assert!(said.contains("I recall that it did."), "the rest of what it said is: {said}");
    assert_eq!(outcome.delegated.len(), 1);
}

/// The same verdict from a checker that did reach for something stands.
#[tokio::test]
async fn a_verdict_backed_by_a_command_stands() {
    let f = fixture();
    let session = f.rook.start_session("ran").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "the directory is readable" })),
        call("list_dir", serde_json::json!({ "path": "." })),
        reply("it listed.\n\nVERDICT: holds"),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("check it").await.unwrap();

    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(!said.contains("reached for nothing"), "{said}");
    assert!(said.contains("VERDICT: holds"), "{said}");
}

/// Two facts a model otherwise supplies from training: which shell it has, and
/// what day it is. They go in different places on purpose — the shell is the
/// same every turn and belongs in the cached prefix; the date is not and would
/// invalidate everything behind it.
#[tokio::test]
async fn the_shell_is_in_the_prompt_and_the_date_is_beside_it() {
    let f = fixture();
    let session = f.rook.start_session("facts").unwrap();
    let provider = ScriptedProvider::new(vec![reply("noted")]);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.run("what have I got").await.unwrap();

    let requests = seen.lock().unwrap();
    let system = &requests[0].messages[0];
    assert_eq!(system.role, Role::System);
    assert!(
        system.content.contains(&format!("shell: {}", rook_core::SHELL)),
        "the shell is the same every turn, so it belongs in the cached prefix: {}",
        system.content
    );
    assert!(
        !system.content.contains("Today is"),
        "and the date is not, because a prefix that varies caches nothing: {}",
        system.content
    );

    let beside = requests[0].messages.iter().find(|m| m.content.starts_with("Today is"));
    let beside = beside.expect("the date has to reach the model somewhere");
    assert!(beside.content.contains(&rook_store::today()), "{}", beside.content);
}

/// The estimate counts message text and not the tool schemas, which are the
/// larger part of a small request. The provider counted what it actually
/// received, so anchoring on that shrinks the error to one turn's worth — and
/// the error is in the direction that ends a turn with a limit nobody saw
/// coming.
#[tokio::test]
async fn the_context_size_follows_what_the_provider_reported() {
    let f = fixture();
    let session = f.rook.start_session("anchored").unwrap();

    // Far more than the messages weigh, which is what a real provider reports:
    // it counted the schemas too.
    let heavy = |content: &str| Response {
        message: Message::assistant(content),
        stop_reason: StopReason::ToolUse,
        usage: Usage { input_tokens: 3_000, output_tokens: 10, ..Default::default() },
        model: "scripted".into(),
    };
    let mut first = heavy("");
    first.message.tool_calls =
        vec![ToolCall { id: "call_1".into(), name: "list_dir".into(), arguments: serde_json::json!({}) }];

    let mut agent =
        AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![first, reply("done")])), session);
    agent.allow_everything_not_denied();
    agent.set_window_for_test(4_000);
    let outcome = agent.run("go").await.unwrap();

    assert_eq!(
        outcome.compactions, 1,
        "3,000 reported against a 4,000 window is over the threshold, and the estimate of these \
         few short messages is not — so a turn that never compacted would mean the report was \
         ignored"
    );
}

/// End to end, through the loop that has to notice: a read makes this turn the
/// one that has seen the file, and an overwrite by a turn that has not is
/// refused with what to do instead.
#[tokio::test]
async fn a_turn_may_not_overwrite_what_another_turn_looked_at_last() {
    let f = fixture();
    let target = f.workspace.path().join("shared.txt");
    std::fs::write(&target, "as it was\n").unwrap();

    let theirs = f.rook.start_session("theirs").unwrap();
    // As the loop will see it: paths are resolved through symlinks before they
    // reach the registry, and on macOS `/var` is one.
    f.rook.touched(theirs, &[target.canonicalize().unwrap()]);

    let mine = f.rook.start_session("mine").unwrap();
    let script = vec![
        call("write_file", serde_json::json!({ "path": "shared.txt", "content": "mine\n" })),
        call("read_file", serde_json::json!({ "path": "shared.txt" })),
        call("write_file", serde_json::json!({ "path": "shared.txt", "content": "mine\n" })),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), mine);
    agent.allow_everything_not_denied();
    agent.run("rewrite it").await.unwrap();

    let results: Vec<String> = f
        .rook
        .transcript(mine, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "tool-result")
        .map(|e| e.body)
        .collect();

    assert!(results[0].contains("edit_file"), "the blind overwrite is refused: {}", results[0]);
    assert!(!results[0].contains("overwrote"), "and does not happen: {}", results[0]);
    assert!(
        results[2].contains("overwrote"),
        "after reading it, the same write goes through: {}",
        results[2]
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine\n", "which is the file it left");
}

/// A verdict from a checker that called nothing is reported as unproven. Reaching
/// for a tool it was never given and being refused must not count as calling
/// something, or the rule is one `write_skill` away from being satisfied by a
/// recollection.
#[tokio::test]
async fn a_refused_reach_is_not_a_check_either() {
    let f = fixture();
    let session = f.rook.start_session("refused").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "the release shipped" })),
        call("write_skill", serde_json::json!({ "name": "notes", "description": "x", "body": "y" })),
        reply("I still think it did.\n\nVERDICT: holds"),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("check it").await.unwrap();

    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("unproven"), "a refused reach is not evidence: {said}");
    assert!(said.contains("reached for nothing"), "{said}");
}

/// A checker that narrates what it would run and stops has not answered. Asked
/// once more in its own session, it does the thing and commits; the parent sees
/// the verdict, not the plan.
#[tokio::test]
async fn a_checker_that_stops_without_a_verdict_is_asked_once_to_finish() {
    let f = fixture();
    let session = f.rook.start_session("nudge").unwrap();
    std::fs::write(f.workspace.path().join("lib.rs"), "fn add(a: i32, b: i32) -> i32 { a - b }\n").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "add returns the sum" })),
        reply("To verify this I will read lib.rs and quote the line."),
        call("read_file", serde_json::json!({ "path": "lib.rs" })),
        reply("`a - b` subtracts.\n\nVERDICT: fails"),
        reply("it fails"),
    ];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("check it").await.unwrap();

    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("VERDICT: fails"), "the verdict reached after the nudge stands: {said}");
    assert!(!said.contains("did not answer"), "{said}");
    let nudged = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.messages.last().is_some_and(|m| m.content.contains("stopped without a verdict")))
        .count();
    assert_eq!(nudged, 1, "asked once, in the checker's own session");
    assert_eq!(outcome.delegated.len(), 1, "and not as a second checker");
}

/// Asked once. A checker that will not commit when told plainly to is reported
/// as not having answered, not asked a third time.
#[tokio::test]
async fn a_checker_silent_twice_is_reported_as_silent() {
    let f = fixture();
    let session = f.rook.start_session("silent").unwrap();

    let script = vec![
        call("verify", serde_json::json!({ "claim": "the release shipped" })),
        reply("I would look at the changelog."),
        reply("The changelog would say."),
        reply("unchecked then"),
    ];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.allow_everything_not_denied();
    agent.run("check it").await.unwrap();

    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("did not answer with a verdict"), "{said}");
    let nudged = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.messages.last().is_some_and(|m| m.content.contains("stopped without a verdict")))
        .count();
    assert_eq!(nudged, 1, "once: {nudged}");
}

/// A tool call the model wrote as text, with the tools offered natively, is a
/// call: `qwen2.5-coder:3b` answered every smoke scenario with one and the turn
/// ended with nothing called. The result reaches the model like any other.
#[tokio::test]
async fn a_call_written_as_text_under_native_tools_is_still_a_call() {
    let f = fixture();
    let session = f.rook.start_session("text-call").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let script = vec![reply(r#"{"name": "read_file", "arguments": {"path": "config.rs"}}"#), reply("8443")];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("what port?").await.unwrap();

    assert_eq!(outcome.tools_called, vec!["read_file".to_string()], "{outcome:?}");
    assert_eq!(outcome.reply, "8443");
    let second = seen.lock().unwrap()[1].clone();
    let result = second.messages.iter().find(|m| m.role == Role::Tool).expect("the result went back");
    assert!(result.content.contains("8443"), "{}", result.content);
}

/// And an object that names no offered tool is an answer, not a call: a model
/// asked for JSON gives JSON.
#[tokio::test]
async fn an_object_naming_nothing_offered_is_an_answer() {
    let f = fixture();
    let session = f.rook.start_session("json-answer").unwrap();
    let json = r#"{"name": "widget", "arguments": {"size": 3}}"#;
    let outcome = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply(json)])), session)
        .run("describe a widget as JSON")
        .await
        .unwrap();
    assert!(outcome.tools_called.is_empty(), "{:?}", outcome.tools_called);
    assert_eq!(outcome.reply, json);
}

/// A model that did the work and stopped without a word is asked once to say
/// what it found. The transcript keeps the silence and the ask; the outcome
/// carries the answer.
#[tokio::test]
async fn a_turn_that_did_the_work_and_said_nothing_is_asked_once_to_say_it() {
    let f = fixture();
    let session = f.rook.start_session("say-it").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let script =
        vec![call("read_file", serde_json::json!({ "path": "config.rs" })), reply(""), reply("8443")];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("what port?").await.unwrap();

    assert_eq!(outcome.reply, "8443");
    assert_eq!(outcome.tools_called, vec!["read_file".to_string()]);
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.messages.last().is_some_and(|m| m.content.contains("without saying anything")))
        .count();
    assert_eq!(asked, 1, "asked once");
}

/// Asked once. A second silence ends the turn as a silence, named as such,
/// rather than a third request.
#[tokio::test]
async fn a_turn_silent_twice_ends_as_a_silence() {
    let f = fixture();
    let session = f.rook.start_session("silent").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let script = vec![
        call("read_file", serde_json::json!({ "path": "config.rs" })),
        reply(""),
        reply(""),
        reply("never reached"),
    ];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("what port?").await.unwrap();

    assert!(outcome.reply.contains("without saying anything"), "{}", outcome.reply);
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "the model was called for the read, the silence, and the ask — not again"
    );
}

/// The call a small model writes back after a refusal: prose, then the object
/// in a fence. Adopted like a bare one, with the prose kept as what it said.
#[tokio::test]
async fn a_fenced_call_after_prose_is_adopted_under_native_tools() {
    let f = fixture();
    let session = f.rook.start_session("fenced").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 9001;\n").unwrap();

    let fenced = "It seems there was an error in the previous response. Let's try again by providing the \
                  correct format for the `edit_file` function.\n\n```json\n{\"name\": \"edit_file\", \
                  \"arguments\": {\"files\":[{\"path\":\"config.rs\",\"edits\":[{\"from\":\"9001\",\"to\":\"9000\"}]}]}}\n```";
    let script = vec![reply(fenced), reply("changed")];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("set the port to 9000").await.unwrap();

    assert_eq!(outcome.tools_called, vec!["edit_file".to_string()], "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(f.workspace.path().join("config.rs")).unwrap(),
        "pub const PORT: u16 = 9000;\n"
    );
}

/// A task is words. A model handed `delegate` a tool call in place of one and
/// was told it needed a task, which it thought it had given; the refusal now
/// names the shape and shows the one that works.
#[tokio::test]
async fn a_task_given_as_a_tool_call_is_refused_by_its_shape() {
    let f = fixture();
    let session = f.rook.start_session("shape").unwrap();
    let script = vec![
        call(
            "delegate",
            serde_json::json!({ "tasks": [{ "tool": "read_file", "arguments": { "path": "x" } }] }),
        ),
        reply("noted"),
    ];
    let outcome =
        AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session).run("go").await.unwrap();

    assert!(outcome.delegated.is_empty(), "{:?}", outcome.delegated);
    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("words, not an object with arguments, tool"), "{said}");
    assert!(said.contains("tasks: [\""), "and shows the shape that works: {said}");
}

/// An object with the sentence under `task` is read for it.
#[tokio::test]
async fn a_task_given_as_an_object_carrying_task_is_read() {
    let f = fixture();
    let session = f.rook.start_session("object-task").unwrap();
    let script = vec![
        call("delegate", serde_json::json!({ "tasks": [{ "task": "say hello", "why": "a test" }] })),
        reply("hello"),
        reply("it said hello"),
    ];
    let outcome =
        AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session).run("go").await.unwrap();
    assert_eq!(outcome.delegated.len(), 1, "{:?}", outcome.delegated);
    assert_eq!(outcome.reply, "it said hello");
}

/// A provider that reports the end of the turn beside a call it also
/// delivered has delivered a call. The loop runs what the message carries.
#[tokio::test]
async fn a_call_delivered_beside_an_end_turn_is_still_run() {
    let f = fixture();
    let session = f.rook.start_session("stop-beside-call").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let mut called = call("read_file", serde_json::json!({ "path": "config.rs" }));
    called.stop_reason = StopReason::EndTurn;
    let script = vec![called, reply("8443")];
    let outcome = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session)
        .run("what port?")
        .await
        .unwrap();

    assert_eq!(outcome.tools_called, vec!["read_file".to_string()], "{outcome:?}");
    assert_eq!(outcome.reply, "8443");
}

/// A budget of one step is a sub-agent that reads and cannot say what it read.
/// The model's number is a ceiling it may lower, never below a call, a look,
/// and an answer.
#[tokio::test]
async fn a_sub_task_budget_is_never_below_what_a_task_needs() {
    let f = fixture();
    let session = f.rook.start_session("budget").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let script = vec![
        call(
            "delegate",
            serde_json::json!({ "tasks": ["read config.rs and report the port"], "max_steps": 1 }),
        ),
        call("read_file", serde_json::json!({ "path": "config.rs" })),
        reply("8443"),
        reply("the port is 8443"),
    ];
    let outcome =
        AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session).run("port?").await.unwrap();

    assert_eq!(outcome.delegated.len(), 1, "{:?}", outcome.delegated);
    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("8443"), "the child had a step to answer in: {said}");
    assert!(!said.contains("did not finish"), "{said}");
}

/// A child that ran out of steps with a call as its last word is reported by
/// what it called, not by the nothing it said.
#[tokio::test]
async fn an_unfinished_sub_task_is_reported_by_what_it_called() {
    let f = fixture();
    let session = f.rook.start_session("unfinished").unwrap();
    std::fs::write(f.workspace.path().join("a.txt"), "a\n").unwrap();

    let script = vec![
        call("delegate", serde_json::json!({ "tasks": ["look around"], "max_steps": 3 })),
        // Three different calls: the same one three times over would be
        // refused as the loop it is, and the report is about the budget.
        call("read_file", serde_json::json!({ "path": "a.txt" })),
        call("list_dir", serde_json::json!({ "path": "." })),
        call("read_file", serde_json::json!({ "path": "a.txt" })),
        // Its last word, asked for, is nothing — so what it called is the report.
        reply(""),
        reply("it kept reading"),
    ];
    let outcome =
        AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session).run("go").await.unwrap();

    assert_eq!(outcome.delegated.len(), 1, "{:?}", outcome.delegated);
    let said = f
        .rook
        .transcript(session, 0, usize::MAX, 4096)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "tool-result")
        .unwrap()
        .body;
    assert!(said.contains("did not finish — max_steps after 3 steps"), "{said}");
    assert!(said.contains("called read_file, list_dir, read_file, and the budget ended"), "{said}");
}

/// Two calls written in one reply are two calls, both run, both answered:
/// given only the first back, a small model wrote the second again every step.
#[tokio::test]
async fn two_calls_written_in_one_reply_are_both_run() {
    let f = fixture();
    let session = f.rook.start_session("two-text-calls").unwrap();
    std::fs::write(f.workspace.path().join("a.txt"), "alpha\n").unwrap();
    std::fs::write(f.workspace.path().join("b.txt"), "beta\n").unwrap();

    let both = "{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.txt\"}}\n\
                {\"name\": \"read_file\", \"arguments\": {\"path\": \"b.txt\"}}";
    let script = vec![reply(both), reply("alpha and beta")];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("read both").await.unwrap();

    assert_eq!(outcome.tools_called, vec!["read_file".to_string(), "read_file".to_string()], "{outcome:?}");
    let second = seen.lock().unwrap()[1].clone();
    let results: Vec<&str> =
        second.messages.iter().filter(|m| m.role == Role::Tool).map(|m| m.content.as_str()).collect();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results[0].contains("alpha") && results[1].contains("beta"), "{results:?}");
}

/// A turn that ran out of steps with a call as its last word is asked once,
/// with no tools to reach for, what it found — so it ends on the answer
/// rather than on the limit.
#[tokio::test]
async fn a_turn_out_of_steps_with_work_done_gets_a_last_word() {
    let f = fixture();
    let session = f.rook.start_session("last-word").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let script = vec![
        call("read_file", serde_json::json!({ "path": "config.rs" })),
        call("read_file", serde_json::json!({ "path": "config.rs" })),
        reply("8443"),
    ];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.max_steps = 2;
    let outcome = agent.run("what port?").await.unwrap();

    assert_eq!(outcome.stopped, "max_steps", "the precondition: the limit was what ended it");
    assert_eq!(outcome.reply, "8443");
    let last = seen.lock().unwrap().last().cloned().unwrap();
    assert!(last.tools.is_empty(), "nothing to reach for on the last word");
    assert!(last.messages.last().unwrap().content.contains("out of steps"), "{:?}", last.messages.last());
}

/// A turn that reached its limit having said something is not asked again.
#[tokio::test]
async fn a_turn_out_of_steps_that_already_spoke_is_left_as_it_is() {
    let f = fixture();
    let session = f.rook.start_session("spoke").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let mut second = call("read_file", serde_json::json!({ "path": "config.rs" }));
    second.message.content = "still looking".into();
    let script =
        vec![call("read_file", serde_json::json!({ "path": "config.rs" })), second, reply("never asked")];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let mut agent = AgentLoop::new(&f.rook, Arc::new(provider), session);
    agent.max_steps = 2;
    let outcome = agent.run("what port?").await.unwrap();

    assert_eq!(outcome.stopped, "max_steps");
    assert_eq!(outcome.reply, "still looking");
    assert_eq!(seen.lock().unwrap().len(), 2, "two steps and no last word");
}

/// The limit is the model's, not the children's. A parent that runs out of
/// steps with a sub-agent still out waits for it and appends what came back,
/// as a turn that finished would — and has nothing further to say itself.
#[tokio::test]
async fn a_turn_out_of_steps_still_collects_the_sub_agents_it_started() {
    let f = fixture();
    let session = f.rook.start_session("limit-with-children").unwrap();
    let provider = Arc::new(ByPrompt(vec![
        ("count the files", call("delegate", serde_json::json!({ "tasks": ["tally"], "wait": false }))),
        ("started: task01", call("list_dir", serde_json::json!({ "path": "." }))),
        // Before the child's rule: the hand-over quotes the task's name.
        ("out of steps", reply("three, per the sub-agent")),
        ("tally", reply("there are three")),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.max_steps = 2;
    let outcome = agent.run("count the files").await.unwrap();

    assert_eq!(outcome.stopped, "max_steps");
    assert_eq!(outcome.delegated.len(), 1, "the child's cost is the turn's");
    // The child's answer went in front of the model for its last word, which
    // is the answer; the transcript keeps the hand-over.
    assert_eq!(outcome.reply, "three, per the sub-agent");
    let transcript = f.rook.transcript(session, 0, 200, 8000).unwrap();
    assert!(transcript.iter().any(|e| e.body.contains("there are three")), "the child's answer was recorded");
}

/// The same call answered the same way twice is a loop. The third is not made;
/// the model is pointed at the answer it already has.
#[tokio::test]
async fn the_same_call_answered_the_same_way_twice_is_refused_the_third_time() {
    let f = fixture();
    let session = f.rook.start_session("loop").unwrap();
    std::fs::write(f.workspace.path().join("a.txt"), "alpha\n").unwrap();

    let read = || call("read_file", serde_json::json!({ "path": "a.txt" }));
    let script = vec![read(), read(), read(), reply("alpha, then")];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("read a").await.unwrap();

    assert_eq!(outcome.tools_called.len(), 2, "the third was not made: {:?}", outcome.tools_called);
    let last = seen.lock().unwrap().last().cloned().unwrap();
    let told = last.messages.iter().rev().find(|m| m.role == Role::Tool).unwrap().content.clone();
    assert!(told.contains("made 2 times this turn and answered the same"), "{told}");
    assert_eq!(outcome.reply, "alpha, then");
}

/// Same result is the test, not same arguments: a file read again after it
/// changed is a different answer, and the count starts over.
#[tokio::test]
async fn a_call_whose_answer_changed_is_not_a_loop() {
    let f = fixture();
    let session = f.rook.start_session("changed").unwrap();
    std::fs::write(f.workspace.path().join("a.txt"), "alpha\n").unwrap();

    let read = || call("read_file", serde_json::json!({ "path": "a.txt" }));
    let script = vec![
        read(),
        read(),
        call(
            "edit_file",
            serde_json::json!({ "path": "a.txt", "edits": [{ "old": "alpha", "new": "beta" }] }),
        ),
        read(),
        read(),
        reply("beta now"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    let outcome = agent.run("read a").await.unwrap();

    assert_eq!(
        outcome.tools_called.len(),
        5,
        "every call ran: {:?} — stopped {} reply {:?}",
        outcome.tools_called,
        outcome.stopped,
        outcome.reply
    );
    assert_eq!(outcome.reply, "beta now");
}

/// A parent that ends the turn with a sub-agent still out is handed what came
/// back and asked to go on, so it answers from it rather than from memory:
/// asked what it found with the readers' answers appended below where it
/// never looked, a model made the number up.
#[tokio::test]
async fn an_uncollected_sub_agents_answer_reaches_the_model_before_the_turn_ends() {
    let f = fixture();
    let session = f.rook.start_session("handed").unwrap();
    let provider = Arc::new(ByPrompt(vec![
        ("count the files", call("delegate", serde_json::json!({ "tasks": ["tally"], "wait": false }))),
        ("started: task01", reply("done for now")),
        // Before the child's rule: the hand-over quotes the task's name.
        ("did not collect", reply("it says three")),
        ("tally", reply("there are three")),
    ]));
    let outcome = AgentLoop::new(&f.rook, provider, session).run("count the files").await.unwrap();

    assert_eq!(outcome.reply, "it says three");
    assert_eq!(outcome.tools_called, ["delegate"]);
    assert_eq!(outcome.delegated.len(), 1);
}

/// The directories `[sandbox] writable` names reach the command's containment,
/// with `~` the home directory, and the switches reach it as they are.
#[test]
fn the_sandbox_config_reaches_the_tool_context() {
    let mut config = Config::default();
    config.sandbox.writable = vec!["~/.cargo".into(), "/opt/cache".into()];
    config.sandbox.network = false;
    config.sandbox.isolate = rook_tools::isolate::Mode::Required;
    let workspace = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let ctx = rook_core::agent::tool_context(&config, workspace.path(), out.path());

    assert_eq!(ctx.isolate, rook_tools::isolate::Mode::Required);
    assert!(!ctx.isolation.network);
    let home = rook_core::paths::user_home();
    assert!(ctx.isolation.scratch.contains(&home.join(".cargo")), "{:?}", ctx.isolation.scratch);
    assert!(
        ctx.isolation.scratch.contains(&std::path::PathBuf::from("/opt/cache")),
        "{:?}",
        ctx.isolation.scratch
    );
    assert!(
        ctx.isolation.scratch.contains(&std::env::temp_dir()),
        "the temporary directory stays: {:?}",
        ctx.isolation.scratch
    );
}

/// A reply cut at the output limit is neither an answer nor a call. Asked
/// once to go on, the model makes the call it was writing; the turn ends on
/// the answer rather than on the limit.
#[tokio::test]
async fn a_reply_cut_at_the_output_limit_is_asked_once_to_go_on() {
    let f = fixture();
    let session = f.rook.start_session("cut").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "pub const PORT: u16 = 8443;\n").unwrap();

    let mut cut = reply("```json\n{\"name\": \"read_file\", \"arguments\": {\"pa");
    cut.stop_reason = StopReason::MaxTokens;
    let script = vec![cut, call("read_file", serde_json::json!({ "path": "config.rs" })), reply("8443")];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("what port?").await.unwrap();

    assert_eq!(outcome.reply, "8443");
    assert_eq!(outcome.tools_called, vec!["read_file".to_string()]);
    let asked = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.messages.last().is_some_and(|m| m.content.contains("cut off at the output limit")))
        .count();
    assert_eq!(asked, 1, "asked once");
}

/// Asked once. A second cut ends the turn as what it is, `max_tokens`.
#[tokio::test]
async fn a_reply_cut_twice_ends_as_cut() {
    let f = fixture();
    let session = f.rook.start_session("cut-twice").unwrap();
    let mut first = reply("a long answer that");
    first.stop_reason = StopReason::MaxTokens;
    let mut second = reply("goes on and");
    second.stop_reason = StopReason::MaxTokens;
    let provider = ScriptedProvider::new(vec![first, second, reply("never reached")]);
    let seen = provider.share();
    let outcome = AgentLoop::new(&f.rook, Arc::new(provider), session).run("explain").await.unwrap();

    assert_eq!(outcome.stopped, "max_tokens");
    assert_eq!(outcome.reply, "goes on and");
    assert_eq!(seen.lock().unwrap().len(), 2, "the reply, the ask, and not a third");
}

/// A tool that declares its paths is checkpointed and diffed exactly. A command
/// declares none, so "what did this turn change" was answered with silence — a
/// turn that ran `sed -i` reported no files changed at all, which is not a gap
/// in an answer but a wrong one.
#[tokio::test]
async fn a_file_a_command_wrote_is_named_in_what_the_session_changed() {
    let f = fixture();
    let session = f.rook.start_session("commands").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "port = 8080\n").unwrap();

    let script = vec![
        call("run_command", serde_json::json!({ "command": "printf 'port = 9000\\n' > config.rs" })),
        reply("changed it"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("set the port to 9000").await.unwrap();

    assert_eq!(
        std::fs::read_to_string(f.workspace.path().join("config.rs")).unwrap(),
        "port = 9000\n",
        "the precondition: the command wrote the file"
    );

    let changed = f.rook.changes(session, false).unwrap();
    assert!(changed.watched, "the workspace was small enough to walk");
    assert!(
        changed.written_by_commands.iter().any(|p| p == "config.rs"),
        "the command's file has to be named: {:?}",
        changed.written_by_commands
    );
    assert_eq!(changed.touched(), 1, "and counted as a file this session changed");
    assert!(changed.files.is_empty(), "with no diff, because nothing holds what it was");
}

/// A read is not a write. Watching every call would put half the workspace in
/// the log for a turn that only looked at it.
#[tokio::test]
async fn a_command_that_writes_nothing_leaves_nothing_behind() {
    let f = fixture();
    let session = f.rook.start_session("reading").unwrap();
    std::fs::write(f.workspace.path().join("a.txt"), "hello\n").unwrap();

    let script = vec![call("run_command", serde_json::json!({ "command": "cat a.txt" })), reply("hello")];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("read it").await.unwrap();

    let changed = f.rook.changes(session, false).unwrap();
    assert!(changed.written_by_commands.is_empty(), "{:?}", changed.written_by_commands);
    assert_eq!(changed.touched(), 0);
}

/// A tool that says what it touches is checkpointed and diffed, and must not
/// also be reported as an unknown write: that is one file twice, once with its
/// diff and once without.
#[tokio::test]
async fn a_tool_that_declares_its_paths_is_diffed_rather_than_only_named() {
    let f = fixture();
    let session = f.rook.start_session("declared").unwrap();
    std::fs::write(f.workspace.path().join("config.rs"), "port = 8080\n").unwrap();

    let script = vec![
        call(
            "edit_file",
            serde_json::json!({ "path": "config.rs", "edits": [{ "old": "8080", "new": "9000" }] }),
        ),
        reply("done"),
    ];
    let mut agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(script)), session);
    agent.allow_everything_not_denied();
    agent.run("set the port").await.unwrap();

    let changed = f.rook.changes(session, false).unwrap();
    assert!(changed.written_by_commands.is_empty(), "{:?}", changed.written_by_commands);
    assert_eq!(changed.files.len(), 1, "{:?}", changed.files);
    assert_eq!(changed.files[0].lines_added, 1, "the diff is the whole point of declaring paths");
}

/// A window too small for the work turns a turn into summarising: each
/// compaction is a model call, and a turn compacting every few steps spends
/// most of itself on bookkeeping. Watched live it looks like an hour of
/// nothing — and the count only arrived at the end, when it was too late to
/// act on. Said once, when it stops being incidental.
#[tokio::test]
async fn a_turn_that_keeps_compacting_says_the_window_is_the_reason() {
    let f = fixture();
    let session = long_session(&f, 60);
    std::fs::write(f.workspace.path().join("a.txt"), "x".repeat(6_000)).unwrap();

    // By prompt, because a compaction is itself a model call: a scripted list
    // would have the summariser eating the replies meant for the turn.
    // Each read refills what the compaction freed, which is what a real turn
    // does with a window this size.
    let provider = Arc::new(ByPrompt(vec![
        ("compacting an agent's working transcript", reply("## Goal\nread it\n\n## Done\nread once more")),
        // Anything else: read again, which refills what the compaction freed.
        // A compacted turn's last message is the summary, not the prompt, so a
        // rule keyed on the prompt would stop matching after the first one.
        ("", call("read_file", serde_json::json!({ "path": "a.txt" }))),
    ]));
    let mut agent = AgentLoop::new(&f.rook, provider, session);
    agent.set_window_for_test(4_000);
    agent.max_steps = 14;
    let outcome = agent.run("read the file a few times").await.unwrap();

    assert!(outcome.compactions >= 3, "the precondition: it compacted enough to matter: {outcome:?}");
    let said: Vec<&String> = outcome.open_questions.iter().filter(|q| q.contains("compacted")).collect();
    assert_eq!(said.len(), 1, "said once, not once a compaction: {:?}", outcome.open_questions);
    assert!(said[0].contains("context_window"), "and names what fixes it: {}", said[0]);
}

/// The arm ADR-0010 declined, built so the measurement can be repeated here
/// rather than believed: a checklist tool, off by default. Both halves have to
/// work for the comparison to mean anything — the list is kept, and it comes
/// back to the model, because a checklist it cannot see is one it cannot check
/// off.
#[tokio::test]
async fn the_todo_tool_keeps_a_list_and_hands_it_back() {
    let f = fixture();
    let mut config = Config::default();
    config.agent.todo_tool = true;
    let rook = with_config(&f, "planning", config);
    let session = rook.start_session("planning").unwrap();

    let script = vec![
        call(
            "plan",
            serde_json::json!({ "steps": [
                { "step": "read the file", "done": true },
                { "step": "fix the bug" }
            ] }),
        ),
        reply("one down"),
    ];
    let provider = ScriptedProvider::new(script);
    let seen = provider.share();
    let outcome = AgentLoop::new(&rook, Arc::new(provider), session).run("fix it").await.unwrap();

    assert_eq!(outcome.tools_called, ["plan"]);
    assert_eq!(
        rook.plan(session).unwrap().as_deref(),
        Some("- [x] read the file\n- [ ] fix the bug"),
        "the list is kept"
    );

    // Within the turn the list comes back as the call's own result, which is
    // what lets the next step check a step off.
    let second = seen.lock().unwrap()[1].clone();
    let answered = second.messages.iter().rev().find(|m| m.role == Role::Tool).unwrap().content.clone();
    assert!(answered.contains("1 step(s) left"), "{answered}");
    assert!(answered.contains("[ ] fix the bug"), "{answered}");

    // And a later turn starts with it, beside the date, because a turn that
    // began after a break has no tool result to remember it by.
    let later = ScriptedProvider::new(vec![reply("carrying on")]);
    let seen = later.share();
    AgentLoop::new(&rook, Arc::new(later), session).run("go on").await.unwrap();
    let carried: String = seen.lock().unwrap()[0].messages.iter().map(|m| m.content.clone()).collect();
    assert!(carried.contains("The plan you are keeping"), "{carried}");
    assert!(carried.contains("[ ] fix the bug"), "{carried}");
}

/// Off by default, which is the decision: no tool, and the line that asks for a
/// sentence and forbids the bookkeeping.
#[tokio::test]
async fn without_the_flag_there_is_no_tool_and_the_line_says_no_checklist() {
    let f = fixture();
    let session = f.rook.start_session("default").unwrap();
    let agent = AgentLoop::new(&f.rook, Arc::new(ScriptedProvider::new(vec![reply("ok")])), session);

    assert!(!agent.tool_specs().iter().any(|t| t.name == "plan"), "no tool by default");
    let prompt = agent.system_prompt();
    assert!(prompt.contains("Do not keep a checklist"), "{prompt}");
    assert!(!prompt.contains("write the plan with `plan`"), "{prompt}");
}
