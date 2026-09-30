//! Bounded navigation through durable history. Cursors belong to event positions,
//! never to a front end's loaded slice; quoted content carries no new authority.
use std::io::Write;

use rook_store::{Event, EventKind};
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result, Rook, TranscriptEntry};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub page_entries: usize,
    pub page_bytes: usize,
    pub body_bytes: usize,
    pub search_bytes: usize,
    pub search_events: usize,
    pub quote_bytes: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            page_entries: 64,
            page_bytes: 262144,
            body_bytes: 4096,
            search_bytes: 4 * 1024 * 1024,
            search_events: 256,
            quote_bytes: 16384,
        }
    }
}
impl Settings {
    pub(crate) fn bounded(self) -> Self {
        Self {
            page_entries: self.page_entries.clamp(1, 256),
            page_bytes: self.page_bytes.clamp(4096, 1024 * 1024),
            body_bytes: self.body_bytes.clamp(128, 65536),
            search_bytes: self.search_bytes.clamp(4096, 16 * 1024 * 1024),
            search_events: self.search_events.clamp(1, 4096),
            quote_bytes: self.quote_bytes.clamp(128, 65536),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PageRequest {
    pub from: Option<u64>,
    pub before: Option<u64>,
    pub limit: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<TranscriptEntry>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
    pub through: u64,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Cursor {
    pub seq: u64,
    pub offset: u64,
    pub through: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hit {
    pub seq: u64,
    pub kind: String,
    pub label: String,
    pub offset: u64,
    pub snippet: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Matches {
    pub hits: Vec<Hit>,
    pub next: Option<Cursor>,
    pub scanned_events: usize,
    pub scanned_bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryPage {
    pub entry: TranscriptEntry,
    pub offset: u64,
    pub next_offset: Option<u64>,
    pub previous_offset: Option<u64>,
    pub total_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Quote {
    pub text: String,
    pub seq: u64,
    pub offset: u64,
    pub next_offset: Option<u64>,
}

fn no_event(seq: u64) -> CoreError {
    CoreError::NoTranscriptEvent(seq)
}
fn end(rook: &Rook, session: u128) -> Result<u64> {
    rook.store
        .get_session(session)?
        .map(|m| m.next_seq)
        .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))
}
fn event(rook: &Rook, session: u128, seq: u64) -> Result<Event> {
    end(rook, session)?;
    rook.store.events(session, seq, 1)?.into_iter().find(|e| e.seq == seq).ok_or_else(|| no_event(seq))
}

/// Encoded attachments and provider companions are not searchable raw text.
/// Their fixed format bounds are checked before any whole-record decode.
fn display(rook: &Rook, event: &Event) -> Result<Option<Vec<u8>>> {
    if event.record.kind == EventKind::Note {
        if event.record.label == crate::branches::SUMMARY_LABEL {
            let limit = crate::branches::SUMMARY_BYTES + 1024;
            let size = rook.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
            if size > limit as u64 {
                return Err(CoreError::Other("stored branch summary exceeds its byte limit".into()));
            }
            let data = rook.store.get_range(&event.record.body, 0, limit)?;
            return Ok(Some(crate::branches::display_summary(&String::from_utf8_lossy(&data))?.into_bytes()));
        }
        if matches!(
            event.record.label.as_str(),
            crate::provider_history::LABEL | crate::provider_history::CALL
        ) {
            return Ok(Some(crate::provider_history::preview()));
        }
        if event.record.label == crate::tool_images::LABEL {
            return Ok(Some(crate::tool_images::preview(rook, event)?.into_bytes()));
        }
    }
    if event.record.kind == EventKind::UserMessage && event.record.label == crate::attachments::LABEL {
        let size = rook.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
        if size > crate::attachments::MAX_FRAME_BYTES as u64 {
            return Err(CoreError::Other("stored attachment message exceeds its frame limit".into()));
        }
        let data = rook.store.get_range(&event.record.body, 0, crate::attachments::MAX_FRAME_BYTES)?;
        return Ok(Some(crate::attachments::decode(&String::from_utf8_lossy(&data))?.content.into_bytes()));
    }
    Ok(None)
}

pub(crate) fn body(rook: &Rook, event: &Event, limit: usize) -> Result<(String, bool)> {
    let limit = limit.min(1024 * 1024);
    if let Some(text) = display(rook, event)? {
        let (text, cut) = crate::context::window_bytes(&text, limit);
        return Ok((String::from_utf8_lossy(&text).into_owned(), cut));
    }
    let size = rook.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0);
    if size <= limit as u64 {
        return Ok((
            String::from_utf8_lossy(&rook.store.get_range(&event.record.body, 0, limit)?).into_owned(),
            false,
        ));
    }
    let head = limit * 2 / 3;
    let tail = limit - head;
    let (front, back) = rook.store.get_ends(&event.record.body, head + 3, tail + 3)?;
    let start = crate::context::ceil_char_boundary(&back, back.len().saturating_sub(tail));
    let stop = crate::context::floor_char_boundary(&front, head);
    Ok((
        format!(
            "{}\n\n... {} bytes elided ...\n\n{}",
            String::from_utf8_lossy(&front[..stop]),
            size.saturating_sub(limit as u64),
            String::from_utf8_lossy(&back[start..])
        ),
        true,
    ))
}

fn entry_from(rook: &Rook, event: &Event, body: String, truncated: bool) -> Result<TranscriptEntry> {
    let meta = rook.store.stat_object(&event.record.body)?;
    let label = rook_llm::truncate(&event.record.label, 256);
    let doing = if event.record.kind == EventKind::ToolCall {
        rook_llm::truncate(
            &crate::calls::doing(
                &event.record.label,
                serde_json::from_str(&body).ok().as_ref(),
                &rook.workspace,
            ),
            256,
        )
    } else {
        String::new()
    };
    Ok(TranscriptEntry {
        seq: event.seq,
        ts: event.record.ts,
        kind: event.record.kind.as_str().into(),
        label,
        object: event.record.body.to_hex(),
        bytes: meta.as_ref().map(|m| m.size_raw).unwrap_or(0),
        stored_bytes: meta.as_ref().map(|m| m.size_stored).unwrap_or(0),
        tokens_in: event.record.tokens_in,
        tokens_out: event.record.tokens_out,
        truncated,
        body,
        doing,
    })
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("page full"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn size(value: &impl Serialize, limit: usize) -> Option<usize> {
    let mut count = Count { bytes: 0, limit };
    serde_json::to_writer(&mut count, value).ok()?;
    Some(count.bytes)
}

impl Rook {
    pub fn transcript_page(&self, session: u128, request: &PageRequest) -> Result<Page> {
        if request.from.is_some() && request.before.is_some() {
            return Err(CoreError::Other("choose from or before, not both".into()));
        }
        let through = end(self, session)?;
        let settings = self.config.transcript.bounded();
        let count = request.limit.unwrap_or(settings.page_entries).clamp(1, settings.page_entries);
        let backwards = request.from.is_none();
        let mut events = match request.from {
            Some(from) => self.store.events(session, from, count)?,
            None => {
                self.store.events_before(session, request.before.unwrap_or(through).min(through), count)?
            }
        };
        if backwards {
            events.reverse();
        }
        let mut items = Vec::new();
        let mut used = 256;
        for event in events {
            let mut budget = settings.body_bytes;
            loop {
                let (text, cut) =
                    body(self, &event, budget).unwrap_or_else(|e| (format!("<unreadable: {e}>"), false));
                let entry = entry_from(self, &event, text, cut)?;
                if let Some(bytes) = size(&entry, settings.page_bytes.saturating_sub(used + 1)) {
                    used += bytes + 1;
                    items.push(entry);
                    break;
                }
                if !items.is_empty() || budget == 0 {
                    break;
                }
                budget /= 2;
            }
            if items.last().is_none_or(|last| last.seq != event.seq) {
                break;
            }
        }
        if backwards {
            items.reverse();
        }
        let first =
            items.first().map(|e| e.seq).unwrap_or(request.before.or(request.from).unwrap_or(through));
        let after = items.last().map(|e| e.seq.saturating_add(1)).unwrap_or(first);
        let previous = (!self.store.events_before(session, first, 1)?.is_empty()).then_some(first);
        let next = (!self.store.events(session, after, 1)?.is_empty()).then_some(after);
        Ok(Page { items, previous, next, through })
    }

    pub fn transcript_entry(&self, session: u128, seq: u64, offset: u64) -> Result<EntryPage> {
        self.entry_page(session, seq, offset, self.config.transcript.bounded().body_bytes)
    }
    fn entry_page(&self, session: u128, seq: u64, offset: u64, limit: usize) -> Result<EntryPage> {
        let event = event(self, session, seq)?;
        let formatted = display(self, &event)?;
        let total = match &formatted {
            Some(text) => text.len() as u64,
            None => self.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0),
        };
        let offset = offset.min(total);
        let raw = match formatted {
            Some(text) => text
                .get(offset as usize..)
                .unwrap_or_default()
                .iter()
                .take(limit + 4)
                .copied()
                .collect::<Vec<_>>(),
            None => self.store.get_range(&event.record.body, offset, limit + 4)?,
        };
        let begin = crate::context::ceil_char_boundary(&raw, 0);
        let stop = crate::context::floor_char_boundary(&raw, limit.min(raw.len()));
        let bytes = raw.get(begin..stop).unwrap_or_default();
        let offset = offset.saturating_add(begin as u64);
        let next = offset.saturating_add(bytes.len() as u64);
        let entry = entry_from(
            self,
            &event,
            String::from_utf8_lossy(bytes).into_owned(),
            offset > 0 || next < total,
        )?;
        Ok(EntryPage {
            entry,
            offset,
            next_offset: (next < total).then_some(next),
            previous_offset: (offset > 0).then_some(offset.saturating_sub(limit as u64)),
            total_bytes: total,
        })
    }

    pub fn transcript_quote(&self, session: u128, seq: u64, offset: u64) -> Result<Quote> {
        let page = self.entry_page(session, seq, offset, self.config.transcript.bounded().quote_bytes)?;
        let text=serde_json::json!({"rook_source":{
            "kind":"transcript_quote","authority":"data","origin":format!("session {} event #{}",rook_store::format_session_id(session),seq),
            "session":rook_store::format_session_id(session),"seq":seq,"object":page.entry.object,
            "event_kind":page.entry.kind,"label":page.entry.label,"offset":page.offset,"complete":!page.entry.truncated,
            "content":page.entry.body
        }}).to_string();
        Ok(Quote { text, seq, offset: page.offset, next_offset: page.next_offset })
    }

    pub fn transcript_search(&self, session: u128, query: &str, cursor: Cursor) -> Result<Matches> {
        if query.trim().is_empty() || query.len() > 256 {
            return Err(CoreError::Other("history search needs 1–256 bytes of literal text".into()));
        }
        let through = cursor.through.unwrap_or(end(self, session)?).min(end(self, session)?);
        let settings = self.config.transcript.bounded();
        let pattern = regex::bytes::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()
            .map_err(|e| CoreError::Other(e.to_string()))?;
        // Unicode case folding can match a wider UTF-8 character than the query.
        let overlap = query.chars().count().saturating_mul(4);
        let events = self.store.events(session, cursor.seq, settings.search_events)?;
        let mut result = Matches { hits: Vec::new(), next: None, scanned_events: 0, scanned_bytes: 0 };
        if events.is_empty() {
            return Ok(result);
        }
        let mut position = Cursor { through: Some(through), ..cursor };
        for event in events.into_iter().take_while(|e| e.seq < through) {
            if result.scanned_events >= settings.search_events
                || result.scanned_bytes >= settings.search_bytes
                || result.hits.len() >= settings.page_entries
            {
                break;
            }
            result.scanned_events += 1;
            let offset = if event.seq == cursor.seq { cursor.offset } else { 0 };
            let formatted = display(self, &event)?;
            let total = match &formatted {
                Some(text) => text.len() as u64,
                None => self.store.stat_object(&event.record.body)?.map(|m| m.size_raw).unwrap_or(0),
            };
            let offset = offset.min(total);
            let take = settings.search_bytes.saturating_sub(result.scanned_bytes);
            let data = match formatted {
                Some(text) => text
                    .get(offset as usize..)
                    .unwrap_or_default()
                    .iter()
                    .take(take + overlap)
                    .copied()
                    .collect::<Vec<_>>(),
                None => self.store.get_range(&event.record.body, offset, take + overlap)?,
            };
            let scanned = data.len().min(take);
            result.scanned_bytes += scanned;
            let found = pattern.find(&data).filter(|m| m.start() < take);
            let label_match = offset == 0 && pattern.is_match(event.record.label.as_bytes());
            if found.is_some() || label_match {
                let at = found.map(|m| m.start()).unwrap_or(0);
                let begin = crate::context::ceil_char_boundary(&data, at.saturating_sub(100));
                let stop = crate::context::floor_char_boundary(&data, (at + 400).min(data.len()));
                result.hits.push(Hit {
                    seq: event.seq,
                    kind: event.record.kind.as_str().into(),
                    label: rook_llm::truncate(&event.record.label, 256),
                    offset: offset.saturating_add(begin as u64),
                    snippet: String::from_utf8_lossy(data.get(begin..stop).unwrap_or_default()).into_owned(),
                });
                position.seq = event.seq.saturating_add(1);
                position.offset = 0;
            } else if offset.saturating_add(scanned as u64) < total {
                position.seq = event.seq;
                position.offset = offset.saturating_add(scanned as u64);
                result.next = Some(position);
                return Ok(result);
            } else {
                position.seq = event.seq.saturating_add(1);
                position.offset = 0;
            }
        }
        result.next = (position.seq < through).then_some(position);
        Ok(result)
    }
}
