use redb::TableDefinition;
use serde::{Deserialize, Serialize};

use crate::object::ObjectId;

/// Bump only for changes that older builds cannot read. `Store::open` refuses a
/// store written by a newer format rather than silently corrupting it.
pub const FORMAT_VERSION: u32 = 1;

/// hash -> postcard(ObjectMeta)
pub const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("objects");
/// hash -> encoded payload, for objects small enough to live in the index.
pub const BLOBS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blobs");
/// human-readable name -> hash. This is what makes history addressable:
/// `skills/pdf@2.1.0`, `snapshots/2026-08-26T12:00`, `memory/head`.
pub const REFS: TableDefinition<&str, &[u8]> = TableDefinition::new("refs");
/// 16-byte big-endian session id -> postcard(SessionMeta)
pub const SESSIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("sessions");
/// 24-byte big-endian (session id, seq) -> postcard(EventRecord)
pub const EVENTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("events");
/// Small operational values: cursors, counters, config snapshots.
pub const KV: TableDefinition<&str, &[u8]> = TableDefinition::new("kv");

/// Payloads at or below this size are stored in the index rather than as their
/// own file. An agent produces enormous numbers of tiny objects; one inode each
/// would waste more space in slack than the payloads themselves occupy.
pub const INLINE_MAX: usize = 1 << 20; // 1 MiB

pub fn session_key(id: u128) -> [u8; 16] {
    id.to_be_bytes()
}

/// Big-endian so redb's lexicographic key order is also chronological order:
/// events for one session form a contiguous, correctly ordered range.
pub fn event_key(session: u128, seq: u64) -> [u8; 24] {
    let mut k = [0u8; 24];
    k[..16].copy_from_slice(&session.to_be_bytes());
    k[16..].copy_from_slice(&seq.to_be_bytes());
    k
}

pub fn parse_event_key(k: &[u8]) -> Option<(u128, u64)> {
    if k.len() != 24 {
        return None;
    }
    let sid = u128::from_be_bytes(k[..16].try_into().ok()?);
    let seq = u64::from_be_bytes(k[16..].try_into().ok()?);
    Some((sid, seq))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: u128,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Absolute path the session was rooted at, for grouping in the UIs.
    pub workspace: String,
    pub agent: String,
    pub model: String,
    /// Sequence number the next appended event will receive.
    pub next_seq: u64,
    pub event_count: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Set when this session was forked from another, so a rewind can branch
    /// instead of destroying history.
    pub parent: Option<u128>,
    pub tags: Vec<String>,
}

impl SessionMeta {
    pub fn new(id: u128, title: impl Into<String>, workspace: impl Into<String>, now: i64) -> Self {
        Self {
            id,
            title: title.into(),
            created_at: now,
            updated_at: now,
            workspace: workspace.into(),
            agent: String::new(),
            model: String::new(),
            next_seq: 0,
            event_count: 0,
            tokens_in: 0,
            tokens_out: 0,
            parent: None,
            tags: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum EventKind {
    UserMessage = 0,
    AssistantMessage = 1,
    Reasoning = 2,
    ToolCall = 3,
    ToolResult = 4,
    SkillLoaded = 5,
    Checkpoint = 6,
    Compaction = 7,
    Error = 8,
    Note = 9,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::UserMessage => "user",
            EventKind::AssistantMessage => "assistant",
            EventKind::Reasoning => "reasoning",
            EventKind::ToolCall => "tool-call",
            EventKind::ToolResult => "tool-result",
            EventKind::SkillLoaded => "skill",
            EventKind::Checkpoint => "checkpoint",
            EventKind::Compaction => "compaction",
            EventKind::Error => "error",
            EventKind::Note => "note",
        }
    }
}

/// One entry in a session's append-only log.
///
/// The record itself is deliberately tiny — the payload lives in the object
/// store, addressed by content. Replaying the same 40 KB file into context
/// twenty times costs twenty ~50-byte records, not 800 KB.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts: i64,
    pub kind: EventKind,
    pub body: ObjectId,
    /// Tool name, skill id, or model name depending on `kind`. Empty when unused.
    pub label: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
}

/// A log entry about to be appended.
///
/// Grouped into a struct rather than a long parameter list: six positional
/// arguments of which two are `u32` is a call site where a transposition is
/// invisible at the point of the mistake.
#[derive(Clone, Debug)]
pub struct NewEvent<'a> {
    pub kind: EventKind,
    /// Tool name, skill id or model name, depending on `kind`.
    pub label: &'a str,
    pub body_kind: crate::object::Kind,
    pub body: &'a [u8],
    pub tokens_in: u32,
    pub tokens_out: u32,
}

impl<'a> NewEvent<'a> {
    pub fn new(kind: EventKind, body_kind: crate::object::Kind, body: &'a [u8]) -> Self {
        Self { kind, label: "", body_kind, body, tokens_in: 0, tokens_out: 0 }
    }

    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    pub fn usage(mut self, tokens_in: u32, tokens_out: u32) -> Self {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
        self
    }
}

/// An event paired with the position it was read from.
#[derive(Clone, Debug)]
pub struct Event {
    pub session: u128,
    pub seq: u64,
    pub record: EventRecord,
}
