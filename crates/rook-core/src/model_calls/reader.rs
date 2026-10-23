//! Bounded joins of immutable per-attempt records, never reconstructed history.
use super::{INPUT, InputRecord, MAX_RECORD_BYTES, OUTCOME, Omissions, Omitted, OutcomeRecord, Payload};
use crate::{CoreError, Result, Rook, model_attempt, model_route::Purpose};
use rook_store::{Event, EventKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_EVENTS: usize = 2048;
const MAX_CALLS: usize = 128;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Preview,
    Records,
    Dataset,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReadRequest {
    pub from: u64,
    /// Inclusive final event; omission freezes the current saved prefix.
    pub through: Option<u64>,
    pub purpose: Option<Purpose>,
    pub format: Format,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureInfo {
    pub seq: u64,
    pub object: String,
    pub payload_bytes: usize,
    pub omitted: Option<Omitted>,
    pub known_secret_redacted: bool,
    pub omissions: Omissions,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Call {
    pub attempt: String,
    pub purpose: Option<Purpose>,
    pub admission_seq: Option<u64>,
    pub ending_seq: Option<u64>,
    pub state: String,
    pub completion_confirmed: bool,
    pub dispatch: Option<rook_llm::Dispatch>,
    pub input: Option<CaptureInfo>,
    pub output: Option<CaptureInfo>,
    pub eligible: bool,
    pub exclusions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

/// A single captured prompt and its response. Historical assistant messages
/// belong to the prompt; only `response` is a target. No history stitching.
#[derive(Debug, Serialize, Deserialize)]
pub struct Example {
    pub format: String,
    pub session: String,
    pub attempt: String,
    pub purpose: Purpose,
    pub dispatch: rook_llm::Dispatch,
    pub input: CaptureInfo,
    pub output: CaptureInfo,
    pub request: Value,
    pub response: Value,
    pub target: String,
    pub review_required: bool,
    pub quality: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub version: u8,
    pub session: String,
    pub from: u64,
    pub through: Option<u64>,
    pub purpose: Option<Purpose>,
    pub format: Format,
    pub scanned_events: usize,
    pub source_bytes: usize,
    pub attempts_seen: usize,
    pub filtered_attempts: usize,
    pub unsupported_capture_records: usize,
    pub eligible: usize,
    pub exclusions: BTreeMap<String, usize>,
    pub scope: String,
    pub calls: Vec<Call>,
    pub examples: Vec<Example>,
}
impl Artifact {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        crate::persistence::encode_with_limit(self, MAX_ARTIFACT_BYTES)
    }
}

fn bad(message: &str) -> CoreError {
    CoreError::Other(message.into())
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(bad(message)) }
}

struct Input {
    event: Event,
    row: InputRecord,
}
struct Output {
    event: Event,
    row: OutcomeRecord,
}
#[derive(Default)]
struct Joined {
    purpose: Option<Purpose>,
    admission: Option<(u64, model_attempt::Record)>,
    ending: Option<(u64, model_attempt::Record)>,
    input: Option<Input>,
    output: Option<Output>,
}
impl Joined {
    fn purpose(&mut self, purpose: Purpose) -> Result<()> {
        require(self.purpose.is_none_or(|p| p == purpose), "model-call purpose changed within one attempt")?;
        self.purpose = Some(purpose);
        Ok(())
    }
    fn first(&self) -> u64 {
        self.admission
            .as_ref()
            .map(|a| a.0)
            .or_else(|| self.input.as_ref().map(|i| i.event.seq))
            .or_else(|| self.output.as_ref().map(|o| o.event.seq))
            .or_else(|| self.ending.as_ref().map(|a| a.0))
            .unwrap_or(0)
    }
}

fn body(rook: &Rook, event: &Event, limit: usize, used: &mut usize) -> Result<Vec<u8>> {
    let remaining = MAX_SOURCE_BYTES.saturating_sub(*used);
    let bytes =
        rook.store.get_limited(&event.record.body, limit.min(remaining), limit + 65536).map_err(|_| {
            bad("model-call source object exceeds admission bounds or is unavailable; narrow the event range")
        })?;
    *used += bytes.len();
    Ok(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|_| bad("malformed saved model-call record; preserve the source for inspection"))
}
fn payload(payload: &Payload, bytes: usize) -> Result<()> {
    require(payload.value.is_some() != payload.omitted.is_some(), "ambiguous model-call payload/omission")?;
    if let Some(value) = &payload.value {
        let mut nodes = 0;
        require(super::json_shape(value, 0, &mut nodes), "model-call payload exceeds shape bounds")?;
        require(
            crate::persistence::encode_with_limit(value, 1024 * 1024)?.len() == bytes,
            "model-call payload length changed",
        )?;
    } else {
        require(bytes == 0 && !payload.known_secret_redacted, "invalid model-call omission length")?;
    }
    Ok(())
}
fn info(event: &Event, payload: &Payload, bytes: usize) -> CaptureInfo {
    CaptureInfo {
        seq: event.seq,
        object: event.record.body.to_hex(),
        payload_bytes: bytes,
        omitted: payload.omitted,
        known_secret_redacted: payload.known_secret_redacted,
        omissions: payload.omissions,
    }
}

fn keys(value: &Value, expected: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == expected.len() && expected.iter().all(|key| o.contains_key(*key)))
}
fn id(value: &Value, limit: usize) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control))
}
fn calls(value: &Value) -> bool {
    value.as_array().is_some_and(|calls| {
        calls.len() <= 256
            && calls
                .iter()
                .all(|c| keys(c, &["id", "name", "arguments"]) && id(&c["id"], 256) && id(&c["name"], 128))
    })
}
fn message(value: &Value) -> bool {
    keys(value, &["role", "content", "tool_calls", "tool_call_id", "cache"])
        && matches!(value["role"].as_str(), Some("system" | "user" | "assistant" | "tool"))
        && value["content"].is_string()
        && value["cache"].is_boolean()
        && calls(&value["tool_calls"])
        && (value["tool_call_id"].is_null() || id(&value["tool_call_id"], 256))
}

/// Eligibility is deliberately separate from captured content. Invalid or
/// incomplete tool histories remain inspectable, but are not learning targets.
fn content_eligible(request: &Value, response: &Value) -> std::result::Result<(), &'static str> {
    if !keys(
        request,
        &[
            "messages",
            "tools",
            "max_output_tokens",
            "temperature",
            "effort",
            "cache_ttl",
            "model_capabilities",
        ],
    ) || !keys(response, &["message", "stop_reason"])
    {
        return Err("unsupported_content_shape");
    }
    let messages = request["messages"].as_array().ok_or("unsupported_content_shape")?;
    let tools = request["tools"].as_array().ok_or("unsupported_content_shape")?;
    if messages.is_empty()
        || messages.len() > 4096
        || tools.len() > 256
        || !request["max_output_tokens"].as_u64().is_some_and(|n| n > 0 && n <= u32::MAX.into())
        || request["temperature"].as_f64().is_none()
        || !(request["effort"].is_null()
            || matches!(request["effort"].as_str(), Some("low" | "medium" | "high" | "xhigh" | "max")))
        || !matches!(request["cache_ttl"].as_str(), Some("5m" | "1h"))
        || !capabilities(&request["model_capabilities"])
    {
        return Err("unsupported_content_shape");
    }
    let mut advertised = BTreeSet::new();
    for tool in tools {
        if !keys(tool, &["name", "description", "parameters"])
            || !id(&tool["name"], 128)
            || !tool["description"].is_string()
            || !(tool["parameters"].is_object() || tool["parameters"].is_boolean())
            || !advertised.insert(tool["name"].as_str().ok_or("invalid_tool_definitions")?)
        {
            return Err("invalid_tool_definitions");
        }
    }
    let mut pending = BTreeSet::new();
    for m in messages {
        if !message(m) {
            return Err("unsupported_content_shape");
        }
        let role = m["role"].as_str().ok_or("unsupported_content_shape")?;
        let tc = m["tool_calls"].as_array().ok_or("unsupported_content_shape")?;
        if role == "tool" {
            if !tc.is_empty() || !m["tool_call_id"].as_str().is_some_and(|id| pending.remove(id)) {
                return Err("orphan_tool_result");
            }
        } else {
            if !pending.is_empty() {
                return Err("incomplete_tool_history");
            }
            if !m["tool_call_id"].is_null() || role != "assistant" && !tc.is_empty() {
                return Err("invalid_tool_history");
            }
            for c in tc {
                if !c["arguments"].is_object() {
                    return Err("invalid_tool_arguments");
                }
                if !pending.insert(c["id"].as_str().ok_or("invalid_tool_history")?) {
                    return Err("duplicate_tool_call_id");
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err("incomplete_tool_history");
    }
    let reply = &response["message"];
    if !message(reply) || reply["role"] != "assistant" || !reply["tool_call_id"].is_null() {
        return Err("unsupported_content_shape");
    }
    let tc = reply["tool_calls"].as_array().ok_or("unsupported_content_shape")?;
    match response["stop_reason"].as_str() {
        Some("ToolUse") if !tc.is_empty() => {
            let mut seen = BTreeSet::new();
            for call in tc {
                if !call["arguments"].is_object() {
                    return Err("invalid_tool_arguments");
                }
                if !advertised.contains(call["name"].as_str().ok_or("unsupported_content_shape")?) {
                    return Err("unadvertised_tool");
                }
                if !seen.insert(call["id"].as_str().ok_or("unsupported_content_shape")?) {
                    return Err("duplicate_tool_call_id");
                }
            }
        }
        Some("EndTurn" | "Refusal")
            if tc.is_empty() && reply["content"].as_str().is_some_and(|s| !s.trim().is_empty()) => {}
        Some("MaxTokens") => return Err("max_token_reply"),
        _ => return Err("unsupported_reply_ending"),
    }
    Ok(())
}

fn capabilities(value: &Value) -> bool {
    // Unknown capability fields can change the request's meaning. Keep such
    // records inspectable without silently admitting a future schema.
    value.as_object().is_some_and(|object| {
        object.keys().all(|key| {
            matches!(
                key.as_str(),
                "tools"
                    | "image_input"
                    | "reasoning"
                    | "adaptive_thinking"
                    | "effort_levels"
                    | "reasoning_override"
            )
        })
    }) && serde_json::from_value::<rook_llm::ModelCapabilities>(value.clone()).is_ok()
}

pub fn read(rook: &Rook, session: u128, request: &ReadRequest) -> Result<Artifact> {
    let meta = rook
        .store
        .get_session_limited(session, 64 * 1024)?
        .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
    let before = match request.through {
        Some(through) => {
            let end =
                through.checked_add(1).ok_or_else(|| bad("model-call through exceeds the event range"))?;
            require(end <= meta.next_seq, "model-call through is beyond saved history")?;
            end
        }
        None => meta.next_seq,
    };
    require(request.from <= before, "model-call from is beyond the selected history")?;
    let events = rook.store.events_range_bounded(session, request.from, before, MAX_EVENTS + 1, 16 * 1024)?;
    require(events.len() <= MAX_EVENTS, "model-call scan exceeds 2048 events; narrow the range")?;
    let mut joined = BTreeMap::<String, Joined>::new();
    let mut used = 0;
    let mut unsupported = 0;
    for event in &events {
        if event.record.kind != EventKind::Note {
            continue;
        }
        let label = event.record.label.as_str();
        if !matches!(label, model_attempt::LABEL | INPUT | OUTCOME) {
            if label.starts_with("rook:model-call-") {
                unsupported += 1;
            }
            continue;
        }
        let bytes = body(
            rook,
            event,
            if label == model_attempt::LABEL { 4096 } else { MAX_RECORD_BYTES },
            &mut used,
        )?;
        if label == model_attempt::LABEL {
            let row = model_attempt::Record::read(&bytes)
                .map_err(|_| bad("malformed saved physical-attempt record"))?;
            let call = joined.entry(row.id.clone()).or_default();
            call.purpose(row.purpose)?;
            if matches!(row.state, model_attempt::State::Started) {
                require(
                    call.admission.is_none()
                        && call.ending.is_none()
                        && call.input.is_none()
                        && call.output.is_none(),
                    "duplicate or out-of-order model-call admission",
                )?;
                call.admission = Some((event.seq, row));
            } else {
                require(call.ending.is_none(), "duplicate physical-attempt ending")?;
                call.ending = Some((event.seq, row));
            }
        } else if label == INPUT {
            let row: InputRecord = decode(&bytes)?;
            require(
                row.version == 1 && rook_store::parse_session_id(&row.attempt).is_some(),
                "unsupported model-call input identity/version",
            )?;
            require(
                row.admitted_bytes <= 1024 * 1024 && row.payload_bytes <= row.admitted_bytes,
                "model-call input admission exceeded",
            )?;
            payload(&row.request, row.payload_bytes)?;
            let call = joined.entry(row.attempt.clone()).or_default();
            call.purpose(row.purpose)?;
            require(
                call.input.is_none() && call.output.is_none() && call.ending.is_none(),
                "duplicate or out-of-order model-call input",
            )?;
            call.input = Some(Input { event: event.clone(), row });
        } else {
            let row: OutcomeRecord = decode(&bytes)?;
            require(
                row.version == 1 && rook_store::parse_session_id(&row.attempt).is_some(),
                "unsupported model-call outcome identity/version",
            )?;
            require(
                row.input_seq.is_none_or(|seq| seq < event.seq),
                "model-call outcome refers to a future input",
            )?;
            payload(&row.reply, row.payload_bytes)?;
            let call = joined.entry(row.attempt.clone()).or_default();
            require(
                call.output.is_none() && call.ending.is_none(),
                "duplicate or out-of-order model-call outcome",
            )?;
            call.output = Some(Output { event: event.clone(), row });
        }
        require(joined.len() <= MAX_CALLS, "model-call selection exceeds 128 attempts; narrow the range")?;
    }
    let attempts_seen = joined.len();
    let mut calls = joined.into_iter().collect::<Vec<_>>();
    calls.sort_by_key(|(_, j)| j.first());
    let mut artifact=Artifact {version:1,session:rook_store::format_session_id(session),from:request.from,through:before.checked_sub(1),purpose:request.purpose,format:request.format,scanned_events:events.len(),source_bytes:used,attempts_seen,filtered_attempts:0,unsupported_capture_records:unsupported,eligible:0,exclusions:BTreeMap::new(),scope:"Recorded Rook physical provider attempts in the frozen range only. External ACP calls are outside this capture. Dataset targets are single responses, never historical assistant messages. Completion is not a quality label; review is required.".into(),calls:Vec::new(),examples:Vec::new()};
    for (attempt, mut joined) in calls {
        if request.purpose.is_some() && joined.purpose != request.purpose {
            artifact.filtered_attempts += 1;
            continue;
        }
        let mut reasons = Vec::new();
        if joined.admission.is_none() {
            reasons.push("missing_admission");
        }
        let ending = joined.ending.as_ref().map(|(_, r)| r);
        let state = match ending.map(|r| r.state) {
            Some(model_attempt::State::Completed) => "completed",
            Some(model_attempt::State::Failed) => "failed",
            Some(model_attempt::State::Incomplete) => "incomplete",
            Some(model_attempt::State::Interrupted) => "interrupted",
            _ => "started",
        };
        let confirmed = ending
            .is_some_and(|r| matches!(r.state, model_attempt::State::Completed) && r.completion_confirmed);
        if !confirmed {
            reasons.push("unconfirmed_or_incomplete_attempt");
        }
        if joined.input.is_none() {
            reasons.push("missing_input");
        }
        if joined.output.is_none() {
            reasons.push("missing_outcome");
        }
        if let (Some(input), Some(output)) = (&joined.input, &joined.output) {
            require(
                output.row.input_seq == Some(input.event.seq),
                "model-call outcome/input binding mismatch",
            )?;
            require(
                output.row.payload_bytes <= input.row.admitted_bytes.saturating_sub(input.row.payload_bytes),
                "model-call reply exceeded its reserved bytes",
            )?;
        }
        let dispatch =
            ending.or_else(|| joined.admission.as_ref().map(|(_, r)| r)).and_then(|r| r.dispatch.clone());
        if dispatch.is_none() {
            reasons.push("unknown_physical_model");
        }
        if let (Some((_, first)), Some((_, last))) = (&joined.admission, &joined.ending) {
            require(
                serde_json::to_value(&first.dispatch)? == serde_json::to_value(&last.dispatch)?,
                "physical dispatch changed within one attempt",
            )?;
        }
        for (name, p) in joined
            .input
            .as_ref()
            .map(|i| ("input", &i.row.request))
            .into_iter()
            .chain(joined.output.as_ref().map(|o| ("output", &o.row.reply)))
        {
            if p.value.is_none() {
                reasons.push(if name == "input" {
                    "input_payload_omitted"
                } else {
                    "output_payload_omitted"
                });
            }
            if p.known_secret_redacted {
                reasons.push(if name == "input" { "input_was_redacted" } else { "output_was_redacted" });
            }
            if p.omissions.images > 0 || p.omissions.provider_state > 0 || p.omissions.hidden_reasoning {
                reasons.push(if name == "input" {
                    "input_context_omitted"
                } else {
                    "output_context_omitted"
                });
            }
        }
        let input_value = joined.input.as_mut().and_then(|i| i.row.request.value.take());
        let output_value = joined.output.as_mut().and_then(|o| o.row.reply.value.take());
        if let (Some(input), Some(output)) = (&input_value, &output_value)
            && let Err(reason) = content_eligible(input, output)
        {
            reasons.push(reason);
        }
        let eligible = reasons.is_empty();
        artifact.eligible += usize::from(eligible);
        for reason in &reasons {
            *artifact.exclusions.entry((*reason).into()).or_default() += 1;
        }
        let input = joined.input.as_ref().map(|i| info(&i.event, &i.row.request, i.row.payload_bytes));
        let output = joined.output.as_ref().map(|o| info(&o.event, &o.row.reply, o.row.payload_bytes));
        if request.format == Format::Dataset && eligible {
            let (
                Some(purpose),
                Some(physical),
                Some(input),
                Some(output),
                Some(input_value),
                Some(output_value),
            ) = (joined.purpose, dispatch.clone(), input, output, input_value, output_value)
            else {
                return Err(bad("eligible model-call pair is missing its confirmed source context"));
            };
            artifact.examples.push(Example {
                format: "rook-canonical-visible-pair-v1".into(),
                session: artifact.session.clone(),
                attempt: attempt.clone(),
                purpose,
                dispatch: physical,
                input,
                output,
                request: input_value,
                response: output_value,
                target: "response_only".into(),
                review_required: true,
                quality: "unlabelled".into(),
            });
            artifact.calls.push(Call {
                attempt,
                purpose: joined.purpose,
                admission_seq: joined.admission.as_ref().map(|a| a.0),
                ending_seq: joined.ending.as_ref().map(|a| a.0),
                state: state.into(),
                completion_confirmed: confirmed,
                dispatch,
                input: None,
                output: None,
                eligible,
                exclusions: Vec::new(),
                request: None,
                response: None,
            });
        } else {
            artifact.calls.push(Call {
                attempt,
                purpose: joined.purpose,
                admission_seq: joined.admission.as_ref().map(|a| a.0),
                ending_seq: joined.ending.as_ref().map(|a| a.0),
                state: state.into(),
                completion_confirmed: confirmed,
                dispatch,
                input,
                output,
                eligible,
                exclusions: reasons.into_iter().map(str::to_owned).collect(),
                request: if request.format == Format::Records { input_value } else { None },
                response: if request.format == Format::Records { output_value } else { None },
            });
        }
    }
    artifact.bytes()?;
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Vault};
    use rook_llm::{Attempt, AttemptFacts, AttemptStatus, Message, Request, Response, StopReason};
    use rook_skills::{Environment, SkillIndex};
    use rook_store::{Kind, NewEvent, Store};
    use serde_json::json;
    use std::sync::Arc;

    fn engine(path: &std::path::Path) -> Rook {
        Rook::from_parts(
            Store::open(path.join("store")).unwrap(),
            Config::default(),
            Environment::bare("windows", "x86_64", "0.1.0"),
            SkillIndex::default(),
            path.into(),
        )
    }
    fn start(r: &Rook, s: u128, purpose: Purpose, enabled: bool, vault: Arc<Vault>) -> Box<dyn Attempt> {
        model_attempt::observer_with_capture(
            r,
            s,
            vault,
            purpose,
            Arc::new(crate::model_route::Prices::new(&r.config)),
            None,
            Some(enabled),
        )
        .start(rook_llm::Dispatch::bounded("owned", "fixture-model", true).as_ref())
        .unwrap()
    }
    fn finish(a: &mut dyn Attempt, status: AttemptStatus, stop: StopReason) {
        a.capture_response(&Response {
            message: Message::assistant("owned reply"),
            stop_reason: stop,
            usage: Default::default(),
            model: "fixture-model".into(),
        });
        a.finish(
            status,
            &AttemptFacts { completion_confirmed: status == AttemptStatus::Completed, ..Default::default() },
        )
        .unwrap();
    }
    fn complete(r: &Rook, s: u128, request: &Request, purpose: Purpose) {
        let mut a = start(r, s, purpose, true, Arc::new(Vault::empty()));
        a.capture_request(request).unwrap();
        finish(a.as_mut(), AttemptStatus::Completed, StopReason::EndTurn);
    }
    fn inspect(r: &Rook, s: u128, format: Format) -> Artifact {
        read(r, s, &ReadRequest { format, ..Default::default() }).unwrap()
    }
    #[test]
    fn preview_records_and_dataset_keep_single_response_targets_with_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("reader").unwrap();
        complete(
            &r,
            s,
            &Request::new(vec![Message::system("policy A"), Message::user("question")]),
            Purpose::Main,
        );
        let preview = inspect(&r, s, Format::Preview);
        assert_eq!(preview.eligible, 1);
        assert!(!String::from_utf8(preview.bytes().unwrap()).unwrap().contains("policy A"));
        let records = inspect(&r, s, Format::Records);
        assert_eq!(records.calls[0].request.as_ref().unwrap()["messages"][0]["content"], "policy A");
        let dataset = inspect(&r, s, Format::Dataset);
        let ex = &dataset.examples[0];
        assert_eq!(ex.target, "response_only");
        assert!(ex.review_required);
        assert_eq!(ex.quality, "unlabelled");
        assert_eq!(ex.input.object.len(), 64);
        assert!(ex.output.seq > ex.input.seq);
        assert_eq!(ex.response["message"]["content"], "owned reply");
        assert!(dataset.calls[0].request.is_none());
    }
    #[test]
    fn frozen_range_keeps_pending_and_does_not_probe_future_event_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("frozen").unwrap();
        let mut a = start(&r, s, Purpose::Main, true, Arc::new(Vault::empty()));
        a.capture_request(&Request::new(vec![Message::user("question")])).unwrap();
        let prefix = inspect(&r, s, Format::Preview);
        assert_eq!(prefix.calls[0].state, "started");
        assert!(prefix.calls[0].dispatch.is_some());
        finish(a.as_mut(), AttemptStatus::Completed, StopReason::EndTurn);
        assert_eq!(inspect(&r, s, Format::Preview).eligible, 1);
        r.store
            .append_event(
                s,
                NewEvent::new(EventKind::Note, Kind::Message, b"ignored").label(&"x".repeat(20000)),
            )
            .unwrap();
        let frozen = read(&r, s, &ReadRequest { through: prefix.through, ..Default::default() }).unwrap();
        assert_eq!(frozen.bytes().unwrap(), prefix.bytes().unwrap());
        assert!(read(&r, s, &ReadRequest::default()).is_err());
    }
    #[test]
    fn incomplete_failed_interrupted_and_max_token_replies_are_not_examples() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("endings").unwrap();
        for (status, stop) in [
            (AttemptStatus::Incomplete, StopReason::EndTurn),
            (AttemptStatus::Failed, StopReason::EndTurn),
            (AttemptStatus::Interrupted, StopReason::EndTurn),
            (AttemptStatus::Completed, StopReason::MaxTokens),
        ] {
            let mut a = start(&r, s, Purpose::Main, true, Arc::new(Vault::empty()));
            a.capture_request(&Request::new(vec![Message::user("question")])).unwrap();
            finish(a.as_mut(), status, stop);
        }
        let data = inspect(&r, s, Format::Dataset);
        assert!(data.examples.is_empty());
        assert_eq!(data.exclusions["unconfirmed_or_incomplete_attempt"], 3);
        assert_eq!(data.exclusions["max_token_reply"], 1);
    }
    #[test]
    fn absent_capture_or_admission_is_never_an_empty_successful_pair() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("absent").unwrap();
        let mut a = start(&r, s, Purpose::Main, false, Arc::new(Vault::empty()));
        a.capture_request(&Request::new(vec![Message::user("not saved")])).unwrap();
        finish(a.as_mut(), AttemptStatus::Completed, StopReason::EndTurn);
        assert!(inspect(&r, s, Format::Dataset).examples.is_empty());
        complete(&r, s, &Request::new(vec![Message::user("saved")]), Purpose::Main);
        let records = inspect(&r, s, Format::Preview);
        let from = records.calls[1].input.as_ref().unwrap().seq;
        let partial =
            read(&r, s, &ReadRequest { from, format: Format::Dataset, ..Default::default() }).unwrap();
        assert!(partial.examples.is_empty());
        assert_eq!(partial.exclusions["missing_admission"], 1);
    }
    #[test]
    fn redaction_and_omitted_image_context_stay_inspectable_but_ineligible() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("privacy").unwrap();
        let vault = Arc::new(Vault::empty());
        vault.also_hide("private-token");
        let mut a = start(&r, s, Purpose::Main, true, vault);
        a.capture_request(&Request::new(vec![Message::user("private-token")])).unwrap();
        finish(a.as_mut(), AttemptStatus::Completed, StopReason::EndTurn);
        let mut m = Message::user("image");
        m.images.push(rook_llm::Image {
            mime_type: "image/png".into(),
            data: "omitted-image".into(),
            width: 1,
            height: 1,
        });
        complete(&r, s, &Request::new(vec![m]), Purpose::Main);
        let data = inspect(&r, s, Format::Records);
        assert_eq!(data.eligible, 0);
        assert_eq!(data.exclusions["input_was_redacted"], 1);
        assert_eq!(data.exclusions["input_context_omitted"], 1);
        assert!(!String::from_utf8(data.bytes().unwrap()).unwrap().contains("private-token"));
    }
    #[test]
    fn changed_system_tools_arguments_and_names_never_stitch_histories() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("fidelity").unwrap();
        let cases = [
            ("A", "read", "a", "read"),
            ("A", "read", "a", "write"), // changed current catalog only
            ("B", "read", "a", "read"),  // changed system only
            ("A", "read", "b", "read"),  // same ID, changed argument only
            ("A", "write", "a", "read"), // same ID, changed historical name only
            ("A", "read", "a", "read"),  // unchanged positive control
        ];
        for (policy, name, argument, advertised) in cases {
            let mut historical = Message::assistant("");
            historical.tool_calls.push(rook_llm::ToolCall {
                id: "same-id".into(),
                name: name.into(),
                arguments: json!({"file":argument}),
            });
            let mut result = Message::user("historical result");
            result.role = rook_llm::Role::Tool;
            result.tool_call_id = Some("same-id".into());
            let mut request =
                Request::new(vec![Message::system(policy), historical, result, Message::user("continue")]);
            request.tools.push(rook_llm::ToolSpec {
                name: advertised.into(),
                description: "current schema".into(),
                parameters: json!({"type":"object"}),
            });
            complete(&r, s, &request, Purpose::Main);
        }
        let data = inspect(&r, s, Format::Dataset);
        assert_eq!(data.examples.len(), cases.len());
        for (index, (policy, name, argument, advertised)) in cases.into_iter().enumerate() {
            let request = &data.examples[index].request;
            assert_eq!(request["messages"][0]["content"], policy);
            assert_eq!(request["tools"][0]["name"], advertised);
            assert_eq!(request["messages"][1]["tool_calls"][0]["arguments"]["file"], argument);
            assert_eq!(request["messages"][1]["tool_calls"][0]["name"], name);
        }
    }
    #[test]
    fn purpose_range_source_body_and_duplicate_controls_refuse_unsafe_reads() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("admission").unwrap();
        for purpose in [Purpose::Main, Purpose::Compaction] {
            complete(&r, s, &Request::new(vec![Message::user("q")]), purpose);
        }
        let request = ReadRequest { purpose: Some(Purpose::Compaction), ..Default::default() };
        let selected = read(&r, s, &request).unwrap();
        assert_eq!(selected.calls.len(), 1);
        assert_eq!(selected.filtered_attempts, 1);
        assert!(read(&r, s, &ReadRequest { through: Some(100000), ..Default::default() }).is_err());
        let old = inspect(&r, s, Format::Preview);
        r.store
            .append_event(
                s,
                NewEvent::new(EventKind::Note, Kind::Message, &vec![b'x'; MAX_RECORD_BYTES + 1]).label(INPUT),
            )
            .unwrap();
        assert!(read(&r, s, &ReadRequest::default()).unwrap_err().to_string().contains("source object"));
        assert_eq!(
            read(&r, s, &ReadRequest { through: old.through, ..Default::default() })
                .unwrap()
                .bytes()
                .unwrap(),
            old.bytes().unwrap()
        );
        let another = r.start_session("duplicate").unwrap();
        complete(&r, another, &Request::new(vec![Message::user("q")]), Purpose::Main);
        let events = r.store.events(another, 0, 64).unwrap();
        let input = events.iter().find(|e| e.record.label == INPUT).unwrap();
        let body = r.store.get(&input.record.body).unwrap();
        r.store
            .append_event(another, NewEvent::new(EventKind::Note, Kind::Message, &body).label(INPUT))
            .unwrap();
        assert!(read(&r, another, &ReadRequest::default()).unwrap_err().to_string().contains("out-of-order"));
    }
    #[test]
    fn invalid_arguments_orphan_results_and_unadvertised_tools_are_excluded() {
        let request = json!({"messages":[],"tools":[],"max_output_tokens":1,"temperature":0,"effort":null,"cache_ttl":"5m","model_capabilities":{}});
        let mut req = request;
        let mut msg =
            json!({"role":"tool","content":"result","tool_calls":[],"tool_call_id":"orphan","cache":false});
        req["messages"] = json!([msg]);
        let mut reply = json!({"message":{"role":"assistant","content":"","tool_calls":[{"id":"call","name":"absent","arguments":{}}],"tool_call_id":null,"cache":false},"stop_reason":"ToolUse"});
        assert_eq!(content_eligible(&req, &reply), Err("orphan_tool_result"));
        msg["role"] = json!("user");
        msg["tool_call_id"] = Value::Null;
        req["messages"] = json!([msg]);
        assert_eq!(content_eligible(&req, &reply), Err("unadvertised_tool"));
        reply["message"]["tool_calls"][0]["arguments"] = json!("broken");
        assert_eq!(content_eligible(&req, &reply), Err("invalid_tool_arguments"));
    }

    #[test]
    fn every_existing_purpose_survives_the_same_capture_and_reader_without_a_second_taxonomy() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("purposes").unwrap();
        let purposes = [
            Purpose::Main,
            Purpose::Checking,
            Purpose::CompletionCheck,
            Purpose::OutputRepair,
            Purpose::Compaction,
            Purpose::Aside,
            Purpose::FinalAnswer,
            Purpose::BranchSummary,
            Purpose::Learning,
        ];
        for purpose in purposes {
            complete(&r, s, &Request::new(vec![Message::user("q")]), purpose);
        }
        let dataset = inspect(&r, s, Format::Dataset);
        assert_eq!(dataset.eligible, purposes.len());
        assert_eq!(dataset.examples.iter().map(|e| e.purpose).collect::<Vec<_>>(), purposes);
    }

    #[test]
    fn event_and_attempt_admission_fail_before_unbounded_joins() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("events").unwrap();
        for _ in 0..=MAX_EVENTS {
            r.store.append_event(s, NewEvent::new(EventKind::Note, Kind::Message, b"x")).unwrap();
        }
        assert!(read(&r, s, &ReadRequest::default()).unwrap_err().to_string().contains("2048"));
        let old = read(&r, s, &ReadRequest { through: Some(10), ..Default::default() }).unwrap();
        assert_eq!(old.scanned_events, 11);
        let s = r.start_session("calls").unwrap();
        for _ in 0..=MAX_CALLS {
            let _attempt = start(&r, s, Purpose::Main, false, Arc::new(Vault::empty()));
        }
        assert!(read(&r, s, &ReadRequest::default()).unwrap_err().to_string().contains("128 attempts"));
    }

    #[test]
    fn payload_exports_refuse_output_overflow_while_metadata_preview_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = engine(dir.path());
        let s = r.start_session("output bound").unwrap();
        r.config.telemetry.model_calls.max_call_bytes = 1024 * 1024;
        let request = Request::new(vec![Message::user("z".repeat(450000))]);
        for _ in 0..10 {
            complete(&r, s, &request, Purpose::Main);
        }
        assert_eq!(inspect(&r, s, Format::Preview).eligible, 10);
        for format in [Format::Records, Format::Dataset] {
            assert!(read(&r, s, &ReadRequest { format, ..Default::default() }).is_err());
        }
        assert_eq!(
            read(&r, s, &ReadRequest { format: Format::Records, through: Some(3), ..Default::default() })
                .unwrap()
                .eligible,
            1
        );
    }

    #[test]
    fn future_capture_labels_report_coverage_without_opening_unsupported_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let r = engine(dir.path());
        let s = r.start_session("future").unwrap();
        r.store
            .append_event(
                s,
                NewEvent::new(EventKind::Note, Kind::Message, &vec![b'x'; MAX_RECORD_BYTES + 1])
                    .label("rook:model-call-input:v2"),
            )
            .unwrap();
        let report = inspect(&r, s, Format::Preview);
        assert_eq!(report.unsupported_capture_records, 1);
        assert_eq!(report.source_bytes, 0);
        assert!(report.examples.is_empty());
    }
}
