//! Wire types shared by the daemon, the CLI and the web UI.
//!
//! Kept in its own crate so the HTTP surface has exactly one definition. A web
//! UI that drifts from the CLI is how two front ends end up disagreeing about
//! what the agent did.
//!
//! # Interoperability
//!
//! Rook speaks three external protocols, and none of them is invented here:
//!
//! * **Agent Skills** — the on-disk `SKILL.md` format. Implemented; see
//!   `rook-skills`, which reads skills written for any conforming agent.
//! * **ACP** (Agent Client Protocol) — JSON-RPC 2.0 over stdio, how editors talk
//!   to agents. Implemented by `rook acp`; see `rook-acp`.
//! * **MCP** (Model Context Protocol) — for consuming third-party tools. Planned.
//! * **Agent Plugins** — `plugin.json` packaging around skills and MCP servers.
//!   Planned; it defers to Agent Skills for the skill format itself.
//!
//! The HTTP API below is Rook's own, for its CLI and web UI only.

use serde::{Deserialize, Serialize};

/// Bumped when the HTTP surface changes incompatibly. Clients send it in
/// `X-Rook-Api`; the daemon refuses a mismatch rather than misbehaving quietly.
pub const API_VERSION: u32 = 1;

pub mod routes {
    pub const HEALTH: &str = "/api/health";
    pub const STATS: &str = "/api/store/stats";
    pub const OBJECTS: &str = "/api/store/objects";
    pub const OBJECT: &str = "/api/store/objects/{id}";
    pub const REFS: &str = "/api/store/refs";
    pub const SESSIONS: &str = "/api/sessions";
    pub const SESSION: &str = "/api/sessions/{id}";
    pub const TRANSCRIPT: &str = "/api/sessions/{id}/transcript";
    pub const SKILLS: &str = "/api/skills";
    pub const SKILL: &str = "/api/skills/{name}";
    pub const SKILL_HISTORY: &str = "/api/skills/{name}/history";
    pub const CHECKPOINTS: &str = "/api/checkpoints";
    pub const MAINTENANCE: &str = "/api/maintenance";
    pub const EVENTS_WS: &str = "/api/events";
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub version: String,
    pub api_version: u32,
    pub store_root: String,
    pub workspace: String,
    pub os: String,
    pub arch: String,
    pub uptime_secs: u64,
}

/// One page of results. Cursor rather than offset: an append-only log grows
/// underneath a reader, and offset paging silently skips or repeats entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>) -> Self {
        Self { items, next_cursor: None, total: None }
    }

    pub fn with_cursor(mut self, cursor: Option<String>) -> Self {
        self.next_cursor = cursor;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
    /// Machine-readable discriminant, e.g. `not_found`, `capture_too_big`.
    pub kind: String,
    /// What the caller can do about it, when there is something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ApiError {
    pub fn new(kind: &str, error: impl Into<String>) -> Self {
        Self { error: error.into(), kind: kind.to_string(), hint: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// What a browser sends over the chat socket.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Prompt {
        session: Option<String>,
        text: String,
    },
    /// Answer to an [`ChatEvent::Approval`], by its id.
    Approval {
        id: String,
        decision: ApprovalDecision,
    },
    /// Answers to a [`ChatEvent::Ask`], in the order the questions came, one
    /// entry each. An empty `chosen` is a question the user skipped.
    Answers {
        id: String,
        answers: Vec<Vec<String>>,
    },
    /// Change a session setting for the rest of the connection: `mode` takes
    /// auto/ask/readonly, `effort` takes low…max.
    Setting {
        name: String,
        value: String,
    },
    /// Stop the turn in flight, leaving what it already did in the log.
    Cancel,
}

/// One question on a form the agent put to the user.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AskQuestion {
    pub question: String,
    /// Empty means free text.
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub multi: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Once,
    ForRun,
    Deny,
}

/// What the server streams back during a turn.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Started {
        session: String,
    },
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Tool {
        name: String,
    },
    /// The call named by the most recent [`ChatEvent::Tool`] has finished.
    ToolDone {
        name: String,
        failed: bool,
    },
    /// What a turn changed about what the agent believes, said the way file
    /// changes are: an agent that quietly drops what it was told to remember is
    /// as bad as one that quietly remembers something nobody sees.
    Remembered {
        text: String,
    },
    Forgot {
        text: String,
    },
    /// What the turn has spent so far, sent after each reply from the model so
    /// a long turn shows its cost while it can still change a decision.
    Spent {
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
    },
    /// The turn is blocked until an [`ClientMessage::Approval`] with this id.
    /// Something the user said while a turn was running, echoed back so the page
    /// can show it landed rather than leaving the box looking ignored.
    Interjected {
        text: String,
    },
    Approval {
        id: String,
        tool: String,
        action: String,
        /// What the call would change, when the tool can say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    /// The turn is blocked until a [`ClientMessage::Answers`] with this id.
    Ask {
        id: String,
        questions: Vec<AskQuestion>,
    },
    /// What the settings are now, after a [`ClientMessage::Setting`] or on
    /// connecting.
    Settings {
        mode: String,
        effort: String,
    },
    /// The turn was stopped before it finished. Sent instead of `Done`, so a
    /// client waiting on one of them is never left waiting.
    Cancelled,
    Done {
        steps: u32,
        input_tokens: u32,
        output_tokens: u32,
        delegated: Vec<String>,
        compactions: u32,
        #[serde(default)]
        decisions: Vec<String>,
        #[serde(default)]
        open_questions: Vec<String>,
    },
    Error {
        message: String,
    },
}

/// Server-pushed events over the websocket.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A new entry landed in a session log.
    Event {
        session: String,
        seq: u64,
        kind: String,
        label: String,
    },
    /// Long-running maintenance progress, so a big prune is not a frozen UI —
    /// the shape of failure where a backup runs past the gateway timeout and the
    /// user is told it failed while it is still working.
    Progress {
        job: String,
        done: u64,
        total: Option<u64>,
        message: String,
    },
    Done {
        job: String,
        ok: bool,
        message: String,
    },
}
