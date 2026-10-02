//! Provider-owned assistant state and explicit bindings for its tool results.
//!
//! Companion notes leave postcard records unchanged and travel with session
//! forks. The transcript shows a summary, never signed or encrypted payloads.
use std::collections::BTreeSet;

use rook_llm::{Message, Response, Role};
use rook_store::{Event, EventKind, Kind, NewEvent};
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result, Rook, Vault};

pub(crate) const LABEL: &str = "rook:assistant-state:v1";
pub(crate) const CALL: &str = "rook:call-state:v1";

fn limit(rook: &Rook) -> Result<usize> {
    let limit = rook.config.agent.max_provider_state_bytes;
    if !(1024..=32 * 1024 * 1024).contains(&limit) {
        return Err(CoreError::Other("agent.max_provider_state_bytes must be 1024..=33554432".into()));
    }
    Ok(limit)
}

fn validate(message: &Message) -> Result<()> {
    let mut ids = BTreeSet::new();
    if message.role != Role::Assistant
        || !message.images.is_empty()
        || message.tool_call_id.is_some()
        || message.tool_calls.len() > 256
        || message
            .tool_calls
            .iter()
            .any(|call| call.id.is_empty() || call.name.is_empty() || !ids.insert(call.id.as_str()))
    {
        return Err(CoreError::Other("invalid stored assistant state or duplicate tool call ID".into()));
    }
    Ok(())
}

fn redact(value: &mut serde_json::Value, vault: &Vault) -> bool {
    match value {
        serde_json::Value::String(text) => {
            let safe = vault.redact(text);
            let changed = safe != *text;
            *text = safe;
            changed
        }
        serde_json::Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= redact(item, vault);
            }
            changed
        }
        serde_json::Value::Object(fields) => {
            let mut changed = false;
            let old = std::mem::take(fields);
            for (key, mut value) in old {
                let safe = vault.redact(&key);
                changed |= key != safe;
                changed |= redact(&mut value, vault);
                fields.insert(safe, value);
            }
            changed
        }
        _ => false,
    }
}

/// Persist before effects. An atomic pair prevents a transcript answer without
/// its provider state (or a state with an absent visible answer) after a crash.
pub(crate) fn record(
    rook: &Rook,
    session: u128,
    response: &Response,
    vault: &Vault,
    route: Option<&crate::model_route::Receipt>,
) -> Result<Option<u64>> {
    let route =
        route.map(|r| crate::persistence::encode_with_limit(r, crate::model_route::MAX_BYTES)).transpose()?;
    let text = vault.redact(&response.message.content);
    let visible = if text.is_empty() {
        NewEvent::new(EventKind::Note, Kind::Message, b"tool-only response").label("usage")
    } else {
        NewEvent::new(EventKind::AssistantMessage, Kind::Message, text.as_bytes()).label(&response.model)
    }
    .usage(response.usage.input_tokens, response.usage.output_tokens);
    if response.message.reasoning.is_empty() {
        match route {
            Some(bytes) => {
                rook.store.append_event_pair(
                    session,
                    visible,
                    NewEvent::new(EventKind::Note, Kind::Message, &bytes).label(crate::model_route::LABEL),
                )?;
            }
            None => {
                rook.store.append_event(session, visible)?;
            }
        }
        return Ok(None);
    }
    let max = limit(rook)?;
    // Check size before cloning/redacting a potentially large response.
    crate::persistence::encode_with_limit(&response.message, max)?;
    let mut message = response.message.clone();
    validate(&message)?;
    message.content = vault.redact(&message.content);
    let mut visible_changed = message.content != response.message.content;
    for call in &mut message.tool_calls {
        if vault.redact(&call.id) != call.id || vault.redact(&call.name) != call.name {
            return Err(CoreError::Other("provider call identifiers contain a known secret".into()));
        }
        visible_changed |= redact(&mut call.arguments, vault);
    }
    // A redacted signature is not a signature. If any plaintext inside the
    // opaque envelope contains a known secret, discard the entire envelope;
    // the sanitized visible text and original call IDs still survive.
    let mut state = serde_json::to_value(&message.reasoning)?;
    let withheld = redact(&mut state, vault) || visible_changed;
    if withheld {
        message.reasoning.clear();
    }
    let bytes = crate::persistence::encode_with_limit(&message, max)?;
    let state = NewEvent::new(EventKind::Note, Kind::Message, &bytes).label(LABEL);
    let seq = match route {
        Some(route) => rook.store.append_events_with_values(
            session,
            [
                state,
                visible,
                NewEvent::new(EventKind::Note, Kind::Message, &route).label(crate::model_route::LABEL),
            ],
            &[],
        )?[0],
        None => rook.store.append_event_pair(session, state, visible)?[0],
    };
    if withheld {
        rook.log(session, EventKind::Note, "provider-state",
            "Signed provider state contained a known secret and was not retained. Resumed reasoning may need to restart.").ok();
    }
    Ok(Some(seq))
}

pub(crate) fn load(rook: &Rook, event: &Event) -> Result<Message> {
    let size = rook.store.stat_object(&event.record.body)?.map(|meta| meta.size_raw).unwrap_or(0);
    if size > limit(rook)? as u64 {
        return Err(CoreError::Other("stored assistant state exceeds agent.max_provider_state_bytes".into()));
    }
    let message = serde_json::from_slice(&rook.store.get(&event.record.body)?)?;
    validate(&message)?;
    Ok(message)
}

#[derive(Serialize, Deserialize)]
struct Binding {
    assistant: u64,
    id: String,
}

pub(crate) fn begin(rook: &Rook, session: u128, assistant: Option<u64>, id: &str) -> Result<()> {
    if let Some(assistant) = assistant {
        let bytes =
            crate::persistence::encode_with_limit(&Binding { assistant, id: id.into() }, limit(rook)?)?;
        rook.store
            .append_event(session, NewEvent::new(EventKind::Note, Kind::Message, &bytes).label(CALL))?;
    }
    Ok(())
}

pub(crate) fn preview() -> Vec<u8> {
    b"[provider replay state; opaque payload omitted]".to_vec()
}

pub(crate) struct Batch {
    pub seq: u64,
    calls: Vec<rook_llm::ToolCall>,
    selected: Option<String>,
    results: Vec<Message>,
    pub auxiliary: Vec<Message>,
}

impl Batch {
    pub fn new(seq: u64, message: &Message) -> Self {
        Self {
            seq,
            calls: message.tool_calls.clone(),
            selected: None,
            results: Vec::new(),
            auxiliary: Vec::new(),
        }
    }

    pub fn bind(&mut self, rook: &Rook, event: &Event) -> Result<()> {
        let size = rook.store.stat_object(&event.record.body)?.map(|meta| meta.size_raw).unwrap_or(0);
        if size > limit(rook)? as u64 {
            return Err(CoreError::Other("stored tool binding exceeds provider state limit".into()));
        }
        let binding: Binding = serde_json::from_slice(&rook.store.get(&event.record.body)?)?;
        if binding.assistant != self.seq || !self.calls.iter().any(|c| c.id == binding.id) {
            return Err(CoreError::Other(
                "tool binding does not belong to the preceding assistant response".into(),
            ));
        }
        self.selected = Some(binding.id);
        Ok(())
    }

    pub fn selected(&self, name: &str) -> Option<&str> {
        self.selected.as_deref().filter(|id| self.calls.iter().any(|c| c.id == *id && c.name == name))
    }

    pub fn result(&mut self, message: Message) {
        if let Some(slot) = self.results.iter_mut().find(|old| old.tool_call_id == message.tool_call_id) {
            *slot = message;
        } else {
            self.results.push(message);
        }
        crate::tool_images::bound(&mut self.results);
    }

    pub fn finish(mut self, messages: &mut Vec<Message>) {
        let has_images = self.results.iter().any(|message| !message.images.is_empty());
        for call in self.calls {
            let result = self
                .results
                .iter()
                .position(|m| m.tool_call_id.as_deref() == Some(call.id.as_str()))
                .map(|at| self.results.remove(at));
            messages.push(result.unwrap_or_else(|| {
                Message::tool_result(
                    call.id,
                    "no result was recorded: the turn did not finish; inspect effects before retrying",
                )
            }));
        }
        messages.append(&mut self.auxiliary);
        if has_images {
            crate::tool_images::bound(messages);
        }
    }
}

/// If a retained entry belongs to a provider response, retain its complete
/// batch and companion. Cutting midway would orphan results or signed state.
pub(crate) fn batch_start(events: &[Event], retained: u64) -> Option<u64> {
    let mut start = None;
    for event in events.iter().take_while(|event| event.seq <= retained) {
        if event.record.kind == EventKind::Note && event.record.label == LABEL {
            start = Some(event.seq);
        } else if ends_batch(event, start) {
            start = None;
        }
    }
    start
}

pub(crate) fn batch_end(events: &[Event], start: u64) -> Option<u64> {
    events
        .iter()
        .find(|event| {
            event.seq > start
                && ((event.record.kind == EventKind::Note && event.record.label == LABEL)
                    || ends_batch(event, Some(start)))
        })
        .map(|event| event.seq)
}

fn ends_batch(event: &Event, start: Option<u64>) -> bool {
    matches!(event.record.kind, EventKind::UserMessage | EventKind::Reasoning)
        || (event.record.kind == EventKind::AssistantMessage && start != event.seq.checked_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(root: &std::path::Path) -> Rook {
        Rook::from_parts(
            rook_store::Store::open(root.join("store")).unwrap(),
            Default::default(),
            rook_skills::Environment::bare("linux", "x86_64", "0.1"),
            Default::default(),
            root.into(),
        )
    }
    fn response() -> Response {
        let mut message = Message::assistant("checked");
        message
            .reasoning
            .push(json!({"type":"thinking","thinking":"worked it out","signature":"unchanged-signature"}));
        message.tool_calls.push(rook_llm::ToolCall {
            id: "original-id".into(),
            name: "read_file".into(),
            arguments: json!({"path":"one.txt"}),
        });
        Response {
            message,
            model: "test".into(),
            stop_reason: rook_llm::StopReason::ToolUse,
            usage: Default::default(),
        }
    }

    #[test]
    fn state_limits_fail_before_recording_an_answer_or_executing_a_tool() {
        let root = tempfile::tempdir().unwrap();
        let mut rook = fixture(root.path());
        rook.config.agent.max_provider_state_bytes = 1024;
        let session = rook.start_session("bound").unwrap();
        let before = rook.store.get_session(session).unwrap().unwrap().next_seq;
        let mut response = response();
        response.message.reasoning[0]["thinking"] = json!("x".repeat(2048));
        assert!(serde_json::to_vec(&response.message).unwrap().len() > 1024);
        assert!(
            record(&rook, session, &response, &Vault::empty(), None)
                .unwrap_err()
                .to_string()
                .contains("1024")
        );
        assert_eq!(rook.store.get_session(session).unwrap().unwrap().next_seq, before);
        rook.config.agent.max_provider_state_bytes = 4096;
        let seq = record(&rook, session, &response, &Vault::empty(), None).unwrap().unwrap();
        let event = rook.store.events(session, seq, 1).unwrap().remove(0);
        rook.config.agent.max_provider_state_bytes = 1024;
        assert!(load(&rook, &event).unwrap_err().to_string().contains("max_provider_state_bytes"));
    }

    #[test]
    fn redaction_never_persists_clear_secrets_or_edits_a_signed_block_in_place() {
        let root = tempfile::tempdir().unwrap();
        let rook = fixture(root.path());
        let session = rook.start_session("redaction").unwrap();
        let vault = Vault::empty();
        vault.also_hide("private-access-token");
        let mut response = response();
        response.message.content = "private-access-token".into();
        response.message.tool_calls[0].arguments = json!({"nested":["private-access-token"]});
        response.message.reasoning[0]["thinking"] = json!("private-access-token");
        let seq = record(&rook, session, &response, &vault, None).unwrap().unwrap();
        let events = rook.store.events(session, seq, 100).unwrap();
        for event in &events {
            assert!(
                !String::from_utf8_lossy(&rook.store.get(&event.record.body).unwrap())
                    .contains("private-access-token")
            );
        }
        let message = load(&rook, &events[0]).unwrap();
        assert!(message.reasoning.is_empty());
        assert_eq!(message.tool_calls[0].id, "original-id");
        assert_eq!(message.tool_calls[0].arguments["nested"][0], "${secret}");
        assert!(events.iter().any(|event| event.record.label == "provider-state"));
    }

    #[test]
    fn an_explicit_binding_cannot_attach_a_result_to_another_response() {
        let root = tempfile::tempdir().unwrap();
        let rook = fixture(root.path());
        let session = rook.start_session("binding").unwrap();
        let response = response();
        let seq = record(&rook, session, &response, &Vault::empty(), None).unwrap().unwrap();
        begin(&rook, session, Some(seq + 99), "original-id").unwrap();
        let marker = rook.store.events(session, seq + 2, 1).unwrap().remove(0);
        let mut batch = Batch::new(seq, &response.message);
        assert!(batch.bind(&rook, &marker).is_err());
    }
    #[test]
    fn compaction_cannot_cut_inside_a_signed_batch() {
        let root = tempfile::tempdir().unwrap();
        let rook = fixture(root.path());
        let session = rook.start_session("compaction boundary").unwrap();
        let seq = record(&rook, session, &response(), &Vault::empty(), None).unwrap().unwrap();
        begin(&rook, session, Some(seq), "original-id").unwrap();
        let call = rook.log(session, EventKind::ToolCall, "read_file", "{}").unwrap();
        let result = rook.log(session, EventKind::ToolResult, "read_file", "done").unwrap();
        let user = rook.log(session, EventKind::UserMessage, "", "next turn").unwrap();
        let events = rook.store.events(session, 0, 100).unwrap();
        for retained in [seq + 1, call, result] {
            assert_eq!(batch_start(&events, retained), Some(seq));
        }
        assert_eq!(batch_start(&events, user), None);
        assert_eq!(batch_end(&events, seq), Some(user));
    }
    #[test]
    fn pruning_replays_original_ids_and_reports_the_actual_signed_context() {
        let root = tempfile::tempdir().unwrap();
        let mut rook = fixture(root.path());
        rook.config.agent.prune_tool_results_min_tokens = 1;
        rook.config.agent.prune_tool_results_keep_tokens = 0;
        let session = rook.start_session("pruning").unwrap();
        let mut response = response();
        response.message.reasoning[0]["thinking"] = json!("signed working ".repeat(1000));
        response.message.tool_calls = (0..9)
            .map(|n| rook_llm::ToolCall {
                id: format!("original-{n}"),
                name: "read_file".into(),
                arguments: json!({"path":format!("file-{n}")}),
            })
            .collect();
        let seq = record(&rook, session, &response, &Vault::empty(), None).unwrap().unwrap();
        for call in &response.message.tool_calls {
            begin(&rook, session, Some(seq), &call.id).unwrap();
            rook.log(session, EventKind::ToolCall, &call.name, &call.arguments.to_string()).unwrap();
            rook.log(session, EventKind::ToolResult, &call.name, &"result-data ".repeat(500)).unwrap();
        }
        let mut messages = crate::agent::history::replay(&rook, session).unwrap();
        assert!(crate::results::prune(&rook, session, &mut messages).unwrap() > 0);
        assert!(crate::results::watermark(&rook, session).unwrap().is_some());
        let replayed = crate::agent::history::replay(&rook, session).unwrap();
        let first = replayed.iter().find(|message| message.role == Role::Tool).unwrap();
        assert_eq!(first.tool_call_id.as_deref(), Some("original-0"));
        assert!(first.content.contains("Old result omitted"));
        assert_eq!(replayed[0].reasoning, response.message.reasoning);
        let context = rook.context_usage(session, Some(128000)).unwrap();
        assert_eq!(context.live_tokens, replayed.iter().map(crate::attachments::tokens).sum::<usize>());
        assert!(
            context.live_tokens > 3000,
            "signed reasoning is counted even though its companion is a note"
        );
    }
}
