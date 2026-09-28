//! Durable scheduling and steering tested without a model or wall-clock days.
use async_trait::async_trait;
use rook_core::{Rook, agent::AgentLoop, work::managed as work};
use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
use rook_proto::work::{Action, Start, Status, Steer};
use std::sync::{Arc, Mutex};

fn engine(workspace: &std::path::Path, store: &std::path::Path) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(store).unwrap(),
        rook_core::Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "0.7.2"),
        rook_skills::SkillIndex::discover(&[]).0,
        workspace.into(),
    )
}

fn start(rook: &Rook) -> rook_proto::work::Run {
    work::start(
        rook,
        Start {
            conversation: None,
            goal: "Inspect evidence.txt and report its contents".into(),
            workspace: None,
            autonomous: true,
            max_iterations: None,
            max_tokens: None,
            max_seconds: None,
        },
    )
    .unwrap()
}

fn correction(id: &str, text: &str) -> Steer {
    Steer { id: id.into(), text: text.into() }
}

#[test]
fn steering_is_durable_idempotent_bounded_and_acknowledged_only_on_delivery() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.max_messages = 1;
    rook.config.work.max_message_bytes = 12;
    rook.config.work.max_runs = 1;
    let run = start(&rook);
    assert!(
        work::start(
            &rook,
            Start {
                conversation: None,
                goal: "duplicate".into(),
                workspace: None,
                autonomous: false,
                max_iterations: None,
                max_tokens: None,
                max_seconds: None
            }
        )
        .is_err()
    );
    let receipt = work::steer(&rook, &run.id, correction("one", "use Russian")).unwrap();
    assert!(receipt.applied_at.is_none());
    assert_eq!(work::pending(&rook, &run.id).unwrap().len(), 1);
    assert_eq!(work::steer(&rook, &run.id, correction("one", "use Russian")).unwrap().id, "one");
    assert!(work::steer(&rook, &run.id, correction("one", "different")).is_err());
    assert!(work::steer(&rook, &run.id, correction("two", "another")).is_err());
    assert!(work::steer(&rook, &run.id, correction("long", "x".repeat(13).as_str())).is_err());
    work::control(&rook, &run.id, Action::Pause).unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert_eq!(work::read(&rook, &run.id).unwrap().run.status, Status::Paused);
    let session = rook.start_session("receipt").unwrap();
    let incoming = work::pending(&rook, &run.id).unwrap();
    work::heard(&rook, &run.id, session, &incoming[0]).unwrap();
    assert!(work::pending(&rook, &run.id).unwrap().is_empty());
    assert!(rook.goal(session).unwrap().unwrap().contains("use Russian"));
    assert_eq!(
        work::read(&rook, &run.id).unwrap().run.instructions[0].session.as_deref(),
        Some(rook_store::format_session_id(session).as_str())
    );
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    work::forget(&rook, &run.id).unwrap();
    assert!(work::list(&rook).unwrap().is_empty());
    assert!(rook.store.kv_get(&format!("work/managed/{}", run.id)).unwrap().is_none());
}

struct Script {
    replies: Mutex<Vec<rook_llm::Result<Response>>>,
    seen: Mutex<Vec<String>>,
}
impl Script {
    fn new(replies: Vec<rook_llm::Result<Response>>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies), seen: Mutex::new(Vec::new()) })
    }
}
#[async_trait]
impl Provider for Script {
    fn id(&self) -> &str {
        "managed/script"
    }
    fn context_window(&self) -> usize {
        32_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        if request.messages.iter().any(|m| m.content.starts_with("Classify whether")) {
            return answer(r#"{"action":"finish"}"#);
        }
        self.seen
            .lock()
            .unwrap()
            .push(request.messages.iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n"));
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            return Err(rook_llm::LlmError::Other("script exhausted".into()));
        }
        replies.remove(0)
    }
}
fn answer(text: &str) -> rook_llm::Result<Response> {
    Ok(Response {
        message: Message::assistant(text),
        stop_reason: StopReason::EndTurn,
        usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
        model: "managed/script".into(),
    })
}
fn read() -> rook_llm::Result<Response> {
    let mut response = answer("").unwrap();
    response.message.tool_calls.push(ToolCall {
        id: "read".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"evidence.txt"}),
    });
    response.stop_reason = StopReason::ToolUse;
    Ok(response)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_outage_resumes_the_saved_session_and_a_correction_reaches_verification() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider =
        Script::new(vec![read(), Err(rook_llm::LlmError::Other("provider temporarily unavailable".into()))]);
    let first =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(first.status, Status::RetryWait);
    assert_eq!(first.iterations, 0, "outages are not idle turns");
    assert!(first.tokens > 0, "work before the failure is billed");
    let session = first.session.clone();
    work::steer(&rook, &run.id, correction("answer-language", "Include the file name")).unwrap();
    work::update(&rook, &run.id, |s| {
        s.run.next_attempt_at = Some(0);
        Ok(())
    })
    .unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let provider = Script::new(vec![
        answer("evidence.txt contains done."),
        read(),
        answer("Verified contents and answer.\nVERDICT: holds"),
    ]);
    let result =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(result.session, session);
    assert!(result.tokens > first.tokens);
    assert!(result.instructions[0].applied_at.is_some());
    let seen = provider.seen.lock().unwrap();
    assert!(seen[0].contains("Include the file name"));
    assert!(seen.iter().any(|prompt| prompt.contains("the agent has just finished a turn")
        && prompt.contains("Include the file name")));
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_turns_continue_and_total_budgets_survive_days_and_resume() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::update(&rook, &run.id, |s| {
        s.run.created_at = work::now() - 3 * 86400;
        s.run.max_iterations = 3;
        Ok(())
    })
    .unwrap();
    for count in 1..=3 {
        let provider = Script::new(vec![read(), answer("More work remains")]);
        let result = work::advance(
            &rook,
            &run.id,
            |session| {
                let mut agent = AgentLoop::new(&rook, provider.clone(), session);
                agent.max_steps = 1;
                Ok(agent)
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.iterations, count);
        assert_eq!(result.status, if count == 3 { Status::Limited } else { Status::Queued }, "{result:?}");
    }
    assert!(work::control(&rook, &run.id, Action::Resume).is_err());
    let bill = work::read(&rook, &run.id).unwrap().run.tokens;
    assert!(bill > 0);
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    assert_eq!(work::read(&rook, &run.id).unwrap().run.tokens, bill);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_correction_received_during_verification_prevents_false_completion() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider = Script::new(vec![answer("done"), read(), answer("VERDICT: holds")]);
    let mut corrected = false;
    let result = work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
        |event| {
            if !corrected && matches!(event, rook_core::agent::Progress::Delegating { .. }) {
                work::steer(&rook, &run.id, correction("late", "Also explain the evidence")).unwrap();
                corrected = true;
            }
        },
    )
    .await
    .unwrap();
    assert!(corrected, "the correction must really arrive during verification");
    assert_eq!(result.status, Status::Queued, "{result:?}");
    assert!(result.instructions[0].applied_at.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn exceeding_time_or_tokens_stops_before_another_provider_call() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    work::update(&rook, &run.id, |s| {
        s.run.created_at = work::now() - 8 * 86400;
        Ok(())
    })
    .unwrap();
    let result =
        work::advance(&rook, &run.id, |_| panic!("no model call after seven days"), |_| {}).await.unwrap();
    assert_eq!(result.status, Status::Limited);
    work::control(&rook, &run.id, Action::Cancel).unwrap();
    let run = start(&rook);
    work::update(&rook, &run.id, |s| {
        s.run.max_tokens = 10;
        s.run.tokens = 10;
        Ok(())
    })
    .unwrap();
    let result =
        work::advance(&rook, &run.id, |_| panic!("no model call over token budget"), |_| {}).await.unwrap();
    assert_eq!(result.status, Status::Limited);
}

struct Held {
    script: Arc<Script>,
    first: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl Provider for Held {
    fn id(&self) -> &str {
        "managed/held"
    }
    fn context_window(&self) -> usize {
        32_000
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.script.complete(request).await
    }
}
fn held(script: Vec<rook_llm::Result<Response>>) -> Arc<Held> {
    Arc::new(Held {
        script: Script::new(script),
        first: true.into(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_sent_during_a_model_request_is_acknowledged_at_the_next_boundary() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider =
        held(vec![read(), answer("evidence.txt contains done."), read(), answer("VERDICT: holds")]);
    let running =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {});
    let steering = async {
        provider.entered.notified().await;
        let receipt = work::steer(&rook, &run.id, correction("during", "Include the filename")).unwrap();
        assert!(receipt.applied_at.is_none(), "receiving is not delivering");
        provider.release.notify_one();
    };
    let (result, ()) = tokio::join!(running, steering);
    let result = result.unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert!(result.instructions[0].applied_at.is_some());
    let seen = provider.script.seen.lock().unwrap();
    assert!(!seen[0].contains("Include the filename"));
    assert!(seen[1].contains("Include the filename"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_cancel_wait_for_the_current_request_without_starting_another_turn() {
    for action in [Action::Pause, Action::Cancel] {
        let workspace = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let rook = engine(workspace.path(), store.path());
        let run = start(&rook);
        let provider = held(vec![Err(rook_llm::LlmError::Other("provider unavailable".into()))]);
        let running = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        );
        let controlling = async {
            provider.entered.notified().await;
            work::control(&rook, &run.id, action).unwrap();
            provider.release.notify_one();
        };
        let (result, ()) = tokio::join!(running, controlling);
        let result = result.unwrap();
        assert_eq!(
            result.status,
            match action {
                Action::Pause => Status::Paused,
                _ => Status::Cancelled,
            }
        );
        if matches!(action, Action::Cancel) {
            assert!(work::read(&rook, &run.id).unwrap().active.is_none());
            work::forget(&rook, &run.id).unwrap();
        } else {
            let again =
                work::advance(&rook, &run.id, |_| panic!("paused runs cannot start requests"), |_| {})
                    .await
                    .unwrap();
            assert_eq!(again.status, Status::Paused);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_effect_blocks_the_scheduler_before_it_can_retry() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let session = rook.start_session("unknown effect").unwrap();
    let named = rook_store::format_session_id(session);
    work::update(&rook, &run.id, |s| {
        s.active = Some(work::Active {
            start_seq: 0,
            session: named.clone(),
            before: Default::default(),
            answer: None,
            accounted_tokens: 0,
        });
        s.run.session = Some(named.clone());
        Ok(())
    })
    .unwrap();
    let receipt = serde_json::json!({
        "version":1,"session":named,"turn":"old-turn","owner":"dead-process","pid":0,
        "started_at":0,"updated_at":0,"status":"interrupted","task":"write one file","start_seq":0,
        "completed_operations":0,"last_result_seq":null,"background":[],"pending":null,
        "unknown":[{"session":named,"id":"operation-one","tool":"write_file","arguments":"{}",
            "started_at":0,"call_seq":0,"may_have_effects":true,"job":null,"registry":null}]
    });
    rook.store.kv_set(&format!("execution/{session:032x}"), &serde_json::to_vec(&receipt).unwrap()).unwrap();
    let result = work::advance(
        &rook,
        &run.id,
        |_| panic!("must inspect unknown effects before using the model"),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Blocked);
    assert!(result.reason.contains("unknown"), "{}", result.reason);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verification_outage_retries_the_check_without_losing_the_completed_turn() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let provider = Script::new(vec![
        answer("evidence.txt contains done."),
        Err(rook_llm::LlmError::Other("provider temporarily unavailable".into())),
    ]);
    let first =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(first.status, Status::RetryWait);
    assert_eq!(first.iterations, 0);
    assert!(work::read(&rook, &run.id).unwrap().active.unwrap().answer.is_some());
    work::update(&rook, &run.id, |s| {
        s.run.next_attempt_at = Some(0);
        Ok(())
    })
    .unwrap();
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    let provider = Script::new(vec![read(), answer("VERDICT: holds")]);
    let result =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, provider.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(result.reply, "evidence.txt contains done.");
    assert_eq!(result.session, first.session);
    assert!(provider.seen.lock().unwrap()[0].contains("the agent has just finished a turn"));
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_back_off_until_the_outage_window_requires_attention() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let mut rook = engine(workspace.path(), store.path());
    rook.config.work.retry_initial_secs = 9;
    rook.config.work.retry_max_secs = 12;
    let run = start(&rook);
    for attempt in 1..=3 {
        let provider = Script::new(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
        let result = work::advance(
            &rook,
            &run.id,
            |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(result.consecutive_failures, attempt);
        if attempt == 3 {
            assert_eq!(result.status, Status::Blocked);
        } else {
            assert_eq!(result.status, Status::RetryWait);
            let delay = result.next_attempt_at.unwrap().saturating_sub(result.updated_at);
            assert_eq!(delay, if attempt == 1 { 9 } else { 12 });
            work::update(&rook, &run.id, |s| {
                s.run.next_attempt_at = Some(0);
                if attempt == 2 {
                    s.failed_since = Some(work::now() - 86401);
                }
                Ok(())
            })
            .unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pausing_between_tools_keeps_the_remaining_batch_from_running() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let rook = engine(workspace.path(), store.path());
    let run = start(&rook);
    let mut response = answer("").unwrap();
    response.stop_reason = StopReason::ToolUse;
    for path in ["first.txt", "second.txt"] {
        response.message.tool_calls.push(ToolCall {
            id: path.into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":path,"content":"effect"}),
        });
    }
    let provider = Script::new(vec![Ok(response)]);
    let result = work::advance(
        &rook,
        &run.id,
        |session| Ok(AgentLoop::new(&rook, provider.clone(), session)),
        |event| {
            if matches!(event, rook_core::agent::Progress::ToolDone { .. }) {
                work::control(&rook, &run.id, Action::Pause).unwrap();
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Paused);
    assert_eq!(std::fs::read_to_string(workspace.path().join("first.txt")).unwrap(), "effect");
    assert!(
        !workspace.path().join("second.txt").exists(),
        "the remaining operation was never authorized to continue after pause"
    );
    work::control(&rook, &run.id, Action::Resume).unwrap();
    let resumed = Script::new(vec![Err(rook_llm::LlmError::Other("temporary outage".into()))]);
    let continued =
        work::advance(&rook, &run.id, |session| Ok(AgentLoop::new(&rook, resumed.clone(), session)), |_| {})
            .await
            .unwrap();
    assert_eq!(continued.status, Status::RetryWait);
    assert_eq!(continued.session, result.session, "pause preserves the partial turn's context");
    assert!(resumed.seen.lock().unwrap()[0].contains("first.txt"));
    assert!(!workspace.path().join("second.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_continues_the_existing_conversation_across_limits_and_restart() {
    let workspace = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("evidence.txt"), "done").unwrap();
    let mut rook = engine(workspace.path(), store.path());
    let session = rook.start_session("existing conversation").unwrap();
    let named = rook_store::format_session_id(session);
    let provider = Script::new(vec![answer("Earlier discussion saved.")]);
    AgentLoop::new(&rook, provider, session).run("ORIGINAL_REQUIREMENT: preserve the API").await.unwrap();
    rook.config.work.max_iterations = 1;
    rook.config.work.max_tokens = 1;
    rook.config.work.max_seconds = 1;
    let run = work::start(
        &rook,
        Start {
            conversation: Some(rook_proto::work::Conversation {
                session: named.clone(),
                model: None,
                effort: "high".into(),
                stance: "assist".into(),
                options: Default::default(),
            }),
            goal: "Inspect evidence.txt".into(),
            workspace: None,
            autonomous: false,
            max_iterations: Some(0),
            max_tokens: Some(0),
            max_seconds: Some(0),
        },
    )
    .unwrap();
    assert_eq!(run.id, named, "the goal is this session, not a new user-visible task");
    work::update(&rook, &run.id, |s| {
        s.run.created_at = work::now() - 9 * 86400;
        Ok(())
    })
    .unwrap();
    let provider = Script::new(vec![read(), answer("More work remains")]);
    let first = work::advance(
        &rook,
        &run.id,
        |id| {
            assert_eq!(id, session);
            let mut agent = AgentLoop::new(&rook, provider.clone(), id);
            agent.max_steps = 1;
            Ok(agent)
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(first.recent[0].stopped, "max_steps", "the stage really reaches its bound");
    assert_eq!(first.tokens, 30, "earlier chat tokens are not billed again");
    assert_eq!(first.status, Status::Queued);
    assert!(
        rook.store.get_session(session).unwrap().unwrap().tags.iter().any(|tag| tag == "rook:work"),
        "retention must keep the conversation between stages"
    );
    assert!(provider.seen.lock().unwrap()[0].contains("ORIGINAL_REQUIREMENT"));
    drop(rook);
    let rook = engine(workspace.path(), store.path());
    work::steer(&rook, &named, correction("new-scope", "/goal Report the file name and contents")).unwrap();
    let provider = Script::new(vec![
        answer("evidence.txt contains done."),
        read(),
        answer("Checked evidence.\nVERDICT: holds"),
    ]);
    let result = work::advance(
        &rook,
        &named,
        |id| {
            assert_eq!(id, session);
            Ok(AgentLoop::new(&rook, provider.clone(), id))
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(result.session.as_deref(), Some(named.as_str()));
    assert_eq!(result.iterations, 2, "a previous completed stage must not be replayed");
    assert_eq!(result.goal, "Report the file name and contents");
    assert!(
        !rook.store.get_session(session).unwrap().unwrap().tags.iter().any(|tag| tag == "rook:work"),
        "completed goals no longer pin their transcript"
    );
    assert!(result.instructions[0].applied_at.is_some());
    assert!(provider.seen.lock().unwrap()[0].contains("ORIGINAL_REQUIREMENT"));
    assert!(provider.seen.lock().unwrap().iter().any(|p| p.contains("More work remains")));
}
