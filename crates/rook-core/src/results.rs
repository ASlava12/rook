//! Stored results stay intact; only their representation in a request gets smaller.
use std::io::{Read, Seek, SeekFrom};

use rook_llm::{Message, Role};
use rook_store::{Event, EventKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CoreError, Result, Rook, context::estimate_tokens};

pub(crate) const READ_RESULT: &str = "read_result";
const PAGE: usize = 16 * 1024;

fn mark_key(session: u128) -> String {
    format!("pruned-results/{session:032x}")
}

pub(crate) fn watermark(rook: &Rook, session: u128) -> Result<Option<u64>> {
    Ok(rook.store.kv_get(&mark_key(session))?.and_then(|v| serde_json::from_slice(&v).ok()))
}

fn protected(name: &str) -> bool {
    // A person's answers and scoped skill instructions must survive this policy.
    matches!(name, "ask" | "load_skill")
}

fn stub(name: &str, seq: u64) -> String {
    envelope(
        name,
        seq,
        "[Old result omitted from context. Use read_result with this result_id to retrieve it.]",
    )
}

fn envelope(name: &str, seq: u64, text: &str) -> String {
    let mut value: Value =
        serde_json::from_str(&crate::sources::tool_result(name, text)).unwrap_or_else(|_| json!({}));
    value["result_id"] = json!(seq);
    value.to_string()
}

pub(crate) fn fresh(event: &Event, body: &str) -> String {
    // The existing replay ceiling must not become a new limit on a tool answer
    // the model has only just requested. Old results are pruned in batches.
    envelope(&event.record.label, event.seq, body)
}

pub(crate) fn render(rook: &Rook, event: &Event, body: &str, through: Option<u64>) -> String {
    let name = &event.record.label;
    if !protected(name) && through.is_some_and(|seq| event.seq <= seq) {
        return stub(name, event.seq);
    }
    // Keep structured human answers whole: shortening their JSON would turn them
    // into ordinary data, losing the authority of the chosen answers.
    let kept = if protected(name) || name == READ_RESULT {
        body.to_string()
    } else {
        crate::context::shorten_result(body, rook.config.agent.max_replayed_result_tokens)
    };
    envelope(name, event.seq, &kept)
}

/// Batch changes to the old prefix, rather than invalidating it at every step.
/// The durable watermark makes resumed turns render precisely the same stubs.
pub(crate) fn prune(rook: &Rook, session: u128, messages: &mut [Message]) -> Result<usize> {
    let minimum = rook.config.agent.prune_tool_results_min_tokens;
    if minimum == 0 {
        return Ok(0);
    }
    let mut recent = 0;
    let mut count = 0;
    let mut through = None;
    let mut saved = 0i128;
    for message in messages.iter().rev() {
        if message.role != Role::Tool {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&message.content) else { continue };
        let Some(seq) = value["result_id"].as_u64() else { continue };
        let Some(name) = value["rook_source"]["origin"].as_str() else { continue };
        let cost = estimate_tokens(&message.content);
        count += 1;
        let keep = recent < rook.config.agent.prune_tool_results_keep_tokens;
        recent += cost;
        if count <= 8 || keep || protected(name) {
            continue;
        }
        let replacement = stub(name, seq);
        let saving = cost as i128 - estimate_tokens(&replacement) as i128;
        if through.is_some() || saving > 0 {
            through = Some(through.unwrap_or(seq));
            saved += saving;
        }
    }
    if saved < minimum as i128 {
        return Ok(0);
    }
    if let Some(last) = through {
        let through = watermark(rook, session)?.unwrap_or(0).max(last);
        // Commit before changing the in-memory view: a failed write must not
        // produce a request that a restart cannot reconstruct.
        rook.store.kv_set(&mark_key(session), &serde_json::to_vec(&through)?)?;
        for message in messages.iter_mut().filter(|m| m.role == Role::Tool) {
            let Ok(value) = serde_json::from_str::<Value>(&message.content) else { continue };
            if let (Some(seq), Some(name)) =
                (value["result_id"].as_u64(), value["rook_source"]["origin"].as_str())
                && seq <= through
                && !protected(name)
            {
                message.content = stub(name, seq);
            }
        }
    }
    Ok(saved as usize)
}

#[derive(Serialize, Deserialize)]
struct Output {
    file: String,
    complete: bool,
}

fn output_key(session: u128, seq: u64) -> String {
    // Session suffix lets ordinary session retention remove this metadata.
    format!("command-output/{seq}/{session:032x}")
}

/// Only built-in run_command/job structured metadata can register a spill. Text printed by
/// a process (or metadata from an MCP tool) cannot grant file-read capability.
pub(crate) fn register_output(
    rook: &Rook,
    session: u128,
    seq: u64,
    meta: &std::collections::BTreeMap<String, Value>,
) -> Result<()> {
    register_capture(&rook.store, &rook.output_dir, session, seq, meta)
}

pub(crate) fn register_capture(
    store: &rook_store::Store,
    output_dir: &std::path::Path,
    session: u128,
    seq: u64,
    meta: &std::collections::BTreeMap<String, Value>,
) -> Result<()> {
    let Some(path) = meta.get("output_file").and_then(Value::as_str) else { return Ok(()) };
    let path = std::path::Path::new(path);
    if path.parent() != Some(output_dir) {
        return Err(CoreError::Other("command output is outside the output directory".into()));
    }
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CoreError::Other("invalid output filename".into()))?;
    let output = Output {
        file: file.into(),
        complete: meta.get("output_complete").and_then(Value::as_bool).unwrap_or(false),
    };
    store.kv_set(&output_key(session, seq), &serde_json::to_vec(&output)?)?;
    Ok(())
}

/// Forks copy event bodies, so their saved-output capabilities must follow the
/// copied results too. Never inherit a watermark past the new log's end.
pub(crate) fn inherit(rook: &Rook, parent: u128, child: u128) -> Result<()> {
    let events = rook.store.events(child, 0, usize::MAX)?;
    if let (Some(last), Some(through)) = (events.last(), watermark(rook, parent)?) {
        rook.store.kv_set(&mark_key(child), &serde_json::to_vec(&through.min(last.seq))?)?;
    }
    for event in events.into_iter().filter(|e| e.record.kind == EventKind::ToolResult) {
        if let Some(bytes) = rook.store.kv_get(&output_key(parent, event.seq))? {
            rook.store.kv_set(&output_key(child, event.seq), &bytes)?;
        }
    }
    Ok(())
}

/// Byte offsets, bounded pages, and an explicit next offset make even a long
/// single-line result readable without asking the provider to hold it all.
pub(crate) fn read(rook: &Rook, session: u128, args: &Value) -> Result<String> {
    let session = match args.get("session") {
        None => session,
        Some(value) => {
            let target = value
                .as_str()
                .and_then(rook_store::parse_session_id)
                .ok_or_else(|| CoreError::Other("invalid session id".into()))?;
            if target != session
                && !rook.store.get_session(target)?.is_some_and(|m| m.parent == Some(session))
            {
                return Err(CoreError::Other(
                    "results are readable only from this session or its direct children".into(),
                ));
            }
            target
        }
    };
    if args.get("result_id").is_none() {
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 100) as usize;
        let events = rook.store.events(session, offset, limit)?;
        let next = events.last().and_then(|e| e.seq.checked_add(1));
        let end = rook.store.get_session(session)?.map(|m| m.next_seq).unwrap_or(0);
        let rows: Vec<_> = events
            .iter()
            .filter(|e| e.record.kind == EventKind::ToolResult)
            .map(|e| json!({"result_id":e.seq,"tool":e.record.label,"at":e.record.ts}))
            .collect();
        return Ok(json!({"results":rows,"next_offset":next.filter(|n| *n < end)}).to_string());
    }
    let seq = args
        .get("result_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| CoreError::Other("read_result needs a numeric result_id from a tool result".into()))?;
    let event = rook
        .store
        .events(session, seq, 1)?
        .into_iter()
        .next()
        .filter(|e| e.seq == seq && e.record.kind == EventKind::ToolResult)
        .ok_or_else(|| CoreError::Other("no such tool result in this session".into()))?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let limit =
        args.get("limit").and_then(Value::as_u64).unwrap_or(PAGE as u64).clamp(4, PAGE as u64) as usize;
    let output = rook
        .store
        .kv_get(&output_key(session, seq))?
        .map(|b| serde_json::from_slice::<Output>(&b))
        .transpose()?;
    let source = args.get("source").and_then(Value::as_str).unwrap_or("result");
    let (bytes, total, complete) = match source {
        "output" => {
            let output = output.as_ref().ok_or_else(|| {
                CoreError::Other(
                    "no full output was captured for this result; read source=result for the stored answer"
                        .into(),
                )
            })?;
            // Capability-relative open also refuses symlinks escaping this root.
            let path = std::path::Path::new(&output.file);
            let io = |source| CoreError::Io { path: rook.output_dir.join(path), source };
            let mut file = rook_contain::files::open(&rook.output_dir, path).map_err(io)?;
            let total = file.metadata().map_err(io)?.len();
            if offset > total {
                return Err(CoreError::Other("offset exceeds output size".into()));
            }
            file.seek(SeekFrom::Start(offset)).map_err(io)?;
            let mut bytes = Vec::new();
            file.take(limit as u64).read_to_end(&mut bytes).map_err(io)?;
            (bytes, total, output.complete)
        }
        "result" => {
            let total = rook
                .store
                .stat_object(&event.record.body)?
                .ok_or_else(|| CoreError::Other("stored result is missing".into()))?
                .size_raw;
            (rook.store.get_range(&event.record.body, offset, limit)?, total, true)
        }
        _ => return Err(CoreError::Other("source must be result or output".into())),
    };
    // Do not split a UTF-8 code point between successive pages. Invalid binary
    // output remains readable with replacement characters and byte offsets.
    let end = match std::str::from_utf8(&bytes) {
        Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => e.valid_up_to(),
        _ => bytes.len(),
    };
    let next = offset + end as u64;
    Ok(json!({"result_id": seq, "source": source, "offset": offset, "next_offset": (next < total).then_some(next),
        "total_bytes": total, "capture_complete": complete, "output_available": output.is_some(),
        "content": String::from_utf8_lossy(&bytes[..end])}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Rook, u128) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::Config::default();
        config.agent.max_replayed_result_tokens = 0;
        config.agent.prune_tool_results_keep_tokens = 2_000;
        config.agent.prune_tool_results_min_tokens = 4_000;
        let rook = Rook::from_parts(
            rook_store::Store::open(dir.path().join("store")).unwrap(),
            config,
            rook_skills::Environment::bare("linux", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            dir.path().to_path_buf(),
        );
        let session = rook.start_session("results").unwrap();
        (dir, rook, session)
    }

    #[test]
    fn pruning_saves_tokens_once_and_the_full_result_survives_restart_and_compaction() {
        let (_dir, rook, session) = fixture();
        let mut messages = Vec::new();
        let mut events = Vec::new();
        let body = "данные\n".repeat(500);
        for i in 0..20 {
            rook.log(session, EventKind::ToolCall, "run_command", "{}").unwrap();
            let seq =
                rook.log(session, EventKind::ToolResult, "run_command", &format!("{i}\n{body}")).unwrap();
            let event = rook.store.events(session, seq, 1).unwrap().remove(0);
            let bytes = rook.store.get(&event.record.body).unwrap();
            messages.push(Message::tool_result(
                format!("call_{i}"),
                render(&rook, &event, std::str::from_utf8(&bytes).unwrap(), None),
            ));
            events.push(event);
        }
        let cost = |messages: &[Message]| messages.iter().map(|m| estimate_tokens(&m.content)).sum::<usize>();
        let before = cost(&messages);
        assert!(before > rook.config.agent.prune_tool_results_min_tokens * 4);
        let saved = prune(&rook, session, &mut messages).unwrap();
        assert!(saved > 4_000);
        assert!(cost(&messages) * 2 < before, "before={before}, after={}", cost(&messages));
        eprintln!("result pruning: {before} -> {} estimated tokens", cost(&messages));
        let once = messages.clone();
        assert_eq!(prune(&rook, session, &mut messages).unwrap(), 0);
        assert_eq!(serde_json::to_value(&messages).unwrap(), serde_json::to_value(&once).unwrap());
        for (event, message) in events.iter().zip(&messages) {
            let body = rook.store.get(&event.record.body).unwrap();
            assert_eq!(
                render(&rook, event, std::str::from_utf8(&body).unwrap(), watermark(&rook, session).unwrap()),
                message.content
            );
        }
        rook.log(session, EventKind::Compaction, "", "summary").unwrap();
        let mut whole = String::new();
        let mut offset = 0;
        loop {
            let page: Value = serde_json::from_str(
                &read(&rook, session, &json!({"result_id":events[0].seq,"offset":offset,"limit":97}))
                    .unwrap(),
            )
            .unwrap();
            whole.push_str(page["content"].as_str().unwrap());
            match page["next_offset"].as_u64() {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(whole, format!("0\n{body}"));
        let other = rook.start_session("other").unwrap();
        assert!(read(&rook, other, &json!({"result_id":events[0].seq})).is_err());
        assert!(
            read(
                &rook,
                other,
                &json!({"result_id":events[0].seq,"session":rook_store::format_session_id(session)})
            )
            .is_err()
        );
        let child = rook.fork_for_subtask(session, "child").unwrap();
        let child_result = rook.log(child, EventKind::ToolResult, "run_command", "child answer").unwrap();
        assert!(
            read(
                &rook,
                session,
                &json!({"result_id":child_result,"session":rook_store::format_session_id(child)})
            )
            .unwrap()
            .contains("child answer")
        );
        let listed: Value = serde_json::from_str(&read(&rook, session, &json!({})).unwrap()).unwrap();
        assert_eq!(listed["results"][0]["result_id"], events[0].seq);
        assert!(
            read(&rook, session, &json!({"result_id":0})).is_err(),
            "tool calls cannot be read as results"
        );
    }

    #[test]
    fn output_pages_are_bounded_and_only_registered_command_files_are_readable() {
        let (_dir, rook, session) = fixture();
        std::fs::create_dir_all(&rook.output_dir).unwrap();
        let output = "a".repeat(PAGE * 3);
        let path = rook.output_dir.join("command.log");
        std::fs::write(&path, &output).unwrap();
        let seq = rook.log(session, EventKind::ToolResult, "run_command", "short answer").unwrap();
        let meta = [("output_file".into(), json!(path)), ("output_complete".into(), json!(true))].into();
        register_output(&rook, session, seq, &meta).unwrap();
        let page: Value = serde_json::from_str(
            &read(&rook, session, &json!({"result_id":seq,"source":"output","limit":u64::MAX})).unwrap(),
        )
        .unwrap();
        assert_eq!(page["content"].as_str().unwrap().len(), PAGE);
        assert_eq!(page["next_offset"], PAGE);
        assert_eq!(page["capture_complete"], true);
        let fork = rook.fork_session(session, seq + 1).unwrap();
        let copied = read(&rook, fork.id, &json!({"result_id":seq,"source":"output"})).unwrap();
        assert!(copied.contains(&"a".repeat(PAGE)));
        rook.delete_session(session).unwrap();
        assert!(read(&rook, fork.id, &json!({"result_id":seq,"source":"output"})).is_ok());
        let bad = [("output_file".into(), json!(rook.workspace.join("secret.txt")))].into();
        assert!(register_output(&rook, session, seq, &bad).is_err());
        #[cfg(unix)]
        {
            std::fs::write(rook.workspace.join("secret.txt"), "outside").unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(rook.workspace.join("secret.txt"), &path).unwrap();
            assert!(read(&rook, fork.id, &json!({"result_id":seq,"source":"output"})).is_err());
        }
    }
}
