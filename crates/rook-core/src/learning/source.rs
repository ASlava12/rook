use super::*;
use rook_store::EventKind;
fn identity(s: &Source, settings: &Settings) -> Result<String> {
    let mut copy = s.clone();
    copy.signature.clear();
    hash(&copy, settings.state_bytes)
}
pub(super) fn validate(source: &Source, s: &Settings) -> Result<()> {
    if source.events.len() > s.source_events
        || source.events.iter().map(|e| e.text.len()).sum::<usize>() > s.source_bytes
        || ObjectId::from_hex(&source.result_object).is_none()
        || rook_store::parse_session_id(&source.turn).is_none()
        || source.signature != identity(source, s)?
    {
        return Err(bad("learning evidence identity or budget mismatch"));
    }
    if source.events.iter().any(|e| {
        ObjectId::from_hex(&e.object).is_none()
            || !matches!(e.kind.as_str(), "user_message" | "tool_result" | "note")
    }) {
        return Err(bad("learning evidence must remain source data"));
    }
    Ok(())
}
pub(super) fn capture(rook: &Rook, owner: u128, result: u64, vault: &Vault) -> Result<Source> {
    let s = &rook.config.learning;
    let event = rook
        .store
        .events(owner, result, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| bad("task result event missing"))?;
    if event.seq != result
        || event.record.kind != EventKind::Note
        || event.record.label != crate::turns::RESULT
    {
        return Err(bad("source must be the exact retained turn-result event"));
    }
    let meta =
        rook.store.stat_object(&event.record.body)?.ok_or_else(|| bad("task outcome object missing"))?;
    if meta.size_raw > s.source_bytes as u64 {
        return Err(bad(
            "task outcome exceeds learning.source_bytes; no unsupported lesson will be inferred from a prefix",
        ));
    }
    let raw = rook.store.get_range(&event.record.body, 0, s.source_bytes + 1)?;
    if ObjectId::of(&raw) != event.record.body {
        return Err(bad("task outcome checksum mismatch"));
    }
    let (turn, outcome): (String, crate::agent::TurnOutcome) = serde_json::from_slice(&raw)?;
    if !crate::agent::finished(&outcome.stopped) || !outcome.open_questions.is_empty() {
        return Err(bad("unfinished, failed or unproven tasks cannot produce durable knowledge proposals"));
    }
    let summary_seq = result.checked_sub(1).ok_or_else(|| bad("task summary missing"))?;
    let summary = rook
        .store
        .events(owner, summary_seq, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| bad("task summary missing"))?;
    if summary.seq != summary_seq
        || summary.record.label != crate::turns::LABEL
        || summary.record.kind != EventKind::Note
    {
        return Err(bad("task result lacks its adjacent immutable summary"));
    }
    let bytes = rook.store.get_range(&summary.record.body, 0, 8193)?;
    if bytes.len() > 8192 || ObjectId::of(&bytes) != summary.record.body {
        return Err(bad("task summary oversized or corrupt"));
    }
    let summary: crate::turns::Summary = serde_json::from_slice(&bytes)?;
    if summary.turn != turn
        || summary.session != rook_store::format_session_id(owner)
        || summary.stopped != outcome.stopped
    {
        return Err(bad("task source ownership mismatch"));
    }
    let from = summary.prompt_seq.ok_or_else(|| bad("this recorded turn has no admitted user prompt"))?;
    if from >= summary_seq {
        return Err(bad("task prompt range invalid"));
    }
    let events = rook.store.events(owner, from, s.source_events + 1)?;
    let mut source = Source {
        result,
        turn,
        result_object: event.record.body.to_hex(),
        signature: String::new(),
        events: Vec::new(),
        omitted_events: 0,
    };
    let mut budget = s.source_bytes;
    for event in events.into_iter().filter(|e| e.seq < summary_seq) {
        if source.events.len() == s.source_events {
            source.omitted_events += 1;
            continue;
        }
        if !matches!(event.record.kind, EventKind::UserMessage | EventKind::ToolResult)
            && !(event.record.kind == EventKind::Note
                && matches!(event.record.label.as_str(), "goal check" | "verify"))
        {
            continue;
        }
        let size = rook
            .store
            .stat_object(&event.record.body)?
            .ok_or_else(|| bad("source event object missing"))?
            .size_raw;
        let truncated = size > budget as u64 || size > 16384;
        let text = if truncated {
            String::new()
        } else {
            let bytes = rook.store.get_range(&event.record.body, 0, size as usize + 1)?;
            if bytes.len() != size as usize || ObjectId::of(&bytes) != event.record.body {
                return Err(bad("learning source checksum mismatch"));
            }
            let redacted = vault.redact(&String::from_utf8_lossy(&bytes));
            if redacted.len() > budget {
                return Err(bad("redacted source exceeds learning.source_bytes"));
            }
            budget -= redacted.len();
            redacted
        };
        if event.record.kind == EventKind::Note
            && event.record.label == "goal check"
            && crate::agent::evidence_verdict(&text) != Some("holds")
        {
            return Err(bad(
                "this task's retained goal check failed or remained unproven; do not turn it into a durable rule",
            ));
        }
        source.events.push(Evidence {
            seq: event.seq,
            kind: match event.record.kind {
                EventKind::UserMessage => "user_message",
                EventKind::ToolResult => "tool_result",
                _ => "note",
            }
            .into(),
            object: event.record.body.to_hex(),
            text,
            truncated,
        });
    }
    source.omitted_events = source
        .omitted_events
        .saturating_add((summary_seq - from).saturating_sub(s.source_events as u64) as usize);
    source.signature = identity(&source, s)?;
    validate(&source, s)?;
    Ok(source)
}
