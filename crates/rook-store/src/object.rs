use serde::{Deserialize, Serialize};
use std::fmt;

/// A blake3 content hash. Objects are addressed by content, so identical
/// payloads — a re-read of the same file, a repeated tool result, the system
/// prompt on every turn — cost storage exactly once.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Short form used in CLI listings. Long enough to stay unambiguous in a
    /// store with millions of objects, short enough to read.
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

/// What an object holds. The kind drives which zstd dictionary is used, so
/// thousands of small same-shaped payloads compress far better than they would
/// individually.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Kind {
    /// A single conversation message body (JSON).
    Message = 0,
    /// Output captured from a tool call.
    ToolResult = 1,
    /// Contents of a file the agent read or wrote.
    FileBlob = 2,
    /// A `SKILL.md` document or one of its bundled assets.
    Skill = 3,
    /// A serialized memory record.
    Memory = 4,
    /// A workspace checkpoint manifest.
    Snapshot = 5,
    /// Anything without a more specific kind.
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

/// Index entry describing where and how an object is stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub kind: u8,
    pub codec: u8,
    /// Size of the payload as the caller handed it to us.
    pub size_raw: u64,
    /// Size actually occupied after compression.
    pub size_stored: u64,
    /// Unix seconds, first time this content was seen.
    pub created_at: i64,
    /// Objects above the inline threshold live in `objects/`, not in the index.
    pub external: bool,
}
