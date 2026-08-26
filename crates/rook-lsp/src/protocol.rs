//! The slice of the Language Server Protocol an agent needs.
//!
//! Framing is `Content-Length: N\r\n\r\n<json>`, not the newline-delimited JSON
//! MCP and ACP use — a server that receives a bare line simply never answers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A place in the workspace, with the path already turned back into something
/// printable — a `file://` URI in a tool result is noise the model has to parse.
#[derive(Clone, Debug, Serialize)]
pub struct Location {
    pub path: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Diagnostic {
    #[serde(default)]
    pub range: Range,
    #[serde(default)]
    pub severity: Option<u8>,
    #[serde(default)]
    pub source: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn severity_name(&self) -> &'static str {
        match self.severity {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            Some(4) => "hint",
            _ => "diagnostic",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub path: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

/// LSP numbers symbol kinds; the names are what a person and a model read.
pub fn symbol_kind(kind: u64) -> &'static str {
    const NAMES: [&str; 26] = [
        "file",
        "module",
        "namespace",
        "package",
        "class",
        "method",
        "property",
        "field",
        "constructor",
        "enum",
        "interface",
        "function",
        "variable",
        "constant",
        "string",
        "number",
        "boolean",
        "array",
        "object",
        "key",
        "null",
        "enum-member",
        "struct",
        "event",
        "operator",
        "type-parameter",
    ];
    NAMES.get(kind.saturating_sub(1) as usize).copied().unwrap_or("symbol")
}

pub fn to_uri(path: &std::path::Path) -> String {
    // Good enough for local absolute paths, which is all a workspace holds.
    format!("file://{}", path.display())
}

pub fn from_uri(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}
