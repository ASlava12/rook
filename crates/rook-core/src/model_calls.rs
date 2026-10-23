//! Opt-in canonical visible request/reply snapshots. These are local data, not
//! model instructions, HTTP wire captures or independent quality judgements.

use crate::model_route::Purpose;
use crate::{CoreError, Result, Vault};
use rook_llm::{Delta, Message, Request, Response, StopReason, ToolCall};
use rook_store::{EventKind, Kind, NewEvent, Store};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub(crate) const INPUT: &str = "rook:model-call-input:v1";
pub(crate) const OUTCOME: &str = "rook:model-call-outcome:v1";
pub(crate) const MAX_RECORD_BYTES: usize = 1024 * 1024 + 4096;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub max_call_bytes: usize,
    pub max_turn_bytes: usize,
    pub max_calls: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, max_call_bytes: 256 * 1024, max_turn_bytes: 8 * 1024 * 1024, max_calls: 64 }
    }
}
impl Settings {
    pub(crate) fn bounded(self) -> Self {
        Self {
            enabled: self.enabled,
            max_call_bytes: self.max_call_bytes.clamp(4096, 1024 * 1024),
            max_turn_bytes: self.max_turn_bytes.clamp(4096, 16 * 1024 * 1024),
            max_calls: self.max_calls.clamp(1, 256),
        }
    }
}

#[derive(Default)]
struct Usage {
    calls: usize,
    bytes: usize,
}
pub(crate) struct Budget {
    settings: Settings,
    usage: Mutex<Usage>,
}
impl Budget {
    pub(crate) fn new(settings: Settings) -> Arc<Self> {
        Arc::new(Self { settings: settings.bounded(), usage: Mutex::default() })
    }
    fn reserve(self: &Arc<Self>) -> Option<Ticket> {
        let mut usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if usage.calls >= self.settings.max_calls {
            return None;
        }
        usage.calls += 1;
        let reserved =
            self.settings.max_call_bytes.min(self.settings.max_turn_bytes.saturating_sub(usage.bytes));
        if reserved == 0 {
            return None;
        }
        usage.bytes += reserved;
        Some(Ticket { budget: self.clone(), reserved, used: 0 })
    }
}
struct Ticket {
    budget: Arc<Budget>,
    reserved: usize,
    used: usize,
}
impl Ticket {
    fn remaining(&self) -> usize {
        self.reserved - self.used
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut usage = self.budget.usage.lock().unwrap_or_else(|e| e.into_inner());
        usage.bytes -= self.reserved - self.used;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Omitted {
    Quota,
    Bytes,
    Shape,
    Identifiers,
    Redaction,
    MissingRequest,
    MissingReply,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Omissions {
    pub images: usize,
    pub provider_state: usize,
    pub hidden_reasoning: bool,
}
impl Omissions {
    fn message(&mut self, message: &Message) {
        self.images = self.images.saturating_add(message.images.len());
        self.provider_state = self.provider_state.saturating_add(message.reasoning.len());
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Payload {
    pub value: Option<Value>,
    pub omitted: Option<Omitted>,
    pub known_secret_redacted: bool,
    pub omissions: Omissions,
}
impl Payload {
    fn omitted(why: Omitted, omissions: Omissions) -> Self {
        Self { value: None, omitted: Some(why), known_secret_redacted: false, omissions }
    }
}

#[derive(Serialize)]
struct VisibleMessage<'a> {
    role: rook_llm::Role,
    content: &'a str,
    tool_calls: &'a [ToolCall],
    tool_call_id: Option<&'a str>,
    cache: bool,
}
impl<'a> From<&'a Message> for VisibleMessage<'a> {
    fn from(m: &'a Message) -> Self {
        Self {
            role: m.role,
            content: &m.content,
            tool_calls: &m.tool_calls,
            tool_call_id: m.tool_call_id.as_deref(),
            cache: m.cache,
        }
    }
}
struct Messages<'a>(&'a [Message]);
impl Serialize for Messages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for message in self.0 {
            seq.serialize_element(&VisibleMessage::from(message))?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
struct RequestView<'a> {
    messages: Messages<'a>,
    tools: &'a [rook_llm::ToolSpec],
    max_output_tokens: u32,
    temperature: f32,
    effort: Option<rook_llm::Effort>,
    cache_ttl: rook_llm::CacheTtl,
    model_capabilities: &'a rook_llm::ModelCapabilities,
}

fn json_shape(value: &Value, depth: usize, nodes: &mut usize) -> bool {
    *nodes = nodes.saturating_add(1);
    if depth > 32 || *nodes > 8192 {
        return false;
    }
    match value {
        Value::Array(items) => items.iter().all(|v| json_shape(v, depth + 1, nodes)),
        Value::Object(fields) => fields.values().all(|v| json_shape(v, depth + 1, nodes)),
        _ => true,
    }
}
fn identifier(value: &str, limit: usize, vault: &Vault) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && vault.redact_bounded(value, limit).is_some_and(|safe| safe == value)
}
fn message_shape(message: &Message, vault: &Vault, nodes: &mut usize) -> bool {
    message.tool_calls.len() <= 256
        && message.tool_call_id.as_deref().is_none_or(|id| identifier(id, 256, vault))
        && message.tool_calls.iter().all(|call| {
            identifier(&call.id, 256, vault)
                && identifier(&call.name, 128, vault)
                && json_shape(&call.arguments, 0, nodes)
        })
}

fn redact(value: &mut Value, vault: &Vault, limit: usize, size: &mut usize) -> std::result::Result<bool, ()> {
    let mut changed = false;
    let mask = |text: &str, size: &mut usize| -> std::result::Result<String, ()> {
        let available = limit.saturating_sub(size.saturating_sub(text.len()));
        let safe = vault.redact_bounded(text, available).ok_or(())?;
        *size = size.saturating_sub(text.len()).saturating_add(safe.len());
        Ok(safe)
    };
    match value {
        Value::String(text) => {
            let safe = mask(text, size)?;
            changed = safe != *text;
            *text = safe;
        }
        Value::Array(items) => {
            for item in items {
                changed |= redact(item, vault, limit, size)?;
            }
        }
        Value::Object(fields) => {
            let old = std::mem::take(fields);
            for (key, mut item) in old {
                let safe = mask(&key, size)?;
                changed |= safe != key;
                changed |= redact(&mut item, vault, limit, size)?;
                if fields.insert(safe, item).is_some() {
                    return Err(());
                }
            }
        }
        _ => {}
    }
    Ok(changed)
}

fn payload(value: &impl Serialize, limit: usize, vault: &Vault, omissions: Omissions) -> (Payload, usize) {
    let Ok(bytes) = crate::persistence::encode_with_limit(value, limit) else {
        return (Payload::omitted(Omitted::Bytes, omissions), 0);
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return (Payload::omitted(Omitted::Shape, omissions), 0);
    };
    let mut size = bytes.len();
    let Ok(redacted) = redact(&mut value, vault, limit, &mut size) else {
        return (Payload::omitted(Omitted::Redaction, omissions), 0);
    };
    let Ok(encoded) = crate::persistence::encode_with_limit(&value, limit) else {
        return (Payload::omitted(Omitted::Bytes, omissions), 0);
    };
    (Payload { value: Some(value), omitted: None, known_secret_redacted: redacted, omissions }, encoded.len())
}

#[derive(Serialize, Deserialize)]
pub(crate) struct InputRecord {
    pub version: u8,
    pub attempt: String,
    pub purpose: Purpose,
    pub admitted_bytes: usize,
    pub payload_bytes: usize,
    pub request: Payload,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct OutcomeRecord {
    pub version: u8,
    pub attempt: String,
    pub input_seq: Option<u64>,
    pub payload_bytes: usize,
    pub reply: Payload,
}

#[derive(Default)]
struct Reply {
    text: String,
    calls: Vec<ToolCall>,
    stop: Option<StopReason>,
    cache: bool,
    seen: bool,
    bytes: usize,
    omissions: Omissions,
    omitted: Option<Omitted>,
}
#[derive(Serialize)]
struct ReplyView<'a> {
    message: VisibleMessage<'a>,
    stop_reason: Option<StopReason>,
}

pub(crate) struct Capture {
    ticket: Option<Ticket>,
    input_seq: Option<u64>,
    input_ok: bool,
    reply: Reply,
}
impl Capture {
    pub(crate) fn new(budget: &Arc<Budget>) -> Self {
        Self { ticket: budget.reserve(), input_seq: None, input_ok: false, reply: Reply::default() }
    }
    fn limit(&self) -> usize {
        self.ticket.as_ref().map_or(0, Ticket::remaining)
    }
    fn save<T: Serialize>(store: &Store, session: u128, label: &str, value: &T) -> Result<u64> {
        let bytes = crate::persistence::encode_with_limit(value, MAX_RECORD_BYTES)?;
        Ok(store.append_event_durable(
            session,
            NewEvent::new(EventKind::Note, Kind::Message, &bytes).label(label),
        )?)
    }
    pub(crate) fn request(
        &mut self,
        store: &Store,
        session: u128,
        attempt: &str,
        purpose: Purpose,
        request: &Request,
        vault: &Vault,
    ) -> Result<()> {
        if self.input_seq.is_some() {
            return Err(CoreError::Other("model call input already recorded".into()));
        }
        let mut omissions = Omissions::default();
        let mut nodes = 0;
        let (body, size) = if self.ticket.is_none() {
            (Payload::omitted(Omitted::Quota, omissions), 0)
        } else if request.messages.len() > 4096 || request.tools.len() > 256 {
            (Payload::omitted(Omitted::Shape, omissions), 0)
        } else {
            for message in &request.messages {
                omissions.message(message);
            }
            if !request.messages.iter().all(|m| message_shape(m, vault, &mut nodes))
                || !request
                    .tools
                    .iter()
                    .all(|t| identifier(&t.name, 128, vault) && json_shape(&t.parameters, 0, &mut nodes))
            {
                (Payload::omitted(Omitted::Identifiers, omissions), 0)
            } else {
                payload(
                    &RequestView {
                        messages: Messages(&request.messages),
                        tools: &request.tools,
                        max_output_tokens: request.max_output_tokens,
                        temperature: request.temperature,
                        effort: request.effort,
                        cache_ttl: request.cache_ttl,
                        model_capabilities: &request.model_capabilities,
                    },
                    self.limit(),
                    vault,
                    omissions,
                )
            }
        };
        let input_ok = body.value.is_some();
        let row = InputRecord {
            version: 1,
            attempt: attempt.into(),
            purpose,
            admitted_bytes: self.ticket.as_ref().map_or(0, |t| t.reserved),
            payload_bytes: size,
            request: body,
        };
        let seq = Self::save(store, session, INPUT, &row)?;
        self.input_seq = Some(seq);
        self.input_ok = input_ok;
        if let Some(ticket) = &mut self.ticket {
            ticket.used += size;
        }
        Ok(())
    }
    pub(crate) fn delta(&mut self, delta: &Delta, vault: &Vault) {
        if !self.input_ok {
            return;
        }
        match delta {
            Delta::Reasoning(_) => {
                self.reply.omissions.hidden_reasoning = true;
                return;
            }
            Delta::ReasoningDone(_) => {
                self.reply.omissions.provider_state = self.reply.omissions.provider_state.saturating_add(1);
                return;
            }
            _ => {}
        }
        if self.reply.omitted.is_some() {
            return;
        }
        match delta {
            Delta::Text(text) => self.text(text),
            Delta::ToolCall(call) => self.tool(call, vault),
            Delta::Done { stop_reason, .. } => {
                self.reply.seen = true;
                if self.reply.stop.replace(*stop_reason).is_some() {
                    self.omit_reply(Omitted::Shape);
                }
            }
            _ => {}
        }
    }
    fn text(&mut self, text: &str) {
        if self.reply.omitted.is_some() {
            return;
        }
        if text.len() > self.limit().saturating_sub(self.reply.bytes) {
            self.omit_reply(Omitted::Bytes);
            return;
        }
        self.reply.bytes += text.len();
        self.reply.seen = true;
        self.reply.text.push_str(text);
    }
    fn tool(&mut self, call: &ToolCall, vault: &Vault) {
        if self.reply.omitted.is_some() {
            return;
        }
        let mut nodes = 0;
        if self.reply.calls.len() >= 256
            || !identifier(&call.id, 256, vault)
            || !identifier(&call.name, 128, vault)
            || !json_shape(&call.arguments, 0, &mut nodes)
        {
            self.omit_reply(Omitted::Identifiers);
            return;
        }
        match crate::persistence::encode_with_limit(call, self.limit().saturating_sub(self.reply.bytes)) {
            Ok(bytes) => {
                self.reply.bytes += bytes.len();
                self.reply.seen = true;
                self.reply.calls.push(call.clone());
            }
            Err(_) => self.omit_reply(Omitted::Bytes),
        }
    }
    fn omit_reply(&mut self, why: Omitted) {
        self.reply.text.clear();
        self.reply.calls.clear();
        self.reply.omitted = Some(why);
    }
    pub(crate) fn response(&mut self, response: &Response, vault: &Vault) {
        if !self.input_ok {
            return;
        }
        if self.reply.seen {
            self.omit_reply(Omitted::Shape);
            return;
        }
        self.reply.omissions.message(&response.message);
        if response.message.role != rook_llm::Role::Assistant
            || response.message.tool_call_id.is_some()
            || response.message.tool_calls.len() > 256
        {
            self.omit_reply(Omitted::Shape);
            return;
        }
        self.text(&response.message.content);
        for call in &response.message.tool_calls {
            self.tool(call, vault);
        }
        self.reply.cache = response.message.cache;
        self.reply.stop = Some(response.stop_reason);
    }
    pub(crate) fn finish(
        &mut self,
        store: &Store,
        session: u128,
        attempt: &str,
        vault: &Vault,
    ) -> Result<()> {
        let omissions = self.reply.omissions;
        let (body, size) = if !self.input_ok {
            (Payload::omitted(Omitted::MissingRequest, omissions), 0)
        } else if let Some(why) = self.reply.omitted {
            (Payload::omitted(why, omissions), 0)
        } else if !self.reply.seen {
            (Payload::omitted(Omitted::MissingReply, omissions), 0)
        } else {
            let visible = VisibleMessage {
                role: rook_llm::Role::Assistant,
                content: &self.reply.text,
                tool_calls: &self.reply.calls,
                tool_call_id: None,
                cache: self.reply.cache,
            };
            payload(
                &ReplyView { message: visible, stop_reason: self.reply.stop },
                self.limit(),
                vault,
                omissions,
            )
        };
        Self::save(
            store,
            session,
            OUTCOME,
            &OutcomeRecord {
                version: 1,
                attempt: attempt.into(),
                input_seq: self.input_seq,
                payload_bytes: size,
                reply: body,
            },
        )?;
        if let Some(ticket) = &mut self.ticket {
            ticket.used += size;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_attempt;
    use rook_llm::{AttemptFacts, AttemptStatus, Dispatch, Usage as Tokens};

    fn engine(path: &std::path::Path) -> crate::Rook {
        crate::Rook::from_parts(
            Store::open(path.join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("windows", "x86_64", "0.1.0"),
            rook_skills::SkillIndex::default(),
            path.to_path_buf(),
        )
    }
    fn rows(rook: &crate::Rook, session: u128) -> Vec<(u64, String, Vec<u8>)> {
        rook.store
            .events(session, 0, 256)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.record.label.as_str(), INPUT | OUTCOME))
            .map(|e| (e.seq, e.record.label, rook.store.get(&e.record.body).unwrap()))
            .collect()
    }
    fn observed(
        rook: &crate::Rook,
        session: u128,
        vault: Arc<Vault>,
        override_on: Option<bool>,
        budget: Option<Arc<Budget>>,
    ) -> Box<dyn rook_llm::Attempt> {
        model_attempt::observer_with_capture(
            rook,
            session,
            vault,
            Purpose::Main,
            Arc::new(crate::model_route::Prices::new(&rook.config)),
            budget,
            override_on,
        )
        .start(Dispatch::bounded("physical", "model", true).as_ref())
        .unwrap()
    }
    fn facts() -> AttemptFacts {
        AttemptFacts {
            completion_confirmed: true,
            usage_reported: true,
            usage: Some(Tokens { input_tokens: 12, output_tokens: 3, ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn default_and_explicit_off_never_save_content() {
        for configured in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut rook = engine(dir.path());
            rook.config.telemetry.model_calls.enabled = configured;
            let session = rook.start_session("disabled snapshots").unwrap();
            let mut attempt = observed(
                &rook,
                session,
                Arc::new(Vault::empty()),
                if configured { Some(false) } else { None },
                None,
            );
            attempt.capture_request(&Request::new(vec![Message::user("not captured")])).unwrap();
            attempt.capture_delta(&Delta::Text("not captured either".into()));
            attempt.finish(AttemptStatus::Completed, &facts()).unwrap();
            assert!(rows(&rook, session).is_empty());
        }
    }

    #[test]
    fn explicit_capture_freezes_system_tools_and_request_arguments_with_known_secret_masking() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("snapshots").unwrap();
        let vault = Arc::new(Vault::empty());
        vault.also_hide("private-token");
        let mut request = Request::new(vec![Message::system("policy A"), Message::user("use private-token")]);
        let mut previous = Message::assistant("");
        previous.tool_calls.push(ToolCall {
            id: "old-call".into(),
            name: "read".into(),
            arguments: serde_json::json!({"file":"a.txt"}),
        });
        request.messages.push(previous);
        request.messages.push(Message::tool_result("old-call", "read a.txt"));
        request.tools.push(rook_llm::ToolSpec {
            name: "read".into(),
            description: "a private-token description".into(),
            parameters: serde_json::json!({"type":"object","private-token":"original"}),
        });
        let mut attempt = observed(&rook, session, vault.clone(), Some(true), None);
        attempt.capture_request(&request).unwrap();
        request.messages[0].content = "policy B".into();
        request.messages[2].tool_calls[0].name = "write".into();
        request.messages[2].tool_calls[0].arguments = serde_json::json!({"file":"b.txt"});
        request.messages[3].content = "read b.txt".into();
        request.tools.clear();
        attempt.capture_delta(&Delta::Text("reply private-token".into()));
        attempt.capture_delta(&Delta::Done {
            stop_reason: StopReason::EndTurn,
            usage: Tokens::default(),
            model: "model".into(),
        });
        attempt.finish(AttemptStatus::Completed, &facts()).unwrap();
        let rows = rows(&rook, session);
        assert_eq!(rows.len(), 2);
        let input: InputRecord = serde_json::from_slice(&rows[0].2).unwrap();
        let outcome: OutcomeRecord = serde_json::from_slice(&rows[1].2).unwrap();
        assert_eq!(input.attempt, outcome.attempt);
        assert_eq!(outcome.input_seq, Some(rows[0].0));
        let value = input.request.value.unwrap();
        assert_eq!(value["messages"][0]["content"], "policy A");
        assert_eq!(value["tools"][0]["name"], "read");
        assert_eq!(value["messages"][2]["tool_calls"][0]["name"], "read");
        assert_eq!(value["messages"][2]["tool_calls"][0]["arguments"]["file"], "a.txt");
        assert_eq!(value["messages"][3]["content"], "read a.txt");
        assert!(value["tools"][0]["parameters"].get("${secret}").is_some());
        assert!(input.request.known_secret_redacted && outcome.reply.known_secret_redacted);
        for row in &rows {
            assert!(!String::from_utf8_lossy(&row.2).contains("private-token"));
        }
        assert_eq!(outcome.reply.value.unwrap()["message"]["content"], "reply ${secret}");
        let event = rook.store.events(session, rows[0].0, 1).unwrap().remove(0);
        let (display, _) = crate::transcript::body(&rook, &event, 4096).unwrap();
        assert!(!display.contains("policy A"));
        assert!(display.contains("explicit model-call inspection"));
    }

    #[test]
    fn image_and_provider_state_bytes_are_omitted_before_serializing() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("omissions").unwrap();
        let mut message = Message::user("visible source");
        message.images.push(rook_llm::Image {
            mime_type: "image/png".into(),
            data: "UNSTORED_PIXELS".repeat(100000),
            width: 1,
            height: 1,
        });
        message.reasoning.push(serde_json::json!({"signature":"UNSTORED_SIGNATURE"}));
        let mut attempt = observed(&rook, session, Arc::new(Vault::empty()), Some(true), None);
        attempt.capture_request(&Request::new(vec![message])).unwrap();
        attempt.capture_delta(&Delta::Reasoning("UNSTORED_HIDDEN_THOUGHT".into()));
        attempt.capture_delta(&Delta::ReasoningDone(serde_json::json!({"signature":"UNSTORED_SIGNATURE"})));
        attempt.capture_delta(&Delta::Text("visible answer".into()));
        attempt.finish(AttemptStatus::Interrupted, &AttemptFacts::default()).unwrap();
        let rows = rows(&rook, session);
        assert!(rows.iter().all(|r| !String::from_utf8_lossy(&r.2).contains("UNSTORED")));
        let input: InputRecord = serde_json::from_slice(&rows[0].2).unwrap();
        assert_eq!(input.request.omissions.images, 1);
        assert_eq!(input.request.omissions.provider_state, 1);
        let outcome: OutcomeRecord = serde_json::from_slice(&rows[1].2).unwrap();
        assert!(outcome.reply.omissions.hidden_reasoning);
        assert_eq!(outcome.reply.omissions.provider_state, 1);
        assert_eq!(outcome.reply.value.unwrap()["stop_reason"], Value::Null);
    }

    #[test]
    fn oversized_requests_and_replies_are_metadata_omissions_not_partial_successes() {
        for oversized_request in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let rook = engine(dir.path());
            let session = rook.start_session("bounds").unwrap();
            let budget = Budget::new(Settings { max_call_bytes: 4096, ..Default::default() });
            let mut attempt = observed(&rook, session, Arc::new(Vault::empty()), Some(true), Some(budget));
            let large = "Ж".repeat(4096);
            attempt
                .capture_request(&Request::new(vec![Message::user(if oversized_request {
                    large.as_str()
                } else {
                    "small"
                })]))
                .unwrap();
            attempt.capture_delta(&Delta::Text(large));
            attempt.finish(AttemptStatus::Incomplete, &AttemptFacts::default()).unwrap();
            let rows = rows(&rook, session);
            let input: InputRecord = serde_json::from_slice(&rows[0].2).unwrap();
            let output: OutcomeRecord = serde_json::from_slice(&rows[1].2).unwrap();
            if oversized_request {
                assert_eq!(input.request.omitted, Some(Omitted::Bytes));
                assert_eq!(output.reply.omitted, Some(Omitted::MissingRequest));
            } else {
                assert!(input.request.value.is_some());
                assert_eq!(output.reply.omitted, Some(Omitted::Bytes));
            }
            assert!(rows.iter().all(|r| r.2.len() < 4096));
        }
    }

    #[test]
    fn redacted_identifiers_or_colliding_redacted_keys_are_not_rewritten_into_other_calls() {
        for identifier_secret in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let rook = engine(dir.path());
            let session = rook.start_session("redaction identity").unwrap();
            let vault = Arc::new(Vault::empty());
            vault.also_hide("private-token");
            let mut request = Request::new(vec![Message::user("go")]);
            request.tools.push(rook_llm::ToolSpec {
                name: if identifier_secret { "private-token" } else { "read" }.into(),
                description: "read".into(),
                parameters: serde_json::json!({"private-token":1,"${secret}":2}),
            });
            let mut attempt = observed(&rook, session, vault, Some(true), None);
            attempt.capture_request(&request).unwrap();
            attempt.finish(AttemptStatus::Failed, &AttemptFacts::default()).unwrap();
            let rows = rows(&rook, session);
            let input: InputRecord = serde_json::from_slice(&rows[0].2).unwrap();
            assert_eq!(
                input.request.omitted,
                Some(if identifier_secret { Omitted::Identifiers } else { Omitted::Redaction })
            );
            assert!(rows.iter().all(|r| !String::from_utf8_lossy(&r.2).contains("private-token")));
        }
    }

    #[test]
    fn shared_reservations_bound_concurrent_capture_and_release_unused_bytes() {
        let budget = Budget::new(Settings {
            max_call_bytes: 4096,
            max_turn_bytes: 8192,
            max_calls: 4,
            ..Default::default()
        });
        let first = Capture::new(&budget);
        let second = Capture::new(&budget);
        let third = Capture::new(&budget);
        assert_eq!(first.limit() + second.limit(), 8192);
        assert_eq!(third.limit(), 0);
        drop(first);
        let fourth = Capture::new(&budget);
        assert_eq!(fourth.limit(), 4096);
        drop(second);
        drop(third);
        drop(fourth);
        assert_eq!(
            Capture::new(&budget).limit(),
            0,
            "call-count cap does not reset when byte reservations retire"
        );
    }

    #[test]
    fn request_admission_survives_reopen_without_inventing_an_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let session;
        {
            let rook = engine(dir.path());
            session = rook.start_session("pending input").unwrap();
            let mut attempt = observed(&rook, session, Arc::new(Vault::empty()), Some(true), None);
            attempt.capture_request(&Request::new(vec![Message::user("frozen before crash")])).unwrap();
        }
        let rook = engine(dir.path());
        let rows = rows(&rook, session);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, INPUT);
        let input: InputRecord = serde_json::from_slice(&rows[0].2).unwrap();
        assert_eq!(input.request.value.unwrap()["messages"][0]["content"], "frozen before crash");
    }

    #[test]
    fn oversized_recursive_json_and_nonstreaming_tool_arguments_are_refused_before_copying() {
        let dir = tempfile::tempdir().unwrap();
        let rook = engine(dir.path());
        let session = rook.start_session("shape and reply").unwrap();
        let mut deep = Value::Null;
        for _ in 0..40 {
            deep = serde_json::json!([deep]);
        }
        let mut request = Request::new(vec![Message::user("go")]);
        request.tools.push(rook_llm::ToolSpec {
            name: "read".into(),
            description: "read".into(),
            parameters: deep,
        });
        let mut attempt = observed(&rook, session, Arc::new(Vault::empty()), Some(true), None);
        attempt.capture_request(&request).unwrap();
        attempt.finish(AttemptStatus::Failed, &AttemptFacts::default()).unwrap();
        let first = rows(&rook, session);
        let input: InputRecord = serde_json::from_slice(&first[0].2).unwrap();
        assert!(input.request.value.is_none());
        let mut second = observed(
            &rook,
            session,
            Arc::new(Vault::empty()),
            Some(true),
            Some(Budget::new(Settings { max_call_bytes: 4096, ..Default::default() })),
        );
        second.capture_request(&Request::new(vec![Message::user("go")])).unwrap();
        let mut message = Message::assistant("answer");
        message.tool_calls.push(ToolCall {
            id: "call".into(),
            name: "read".into(),
            arguments: serde_json::json!({"large":"x".repeat(65536)}),
        });
        second.capture_response(&Response {
            message,
            stop_reason: StopReason::ToolUse,
            usage: Tokens::default(),
            model: "model".into(),
        });
        second.finish(AttemptStatus::Completed, &facts()).unwrap();
        let all = rows(&rook, session);
        let last: OutcomeRecord = serde_json::from_slice(&all.last().unwrap().2).unwrap();
        assert_eq!(last.reply.omitted, Some(Omitted::Bytes));
    }

    #[test]
    fn bounded_masking_stops_at_intermediate_expansion_and_preserves_utf8() {
        let vault = Vault::empty();
        vault.also_hide("secret");
        assert_eq!(vault.redact_bounded("Ж secret", 32).as_deref(), Some("Ж ${secret}"));
        assert!(vault.redact_bounded("secretsecret", 12).is_none());
        assert!(vault.redact_bounded("Ж", 1).is_none());
    }
}
