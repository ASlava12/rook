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

#[derive(Debug, Serialize, Deserialize)]
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
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        uri: Option<String>,
    },
    Resource {
        resource: EmbeddedResource,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    pub uri: String,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub blob: Option<String>,
}

impl Prompt {
    pub fn content(self) -> Result<(String, Vec<rook_proto::Attachment>), Error> {
        use base64::Engine;
        let mut text = Vec::new();
        let mut attachments = Vec::new();
        for block in self.prompt {
            match block {
                ContentBlock::Text { text: said } => text.push(said),
                ContentBlock::Image { data, mime_type, uri } => {
                    attachments.push(rook_proto::Attachment::Image {
                        name: uri.unwrap_or_else(|| "ACP image".into()),
                        mime_type,
                        data,
                    })
                }
                ContentBlock::ResourceLink { uri } => attachments.push(rook_proto::Attachment::Text {
                    name: "resource link (reference only; not fetched)".into(),
                    text: uri,
                }),
                ContentBlock::Resource { resource } => {
                    let mime = resource.mime_type.unwrap_or_else(|| "application/octet-stream".into());
                    match (resource.text, resource.blob) {
                        (Some(body), None) => {
                            attachments.push(rook_proto::Attachment::Text { name: resource.uri, text: body })
                        }
                        (None, Some(data)) if mime.starts_with("image/") => {
                            attachments.push(rook_proto::Attachment::Image {
                                name: resource.uri,
                                mime_type: mime,
                                data,
                            })
                        }
                        (None, Some(data))
                            if mime.starts_with("text/")
                                || matches!(mime.as_str(), "application/json" | "application/xml") =>
                        {
                            if data.len() > rook_core::attachments::MAX_TEXT_BYTES.div_ceil(3) * 4 {
                                return Err(Error::invalid_params("embedded text exceeds 256 KiB"));
                            }
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(data)
                                .map_err(|e| Error::invalid_params(e.to_string()))?;
                            let text = String::from_utf8(bytes)
                                .map_err(|_| Error::invalid_params("embedded text must be UTF-8"))?;
                            attachments.push(rook_proto::Attachment::Text { name: resource.uri, text });
                        }
                        _ => {
                            return Err(Error::invalid_params(
                                "embed UTF-8 text or a PNG, JPEG, WebP or GIF; unsupported resource content",
                            ));
                        }
                    }
                }
                ContentBlock::Other => {
                    return Err(Error::invalid_params(
                        "unsupported content block; use text, image, resource or resource_link",
                    ));
                }
            }
            if attachments.len() > rook_core::attachments::MAX_ATTACHMENTS {
                return Err(Error::invalid_params("at most 4 attachments per turn"));
            }
        }
        let text = text.join("\n");
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(Error::invalid_params("the prompt has no content"));
        }
        Ok((text, attachments))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRef {
    pub session_id: String,
}

/// `session/update` payloads. The client renders these as the turn happens.
/// One streamed piece of what the agent is saying, and which message it is part
/// of.
///
/// `message` is how a reader tells one from the next: the protocol says every
/// chunk of a message carries the same id and that a change in it means a new
/// message has started. Sending none at all — which this did — leaves an editor
/// with a turn's thinking, its answer and its next thinking as one unbroken run
/// of text. Found in opencode's own fix for it, where the id was the message's
/// rather than the part's and two thoughts merged into one.
pub fn agent_message_chunk(session: &str, text: &str, message: &str) -> serde_json::Value {
    update(
        session,
        serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": message,
            "content": { "type": "text", "text": text },
        }),
    )
}

/// The same, for what the agent is working out rather than saying.
pub fn agent_thought_chunk(session: &str, text: &str, message: &str) -> serde_json::Value {
    update(
        session,
        serde_json::json!({
            "sessionUpdate": "agent_thought_chunk",
            "messageId": message,
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

/// The envelope every `session/update` goes in.
///
/// `{ sessionId, update }`, with the update nested — which is what
/// `SessionNotification` is, two fields and no flattening. This put the
/// update's own fields beside `sessionId` instead, so a client that
/// deserialises to the schema's type found no `update` at all and every stream
/// this agent produced was unreadable to it. The codebase disagreed with itself
/// about it: `current_mode_update` was written later, by hand, and got it
/// right, which is how this was found. One function now, so there is one answer
/// rather than two.
fn update(session: &str, body: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "sessionId": session, "update": body })
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
/// menu and editing `sandbox.stance` reach the same policy.
pub fn modes(current: rook_tools::policy::Stance) -> serde_json::Value {
    serde_json::json!({
        "currentModeId": mode_id(current),
        "availableModes": rook_tools::policy::Stance::ALL
            .map(|s| serde_json::json!({ "id": s.as_str(), "name": s.title(), "description": s.describe() })),
    })
}

pub fn mode_id(mode: rook_tools::policy::Stance) -> &'static str {
    mode.as_str()
}

pub fn mode_from_id(id: &str) -> Option<rook_tools::policy::Stance> {
    rook_tools::policy::Stance::parse(id)
}

/// Told to the editor when the mode changes for any other reason, so its menu
/// does not drift from what the policy is actually doing.
pub fn current_mode_update(session: &str, mode: rook_tools::policy::Stance) -> serde_json::Value {
    update(
        session,
        serde_json::json!({ "sessionUpdate": "current_mode_update", "currentModeId": mode_id(mode) }),
    )
}

/// The session settings an editor can offer as controls.
///
/// The spec prefers these to `modes` and says modes will be removed, so both
/// are sent: an older client renders the modes, a newer one these.
pub fn config_options(mode: rook_tools::policy::Stance, effort: rook_llm::Effort) -> serde_json::Value {
    serde_json::json!([
        {
            "id": "mode",
            "name": "Approvals",
            "description": "What the agent may do without asking.",
            "category": "mode",
            "type": "select",
            "currentValue": mode_id(mode),
            "options": rook_tools::policy::Stance::ALL
                .map(|s| serde_json::json!({ "value": s.as_str(), "name": s.title(), "description": s.describe() })),
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
