//! Managed waits close the physical turn without another model request.
use super::{AgentLoop, TurnOutcome, effects::Shown, tools::Recorded};
use crate::{
    Result,
    execution::Journal,
    work::managed::{blocked, deferred},
};
use rook_tools::policy::Risk;

impl AgentLoop<'_> {
    pub(super) async fn checkpoint_work(
        &self,
        call: &rook_llm::ToolCall,
        outcome: &mut TurnOutcome,
        children_running: bool,
        journal: &Journal,
    ) -> Result<Recorded> {
        let prepared = if !self.managed_iteration || self.depth != 0 || self.checking || self.review_only {
            Err("managed checkpoints are available only in an admitted root managed stage".into())
        } else if children_running {
            Err("collect running children before checkpointing; a checkpoint cannot abandon scoped child work".into())
        } else if call.name == blocked::TOOL {
            blocked::parse(&call.arguments).map(|reason| (None, reason)).map_err(|error| error.to_string())
        } else {
            deferred::parse(&call.arguments)
                .map(|request| (Some(request.seconds), request.reason))
                .map_err(|error| error.to_string())
        };
        let result = match prepared {
            Err(why) => Err(why),
            Ok((seconds, reason)) => {
                let reason = self.vault.redact(&reason);
                outcome.tools_called.push(call.name.clone());
                let risk = Risk::Write(vec!["managed work continuation".into()]);
                if let Some(why) = self.gate_risk(&call.name, &call.arguments, risk, Shown::Nothing).await {
                    Err(why)
                } else if let Some(identity) = &self.managed_work {
                    let checkpoint = deferred::Call {
                        session: self.session,
                        turn: journal.turn(),
                        id: &call.id,
                        control_revision: self.managed_control_revision.unwrap_or(u64::MAX),
                    };
                    let write = |key: &str, bytes: &[u8], text: &str| {
                        journal.complete_checkpoint(&call.name, text, key, bytes)
                    };
                    match seconds {
                        Some(seconds) => deferred::commit(
                            self.rook,
                            identity,
                            checkpoint,
                            deferred::Request { seconds, reason },
                            outcome,
                            write,
                        ),
                        None => blocked::commit(self.rook, identity, checkpoint, reason, outcome, write),
                    }
                    .map_err(|error| error.to_string())
                } else {
                    Err("the managed goal changed; no wait was committed".into())
                }
            }
        };
        match result {
            Ok((seq, text)) => Ok(Recorded { text, failed: false, seq: Some(seq), job: None }),
            Err(why) => {
                let text = self.vault.redact(&why);
                let seq = journal.complete_with_phase(&text, self.tool_ctx.jobs.as_deref(), None, None)?;
                Ok(Recorded { text, failed: true, seq: Some(seq), job: None })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rook_llm::{Message, Provider, Request, Response, StopReason, ToolCall, Usage};
    use std::sync::{Arc, Mutex};

    struct Script {
        seen: Mutex<Vec<Request>>,
        tool: &'static str,
    }
    #[async_trait]
    impl Provider for Script {
        fn id(&self) -> &str {
            "defer/context-fixture"
        }
        fn context_window(&self) -> usize {
            32768
        }
        async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
            let classify =
                request.messages.iter().any(|message| message.content.starts_with("Classify whether"));
            let mut seen = self.seen.lock().unwrap();
            if seen.len() >= 12 {
                return Err(rook_llm::LlmError::Other("owned context fixture request bound".into()));
            }
            let first = seen.is_empty();
            seen.push(request);
            let mut message = Message::assistant(if classify {
                "{\"action\":\"finish\"}"
            } else {
                "No wait was scheduled"
            });
            if first {
                message.tool_calls.push(ToolCall {
                    id: "forbidden-wait".into(),
                    name: self.tool.into(),
                    arguments: if self.tool == blocked::TOOL {
                        serde_json::json!({"reason":"Needs user input"})
                    } else {
                        serde_json::json!({"seconds":60})
                    },
                });
            }
            Ok(Response {
                message,
                stop_reason: if first { StopReason::ToolUse } else { StopReason::EndTurn },
                usage: Usage::default(),
                model: self.id().into(),
            })
        }
    }

    #[tokio::test]
    async fn unadvertised_contexts_and_readonly_policy_cannot_be_bypassed_by_a_forged_wait_call() {
        use crate::work::managed as work;
        for (mode, tool) in ["ordinary", "checker", "child", "recipe", "readonly"]
            .into_iter()
            .flat_map(|mode| [deferred::TOOL, blocked::TOOL].map(|tool| (mode, tool)))
        {
            let workspace = tempfile::tempdir().unwrap();
            let store = tempfile::tempdir().unwrap();
            let rook = crate::Rook::from_parts(
                rook_store::Store::open(store.path()).unwrap(),
                crate::Config::default(),
                rook_skills::Environment::bare("linux", "x86_64", "0.19.0"),
                rook_skills::SkillIndex::default(),
                workspace.path().into(),
            );
            let run = work::start(
                &rook,
                rook_proto::work::Start {
                    goal: "context boundary fixture".into(),
                    workspace: None,
                    conversation: None,
                    autonomous: false,
                    max_iterations: Some(10),
                    max_tokens: Some(0),
                    max_seconds: Some(300),
                },
            )
            .unwrap();
            let script = Arc::new(Script { seen: Mutex::new(Vec::new()), tool });
            let make = |session| {
                let mut agent = AgentLoop::new(&rook, script.clone(), session);
                agent.allow_everything_not_denied();
                match mode {
                    "checker" => agent.checking = true,
                    "child" => agent.depth = 1,
                    "recipe" => {
                        agent.equipment.tools =
                            Some(["read_file".to_string(), "tool_status".to_string()].into())
                    }
                    "readonly" => agent.policy.set_stance(rook_tools::policy::Stance::ReadOnly),
                    _ => {}
                }
                Ok(agent)
            };
            let session = if mode == "ordinary" {
                let session = rook.start_session("ordinary context").unwrap();
                make(session).unwrap().run("context boundary fixture").await.unwrap();
                session
            } else {
                let after = work::advance(&rook, &run.id, make, |_| {}).await.unwrap();
                rook_store::parse_session_id(after.session.as_ref().unwrap()).unwrap()
            };
            assert!(work::read(&rook, &run.id).unwrap().run.waiting.is_none(), "{mode}");
            let seen = script.seen.lock().unwrap();
            assert!(!seen.is_empty());
            let offered = seen[0].tools.iter().any(|spec| spec.name == tool);
            assert_eq!(offered, mode == "readonly", "catalog scope for {mode}");
            let rows = rook.transcript(session, 0, 128, 4096).unwrap();
            let refusal = rows.iter().find(|row| row.kind == "tool-result" && row.label == tool).unwrap();
            assert!(
                refusal.body.contains(if mode == "recipe" {
                    "unavailable"
                } else if mode == "readonly" {
                    "refused"
                } else {
                    "only"
                }),
                "{mode}: {}",
                refusal.body
            );
        }
    }
}
