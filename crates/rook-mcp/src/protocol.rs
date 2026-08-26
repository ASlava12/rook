use serde::{Deserialize, Serialize};

/// The protocol revision Rook speaks. A server that answers with a different one
/// still works — the subset used here has been stable across revisions — but the
/// mismatch is worth reporting.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Serialize)]
pub struct Request<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct Notification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// An incoming line. Servers send responses and notifications down the same
/// pipe, and a notification carries no `id`, which is how they are told apart.
#[derive(Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ServerInfo {
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default, rename = "serverInfo")]
    pub server: Implementation,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Implementation {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema, passed to the model untouched — Rook never needs to
    /// understand it, only to forward it.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        resource: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

impl ToolResult {
    /// Flatten to what a model can read. Binary parts are described rather than
    /// inlined: a base64 image in the transcript is thousands of useless tokens.
    pub fn to_text(&self) -> String {
        self.content
            .iter()
            .map(|c| match c {
                Content::Text { text } => text.clone(),
                Content::Image { mime_type, data } => {
                    format!("[{mime_type} image, {} bytes base64]", data.len())
                }
                Content::Resource { resource } => resource.to_string(),
                Content::Unknown => "[unsupported content]".into(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
