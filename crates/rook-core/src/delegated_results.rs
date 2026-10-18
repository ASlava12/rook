//! Typed projections of saved child answers. References replace extra spill copies.
use crate::{CoreError, Result, Rook};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const LABEL: &str = "rook:delegated-result:v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub version: u32,
    pub session: String,
    pub through: u64,
    #[serde(default)]
    pub answer_seq: Option<u64>,
    pub status: String,
    pub validation: String,
    pub value: Option<Value>,
    pub omitted: bool,
    pub reason: Option<String>,
    #[serde(default)]
    pub source: String,
}

pub(crate) fn validate_schema(schema: &Value, workspace: &std::path::Path) -> Result<()> {
    crate::persistence::encode_with_limit(schema, 8192)?;
    let mut pending = vec![(schema, 0)];
    let mut nodes = 0;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 16 || nodes > 1024 {
            return Err(CoreError::Other("delegated schema exceeds depth 16 or 1024 nodes".into()));
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(values) => pending.extend(values.values().map(|value| (value, depth + 1))),
            _ => {}
        }
    }
    crate::output::Contract::compile(
        &rook_proto::TurnOptions {
            output_schema: Some(schema.clone()),
            schema_retries: 0,
            ..Default::default()
        },
        workspace,
    )?;
    Ok(())
}

pub(crate) fn schema_for(args: &Value, task: &str, workspace: &std::path::Path) -> Result<Option<Value>> {
    let mut schema = args.get("output_schema");
    let mut own = None;
    for item in args.get("tasks").and_then(Value::as_array).into_iter().flatten() {
        let text = item.as_str().or_else(|| {
            ["task", "goal", "prompt", "description"].iter().find_map(|key| item.get(key)?.as_str())
        });
        if text.map(str::trim) == Some(task)
            && let Some(candidate) = item.get("output_schema")
        {
            if own.is_some_and(|previous| previous != candidate) {
                return Err(CoreError::Other("duplicate delegated task has conflicting schemas".into()));
            }
            own = Some(candidate);
        }
    }
    schema = own.or(schema);
    if let Some(schema) = schema {
        validate_schema(schema, workspace)?;
    }
    Ok(schema.cloned())
}

fn redact(value: &mut Value, vault: &crate::Vault, depth: usize) -> Result<()> {
    if depth > 32 {
        return Err(CoreError::Other("delegated result exceeds depth 32".into()));
    }
    match value {
        Value::String(text) => *text = vault.redact(text),
        Value::Array(values) => {
            for value in values {
                redact(value, vault, depth + 1)?;
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, mut value) in original {
                redact(&mut value, vault, depth + 1)?;
                if values.insert(vault.redact(&key), value).is_some() {
                    return Err(CoreError::Other("delegated keys collide after redaction".into()));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn delivered(reply: &str, vault: &crate::Vault) -> Result<Value> {
    if reply.len() > 65536 {
        return Err(CoreError::Other(
            "delegated result exceeds the 64 KiB validation limit; inspect its saved source".into(),
        ));
    }
    let mut value: Value = serde_json::from_str(reply)
        .map_err(|_| CoreError::Other("delegated result is not valid JSON".into()))?;
    redact(&mut value, vault, 0)?;
    crate::persistence::encode_with_limit(&value, 65536).map_err(|_| {
        CoreError::Other("redacted delegated result exceeds the 64 KiB validation limit".into())
    })?;
    Ok(value)
}

impl Receipt {
    pub(crate) fn project(
        rook: &Rook,
        session: u128,
        schema: &Value,
        outcome: std::result::Result<&crate::agent::TurnOutcome, &CoreError>,
        vault: &crate::Vault,
    ) -> Result<Self> {
        let through = rook
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?
            .next_seq;
        let mut receipt = Self {
            version: 1,
            session: rook_store::format_session_id(session),
            through,
            answer_seq: rook.last_answer(session, false).ok().map(|answer| answer.seq),
            status: "failed".into(),
            validation: "unavailable".into(),
            value: None,
            omitted: true,
            reason: None,
            source: "saved".into(),
        };
        let saved;
        let (reply, finished) = match outcome {
            Ok(outcome) => {
                let finished = crate::agent::finished(&outcome.stopped);
                receipt.status = if finished { "completed" } else { "unfinished" }.into();
                (outcome.reply.as_str(), finished)
            }
            Err(error) => {
                receipt.reason = Some(vault.redact(&error.to_string()).chars().take(512).collect());
                if !error.to_string().contains("output schema")
                    && !error.to_string().contains("delegated result")
                {
                    return Ok(receipt);
                }
                saved = rook.last_answer(session, false)?;
                if saved.truncated {
                    return Ok(receipt);
                }
                (saved.text.as_str(), true)
            }
        };
        if reply.len() > 65536 {
            receipt.reason = Some(
                "answer exceeds the 64 KiB projection/validation limit; read the saved child history".into(),
            );
            return Ok(receipt);
        }
        let mut value: Value = match serde_json::from_str(reply) {
            Ok(value) => value,
            Err(_) => {
                receipt.validation = "invalid".into();
                receipt.reason = Some("child did not deliver JSON".into());
                return Ok(receipt);
            }
        };
        if let Err(error) = redact(&mut value, vault, 0) {
            receipt.validation = "invalid".into();
            receipt.reason = Some(error.to_string());
            return Ok(receipt);
        }
        let delivered = match crate::persistence::encode_with_limit(&value, 65536) {
            Ok(bytes) => bytes,
            Err(_) => {
                receipt.reason =
                    Some("redacted answer exceeds the 64 KiB validation limit; inspect saved source".into());
                return Ok(receipt);
            }
        };
        let text = String::from_utf8(delivered).map_err(|error| CoreError::Other(error.to_string()))?;
        let contract = crate::output::Contract::compile(
            &rook_proto::TurnOptions {
                output_schema: Some(schema.clone()),
                schema_retries: 0,
                ..Default::default()
            },
            &rook.workspace,
        )?;
        receipt.reason =
            contract.violation(&text).map(|reason| vault.redact(&reason).chars().take(512).collect());
        receipt.validation = if receipt.reason.is_none() { "valid" } else { "invalid" }.into();
        if !finished {
            receipt.validation = "unavailable".into();
        }
        receipt.omitted = text.len() > 8192;
        if !receipt.omitted {
            receipt.value = Some(value);
        }
        Ok(receipt)
    }

    pub(crate) fn record(&self, rook: &Rook, parent: u128, session: u128) -> Result<String> {
        let bytes = crate::persistence::encode_with_limit(self, 16384)?;
        rook.store.append_event_durable(
            session,
            rook_store::NewEvent::new(rook_store::EventKind::Note, rook_store::Kind::Message, &bytes)
                .label(LABEL),
        )?;
        if parent != session {
            rook.store.append_event_durable(
                parent,
                rook_store::NewEvent::new(rook_store::EventKind::Note, rook_store::Kind::Message, &bytes)
                    .label(LABEL),
            )?;
        }
        String::from_utf8(bytes).map_err(|error| CoreError::Other(error.to_string()))
    }
}

/// The latest bounded receipt window; full answers remain in child history.
impl Rook {
    pub fn delegated_results(&self, session: u128) -> Result<Vec<Receipt>> {
        let meta = self
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
        let mut receipts = Vec::new();
        for event in self
            .store
            .events_before(session, meta.next_seq, 512)?
            .iter()
            .rev()
            .filter(|event| event.record.kind == rook_store::EventKind::Note && event.record.label == LABEL)
            .take(64)
        {
            let bytes = self.store.get_range(&event.record.body, 0, 16385)?;
            if bytes.len() > 16384 {
                return Err(CoreError::Other("delegated receipt exceeds 16 KiB".into()));
            }
            let mut receipt: Receipt = serde_json::from_slice(&bytes)?;
            let child = rook_store::parse_session_id(&receipt.session)
                .ok_or_else(|| CoreError::Other("invalid delegated result source".into()))?;
            if receipt.version != 1 {
                return Err(CoreError::Other("unsupported delegated receipt version".into()));
            }
            receipt.source = match self.store.get_session(child)? {
                None => "missing",
                Some(meta) if meta.next_seq < receipt.through => "pruned",
                Some(_) => match receipt.answer_seq {
                    Some(seq) => match self.store.events(child, seq, 1)?.first().filter(|event| {
                        event.seq == seq && event.record.kind == rook_store::EventKind::AssistantMessage
                    }) {
                        Some(event) if self.store.stat_object(&event.record.body)?.is_some() => "saved",
                        _ => "pruned",
                    },
                    None => "saved",
                },
            }
            .into();
            receipts.push(receipt);
        }
        Ok(receipts)
    }
}

pub(crate) struct Guard {
    store: std::sync::Arc<rook_store::Store>,
    parent: u128,
    child: u128,
    open: bool,
}
impl Guard {
    pub(crate) fn new(rook: &Rook, parent: u128, child: u128, enabled: bool) -> Self {
        Self { store: rook.store.clone(), parent, child, open: enabled }
    }
    pub(crate) fn finish(&mut self) {
        self.open = false;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let through = self.store.get_session(self.child).ok().flatten().map(|m| m.next_seq);
        let receipt = Receipt {
            version: 1,
            session: rook_store::format_session_id(self.child),
            through: through.unwrap_or(0),
            answer_seq: None,
            status: "interrupted".into(),
            validation: "unavailable".into(),
            value: None,
            omitted: true,
            reason: Some("child result was interrupted; no tool replay was attempted".into()),
            source: if through.is_some() { "saved" } else { "missing" }.into(),
        };
        if let Ok(bytes) = crate::persistence::encode_with_limit(&receipt, 16384) {
            let _ = self.store.append_event_durable(
                self.parent,
                rook_store::NewEvent::new(rook_store::EventKind::Note, rook_store::Kind::Message, &bytes)
                    .label(LABEL),
            );
        }
    }
}
