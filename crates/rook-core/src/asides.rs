//! Bounded, caller-owned questions; they never acquire foreground turn ownership.
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::{CoreError, Result, Rook, agent::AgentLoop};
use rook_proto::aside::{Reply, Request};

pub const TIMEOUT: Duration = Duration::from_secs(180);
static QUESTIONS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));

/// Dropping this future cancels only this auxiliary request. No task is detached,
/// no message is queued, and the existing aside route accounts for its own usage.
pub async fn reply(rook: &Rook, session: u128, request: Request) -> Reply {
    reply_using(rook, session, request, |model| Ok(crate::models::chosen(&rook.config, model)?.into()))
        .await
        .unwrap_or_else(|error| Reply::Failed {
            question: None,
            error: error.to_string().chars().take(4096).collect(),
        })
}

async fn reply_using(
    rook: &Rook,
    session: u128,
    request: Request,
    provider: impl FnOnce(Option<&str>) -> Result<Arc<dyn rook_llm::Provider>>,
) -> Result<Reply> {
    request.validate().map_err(|why| CoreError::Other(why.into()))?;
    let _permit = QUESTIONS
        .clone()
        .try_acquire_owned()
        .map_err(|_| CoreError::Other("side questions are busy; retry after one finishes".into()))?;
    rook.store
        .get_session_limited(session, 256 * 1024)?
        .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
    let provider = provider(request.model.as_deref())?;
    let agent = AgentLoop::new(rook, provider, session);
    let question = Some(agent.aside_display_question(&request.question));
    let result =
        tokio::time::timeout(TIMEOUT, agent.aside(&request.question, |_| {})).await.unwrap_or_else(|_| {
            Err(CoreError::Other("side question timed out; the foreground turn is unchanged".into()))
        });
    Ok(match result {
        Ok(answer) => Reply::Complete { question, answer },
        Err(error) => Reply::Failed { question, error: error.to_string().chars().take(4096).collect() },
    })
}

#[cfg(test)]
async fn answer_using(
    rook: &Rook,
    session: u128,
    request: Request,
    provider: impl FnOnce(Option<&str>) -> Result<Arc<dyn rook_llm::Provider>>,
) -> Result<String> {
    match reply_using(rook, session, request, provider).await? {
        Reply::Complete { answer, .. } => Ok(answer),
        Reply::Failed { error, .. } => Err(CoreError::Other(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Model(Mutex<Vec<rook_llm::Request>>);

    #[derive(Clone, Copy)]
    enum Incomplete {
        Eof,
        UnconfirmedDone,
        Failed,
    }

    #[async_trait::async_trait]
    impl rook_llm::Provider for Incomplete {
        fn id(&self) -> &str {
            "owned/incomplete-aside"
        }
        fn context_window(&self) -> usize {
            32000
        }
        fn visit_connection_secrets(&self, visitor: &mut dyn FnMut(&str)) {
            visitor(OWNED_KEY);
        }
        async fn complete(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            unreachable!("the owned fixture exercises the streaming path")
        }
        async fn stream(&self, _: rook_llm::Request) -> rook_llm::Result<rook_llm::ResponseStream> {
            let mut deltas = vec![Ok(rook_llm::Delta::Text(format!("OWNED_PARTIAL {OWNED_KEY}")))];
            match self {
                Self::Eof => {}
                Self::UnconfirmedDone => {
                    deltas.push(Ok(rook_llm::Delta::ResponseMetadata {
                        usage_reported: true,
                        completion_confirmed: false,
                    }));
                    deltas.push(Ok(rook_llm::Delta::Done {
                        stop_reason: rook_llm::StopReason::EndTurn,
                        usage: rook_llm::Usage { input_tokens: 5, output_tokens: 7, ..Default::default() },
                        model: "owned/incomplete-aside".into(),
                    }));
                }
                Self::Failed => {
                    deltas.push(Err(rook_llm::LlmError::Other(format!("owned partial failure {OWNED_KEY}"))))
                }
            }
            Ok(Box::pin(futures_util::stream::iter(deltas)))
        }
    }

    async fn incomplete_side_question(mode: Incomplete) {
        let home = tempfile::tempdir().unwrap();
        let rook = owned_rook(home.path());
        let session = rook.start_session("owned incomplete aside").unwrap();
        rook.log(session, rook_store::EventKind::UserMessage, "", "OWNED_FOREGROUND").unwrap();
        let mut live = String::new();
        let result = AgentLoop::new(&rook, Arc::new(mode), session)
            .aside("Which port?", |delta| {
                if let rook_llm::Delta::Text(text) = delta {
                    live.push_str(text);
                }
            })
            .await;
        let error = result.expect_err("an unconfirmed or failed stream cannot become a complete side answer");
        assert!(!error.to_string().contains(OWNED_KEY));
        assert!(live.is_empty(), "partial replies are not displayed as completed answers");
        let history = rook.transcript(session, 0, 100, 65536).unwrap();
        assert!(!history.iter().any(|e| e.label == "btw" || e.kind == "assistant"));
        let partial = history
            .iter()
            .find(|e| e.label == "btw incomplete")
            .expect("retain the partial answer explicitly as incomplete");
        assert!(partial.body.contains("OWNED_PARTIAL") && !partial.body.contains(OWNED_KEY));
        let receipt: serde_json::Value =
            serde_json::from_str(&history.iter().find(|e| e.label == "rook:model-aux:v1").unwrap().body)
                .unwrap();
        assert_eq!(receipt["purpose"], "aside");
        assert_eq!(receipt["receipt"]["complete"], false);
        let meta = rook.store.get_session(session).unwrap().unwrap();
        let expected = if matches!(mode, Incomplete::UnconfirmedDone) { (5, 7) } else { (0, 0) };
        assert_eq!(
            (meta.tokens_in, meta.tokens_out),
            expected,
            "retain reported usage without inventing absent counters"
        );
        assert_eq!(history.iter().filter(|e| e.kind == "user").count(), 1);
    }

    #[tokio::test]
    async fn incomplete_side_eof_never_becomes_a_successful_answer() {
        incomplete_side_question(Incomplete::Eof).await;
    }

    #[tokio::test]
    async fn incomplete_side_synthesized_done_keeps_reported_usage_without_claiming_completion() {
        incomplete_side_question(Incomplete::UnconfirmedDone).await;
    }

    #[tokio::test]
    async fn incomplete_side_stream_error_retains_a_masked_partial_without_a_successful_reply() {
        incomplete_side_question(Incomplete::Failed).await;
    }

    const OWNED_KEY: &str = "owned-aside-credential-for-regression-81273645";

    struct KeyEcho {
        seen: Mutex<Vec<rook_llm::Request>>,
        fail: bool,
        split: bool,
    }

    #[async_trait::async_trait]
    impl rook_llm::Provider for KeyEcho {
        fn id(&self) -> &str {
            "owned/key-echo"
        }
        fn context_window(&self) -> usize {
            32000
        }
        fn visit_connection_secrets(&self, visitor: &mut dyn FnMut(&str)) {
            visitor(OWNED_KEY);
        }
        async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            self.seen.lock().unwrap().push(request);
            if self.fail {
                return Err(rook_llm::LlmError::Other(format!("owned refusal {OWNED_KEY}")));
            }
            Ok(rook_llm::Response {
                message: rook_llm::Message::assistant(format!("owned answer {OWNED_KEY}")),
                stop_reason: rook_llm::StopReason::EndTurn,
                usage: Default::default(),
                model: "owned/key-echo".into(),
            })
        }

        async fn stream(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::ResponseStream> {
            let response = self.complete(request).await?;
            let text = response.message.content;
            let mut deltas = Vec::new();
            if self.split {
                let (first, rest) = text.split_at(text.len() / 2);
                deltas.push(Ok(rook_llm::Delta::Text(first.into())));
                deltas.push(Ok(rook_llm::Delta::Text(rest.into())));
            } else {
                deltas.push(Ok(rook_llm::Delta::Text(text)));
            }
            deltas.push(Ok(rook_llm::Delta::Done {
                stop_reason: response.stop_reason,
                usage: response.usage,
                model: response.model,
            }));
            Ok(Box::pin(futures_util::stream::iter(deltas)))
        }
    }

    #[tokio::test]
    async fn side_reply_carries_only_the_masked_question_on_success_and_failure() {
        for fail in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let rook = owned_rook(home.path());
            let session = rook.start_session("owned question display").unwrap();
            let model = Arc::new(KeyEcho { seen: Default::default(), fail, split: false });
            let reply = reply_using(
                &rook,
                session,
                Request { question: format!("What is {OWNED_KEY}?"), model: None },
                |_| Ok(model),
            )
            .await
            .unwrap();
            let encoded = serde_json::to_string(&reply).unwrap();
            assert!(!encoded.contains(OWNED_KEY));
            let question = match reply {
                Reply::Complete { question, .. } => {
                    assert!(!fail);
                    question
                }
                Reply::Failed { question, .. } => {
                    assert!(fail);
                    question
                }
            };
            assert_eq!(question.as_deref(), Some("What is ${secret}?"));
        }
    }

    #[tokio::test]
    async fn aside_masks_its_known_connection_key_in_request_live_answer_and_saved_note() {
        let home = tempfile::tempdir().unwrap();
        let rook = owned_rook(home.path());
        let session = rook.start_session("owned key aside").unwrap();
        let model = Arc::new(KeyEcho { seen: Default::default(), fail: false, split: false });
        let mut live = String::new();
        let answer = AgentLoop::new(&rook, model.clone(), session)
            .aside(&format!("What is {OWNED_KEY}?"), |delta| {
                if let rook_llm::Delta::Text(text) = delta {
                    live.push_str(text);
                }
            })
            .await
            .unwrap();
        let sent = model.seen.lock().unwrap()[0].messages.iter().any(|m| m.content.contains(OWNED_KEY));
        let saved =
            rook.transcript(session, 0, 100, 65536).unwrap().iter().any(|e| e.body.contains(OWNED_KEY));
        let exposed = [sent, live.contains(OWNED_KEY), answer.contains(OWNED_KEY), saved];
        eprintln!("owned aside key exposure [request, live, reply, saved] = {exposed:?}");
        assert_eq!(exposed, [false; 4], "known connection keys must stay out of every aside surface");
    }

    #[tokio::test]
    async fn aside_masks_its_known_connection_key_in_provider_errors() {
        let home = tempfile::tempdir().unwrap();
        let rook = owned_rook(home.path());
        let session = rook.start_session("owned error aside").unwrap();
        let model = Arc::new(KeyEcho { seen: Default::default(), fail: true, split: false });
        let error =
            AgentLoop::new(&rook, model, session).aside("Why did it fail?", |_| {}).await.unwrap_err();
        assert!(!error.to_string().contains(OWNED_KEY), "aside errors must mask known connection keys");
    }

    #[tokio::test]
    async fn aside_masks_a_known_connection_key_split_across_model_fragments() {
        let home = tempfile::tempdir().unwrap();
        let rook = owned_rook(home.path());
        let session = rook.start_session("owned split key aside").unwrap();
        let model = Arc::new(KeyEcho { seen: Default::default(), fail: false, split: true });
        let mut live = String::new();
        let answer = AgentLoop::new(&rook, model, session)
            .aside("Which port?", |delta| {
                if let rook_llm::Delta::Text(text) = delta {
                    live.push_str(text);
                }
            })
            .await
            .unwrap();
        assert!(!live.contains(OWNED_KEY), "masking individual fragments can still reveal the complete key");
        assert_eq!(live, answer);
        assert!(answer.contains("${secret}"));
    }

    struct HeldModel {
        started: tokio::sync::Notify,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    struct Dropped(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl rook_llm::Provider for HeldModel {
        fn id(&self) -> &str {
            "owned/held-aside"
        }
        fn context_window(&self) -> usize {
            32000
        }
        async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            if request.messages.last().is_some_and(|m| m.content.contains("OWNED_SIDE_QUESTION")) {
                assert!(request.tools.is_empty());
                return rook_llm::Provider::complete(&Model::default(), request).await;
            }
            let _guard = Dropped(self.dropped.clone());
            self.started.notify_one();
            std::future::pending().await
        }
    }

    fn owned_rook(home: &std::path::Path) -> Arc<Rook> {
        Arc::new(Rook::from_parts(
            rook_store::Store::open(home.join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            home.into(),
        ))
    }

    #[tokio::test]
    async fn side_answer_completes_while_the_foreground_model_is_still_waiting() {
        let home = tempfile::tempdir().unwrap();
        let rook = owned_rook(home.path());
        let session = rook.start_session("concurrent aside").unwrap();
        let model = Arc::new(HeldModel { started: tokio::sync::Notify::new(), dropped: Default::default() });
        let foreground = tokio::spawn({
            let (rook, model) = (rook.clone(), model.clone());
            async move { AgentLoop::new(&rook, model, session).run("OWNED_FOREGROUND").await }
        });
        tokio::time::timeout(Duration::from_secs(5), model.started.notified()).await.unwrap();
        let answer = tokio::time::timeout(
            Duration::from_secs(5),
            answer_using(
                &rook,
                session,
                Request { question: "OWNED_SIDE_QUESTION".into(), model: None },
                |_| Ok(model.clone()),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(answer, "The selected port is 7717.");
        assert!(!foreground.is_finished());
        assert!(!model.dropped.load(std::sync::atomic::Ordering::SeqCst));
        let history = rook.transcript(session, 0, 100, 65536).unwrap();
        assert_eq!(history.iter().filter(|e| e.kind == "user").count(), 1);
        assert_eq!(history.iter().find(|e| e.kind == "user").unwrap().body, "OWNED_FOREGROUND");
        assert!(!history.iter().any(|e| e.kind == "assistant"));
        assert_eq!(history.iter().filter(|e| e.label == "btw").count(), 1);
        foreground.abort();
        assert!(foreground.await.unwrap_err().is_cancelled());
        assert!(model.dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_a_side_question_cancels_its_provider_without_recording_a_reply() {
        let home = tempfile::tempdir().unwrap();
        let rook = owned_rook(home.path());
        let session = rook.start_session("cancel aside").unwrap();
        let model = Arc::new(HeldModel { started: tokio::sync::Notify::new(), dropped: Default::default() });
        let side = tokio::spawn({
            let (rook, model) = (rook.clone(), model.clone());
            async move {
                answer_using(
                    &rook,
                    session,
                    Request { question: "hold this question".into(), model: None },
                    |_| Ok(model),
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), model.started.notified()).await.unwrap();
        side.abort();
        assert!(side.await.unwrap_err().is_cancelled());
        assert!(model.dropped.load(std::sync::atomic::Ordering::SeqCst));
        let history = rook.transcript(session, 0, 100, 65536).unwrap();
        assert!(!history.iter().any(|e| matches!(e.kind.as_str(), "user" | "assistant") || e.label == "btw"));
    }

    #[async_trait::async_trait]
    impl rook_llm::Provider for Model {
        fn id(&self) -> &str {
            "owned/aside"
        }
        fn context_window(&self) -> usize {
            32000
        }
        async fn complete(&self, request: rook_llm::Request) -> rook_llm::Result<rook_llm::Response> {
            self.0.lock().unwrap().push(request);
            Ok(rook_llm::Response {
                message: rook_llm::Message::assistant("The selected port is 7717."),
                stop_reason: rook_llm::StopReason::EndTurn,
                usage: rook_llm::Usage { input_tokens: 5, output_tokens: 4, ..Default::default() },
                model: "owned/aside".into(),
            })
        }
    }

    #[tokio::test]
    async fn side_question_uses_no_tools_and_records_only_auxiliary_conversation_entries() {
        let home = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            rook_store::Store::open(home.path().join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            home.path().into(),
        );
        let session = rook.start_session("owned aside").unwrap();
        rook.log(session, rook_store::EventKind::UserMessage, "", "Use port 7717").unwrap();
        let model = Arc::new(Model::default());
        let answer =
            answer_using(&rook, session, Request { question: "Which port?".into(), model: None }, |_| {
                Ok(model.clone())
            })
            .await
            .unwrap();
        assert_eq!(answer, "The selected port is 7717.");
        let requests = model.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
        assert!(requests[0].messages.iter().any(|m| m.content.contains("Use port 7717")));
        let entries = rook.transcript(session, 0, 100, 65536).unwrap();
        assert_eq!(entries.iter().filter(|e| e.kind == "user").count(), 1);
        assert!(!entries.iter().any(|e| e.kind == "assistant"));
        assert_eq!(entries.iter().filter(|e| e.label == "btw").count(), 1);
        assert!(rook.execution(session).unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_side_question_is_refused_before_provider_resolution() {
        let home = tempfile::tempdir().unwrap();
        let rook = Rook::from_parts(
            rook_store::Store::open(home.path().join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("test", "test", "1"),
            Default::default(),
            home.path().into(),
        );
        let result = answer_using(&rook, 0, Request { question: "x".repeat(8193), model: None }, |_| {
            panic!("an oversized question must not resolve or call a provider")
        })
        .await;
        assert!(result.unwrap_err().to_string().contains("8192"));
    }
}
