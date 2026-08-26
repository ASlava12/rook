use serde::{Deserialize, Serialize};
use std::fmt;

/// A blake3 content hash. Identical payloads — a re-read of the same file, a
/// repeated tool result — therefore cost storage exactly once.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// 6 bytes: unambiguous across millions of objects, short enough to read.
    pub fn short(self) -> String {
        hex::encode(&self.0[..6])
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let raw = hex::decode(s).ok()?;
        let arr: [u8; 32] = raw.try_into().ok()?;
        Some(Self(arr))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.short())
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// What an object holds. Selects the zstd dictionary, so same-shaped payloads
/// compress against a model trained on their own population.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Kind {
    Message = 0,
    ToolResult = 1,
    FileBlob = 2,
    Skill = 3,
    Memory = 4,
    Snapshot = 5,
    Other = 255,
}

impl Kind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Kind::Message,
            1 => Kind::ToolResult,
            2 => Kind::FileBlob,
            3 => Kind::Skill,
            4 => Kind::Memory,
            5 => Kind::Snapshot,
            _ => Kind::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::ToolResult => "tool-result",
            Kind::FileBlob => "file",
            Kind::Skill => "skill",
            Kind::Memory => "memory",
            Kind::Snapshot => "snapshot",
            Kind::Other => "other",
        }
    }

    pub const ALL: [Kind; 7] = [
        Kind::Message,
        Kind::ToolResult,
        Kind::FileBlob,
        Kind::Skill,
        Kind::Memory,
        Kind::Snapshot,
        Kind::Other,
    ];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub kind: u8,
    pub codec: u8,
    /// As the caller handed it to us, before compression.
    pub size_raw: u64,
    pub size_stored: u64,
    /// Unix seconds, first time this content was seen.
    pub created_at: i64,
    /// Stored in `objects/` rather than inline in the index.
    pub external: bool,
}
