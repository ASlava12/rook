//! Real Responses HTTP through the agent, store reopen, forks and compaction.
use rook_core::{Config, Rook, agent::AgentLoop};
use rook_llm::openai::{Config as HttpConfig, responses::Responses};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static HISTORY_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn output() -> Value {
    json!([
        {"type":"reasoning","id":"rs-original","encrypted_content":"opaque-do-not-print","summary":[]},
        {"type":"message","id":"msg-original","role":"assistant","content":[{"type":"output_text","text":"Inspecting both files."}]},
        {"type":"function_call","id":"fc-one","call_id":"original-one","name":"read_file","arguments":"{\"path\":\"one.txt\"}"},
        {"type":"function_call","id":"fc-two","call_id":"original-two","name":"read_file","arguments":"{\"path\":\"two.txt\"}"}
    ])
}
fn text_output(text: &str) -> Value {
    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}])
}
async fn server(first: Value) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    scripted_server(vec![first]).await
}

async fn scripted_server(
    outputs: Vec<Value>,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let task = tokio::spawn(async move {
        let mut outputs: std::collections::VecDeque<_> = outputs.into();
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let request: Value = loop {
                let mut buffer = [0; 8192];
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break Value::Null;
                }
                raw.extend_from_slice(&buffer[..n]);
                assert!(raw.len() <= 8 * 1024 * 1024);
                let Some(split) = raw.windows(4).position(|part| part == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&raw[..split]);
                let size: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if raw.len() < split + 4 + size {
                    continue;
                }
                break serde_json::from_slice(&raw[split + 4..split + 4 + size]).unwrap();
            };
            if request.is_null() {
                continue;
            }
            let checker =
                request["input"].to_string().contains("Classify whether an assistant's proposed last reply");
            let output = if checker {
                text_output(r#"{"action":"finish"}"#)
            } else {
                assert!(record.lock().unwrap().len() < 64, "fixture request budget reached");
                record.lock().unwrap().push(request.clone());
                outputs.pop_front().unwrap_or_else(|| text_output("The requested inspection is complete."))
            };
            let response = json!({"status":"completed","model":"gpt-6-astra","output":output,"usage":{"input_tokens":1,"output_tokens":1}});
            let (kind, body) = if request["stream"] == true {
                (
                    "text/event-stream",
                    format!("data: {}\r\n\r\n", json!({"type":"response.completed","response":response})),
                )
            } else {
                ("application/json", response.to_string())
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.ok();
            socket.write_all(body.as_bytes()).await.ok();
        }
    });
    (format!("http://{address}/v1"), seen, task)
}
fn rook_at(root: &std::path::Path) -> Rook {
    let mut config = Config::default();
    config.agent.context_window = Some(128_000);
    config.agent.one_script = false;
    config.agent.plan_first = false;
    Rook::from_parts(
        rook_store::Store::open(root.join("store")).unwrap(),
        config,
        rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
        Default::default(),
        root.into(),
    )
}
fn provider(url: &str) -> Arc<Responses> {
    Arc::new(Responses::new("test", "gpt-6-astra", HttpConfig::new(url.into(), None, 128_000)).unwrap())
}
fn assert_batch(request: &Value, incomplete: bool) {
    let items = request["input"].as_array().unwrap();
    let at =
        items.iter().position(|item| item["id"] == "rs-original").expect("original reasoning is retained");
    assert_eq!(&items[at..at + 4], output().as_array().unwrap());
    assert_eq!(items[at + 4]["type"], "function_call_output");
    assert_eq!(items[at + 4]["call_id"], "original-one");
    assert!(items[at + 4]["output"].as_str().unwrap().contains("first-file-value"));
    assert_eq!(items[at + 5]["type"], "function_call_output");
    assert_eq!(items[at + 5]["call_id"], "original-two");
    let second = items[at + 5]["output"].as_str().unwrap();
    assert!(
        second.contains(if incomplete { "no result was recorded" } else { "second-file-value" }),
        "{second}"
    );
    assert_eq!(items.iter().filter(|item| item["call_id"] == "original-one").count(), 2);
    assert_eq!(items.iter().filter(|item| item["call_id"] == "original-two").count(), 2);
}

#[tokio::test]
async fn signed_batches_survive_reopen_fork_interruption_and_compaction_without_leaking_to_transcripts() {
    let _serial = HISTORY_SERIAL.lock().await;
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("ROOK_HOME", home.path());
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("one.txt"), "first-file-value").unwrap();
    std::fs::write(root.path().join("two.txt"), "second-file-value").unwrap();
    let rook = rook_at(root.path());
    let (url, seen, task) = server(output()).await;
    let session = rook.start_session("response replay").unwrap();
    let result = AgentLoop::new(&rook, provider(&url), session).run("inspect both files").await.unwrap();
    assert_eq!(result.tools_called, ["read_file", "read_file"]);
    assert_batch(&seen.lock().unwrap()[1], false);
    let transcript = rook.transcript(session, 0, 1000, 10000).unwrap();
    assert!(transcript.iter().all(|entry| !entry.body.contains("opaque-do-not-print")));
    let first_result = transcript.iter().find(|entry| entry.kind == "tool-result").unwrap().seq;
    let end = rook.store.get_session(session).unwrap().unwrap().next_seq;
    let complete = rook.fork_session(session, end).unwrap().id;
    let interrupted = rook.fork_session(session, first_result + 1).unwrap().id;
    drop(rook);
    let rook = rook_at(root.path());
    for (session, incomplete) in [(session, false), (complete, false), (interrupted, true)] {
        let before = seen.lock().unwrap().len();
        AgentLoop::new(&rook, provider(&url), session).run("report the existing results").await.unwrap();
        assert_batch(&seen.lock().unwrap()[before], incomplete);
    }
    // Many later messages create a real compactable prefix. The request after
    // compaction must not contain the discarded opaque state or orphan outputs.
    for _ in 0..10 {
        rook.log(complete, rook_store::EventKind::UserMessage, "", &"old material ".repeat(100)).unwrap();
        rook.log(complete, rook_store::EventKind::AssistantMessage, "", &"old answer ".repeat(100)).unwrap();
    }
    let mut agent = AgentLoop::new(&rook, provider(&url), complete);
    agent.set_window_for_test(4000);
    agent.compact_now().await;
    assert!(rook.last_compaction(complete).unwrap().1.is_some(), "fixture must compact");
    drop(agent);
    let before = seen.lock().unwrap().len();
    AgentLoop::new(&rook, provider(&url), complete).run("report the summary").await.unwrap();
    let request = seen.lock().unwrap()[before].clone();
    assert!(!request.to_string().contains("opaque-do-not-print"));
    assert!(!request.to_string().contains("original-one"));
    task.abort();

    // A tiny transcript preview must not make a large encrypted response free
    // to retain forever. Its results have already reached the following reply.
    let mut heavy = output();
    heavy[0]["encrypted_content"] = json!("opaque-heavy-".repeat(5000));
    let (url, seen, task) = server(heavy).await;
    let large = rook.start_session("large provider state").unwrap();
    AgentLoop::new(&rook, provider(&url), large).run("inspect both files").await.unwrap();
    assert!(rook.context_usage(large, Some(128000)).unwrap().live_tokens > 10000);
    let mut agent = AgentLoop::new(&rook, provider(&url), large);
    agent.set_window_for_test(8000);
    agent.compact_now().await;
    assert!(
        rook.last_compaction(large).unwrap().1.is_some(),
        "large signed batch must be compactable as a whole"
    );
    drop(agent);
    let before = seen.lock().unwrap().len();
    AgentLoop::new(&rook, provider(&url), large).run("report the summary").await.unwrap();
    assert!(!seen.lock().unwrap()[before].to_string().contains("opaque-heavy-"));
    task.abort();

    let mut rook = rook;
    rook.config.agent.max_provider_state_bytes = 1024;
    let mut oversized = output();
    oversized[0]["encrypted_content"] = json!("too-large-state".repeat(1000));
    let (url, _, task) = server(oversized).await;
    let session = rook.start_session("bounded state").unwrap();
    let error = AgentLoop::new(&rook, provider(&url), session).run("inspect both files").await.unwrap_err();
    assert!(error.to_string().contains("1024"), "{error}");
    let events = rook.store.events(session, 0, 100).unwrap();
    assert!(!events.iter().any(|event| event.record.kind == rook_store::EventKind::ToolCall));
    assert_eq!(events.iter().map(|event| event.record.tokens_in).sum::<u32>(), 1);
    assert_eq!(events.iter().map(|event| event.record.tokens_out).sum::<u32>(), 1);
    task.abort();
}

const CORRECTION: &str = "Correction: retain legacy_api.rs; migrate only the cache adapter.";
const ACCEPTED: &str = "Accepted correction: legacy_api.rs stays; only the cache adapter changes.";
const REJECTED: &str =
    "Rejected approach: deleting legacy_api.rs breaks downstream callers; do not retry it.";
const QUESTION: &str = "Which deployment environment should receive the migration first?";
const SUMMARY: &str = "## Goal\nMigrate only the cache adapter; retain legacy_api.rs (accepted user correction).\n## Done\nInspected one.txt and two.txt; their saved read results remain available.\n## Open\nDeployment environment is unanswered. Continue independent adapter work without inventing an answer. Deleting legacy_api.rs was rejected because it breaks downstream callers; do not retry that approach.";
const NEXT: &str = "Continue independent adapter work. Leave the deployment question unanswered.";

fn progress(rook: &Rook, session: u128, stage: &str, count: usize) {
    for milestone in 0..count {
        let details = (0..18)
            .map(|part| {
                format!("{stage} milestone {milestone} observation {part}: independent adapter inspection.\n")
            })
            .collect::<String>();
        rook.log(
            session,
            rook_store::EventKind::UserMessage,
            "",
            &format!("Inspect {stage} milestone {milestone}."),
        )
        .unwrap();
        rook.log(session, rook_store::EventKind::AssistantMessage, "", &details).unwrap();
    }
}

#[tokio::test]
async fn long_task_compaction_preserves_accepted_correction_unanswered_question_and_rejected_approach() {
    let _serial = HISTORY_SERIAL.lock().await;
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("ROOK_HOME", home.path());
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("one.txt"), "first-file-value").unwrap();
    std::fs::write(root.path().join("two.txt"), "second-file-value").unwrap();
    let rook = rook_at(root.path());
    let session = rook.start_session("long adapter migration").unwrap();
    progress(&rook, session, "early", 50);
    let (url, seen, task) = scripted_server(vec![
        output(),
        text_output("The requested inspection is complete."),
        text_output("Independent adapter work continues; deployment remains undecided."),
        text_output(SUMMARY),
        json!([
            {"type":"reasoning","id":"rs-after","encrypted_content":"opaque-after-compaction","summary":[]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Continuing the adapter migration while preserving the open deployment decision."}]}
        ]),
        text_output("The accepted correction and unresolved decision still apply."),
    ]).await;
    AgentLoop::new(&rook, provider(&url), session)
        .run("Inspect both files before migrating the adapter.")
        .await
        .unwrap();
    rook.log(session, rook_store::EventKind::UserMessage, "", CORRECTION).unwrap();
    rook.log(session, rook_store::EventKind::AssistantMessage, "", ACCEPTED).unwrap();
    rook.log(session, rook_store::EventKind::AssistantMessage, "", REJECTED).unwrap();
    let question_seq = rook.log(session, rook_store::EventKind::AssistantMessage, "", QUESTION).unwrap();
    progress(&rook, session, "later", 14);
    let before = seen.lock().unwrap().len();
    AgentLoop::new(&rook, provider(&url), session).run(NEXT).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), before + 1);
    let request_before = seen.lock().unwrap()[before].clone();
    let text = request_before["input"].to_string();
    for expected in [CORRECTION, ACCEPTED, REJECTED, QUESTION] {
        assert!(text.contains(expected), "pre-compaction request lost {expected}");
    }
    assert_batch(&request_before, false);
    let usage = rook.context_usage(session, Some(16_000)).unwrap();
    assert!(
        usage.live_tokens > usage.compact_at,
        "the actual replay must exceed the configured compaction threshold: {usage:?}"
    );
    let mut agent = AgentLoop::new(&rook, provider(&url), session);
    agent.set_window_for_test(16_000);
    agent.compact_now().await;
    let summary_request = seen.lock().unwrap()[before + 1].clone();
    assert_eq!(seen.lock().unwrap().len(), before + 2);
    let material = summary_request["input"].to_string();
    for expected in [CORRECTION, ACCEPTED, REJECTED, QUESTION, "first-file-value", "second-file-value"] {
        assert!(material.contains(expected), "the actual summarizer input lost {expected}");
    }
    assert!(!material.contains("opaque-do-not-print"), "opaque state is never summary material");
    assert!(!summary_request["tools"].as_array().is_some_and(|tools| !tools.is_empty()));
    let saved = rook.last_compaction(session).unwrap();
    assert_eq!(saved.1.as_deref(), Some(SUMMARY));
    assert!(saved.0 > question_seq, "the question must be compacted, not merely retained in the tail");
    let replacement = serde_json::to_value(agent.history_for_test().unwrap()).unwrap();
    let replacement_text = replacement.to_string();
    let summary_source: Value = serde_json::from_str(replacement[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(summary_source["rook_source"]["kind"], "summary");
    assert_eq!(summary_source["rook_source"]["authority"], "data");
    assert_eq!(summary_source["rook_source"]["content"], SUMMARY);
    assert!(!replacement_text.contains(CORRECTION), "the correction now comes from the saved summary");
    assert!(!replacement_text.contains(QUESTION), "the question now comes from the saved summary");
    assert!(!replacement_text.contains("opaque-do-not-print"));
    assert!(!replacement_text.contains("original-one"));
    assert_eq!(rook.context_usage(session, Some(16_000)).unwrap().compactions, 1);
    drop(agent);
    drop(rook);

    let rook = rook_at(root.path());
    assert_eq!(rook.last_compaction(session).unwrap(), saved);
    let agent = AgentLoop::new(&rook, provider(&url), session);
    assert_eq!(
        serde_json::to_value(agent.history_for_test().unwrap()).unwrap(),
        replacement,
        "replacement replay survives reopening before the next turn"
    );
    drop(agent);
    let next_at = seen.lock().unwrap().len();
    AgentLoop::new(&rook, provider(&url), session).run(NEXT).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), next_at + 1);
    let next = seen.lock().unwrap()[next_at].clone();
    let wire = next["input"].to_string();
    assert!(wire.contains("accepted user correction"));
    assert!(wire.contains("Deployment environment is unanswered"));
    assert!(wire.contains("do not retry that approach"));
    assert!(!wire.contains("opaque-do-not-print"));
    assert!(!wire.contains("original-one"));
    assert_eq!(
        next["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item.to_string().contains("Deployment environment is unanswered"))
            .count(),
        1
    );
    assert_eq!(rook.last_compaction(session).unwrap(), saved);
    let transcript = rook.transcript(session, 0, 1000, 10000).unwrap();
    assert_eq!(
        transcript.iter().filter(|entry| entry.body == QUESTION).count(),
        1,
        "compaction and continuation do not resend the pending question"
    );
    assert!(transcript.iter().all(|entry| !entry.body.contains("opaque-")));
    assert_eq!(std::fs::read_to_string(root.path().join("one.txt")).unwrap(), "first-file-value");
    drop(rook);

    let rook = rook_at(root.path());
    let after = seen.lock().unwrap().len();
    AgentLoop::new(&rook, provider(&url), session).run("Report the independent adapter work.").await.unwrap();
    let final_request = seen.lock().unwrap()[after].clone();
    assert!(final_request["input"].to_string().contains("opaque-after-compaction"));
    assert!(!final_request["input"].to_string().contains("opaque-do-not-print"));
    assert_eq!(rook.last_compaction(session).unwrap(), saved);
    task.abort();
}
