//! Workspace-scoped conversation discovery shares the bounded history reader.
//! Cursors are durable event positions; fork prefixes already discoverable in
//! their retained parent are skipped rather than flooding results with copies.

use rook_store::SessionMeta;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result, Rook, transcript};

pub const TOOL: &str = "session_recall";
const MAX_SESSIONS: usize = 128;
const MAX_EVENTS: usize = 128;
const MAX_BYTES: usize = 256 * 1024;
const MAX_HITS: usize = 8;
const META_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Request {
    pub query: Option<String>,
    pub session: Option<String>,
    pub around: Option<u64>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Position {
    session: String,
    binding: String,
    #[serde(flatten)]
    event: transcript::Cursor,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Hit {
    pub session: String,
    pub title: String,
    #[serde(flatten)]
    pub event: transcript::Hit,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Response {
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<transcript::Page>,
    pub next: Option<String>,
    pub scanned_sessions: usize,
    pub scanned_events: usize,
    pub scanned_bytes: usize,
    pub inherited_prefixes_skipped: usize,
}

fn bad(message: impl Into<String>) -> CoreError {
    CoreError::InvalidConversationRecall(message.into())
}

/// Slash commands accept literal text or the same JSON request as the tool.
pub fn arguments(text: &str) -> Result<Request> {
    if text.len() > 4096 {
        return Err(bad("recall arguments exceed 4096 bytes"));
    }
    if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(|error| bad(error.to_string()))
    } else {
        Ok(Request { query: Some(text.trim().into()), ..Default::default() })
    }
}

/// Plain-text discovery and reading retain the same event links as JSON.
pub fn describe(response: &Response) -> String {
    let mut lines = Vec::new();
    for hit in &response.hits {
        lines.push(format!(
            "{} · {} · #{} ({})\n{}",
            hit.session, hit.title, hit.event.seq, hit.event.kind, hit.event.snippet
        ));
    }
    if let Some(page) = &response.page {
        lines.push(format!(
            "Historical conversation {} through #{}:",
            response.session.as_deref().unwrap_or_default(),
            page.through
        ));
        for entry in &page.items {
            lines.push(format!(
                "#{} {} {}\n{}{}",
                entry.seq,
                entry.kind,
                entry.label,
                entry.body,
                if entry.truncated { "\n[body shortened; use session entry to read more]" } else { "" }
            ));
        }
    } else {
        lines.push(format!(
            "{} hit(s); scanned {} session(s), {} events, {} bytes; skipped {} inherited prefixes.",
            response.hits.len(),
            response.scanned_sessions,
            response.scanned_events,
            response.scanned_bytes,
            response.inherited_prefixes_skipped
        ));
    }
    if let Some(cursor) = &response.next {
        lines.push(format!("More history remains. Continue with cursor: {cursor}"));
    }
    lines.join("\n\n")
}

fn cursor(text: Option<&str>) -> Result<Option<Position>> {
    text.map(|text| {
        if text.len() > 256 {
            return Err(bad("conversation cursor exceeds 256 bytes"));
        }
        let position: Position =
            serde_json::from_str(text).map_err(|_| bad("invalid conversation cursor"))?;
        rook_store::parse_session_id(&position.session)
            .ok_or_else(|| bad("invalid conversation cursor session"))?;
        Ok(position)
    })
    .transpose()
}

fn encoded(session: u128, event: transcript::Cursor, binding: &str) -> Result<String> {
    serde_json::to_string(&Position {
        session: rook_store::format_session_id(session),
        binding: binding.into(),
        event,
    })
    .map_err(|error| bad(error.to_string()))
}

fn scoped(rook: &Rook, session: u128) -> Result<SessionMeta> {
    let meta = rook
        .store
        .get_session_limited(session, META_BYTES)?
        .ok_or_else(|| CoreError::NoSession(rook_store::format_session_id(session)))?;
    if meta.workspace != rook.workspace.to_string_lossy() {
        return Err(bad("conversation recall is restricted to the current workspace"));
    }
    Ok(meta)
}

impl Rook {
    /// Search literal text across this workspace's conversations, then read
    /// around a hit. Every continuation explicitly reports unfinished scanning.
    pub fn session_recall(&self, request: &Request) -> Result<Response> {
        self.session_recall_excluding(request, None)
    }

    // Model discovery excludes its own ongoing conversation to prevent recall
    // outputs and the current query from feeding back into subsequent searches.
    pub(crate) fn session_recall_excluding(
        &self,
        request: &Request,
        caller: Option<u128>,
    ) -> Result<Response> {
        if request.query.as_ref().is_some_and(|q| q.trim().is_empty() || q.len() > 256)
            || request.session.as_ref().is_some_and(|s| rook_store::parse_session_id(s).is_none())
        {
            return Err(bad("provide 1–256 bytes of literal query text or a valid session id"));
        }
        let binding = rook_store::ObjectId::of(&serde_json::to_vec(&(
            self.workspace.to_string_lossy(),
            &request.query,
            &request.session,
            caller,
        ))?)
        .to_hex();
        let position = cursor(request.cursor.as_deref())?;
        if position.as_ref().is_some_and(|p| p.binding != binding) {
            return Err(bad("conversation cursor belongs to another workspace, query or mode"));
        }
        let encoded = |id, at| encoded(id, at, &binding);
        if let Some(session) = &request.session {
            if request.query.is_some() {
                return Err(bad(
                    "reading needs session and optional around; remove query from the arguments",
                ));
            }
            let id = rook_store::parse_session_id(session).ok_or_else(|| bad("invalid session id"))?;
            scoped(self, id)?;
            if position.as_ref().is_some_and(|position| position.session != *session) {
                return Err(bad("conversation cursor belongs to another session"));
            }
            let from = position
                .map(|p| p.event.seq)
                .unwrap_or_else(|| request.around.unwrap_or(0).saturating_sub(2));
            let page = self.transcript_page_bounded(
                id,
                &transcript::PageRequest { from: Some(from), limit: Some(6), ..Default::default() },
                transcript::Settings {
                    page_entries: 6,
                    page_bytes: 16384,
                    body_bytes: 1024,
                    ..Default::default()
                },
            )?;
            let next = page
                .next
                .map(|seq| encoded(id, transcript::Cursor { seq, ..Default::default() }))
                .transpose()?;
            return Ok(Response {
                session: Some(session.clone()),
                page: Some(page),
                next,
                ..Default::default()
            });
        }
        if request.around.is_some() {
            return Err(bad("around needs a session to read"));
        }
        let query = request.query.as_deref().ok_or_else(|| bad("provide a query or a session"))?;
        if query.trim().is_empty() || query.len() > 256 {
            return Err(bad("conversation query needs 1–256 bytes of literal text"));
        }
        let start = position.as_ref().and_then(|p| rook_store::parse_session_id(&p.session));
        let ids = self.store.session_ids_after(start.and_then(|id| id.checked_sub(1)), MAX_SESSIONS + 1)?;
        let mut response = Response::default();
        let mut consumed = 0;
        for &id in ids.iter().take(MAX_SESSIONS) {
            if response.scanned_events >= MAX_EVENTS
                || response.scanned_bytes >= MAX_BYTES
                || response.hits.len() >= MAX_HITS
            {
                response.next = Some(encoded(id, transcript::Cursor::default())?);
                return Ok(response);
            }
            consumed += 1;
            response.scanned_sessions += 1;
            if Some(id) == caller {
                continue;
            }
            let Some(meta) = self.store.get_session_limited(id, META_BYTES)? else { continue };
            if meta.workspace != self.workspace.to_string_lossy() {
                continue;
            }
            let mut at = position.as_ref().filter(|_| Some(id) == start).map(|p| p.event).unwrap_or_default();
            if let Some(parent) = meta.parent
                && let Some(parent) = self.store.get_session_limited(parent, META_BYTES)?
                && parent.workspace == meta.workspace
                && let Some(forked_at) = self.forked_at(id)?
            {
                let prefix = forked_at.min(parent.next_seq);
                if at.seq < prefix {
                    at.seq = prefix;
                    at.offset = 0;
                    response.inherited_prefixes_skipped += 1;
                }
            }
            let found = self.transcript_search_bounded(
                id,
                query,
                at,
                transcript::Settings {
                    page_entries: MAX_HITS - response.hits.len(),
                    search_events: MAX_EVENTS - response.scanned_events,
                    search_bytes: MAX_BYTES - response.scanned_bytes,
                    ..Default::default()
                },
                true,
            )?;
            response.scanned_events += found.scanned_events;
            response.scanned_bytes += found.scanned_bytes;
            response.hits.extend(found.hits.into_iter().map(|event| Hit {
                session: rook_store::format_session_id(id),
                title: rook_llm::truncate(&meta.title, 256),
                event,
            }));
            if let Some(next) = found.next {
                response.next = Some(encoded(id, next)?);
                return Ok(response);
            }
        }
        if let Some(&next) = ids.get(consumed) {
            response.next = Some(encoded(next, transcript::Cursor::default())?);
        }
        Ok(response)
    }
}
