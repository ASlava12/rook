//! The subset of the Agent Client Protocol that Rook speaks.
//!
//! Field names and enum values come from the v1 schema in `references/acp`, not
//! from memory: an editor that gets `sessionUpdate: "agentMessage"` instead of
//! `"agent_message_chunk"` simply shows nothing, with no error to explain it.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct Response<'a> {
    pub jsonrpc: &'static str,
    pub id: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Serialize)]
pub struct Request<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: serde_json::Value,
}

#[derive(Serialize)]
pub struct Notification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    pub params: serde_json::Value,
}

#[derive(Serialize)]
pub struct Error {
    pub code: i64,
    pub message: String,
}

impl Error {
    pub fn method_not_found(method: &str) -> Self {
        Self { code: -32601, message: format!("{method} is not implemented") }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self { code: -32602, message: message.into() }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self { code: -32603, message: message.into() }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSession {
    pub cwd: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub session_id: String,
    #[serde(default)]
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ResourceLink {
        uri: String,
    },
    #[serde(other)]
    Other,
}

impl ContentBlock {
    /// What the model should see. Non-text blocks are named rather than dropped,
    /// so a prompt that was mostly an attachment does not arrive empty.
    pub fn render(&self) -> Option<String> {
        match self {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::ResourceLink { uri } => Some(format!("[attached: {uri}]")),
            ContentBlock::Other => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRef {
    pub session_id: String,
}

/// `session/update` payloads. The client renders these as the turn happens.
pub fn agent_message_chunk(session: &str, text: &str) -> serde_json::Value {
    update(
        session,
        serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text },
        }),
    )
}

pub fn agent_thought_chunk(session: &str, text: &str) -> serde_json::Value {
    update(
        session,
        serde_json::json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": text },
        }),
    )
}

pub fn tool_call(session: &str, id: &str, title: &str, kind: &str) -> serde_json::Value {
    update(
        session,
        serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title,
            "kind": kind,
            "status": "in_progress",
        }),
    )
}

pub fn tool_call_done(session: &str, id: &str, failed: bool) -> serde_json::Value {
    update(
        session,
        serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": if failed { "failed" } else { "completed" },
        }),
    )
}

fn update(session: &str, mut body: serde_json::Value) -> serde_json::Value {
    body["sessionId"] = serde_json::Value::String(session.to_string());
    body
}

/// Which of Rook's tools maps to which ACP tool kind, so an editor can show the
/// right icon and grouping.
pub fn tool_kind(name: &str) -> &'static str {
    match name {
        "read_file" | "list_dir" => "read",
        "write_file" | "edit_file" => "edit",
        "search" => "search",
        "run_command" => "execute",
        "delegate" => "think",
        _ if name.contains("__") => "other",
        _ => "other",
    }
}

/// The approval modes, as an editor offers them.
///
/// The same three the CLI and the config have, so switching from an editor's
/// menu and editing `sandbox.mode` reach the same policy.
pub fn modes(current: rook_tools::policy::Mode) -> serde_json::Value {
    serde_json::json!({
        "currentModeId": mode_id(current),
        "availableModes": [
            { "id": "auto", "name": "Auto",
              "description": "Run anything the deny list does not forbid, without asking." },
            { "id": "ask", "name": "Ask",
              "description": "Ask before anything that changes the machine." },
            { "id": "readonly", "name": "Read only",
              "description": "Nothing that changes the machine runs at all." },
        ],
    })
}

pub fn mode_id(mode: rook_tools::policy::Mode) -> &'static str {
    match mode {
        rook_tools::policy::Mode::Auto => "auto",
        rook_tools::policy::Mode::Ask => "ask",
        rook_tools::policy::Mode::ReadOnly => "readonly",
    }
}

pub fn mode_from_id(id: &str) -> Option<rook_tools::policy::Mode> {
    match id {
        "auto" => Some(rook_tools::policy::Mode::Auto),
        "ask" => Some(rook_tools::policy::Mode::Ask),
        "readonly" => Some(rook_tools::policy::Mode::ReadOnly),
        _ => None,
    }
}

/// Told to the editor when the mode changes for any other reason, so its menu
/// does not drift from what the policy is actually doing.
pub fn current_mode_update(session: &str, mode: rook_tools::policy::Mode) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session,
        "update": { "sessionUpdate": "current_mode_update", "currentModeId": mode_id(mode) },
    })
}

/// The session settings an editor can offer as controls.
///
/// The spec prefers these to `modes` and says modes will be removed, so both
/// are sent: an older client renders the modes, a newer one these.
pub fn config_options(mode: rook_tools::policy::Mode, effort: rook_llm::Effort) -> serde_json::Value {
    serde_json::json!([
        {
            "id": "mode",
            "name": "Approvals",
            "description": "What the agent may do without asking.",
            "category": "mode",
            "type": "select",
            "currentValue": mode_id(mode),
            "options": [
                { "value": "auto", "name": "Auto",
                  "description": "Run anything the deny list does not forbid, without asking." },
                { "value": "ask", "name": "Ask",
                  "description": "Ask before anything that changes the machine." },
                { "value": "readonly", "name": "Read only",
                  "description": "Nothing that changes the machine runs at all." },
            ],
        },
        {
            "id": "effort",
            "name": "Reasoning effort",
            "description": "How much the model may think before answering.",
            "category": "reasoning",
            "type": "select",
            "currentValue": effort.as_str(),
            "options": [
                { "value": "low", "name": "Low", "description": "Fastest, for mechanical work." },
                { "value": "medium", "name": "Medium", "description": "A balance." },
                { "value": "high", "name": "High", "description": "The default." },
                { "value": "xhigh", "name": "Extra high", "description": "Suits most coding work." },
                { "value": "max", "name": "Max", "description": "Slowest, for the hardest problems." },
            ],
        },
    ])
}
