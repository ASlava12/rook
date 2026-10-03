//! Actual child execution with an intentionally delayed display reader.
#![cfg(test)]

use super::*;
use async_trait::async_trait;
use rook_llm::{LlmError, Message, Request, Response, StopReason, ToolCall, Usage};
use std::sync::{Arc, Mutex};

struct Batch {
    first: Mutex<bool>,
    fragments: usize,
}

struct Observations {
    store: Arc<rook_store::Store>,
    workspace: std::path::PathBuf,
    counts: Mutex<(usize, usize, usize, usize)>,
}

impl super::super::observe::Observer for Observations {
    fn observe(&self, event: super::super::observe::Event<'_>) {
        use super::super::observe::Event;
        let mut counts = self.counts.lock().unwrap();
        match event {
            Event::ChildStarted { .. } => counts.0 += 1,
            Event::ChildEnded { result, .. } => {
                assert!(matches!(result, Some(Ok(_))));
                counts.3 += 1;
            }
            Event::Progress { session, workspace, progress } => {
                assert_eq!(workspace, self.workspace);
                match progress {
                    Progress::Delta(Delta::ToolCall(_)) => counts.1 += 1,
                    Progress::ToolDone { failed, result_seq, .. } => {
                        assert!(!failed);
                        let seq = result_seq.unwrap();
                        assert_eq!(
                            self.store.events(session, seq, 1).unwrap()[0].record.kind,
                            EventKind::ToolResult,
                            "the exact result is saved before observation"
                        );
                        counts.2 += 1;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl Provider for Batch {
    fn id(&self) -> &str {
        "audit/delegation"
    }
    fn context_window(&self) -> usize {
        200_000
    }
    fn supports_streaming(&self) -> bool {
        true
    }
    async fn complete(&self, request: Request) -> rook_llm::Result<Response> {
        if request
            .messages
            .first()
            .is_some_and(|m| m.content.starts_with("Classify whether an assistant's proposed last reply"))
        {
            return Ok(Response {
                message: Message::assistant(r#"{"action":"finish"}"#),
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: "audit".into(),
            });
        }
        Err(LlmError::Other("the audit uses streaming".into()))
    }
    async fn stream(&self, _request: Request) -> rook_llm::Result<rook_llm::ResponseStream> {
        let first = std::mem::replace(&mut *self.first.lock().unwrap(), false);
        let mut deltas = Vec::new();
        if first {
            let text = "a".repeat(1024);
            for part in text.as_bytes().chunks(1024 / self.fragments) {
                deltas.push(Ok(Delta::Text(std::str::from_utf8(part).unwrap().into())));
            }
            for index in 0..256 {
                deltas.push(Ok(Delta::ToolCall(ToolCall {
                    id: format!("read-{index}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path":if index == 255 { "last.txt" } else { "notes.txt" }}),
                })));
            }
        } else {
            deltas.push(Ok(Delta::Text("child report".into())));
        }
        deltas.push(Ok(Delta::Done {
            stop_reason: if first { StopReason::ToolUse } else { StopReason::EndTurn },
            usage: Usage::default(),
            model: "audit".into(),
        }));
        Ok(Box::pin(futures_util::stream::iter(deltas)))
    }
}

#[tokio::test]
async fn a_delayed_display_reader_keeps_exact_child_operations_without_fragment_journals() {
    let mut measurements = Vec::new();
    for fragments in [1, 1024] {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("notes.txt"), "actual file content\n").unwrap();
        std::fs::write(workspace.join("last.txt"), "actual file content\n").unwrap();
        let mut config = crate::Config::default();
        config.agent.tool_cycle_guard = false;
        let rook = Rook::from_parts(
            rook_store::Store::open(directory.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "audit"),
            rook_skills::SkillIndex::default(),
            workspace,
        );
        let parent = rook.start_session("parent").unwrap();
        // A full conversation snapshot per display update would carry this text.
        let history = "previous parent conversation ".repeat(1024);
        rook.log(parent, EventKind::UserMessage, "old prompt", &history).unwrap();
        let provider = Arc::new(Batch { first: Mutex::new(true), fragments });
        let observations = Arc::new(Observations {
            store: rook.store.clone(),
            workspace: rook.workspace.clone(),
            counts: Mutex::new((0, 0, 0, 0)),
        });
        let mut agent = AgentLoop::new(&rook, provider, parent);
        agent.observer = Some(observations.clone());
        let crew = agent.crew(0);
        let (doing, mut steps) =
            super::super::delegation_progress::channel(rook.config.agent.delegation_progress_entries);
        let bounds =
            Bounds { isolated: false, steps: Some(3), by: None, tokens: 0, provider: None, effort: None };
        crate::execution::RECEIPT_WRITE_MEASUREMENT.with(|measurement| measurement.set((0, 0, 0)));
        let (child, outcome) = crew
            .run_subtask("read notes and report", None, bounds, doing, 0, Default::default())
            .await
            .unwrap();
        assert_eq!(outcome.reply, "child report");
        assert_eq!(outcome.tools_called.len(), 256);
        assert_eq!(
            *observations.counts.lock().unwrap(),
            (1, 256, 256, 1),
            "coalescing display hints must not coalesce critical observer events"
        );
        let queued = steps.high_water();
        assert_eq!(queued, 1, "256 actual calls coalesce into one child's latest display hint");
        let mut delivered = 0;
        while let Ok((at, doing)) = steps.try_recv() {
            assert_eq!(at, 0);
            assert!(doing.contains("last.txt"), "the latest admitted call must be displayed: {doing}");
            delivered += 1;
        }
        assert_eq!(delivered, queued);
        let child = rook_store::parse_session_id(&child).unwrap();
        let events = rook.store.events(child, 0, 2048).unwrap();
        assert_eq!(events.iter().filter(|e| e.record.kind == EventKind::ToolCall).count(), 256);
        assert_eq!(events.iter().filter(|e| e.record.kind == EventKind::ToolResult).count(), 256);
        let execution = crate::execution::current(&rook, child).unwrap().unwrap();
        assert_eq!(execution.completed_operations, 256);
        assert!(execution.pending.is_none());
        let receipt_writes =
            crate::execution::RECEIPT_WRITE_MEASUREMENT.with(|measurement| measurement.get());
        assert!(
            receipt_writes.0 >= 512,
            "each actual call has beginning and completion writes: {receipt_writes:?}"
        );
        assert!(
            receipt_writes.2 < 8192,
            "a compact receipt must not contain the 28 KiB parent history: {receipt_writes:?}"
        );
        let mut logical_bytes = 0;
        for event in &events {
            let body = rook.store.get(&event.record.body).unwrap();
            assert!(!String::from_utf8_lossy(&body).contains("previous parent conversation"));
            logical_bytes += body.len();
        }
        eprintln!(
            "delegation audit: fragments={fragments}, queued={queued}, records={}, logical_bytes={logical_bytes}, receipt_writes={receipt_writes:?}",
            events.len()
        );
        measurements.push((events.len(), logical_bytes, receipt_writes.0));
    }
    assert_eq!(
        measurements[0].0, measurements[1].0,
        "stream fragments must not multiply durable conversation records"
    );
    // Physical receipts include variable elapsed times, rather than identical bytes.
    assert_eq!(
        measurements[0].2, measurements[1].2,
        "text fragments must not multiply actual execution companion writes"
    );
    assert!(
        measurements[0].1.abs_diff(measurements[1].1) < 1024,
        "fragmenting the same text must not add copied conversation snapshots: {measurements:?}"
    );
}
