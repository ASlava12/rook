use async_trait::async_trait;
use rook_core::{Config, Rook, Vault, agent::AgentLoop, learning};
use rook_llm::{Message, Provider, Request, Response, StopReason};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
static TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn config() -> Config {
    let mut c = Config::default();
    c.memory.enabled = false;
    c.web.enabled = false;
    c.storage.gc_grace_secs = 0;
    c
}
fn open(root: &std::path::Path, c: Config) -> Rook {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        c,
        rook_skills::Environment::bare("test", "test", "0.11.2"),
        rook_skills::SkillIndex::default(),
        workspace,
    )
}
struct Finish {
    store: Arc<rook_store::Store>,
    owner: u128,
    generated: AtomicUsize,
}
#[async_trait]
impl Provider for Finish {
    fn id(&self) -> &str {
        "fixture/learning"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        let first = request.messages.first().map(|m| m.content.as_str()).unwrap_or("");
        let text = if first.starts_with("Propose reusable knowledge") {
            self.generated.fetch_add(1, Ordering::SeqCst);
            let user = self
                .store
                .events(self.owner, 0, 128)
                .unwrap()
                .into_iter()
                .find(|e| e.record.kind == rook_store::EventKind::UserMessage)
                .unwrap();
            let text = String::from_utf8(self.store.get(&user.record.body).unwrap()).unwrap();
            json!([{"target":{"kind":"memory","text":text},"reason":"The user explicitly stated this preference","evidence":[user.seq]}]).to_string()
        } else if first.starts_with("Classify whether an assistant") {
            r#"{"action":"finish"}"#.into()
        } else {
            "Complete.".into()
        };
        Ok(Response {
            message: Message::assistant(text),
            stop_reason: StopReason::EndTurn,
            usage: Default::default(),
            model: self.id().into(),
        })
    }
}
async fn task(rook: &Rook, owner: u128, prompt: &str) -> (u64, u64) {
    let provider = Arc::new(Finish { store: rook.store.clone(), owner, generated: AtomicUsize::new(0) });
    AgentLoop::new(rook, provider, owner).run(prompt).await.unwrap();
    let events = rook.store.events(owner, 0, 256).unwrap();
    let user = events.iter().find(|e| e.record.kind == rook_store::EventKind::UserMessage).unwrap().seq;
    let result = events.iter().find(|e| e.record.label == "turn-result").unwrap().seq;
    (result, user)
}
async fn ask(rook: &Rook, owner: u128, value: Value) -> rook_core::Result<Value> {
    rook.learning_request(owner, learning::arguments(&value.to_string())?, Arc::new(Vault::empty())).await
}
async fn propose(rook: &Rook, owner: u128, result: u64, seq: u64, text: &str, old: Option<&str>) -> Value {
    ask(rook,owner,json!({"action":"propose","result":result,"draft":{"target":{"kind":"memory","text":text,"supersedes":old},"reason":"Review the anchored user preference","evidence":[seq]}})).await.unwrap()
}
async fn apply(rook: &Rook, owner: u128, id: &Value, rollback: bool) -> rook_core::Result<Value> {
    let prepared =
        ask(rook, owner, json!({"action":if rollback{"prepare_rollback"}else{"prepare"},"proposal":id}))
            .await?;
    ask(
        rook,
        owner,
        json!({"action":if rollback{"rollback"}else{"apply"},"proposal":id,"token":prepared["token"]}),
    )
    .await
}
#[tokio::test]
async fn native_task_provenance_is_reviewed_before_memory_changes_and_survives_restart_and_gc() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let c = config();
    let rook = open(root.path(), c.clone());
    let owner = rook.start_session("learning provenance").unwrap();
    let (result, seq) = task(&rook, owner, "Respond in Russian.").await;
    let p = propose(&rook, owner, result, seq, "Respond in Russian.", None).await;
    assert!(rook.memory().unwrap().facts.is_empty());
    let source = ask(&rook, owner, json!({"action":"source","result":result})).await.unwrap();
    assert_eq!(p["source"]["signature"], source["signature"]);
    assert_eq!(source["events"][0]["seq"], seq);
    let again = propose(&rook, owner, result, seq, "Respond in Russian.", None).await;
    assert_eq!(again["duplicate"], p["id"]);
    let preview = ask(&rook, owner, json!({"action":"prepare","proposal":p["id"]})).await.unwrap();
    assert_eq!(preview["diff"]["after"]["source"]["seq"], seq);
    assert!(rook.memory().unwrap().facts.is_empty());
    let applied = apply(&rook, owner, &p["id"], false).await.unwrap();
    assert_eq!(applied["state"], "applied");
    assert_eq!(rook.memory_history().unwrap().len(), 1);
    assert_eq!(rook.memory().unwrap().facts[0].source.as_ref().unwrap().seq, seq);
    let replay =
        ask(&rook, owner, json!({"action":"apply","proposal":p["id"],"token":preview["token"]})).await;
    assert!(replay.is_err());
    drop(rook);
    let rook = open(root.path(), c);
    rook.collect_garbage(false).unwrap();
    let saved = ask(&rook, owner, json!({"action":"inspect","proposal":p["id"]})).await.unwrap();
    assert_eq!(saved["source"], p["source"]);
    let rolled = apply(&rook, owner, &p["id"], true).await.unwrap();
    assert_eq!(rolled["state"], "rolled_back");
    assert!(rook.memory().unwrap().facts.is_empty());
    assert_eq!(rook.memory_history().unwrap().len(), 2);
}
#[tokio::test]
async fn corrections_preserve_pinning_scope_and_unrelated_later_facts_during_rollback() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("correct memory").unwrap();
    let mut original = rook_core::memory::Fact::new("Prefer English.", rook_core::memory::Scope::Global);
    original.pinned = true;
    original.tags = vec!["language".into()];
    rook.remember(original.clone(), None).unwrap();
    let (result, seq) = task(&rook, owner, "Prefer Russian instead.").await;
    let p = propose(&rook, owner, result, seq, "Prefer Russian.", Some(&original.id)).await;
    let applied = apply(&rook, owner, &p["id"], false).await.unwrap();
    assert_eq!(applied["effect"]["after"]["pinned"], true);
    assert_eq!(applied["effect"]["after"]["scope"]["kind"], "global");
    assert_eq!(applied["effect"]["after"]["tags"][0], "language");
    let unrelated = rook_core::memory::Fact::new("Keep independent fact.", rook_core::memory::Scope::Global);
    rook.remember(unrelated.clone(), None).unwrap();
    apply(&rook, owner, &p["id"], true).await.unwrap();
    let book = rook.memory().unwrap();
    assert_eq!(book.get(&original.id), Some(&original));
    assert_eq!(book.get(&unrelated.id), Some(&unrelated));
}
#[tokio::test]
async fn a_head_change_after_review_refuses_application_without_losing_either_receipt_or_fact() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("learning conflict").unwrap();
    let (result, seq) = task(&rook, owner, "Respond in Russian.").await;
    let p = propose(&rook, owner, result, seq, "Respond in Russian.", None).await;
    let preview = ask(&rook, owner, json!({"action":"prepare","proposal":p["id"]})).await.unwrap();
    let independent = rook_core::memory::Fact::new("New edit.", rook_core::memory::Scope::Global);
    rook.remember(independent.clone(), None).unwrap();
    assert!(
        ask(&rook, owner, json!({"action":"apply","proposal":p["id"],"token":preview["token"]}))
            .await
            .is_err()
    );
    assert_eq!(rook.memory().unwrap().facts, vec![independent]);
    assert_eq!(
        ask(&rook, owner, json!({"action":"inspect","proposal":p["id"]})).await.unwrap()["state"],
        "proposed"
    );
}
#[tokio::test]
async fn automatic_review_proposes_once_and_only_a_literal_user_assertion_may_apply_under_policy() {
    let _serial = TESTS.lock().await;
    for automatic in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut c = config();
        c.learning.automatic = automatic;
        c.learning.auto_apply_memory = true;
        c.sandbox.stance = rook_tools::policy::Stance::Autonomous;
        let rook = open(root.path(), c);
        let owner = rook.start_session("automatic learning").unwrap();
        let provider = Arc::new(Finish { store: rook.store.clone(), owner, generated: AtomicUsize::new(0) });
        AgentLoop::new(&rook, provider.clone(), owner).run("Respond in Russian.").await.unwrap();
        assert_eq!(provider.generated.load(Ordering::SeqCst), usize::from(automatic));
        let proposals = ask(&rook, owner, json!({"action":"list"})).await.unwrap();
        assert_eq!(proposals["items"].as_array().unwrap().len(), usize::from(automatic));
        assert_eq!(rook.memory().unwrap().facts.len(), usize::from(automatic));
        if automatic {
            assert_eq!(proposals["items"][0]["state"], "applied");
            assert_eq!(proposals["attempts"][0]["state"], "finished");
        }
    }
}
#[tokio::test]
async fn reached_source_byte_bounds_refuse_proposals_from_truncated_user_evidence() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("bounded source").unwrap();
    let prompt = "x".repeat(16385);
    let (result, seq) = task(&rook, owner, &prompt).await;
    let source = ask(&rook, owner, json!({"action":"source","result":result})).await.unwrap();
    assert_eq!(source["events"][0]["truncated"], true);
    let actual = rook.store.events(owner, seq, 1).unwrap();
    assert_eq!(rook.store.stat_object(&actual[0].record.body).unwrap().unwrap().size_raw, 16385);
    assert!(ask(&rook,owner,json!({"action":"propose","result":result,"draft":{"target":{"kind":"memory","text":"Unsupported prefix lesson"},"reason":"must refuse","evidence":[seq]}})).await.is_err());
    assert!(rook.memory().unwrap().facts.is_empty());
}

struct FailTask;
#[async_trait]
impl Provider for FailTask {
    fn id(&self) -> &str {
        "fixture/failed-task"
    }
    fn context_window(&self) -> usize {
        32768
    }
    async fn complete(&self, _: Request) -> rook_llm::Result<Response> {
        Err(rook_llm::LlmError::Other("fixture model transport failed".into()))
    }
}
#[tokio::test]
async fn a_failed_native_task_never_schedules_a_learning_review_or_changes_memory() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let mut c = config();
    c.learning.automatic = true;
    let rook = open(root.path(), c);
    let owner = rook.start_session("failed task learning").unwrap();
    assert!(AgentLoop::new(&rook, Arc::new(FailTask), owner).run("Find a working procedure").await.is_err());
    let list = ask(&rook, owner, json!({"action":"list"})).await.unwrap();
    assert!(list["items"].as_array().unwrap().is_empty());
    assert!(list["attempts"].as_array().unwrap().is_empty());
    assert!(rook.memory().unwrap().facts.is_empty());
    assert!(ask(&rook,owner,json!({"action":"propose","result":0,"draft":{"target":{"kind":"memory","text":"Unsupported failed procedure"},"reason":"no completion exists","evidence":[0]}})).await.is_err());
}
#[tokio::test]
async fn a_reviewed_revision_keeps_its_parent_and_invalidates_the_old_apply_token() {
    let _serial = TESTS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path(), config());
    let owner = rook.start_session("learning revision").unwrap();
    let (result, seq) = task(&rook, owner, "Respond in Russian.").await;
    let p = propose(&rook, owner, result, seq, "Respond in English.", None).await;
    let old = ask(&rook, owner, json!({"action":"prepare","proposal":p["id"]})).await.unwrap();
    let revised=ask(&rook,owner,json!({"action":"revise","proposal":p["id"],"draft":{"target":{"kind":"memory","text":"Respond in Russian."},"reason":"User reviewed the exact source","evidence":[seq]}})).await.unwrap();
    assert_eq!(revised["predecessor"], p["id"]);
    assert_eq!(revised["source"], p["source"]);
    assert!(
        ask(&rook, owner, json!({"action":"apply","proposal":p["id"],"token":old["token"]})).await.is_err()
    );
    apply(&rook, owner, &revised["id"], false).await.unwrap();
    assert_eq!(rook.memory().unwrap().facts[0].text, "Respond in Russian.");
}

#[test]
fn concurrent_ref_receipt_publication_has_one_consistent_winner_and_deleted_owners_cannot_commit() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(rook_store::Store::open(root.path().join("store")).unwrap());
    let owner = rook_store::new_session_id();
    store
        .create_session(&rook_store::SessionMeta::new(
            owner,
            "atomic receipt",
            root.path().display().to_string(),
            0,
        ))
        .unwrap();
    let old = store.put(rook_store::Kind::Memory, b"old").unwrap();
    store.set_ref("head", &old).unwrap();
    let key = format!("receipt/{owner:032x}");
    store.kv_update_session_values(owner, &[(&key, b"old")]).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for value in [b"first".as_slice(), b"second".as_slice()] {
        let store = store.clone();
        let barrier = barrier.clone();
        let key = key.clone();
        tasks.push(std::thread::spawn(move || {
            let object = store.put(rook_store::Kind::Memory, value).unwrap();
            let history = format!("history/{}", object.to_hex());
            barrier.wait();
            store
                .compare_set_ref_session(rook_store::SessionRefUpdate {
                    session: owner,
                    reference: "head",
                    expected: Some(old),
                    object,
                    history: &history,
                    companion: &key,
                    expected_companion: Some(b"old"),
                    value,
                    maximum: 64,
                })
                .unwrap()
        }));
    }
    barrier.wait();
    let won = tasks.into_iter().map(|task| task.join().unwrap()).filter(|won| *won).count();
    assert_eq!(won, 1);
    let head = store.get_ref("head").unwrap().unwrap();
    let bytes = store.get(&head).unwrap();
    assert_eq!(store.kv_get(&key).unwrap().unwrap(), bytes);
    assert_eq!(store.list_refs("history/").unwrap().len(), 1);
    store.delete_session(owner).unwrap();
    let other = store.put(rook_store::Kind::Memory, b"forbidden").unwrap();
    assert!(
        store
            .compare_set_ref_session(rook_store::SessionRefUpdate {
                session: owner,
                reference: "head",
                expected: Some(head),
                object: other,
                history: "history/forbidden",
                companion: &key,
                expected_companion: None,
                value: b"forbidden",
                maximum: 64
            })
            .is_err()
    );
    assert_eq!(store.get_ref("head").unwrap(), Some(head));
}
