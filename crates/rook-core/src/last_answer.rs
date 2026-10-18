//! Bounded saved-answer selection shared by clipboard and scripted consumers.
use crate::{CoreError, Result, Rook};
use rook_store::EventKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    pub session: String,
    pub seq: u64,
    pub text: String,
    pub summary: bool,
    pub truncated: bool,
    pub scanned: usize,
}

impl Rook {
    /// Select only the current branch's saved prose; streamed drafts are not records.
    pub fn last_answer(&self, session: u128, summary: bool) -> Result<Answer> {
        let meta = self
            .store
            .get_session(session)?
            .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
        let events = self.store.events_before(session, meta.next_seq, 512)?;
        let kind = if summary { EventKind::Compaction } else { EventKind::AssistantMessage };
        for event in events.iter().rev().filter(|event| event.record.kind == kind) {
            let entry =
                self.transcript(session, event.seq, 1, 65536)?.into_iter().next().ok_or_else(|| {
                    CoreError::Other("saved answer disappeared; refresh the session".into())
                })?;
            if entry.seq != event.seq {
                return Err(CoreError::Other("saved answer source changed".into()));
            }
            let mut text = if summary {
                // Parsing an incomplete JSON prefix must never copy its internal fields.
                if entry.truncated {
                    return Err(CoreError::Other(
                        "summary exceeds the 64 KiB copy limit; inspect it in session history".into(),
                    ));
                }
                let value: serde_json::Value = serde_json::from_str(&entry.body)?;
                value.get("summary").and_then(|value| value.as_str()).unwrap_or_default().to_owned()
            } else {
                entry.body
            };
            if text.len() > 65536 {
                let mut end = 65536;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            if text.trim().is_empty() {
                continue;
            }
            return Ok(Answer {
                session: rook_store::format_session_id(session),
                seq: event.seq,
                text,
                summary,
                truncated: entry.truncated,
                scanned: events.len(),
            });
        }
        Err(CoreError::Other(
            "no saved answer in the latest 512 events; inspect older session history".into(),
        ))
    }
}
